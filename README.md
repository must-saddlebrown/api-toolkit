# api-toolkit

CLI Rust yang menjalankan tiga pemeriksaan non-fungsional terhadap sebuah API dengan **satu command**, lalu menghasilkan bukti, verdict, report JSON, dan exit code:

1. **brute_force**: apakah login menolak tebakan password berulang dengan proteksi (mis. HTTP 429) tepat waktu dan tetap protected sampai akhir.
2. **race**: apakah aksi write-once hanya menghasilkan satu side effect saat dikirim bersamaan.
3. **load**: apakah endpoint penting tetap di bawah batas error rate dan p95 latency pada beban paralel terbatas.

Rencana: [docs/PLAN.md](docs/PLAN.md). Keputusan: [docs/DECISIONS.md](docs/DECISIONS.md). Status: [docs/STATUS.md](docs/STATUS.md). Fixture lokal: [docs/fixture.md](docs/fixture.md).

## Status

| Bagian | Status |
| --- | --- |
| Suite lengkap (`check` tanpa `--only`) | Berfungsi, dibuktikan terhadap fixture lokal (`cargo test --test fixture_proof`) |
| Run parsial (`check --only <check>`) | Berfungsi, tidak pernah exit 0 |
| Dijalankan terhadap API nyata | **Belum**. Menunggu target dan config dari pengguna (lihat "Menjalankan terhadap API nyata") |

## Satu command

```sh
api-toolkit check --config <config.json> --out <report.json>
```

(dari repo: `cargo run -q -- check --config ... --out ...`). Tanpa `--only`, tool menjalankan ketiga check **berurutan: race, load, brute_force** (brute_force terakhir karena bisa mengunci akun). Check berikutnya tetap dijalankan walau check sebelumnya FAIL, ERROR, atau INCONCLUSIVE. Satu `suite_deadline_ms` berlaku untuk seluruh suite. Ctrl-C menghentikan run: check yang sedang berjalan dan yang belum jalan menjadi `not_run` dengan reason `cancelled` (load: `cancelled` + `workload_incomplete`) tanpa request baru. Pengecualian: brute_force yang sudah melihat password salah diterima tetap FAIL `invalid_credentials_accepted` (+ `cancelled`, `incomplete`). Check yang sudah selesai mempertahankan verdict-nya, dan report tetap ditulis.

Output: tepat satu baris console per check (urutan race, load, brute_force) di stdout, report JSON di `--out` (ditulis atomic, file lama diganti utuh), dan exit code.

```
race PASS delta=1 before=0 after=1 actions=5/5 skew=1/100ms reasons=single_side_effect
load PASS error_rate=0/0.01 p95=3/500ms attempted=100/100 samples=100 max_in_flight=10/10 timeouts=0 status=200:100 errors=none reasons=thresholds_met
brute_force PASS first_protected=6 tail=3/2 attempts=8/8 reasons=protection_observed
```

Baris console dicetak setelah seluruh suite selesai, jadi suite yang lama tidak menampilkan apa pun sampai selesai.

Command lain: `api-toolkit validate --config <config.json> [--only <check>]` memeriksa config dan bahwa env var terisi, **tanpa mengirim request**. Mencetak jumlah request terencana, total jeda tetap, dan nama (bukan nilai) env var. `check --help` dan `validate --help` menjelaskan opsinya.

## Verdict, reason, dan exit code

Verdict per check:

| Verdict | Arti |
| --- | --- |
| `pass` | Oracle terbukti dan workload lengkap |
| `fail` | Pelanggaran terbukti (proteksi tidak ada/telat, duplikat side effect, threshold terlampaui) |
| `inconclusive` | Precondition tidak terpenuhi atau hasil ambigu, jadi tidak bisa menyatakan pass maupun fail |
| `error` | Verifier/respons tidak bisa dipakai atau transport error (config tidak valid berhenti di exit 2 sebelum ada verdict) |
| `not_run` | Tidak dijalankan atau terhenti (cancel, deadline, `--only`) |

Setiap check juga punya `completion` (`complete` atau `incomplete`). PASS hanya sah bila `complete`. Tool tidak pernah menyatakan PASS bila ada yang tidak bisa dibuktikan (timeout, workload tidak lengkap, respons tak terklasifikasi, cancel).

Reason code (snake_case, bagian dari kontrak report; `report_version` 1):

| Check | Reason |
| --- | --- |
| brute_force | `protection_observed` (pass); `protection_policy_not_met`, `invalid_credentials_accepted` (fail); `account_already_locked` (inconclusive); `baseline_failed`, `probe_unclassifiable`, `overlapping_rules` (error/inconclusive) |
| race | `single_side_effect` (pass); `duplicate_side_effect`, `action_not_executed` (fail); `initial_counter_not_zero`, `action_outcome_ambiguous`, `dispatch_incomplete`, `dispatch_skew_exceeded` (inconclusive); `verifier_failed`, `counter_invalid` (error) |
| load | `thresholds_met` (pass); `error_rate_exceeded`, `latency_exceeded` (fail); `target_not_ready`, `workload_incomplete` (inconclusive/not_run); `preflight_failed`, `runner_error` (error) |
| umum | `transport_error`, `not_started`, `cancelled`, `deadline_exceeded`, `excluded_by_only` |

Exit code suite lengkap (PLAN §10, diimplementasikan di `report::exit_code`):

| Exit | Arti |
| --- | --- |
| 0 | Ketiga check `pass`, `complete`, tanpa reason non-pass. **Satu-satunya jalur exit 0** |
| 1 | Ada minimal satu `fail`, dan tidak ada check `error`, `inconclusive`, `not_run`, atau `incomplete` |
| 2 | Ada `error`, `inconclusive`, `not_run`, atau `incomplete` di check mana pun (walau ada FAIL lain), config atau env tidak valid, `--out` tidak layak, atau report gagal ditulis |
| 130 | Ctrl-C datang saat suite berjalan, termasuk bila report gagal ditulis. Ctrl-C yang sangat awal (sebelum handler terdaftar) mematikan proses tanpa report (shell tetap melihat status 130); Ctrl-C setelah semua check selesai mengikuti verdict, bukan 130 (lihat Keterbatasan) |

Presedensi: cancel (130) mengalahkan error/inconclusive/incomplete (2), yang mengalahkan fail (1), yang mengalahkan pass (0). FAIL yang pasti tetap terlihat di console dan report walau exit-nya 2. Config atau env yang tidak valid menghasilkan exit 2 dengan nol request dan `--out` tidak disentuh.

### Report tidak tertulis

Bila report gagal dibangun atau ditulis, baris check tetap dicetak, lalu stdout memuat baris `report NOT WRITTEN: <path>` dan stderr memuat error. Exit-nya 2 (130 bila dibatalkan), tidak pernah 0, dan isi `--out` yang lama tidak berubah. Jangan menganggap run itu tersimpan.

### Report JSON

Berisi `report_version` (1), `run_id`, `tool_version`, `config_hash` (SHA-256 file config), `started_at`/`finished_at` (RFC3339 UTC, tanpa pecahan detik), `completion`, `scope` (`full` atau `partial`), dan `checks` (urutan race, load, brute_force; masing-masing `verdict`, `reasons`, `completion`, `evidence`). Evidence hanya angka, enum, bool, dan null: tidak pernah body respons, nilai header, nilai env, atau URL ber-secret.

## `--only`: run parsial

```sh
api-toolkit check --config <config.json> --out <report.json> --only race   # atau load, brute_force
```

Menjalankan satu check saja. Check lain dicatat `not_run [excluded_by_only]` dan report memakai `scope: "partial"`. Bagian config lain yang ada tetap harus berbentuk benar, tetapi tidak divalidasi semantik, env-nya tidak wajib, dan tidak masuk jumlah request. **Run parsial tidak pernah exit 0**: FAIL selesai → 1, PASS dan hasil lain → 2, cancel → 130. Baca verdict PASS dari baris console atau report. Stderr memuat catatan `partial run`.

Berguna untuk target nyata, mis. menjalankan brute_force terpisah karena risiko lockout.

## Konfigurasi

JSON, `config_version: 1`. Contoh lengkap dan valid: [examples/core-checks.json](examples/core-checks.json) (fixture healthy), [examples/core-checks-broken.json](examples/core-checks-broken.json) (tiga path rusak). Contoh satu check: `examples/brute-force-only.json`, `race-only.json`, `load-only.json`. Field yang tidak dikenal ditolak. Suite lengkap mewajibkan bagian `brute_force`, `race`, dan `load`.

### Secret: hanya lewat `${ENV:NAME}`

Nilai sensitif (username, password, id resource, token) ditulis di config sebagai string utuh `${ENV:NAME}` dan dibaca dari environment variable saat run. Env yang hilang atau kosong membuat run ditolak (exit 2, nol request). Nilainya tidak pernah dicetak, dilog, atau masuk report. Placeholder di posisi yang tidak di-resolve (mis. `base_url`, `path`, matcher) ditolak. Contoh nama env di config contoh: `LOGIN_USER`, `LOGIN_VALID_PASSWORD`, `LOGIN_INVALID_PASSWORD`, `TEST_ACTION_ID`, `TEST_AUTHORIZATION`. Nama bebas, sesuaikan dengan target Anda.

### `limits`

| Field | Arti |
| --- | --- |
| `request_timeout_ms` | Timeout total per request (termasuk membaca body) |
| `suite_deadline_ms` | Satu deadline untuk seluruh suite. Melewatinya: check yang berjalan dan sisanya `not_run` dengan reason `deadline_exceeded` (load: `deadline_exceeded` + `workload_incomplete`, kecuali bila dibatalkan sebelum preflight dijawab: hanya `deadline_exceeded`), exit 2. Total jeda tetap (interval brute_force + `settle_ms`) yang melebihinya ditolak saat validasi |
| `response_body_limit_bytes` | Batas body respons yang dibaca; body lebih besar menjadi transport error `body_too_large` (di load: pada preflight menjadi ERROR `preflight_failed`, pada sampel dihitung sampel gagal) |
| `connect_timeout_ms` | Timeout connect |

Seluruh objek `limits` opsional, dan tiap field punya default: `connect_timeout_ms` 3000, `request_timeout_ms` 5000, `suite_deadline_ms` 120000, `response_body_limit_bytes` 1048576.

### Threshold brute_force

Alur: satu login **baseline** dengan kredensial benar (harus sukses), lalu `attempts` login **probe** dengan password salah (selalu semuanya dikirim) dengan jeda `interval_ms`.

| Field | Arti |
| --- | --- |
| `baseline_request`, `probe_request` | Login benar dan login salah |
| `success_match` | Respons login berhasil. Pada probe berarti kredensial salah diterima: FAIL |
| `rejection_match` | Penolakan biasa (belum ada proteksi) |
| `protection_match` | Respons proteksi (rate limit atau lockout). `status_in` ketiga matcher tidak boleh beririsan |
| `attempts` | Jumlah probe, 2..=100 |
| `interval_ms` | Jeda antar probe |
| `protect_by_attempt` | Proteksi harus muncul paling lambat di attempt ini (2..=`attempts`) |
| `protected_tail_count` | Minimal sekian attempt terakhir harus protected, berurutan (1..=`attempts`-1) |

Baseline sudah protected atau probe 1 sudah protected = INCONCLUSIVE `account_already_locked` (reset/ganti akun). Baseline ditolak = INCONCLUSIVE `baseline_failed`.

### Threshold race

Alur: verifier dibaca (counter harus 0), `concurrency` request aksi dikirim bersamaan, tunggu `settle_ms`, verifier dibaca lagi. PASS hanya bila counter naik tepat `expected_delta` (1).

| Field | Arti |
| --- | --- |
| `action_request` | Aksi write-once. `{nama}` di `path` diisi dari `path_params` |
| `verify_request`, `verify_match` | Request pembaca counter dan matcher respons yang sah |
| `counter_pointer` | JSON Pointer ke counter integer di respons verifier |
| `initial_counter` | Harus 0 (jika tidak: INCONCLUSIVE `initial_counter_not_zero`) |
| `expected_delta` | Harus 1 |
| `concurrency` | Jumlah request bersamaan, 2..=32 |
| `expected_action_statuses` | Status aksi yang sah (mis. `[200, 409]`); status lain atau timeout = ambigu (INCONCLUSIVE). 202 ditolak |
| `max_dispatch_skew_ms` | Selisih waktu kirim terbesar antar request yang masih dianggap bersamaan; lebih besar = INCONCLUSIVE `dispatch_skew_exceeded` |
| `settle_ms` | Jeda setelah semua aksi selesai sebelum verifier kedua |

### Threshold load

Alur: **1 preflight** (request yang sama dengan load, tidak dihitung sampel), lalu `requests` request dengan paling banyak `concurrency` berjalan bersamaan (closed-loop). Total 1+N request.

| Field | Arti |
| --- | --- |
| `request`, `response_match` | Endpoint dan respons yang dihitung sukses; selain itu (status lain, body tak cocok, timeout, transport error) dihitung gagal |
| `requests` | Jumlah request load, maksimum 1000 |
| `concurrency` | Request bersamaan, maksimum 32 |
| `max_error_rate` | 0..=1. `gagal/dicoba` di atasnya = FAIL `error_rate_exceeded`; sama dengan batas lolos |
| `max_p95_ms` | p95 (nearest-rank, semua sampel termasuk gagal) di atasnya = FAIL `latency_exceeded`; sama dengan batas lolos |

Pooling koneksi dimatikan: setiap request membuka koneksi baru, jadi latency mencakup connect (dan TLS handshake).

## Aturan keselamatan tool

- Tanpa retry otomatis, tanpa mengikuti redirect, verifikasi TLS selalu aktif, proxy dari env diabaikan.
- Satu request = satu koneksi. Jumlah request terencana ada di output `validate`.
- Config tidak valid, env hilang, atau `--out` tidak layak = nol request.

## Setup state dan akun yang dibutuhkan target

- **brute_force**: pakai **akun uji khusus** dengan kredensial benar yang diketahui dan belum terkunci. Check ini sengaja memicu lockout/rate limit pada akun itu. Limiter yang dipakai bersama endpoint lain (per IP, global) bisa ikut terpengaruh.
- **race**: pakai **resource write-once baru** (id yang belum pernah dieksekusi) dengan counter awal 0 yang bisa dibaca verifier. Aksi dijalankan tepat sekali pada state target dan tidak bisa diulang pada id yang sama (run berikutnya butuh id baru atau reset, kalau tidak hasilnya INCONCLUSIVE `initial_counter_not_zero`).
- **load**: endpoint yang aman dipanggil 1+N kali. Pakai identitas terpisah dari akun brute_force dan dari race bila bisa, supaya lockout atau limiter tidak mencemari load.
- Mengulang suite pada fixture lokal wajib didahului reset (lihat di bawah). Pada target nyata, siapkan state awal baru secara manual; tool tidak pernah mereset apa pun.

## Fixture proof: bukti terhadap fixture lokal

Semua jaringan yang dipakai hanya `127.0.0.1`.

Satu perintah (fixture di port acak, binary nyata, tanpa env var, tanpa fixture yang sudah berjalan):

```sh
cargo test --test fixture_proof
```

Membuktikan: suite healthy exit 0; tiga varian rusak exit 1 dengan reason yang diharapkan; satu rusak + dua healthy tetap menampilkan ketiga verdict; ERROR di satu check tidak menyembunyikan FAIL di check lain (exit 2); mengulang tanpa reset tidak pernah PASS palsu (exit 2); setelah reset hasilnya sama dengan run pertama; report valid dan atomic; secret tidak bocor; SIGINT menghasilkan exit 130 dengan report.

Manual (dari root repo, dua terminal):

```sh
# terminal 1
cargo run --example fixture                 # 127.0.0.1:8080, Ctrl-C untuk berhenti

# terminal 2
export LOGIN_USER=demo LOGIN_VALID_PASSWORD=demo-pass LOGIN_INVALID_PASSWORD=wrong-pass
export TEST_ACTION_ID=act-1 TEST_AUTHORIZATION='Bearer fixture-token'

curl -X POST http://127.0.0.1:8080/__admin/reset
cargo run -q -- validate --config examples/core-checks.json
cargo run -q -- check --config examples/core-checks.json --out /tmp/suite.json; echo $?          # healthy: 0

curl -X POST http://127.0.0.1:8080/__admin/reset
cargo run -q -- check --config examples/core-checks-broken.json --out /tmp/broken.json; echo $?  # tiga FAIL: 1
```

**Reset fixture sebelum tiap suite.** Tanpa reset, race INCONCLUSIVE `initial_counter_not_zero` dan brute_force INCONCLUSIVE `account_already_locked` pada config healthy (exit 2); counter flaky pada load juga bergeser. Nilai env di atas adalah konstanta dev publik, hanya untuk fixture. Daftar rute, varian rusak lain, dan detailnya: [docs/fixture.md](docs/fixture.md). Bila fixture dijalankan di port selain 8080, ubah `base_url` di salinan config.

Gate kualitas:

```sh
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets
```

## Menjalankan terhadap API nyata

**Belum dilakukan.** Tool tidak mengarang target, endpoint, kredensial, atau counter. Anda menyediakan config; saat ini validasi API nyata menunggu config itu. Input yang harus Anda siapkan (PLAN §14):

1. **Base URL** target (`scheme://host[:port]`, boleh memuat base path; tanpa userinfo/query/fragment).
2. **Login**: request baseline (kredensial benar) dan probe (password salah), matcher `success_match`/`rejection_match`/`protection_match` sesuai respons nyata, serta policy proteksi (`attempts`, `interval_ms`, `protect_by_attempt`, `protected_tail_count`).
3. **Aksi write-once baru**: request aksi, resource/id baru, **verifier counter** (request pembaca, `verify_match`, `counter_pointer`), status aksi yang sah, `concurrency`, `max_dispatch_skew_ms`, `settle_ms`.
4. **Endpoint load** dan threshold: `request`, `response_match`, `requests`, `concurrency`, `max_error_rate`, `max_p95_ms`.
5. **Nama env var secret** untuk semua nilai sensitif, diisi di shell Anda (`export ...`), tidak ditulis di config.

Urutan yang disarankan: `validate` dulu (nol request), lalu `--only` per check (terutama brute_force), baru suite lengkap.

Peringatan efek samping:

- **brute_force bisa mengunci akun** (lockout) atau memicu rate limit yang menetap. Pakai akun uji khusus. Semua probe selalu dikirim, termasuk setelah proteksi muncul.
- **race mengubah state target satu kali** pada resource yang dipakai dan tidak bisa diulang pada resource yang sama. Aksi yang timeout mungkin tetap dieksekusi server.
- **load mengirim 1+N request**. Preflight memakai request yang sama dengan load, jadi endpoint non-idempoten ikut terkena sekali tambahan.
- **Tidak ada retry otomatis**: request yang gagal tidak diulang dan dihitung apa adanya.
- **Jangan arahkan ke produksi tanpa izin eksplisit.** Hanya uji sistem yang Anda miliki atau yang Anda punya izin tertulis untuk diuji.

## Keterbatasan yang diketahui

- Proteksi brute_force yang hilang di tengah run lalu muncul lagi di ekor tetap `pass` (PLAN §5 hanya menuntut attempt protected pertama tepat waktu dan ekor protected).
- Status "pending" yang hanya ditandai di body aksi tidak terdeteksi (aksi dianggap sinkron). Request aksi yang di-drop saat stop mungkin sudah dieksekusi server.
- Dispatch race dan load berjalan di satu task, jadi skew terukur sangat kecil dibanding klien terdistribusi.
- Cancel sebelum handler SIGINT terdaftar mematikan proses tanpa report (belum ada request yang terkirim).
- Ctrl-C setelah check terakhir selesai (saat console/report ditulis) tidak dipantau lagi: exit mengikuti verdict (0/1/2), bukan 130.
- Detail lain: [docs/STATUS.md](docs/STATUS.md) bagian Known limitations.
