# api-toolkit

CLI Rust yang menjalankan tiga pemeriksaan non-fungsional terhadap sebuah API (proteksi brute-force login, race condition pada aksi write-once, beban paralel terbatas) dan menghasilkan bukti, verdict, report JSON, dan exit code. Rencana lengkap: [docs/PLAN.md](docs/PLAN.md). Status terkini: [docs/STATUS.md](docs/STATUS.md).

Dokumen ini hanya membahas **check brute-force login** (fase P1).

## Status

| Bagian | Status |
| --- | --- |
| Logika check brute-force (`checks::brute_force::run`) | Selesai, teruji (unit dan integrasi terhadap fixture) |
| `api-toolkit validate --config ...` | Berfungsi, memvalidasi config termasuk bagian `brute_force` |
| `api-toolkit check ...` | **Masih stub**: selalu exit 2, tidak mengirim request, tidak menulis report. Penyambungan ke CLI dan penulisan report datang di P4 |
| Check race dan load | Belum (P2, P3) |
| Dijalankan terhadap API nyata | Belum. Hanya fixture lokal yang sudah dibuktikan |

Artinya saat ini check brute-force dijalankan lewat test, bukan lewat perintah `check`.

## Apa yang dibuktikan check ini

Pertanyaannya: **apakah login menolak tebakan password berulang dengan proteksi (misalnya HTTP 429) tepat waktu, dan tetap protected sampai akhir?**

Urutan kerja (berurutan, tidak paralel):

1. **Baseline**: satu login dengan kredensial benar. Harus sukses. Ini memastikan akun belum terkunci dan config benar.
2. **Probe 1..N** (`attempts`, default contoh 8): login dengan password salah, jeda `interval_ms` di antara probe (tidak ada jeda setelah probe terakhir).
3. Setiap respons diklasifikasi memakai matcher dari config: `success_match`, `rejection_match`, `protection_match`.
4. Verdict ditentukan dari attempt pertama yang protected dan apakah ekor (`protected_tail_count` attempt terakhir) tetap protected.

Aturan keselamatan yang berlaku:

- Tidak ada retry otomatis dan tidak ada redirect yang diikuti. Satu request = satu koneksi.
- Setelah baseline sukses, **semua probe selalu dikirim** (workload tetap), termasuk setelah proteksi muncul. Di akun nyata ini memperpanjang lockout.
- Bila baseline gagal, tidak ada probe yang dikirim.
- Tidak pernah PASS bila ada yang tidak bisa dibuktikan: timeout, respons tak terklasifikasi, jumlah probe tidak lengkap, atau cancel berujung `inconclusive`, `error`, atau `not_run`.

## Konfigurasi

Bagian `brute_force` di config v1 (contoh lengkap: [examples/core-checks.json](examples/core-checks.json)):

```json
"brute_force": {
  "baseline_request": {
    "method": "POST", "path": "/login",
    "json": {"username": "${ENV:LOGIN_USER}", "password": "${ENV:LOGIN_VALID_PASSWORD}"}
  },
  "probe_request": {
    "method": "POST", "path": "/login",
    "json": {"username": "${ENV:LOGIN_USER}", "password": "${ENV:LOGIN_INVALID_PASSWORD}"}
  },
  "success_match":   {"status_in": [200], "json_exists": ["/access_token"]},
  "rejection_match": {"status_in": [401]},
  "protection_match": {"status_in": [429]},
  "attempts": 8,
  "interval_ms": 100,
  "protect_by_attempt": 6,
  "protected_tail_count": 2
}
```

| Field | Arti |
| --- | --- |
| `baseline_request` | Login dengan kredensial benar. Harus cocok `success_match` |
| `probe_request` | Login dengan password salah, dikirim `attempts` kali |
| `success_match` | Respons yang berarti login berhasil. Pada probe ini adalah **kegagalan** check (kredensial salah diterima) |
| `rejection_match` | Respons penolakan biasa (belum ada proteksi) |
| `protection_match` | Respons proteksi (rate limit atau lockout) |
| `attempts` | Jumlah probe, 2..=100 |
| `interval_ms` | Jeda antar probe |
| `protect_by_attempt` | Proteksi harus muncul paling lambat di attempt ini (2..=`attempts`) |
| `protected_tail_count` | Minimal sekian attempt terakhir harus protected, berurutan (1..=`attempts`-1) |

Aturan lain:

- Secret **hanya** dari environment variable lewat `${ENV:NAME}`. Nilainya tidak pernah dicetak, dilog, atau masuk report.
- `status_in` ketiga matcher tidak boleh beririsan (ditolak saat `validate`).
- Attempt 1 harus rejection, jadi `protected_tail_count` maksimal `attempts - 1`.

## Verdict

Reason code memakai snake_case dan stabil (bagian dari kontrak report).

| Kondisi | Verdict | Reason |
| --- | --- | --- |
| Baseline sukses, probe 1 ditolak, proteksi mulai paling lambat `protect_by_attempt`, ekor protected | `pass` | `protection_observed` |
| Tidak pernah ada proteksi | `fail` | `protection_policy_not_met` |
| Proteksi telat, atau tidak bertahan sampai ekor | `fail` | `protection_policy_not_met` |
| Kredensial salah diterima (probe cocok `success_match`) | `fail` | `invalid_credentials_accepted` |
| Baseline sudah protected (akun terkunci sebelum test) | `inconclusive` | `account_already_locked` |
| Probe 1 sudah protected | `inconclusive` | `account_already_locked` |
| Baseline cocok `rejection_match` (kredensial benar ditolak) | `inconclusive` | `baseline_failed` |
| Baseline tak terklasifikasi atau transport error | `error` | `baseline_failed` (+ `transport_error`) |
| Probe tak terklasifikasi atau transport error | `error` | `probe_unclassifiable` (+ `transport_error`) |
| Lebih dari satu matcher cocok pada satu respons | `error` | `overlapping_rules` |
| Cancel atau deadline sebelum selesai | `not_run`, completion `incomplete` | `cancelled` atau `deadline_exceeded` |

Catatan:

- Bila beberapa kondisi terjadi sekaligus, urutan prioritas dan daftar reason lengkap ada di unit test tabel `m1_...` sampai `m23_...` di [src/checks/brute_force.rs](src/checks/brute_force.rs) dan di [docs/DECISIONS.md](docs/DECISIONS.md) (entri P1). Contoh: kredensial salah diterima **dan** timeout menghasilkan `fail` dengan kedua reason.
- Kredensial salah yang sudah terbukti diterima tetap `fail` walau ada cancel; completion-nya `incomplete`.
- Keterbatasan yang diketahui: proteksi yang hilang di tengah run lalu muncul lagi di ekor tetap `pass`, karena PLAN §5 hanya menuntut attempt protected pertama tepat waktu dan ekor protected.

## Evidence di report

Setiap check membawa `evidence` berisi **angka, enum, dan null saja**, tidak pernah body, header, atau token:

```json
{
  "attempts_planned": 8,
  "attempts_dispatched": 9,
  "interval_ms": 100,
  "protect_by_attempt": 6,
  "protected_tail_count": 2,
  "first_protected_attempt": 6,
  "protected_tail_observed": 3,
  "baseline": {"attempt": 0, "outcome": "success", "status": 200, "error": null, "offset_ms": 0, "elapsed_ms": 4},
  "attempts": [
    {"attempt": 1, "outcome": "rejection", "status": 401, "error": null, "offset_ms": 4, "elapsed_ms": 3}
  ]
}
```

(`attempts` dipotong di sini; isinya satu entri per probe yang selesai.) `attempts_dispatched` menghitung baseline dan request yang sedang berjalan saat stop; request itu tidak muncul di `attempts`.

Baris console untuk check ini:

```
brute_force PASS first_protected=6 tail=3/2 attempts=8/8
```

`tail` = jumlah protected berurutan yang teramati / `protected_tail_count`. `attempts` = probe yang selesai dan tercatat / direncanakan.

## Mencobanya

Semua dari root repo. Satu-satunya jaringan yang dipakai adalah fixture di `127.0.0.1`.

```sh
# Test check brute-force terhadap fixture (17 test, fixture di port acak, tanpa env var)
cargo test --test brute_force_fixture

# Seluruh gate kualitas
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets

# Validasi config contoh (tidak mengirim request)
cargo run --example fixture &      # terminal terpisah juga boleh
export LOGIN_USER=demo LOGIN_VALID_PASSWORD=demo-pass LOGIN_INVALID_PASSWORD=wrong-pass
export TEST_ACTION_ID=act-1 TEST_AUTHORIZATION='Bearer fixture-token'
cargo run -q -- validate --config examples/core-checks.json
```

### Fixture

[docs/fixture.md](docs/fixture.md) menjelaskan start, reset, dan semua rute. Yang relevan untuk check ini:

| Rute | Perilaku | Verdict yang diharapkan |
| --- | --- | --- |
| `POST /login` (healthy) | 401 untuk gagal ke-1..5, 429 mulai gagal ke-6 | `pass` |
| `POST /broken/login/unprotected` | Tidak pernah 429 | `fail` `protection_policy_not_met` |
| `POST /broken/login/accepts-invalid` | Password salah dijawab 200 + token | `fail` `invalid_credentials_accepted` |
| `/login` setelah 6 login salah | Baseline 429 (akun prelocked) | `inconclusive` `account_already_locked` |
| `GET /__admin/stats` | `{"login_requests", "load_requests"}` | untuk memverifikasi jumlah request |
| `POST /__admin/reset` | Mengembalikan state ke awal | jalankan sebelum mengulang check |

Login benar **tidak** mereset hitungan gagal pada fixture healthy. Menjalankan check dua kali tanpa reset membuat baseline kedua 429, sehingga hasilnya `inconclusive`. Itu perilaku yang disengaja.

## Kode dan test

| Bagian | Lokasi |
| --- | --- |
| Klasifikasi respons dan penentuan verdict (fungsi murni: `classify`, `evaluate`) | [src/checks/brute_force.rs](src/checks/brute_force.rs) |
| Loop pengiriman (`run_with`, `run`) dan baris console (`console_line`) | [src/checks/brute_force.rs](src/checks/brute_force.rs) |
| Cancel dan deadline (`Stop`) | [src/checks/mod.rs](src/checks/mod.rs) |
| Tipe evidence dan report | [src/report.rs](src/report.rs) |
| Unit test (tabel verdict M1-M23, waktu virtual `tokio::time::pause`) | `#[cfg(test)]` di [src/checks/brute_force.rs](src/checks/brute_force.rs) |
| Integration test terhadap fixture | [tests/brute_force_fixture.rs](tests/brute_force_fixture.rs) |
| Fixture | [tests/support/fixture.rs](tests/support/fixture.rs), dijalankan lewat [examples/fixture.rs](examples/fixture.rs) |

## Menjalankan terhadap API nyata

Belum dilakukan dan belum bisa lewat CLI (menunggu P4). Bila nanti dijalankan, ingat:

- Check ini **sengaja memicu lockout** pada akun yang dipakai. Pakai akun uji khusus, jangan akun orang lain.
- Kredensial berasal dari env var pengguna; tool tidak mengarang endpoint atau kredensial.
- Hanya jalankan terhadap sistem yang Anda miliki atau yang Anda punya izin eksplisit untuk diuji.
