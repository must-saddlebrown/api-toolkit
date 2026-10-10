# Status pengembangan

Rencana aktif: `docs/PLAN.md` (Small Core Proof, 9 Oktober 2026). Keputusan setelah rencana: `docs/DECISIONS.md`. Rencana lama ada di `docs/archive/` dan tidak dijalankan.

| Fase | Isi | Status | Catatan |
| --- | --- | --- | --- |
| P0 | Kontrak CLI, runner minimum, fixture | selesai (10 Oktober 2026) | Phase gate PASS; 20 dari 20 AC terpenuhi; 111 test hijau |
| P1 | Login brute-force protection check | selesai (10 Oktober 2026) | Phase gate PASS; 18 dari 18 AC terpenuhi; 185 test hijau |
| P2 | Write-once race check | selesai (10 Oktober 2026) | Phase gate PASS; 25 dari 25 AC terpenuhi; 303 test hijau |
| P3 | Parallel load check | belum dimulai | Siap: `/phase P3` |
| P4 | Single-command proof + satu API nyata | sebagian dimajukan (10 Oktober 2026) | `check --only brute_force` dan `--only race` selesai; suite lengkap, load, dan checkpoint review pengguna belum |

## Fase aktif

Tidak ada. P0, P1, dan P2 selesai; P3 siap dimulai. Sebagian P4 sudah dimajukan: `check --only brute_force` dan `check --only race` bisa dicoba (lihat "Mencoba `check --only`" dan DECISIONS 2026-10-10 "Majukan sebagian P4").

## Ringkasan AC P0

Semua P0-AC1 sampai P0-AC20 terpenuhi, dengan bukti berupa nama test atau output command di handoff `.claude/work/P0-plan.md` (Log, entri phase gate). Ringkas:

- Config v1 (`examples/core-checks.json`) parse; field tak dikenal, `config_version` selain 1, field wajib hilang, env hilang ditolak dengan nol request (AC1–AC9).
- Transport bersama: timeout, batas body streaming, tanpa redirect, tanpa retry, tanpa proxy dari environment, error disanitasi (AC10–AC11).
- Matcher `status_in`, `json_equals`, `json_exists` (AC12).
- Report v1: enum snake_case stabil dengan golden test, exit code sesuai PLAN §10, penulisan atomic (AC13–AC15).
- CLI `validate` dan stub `check` (selalu exit 2, tidak pernah 0) (AC5, AC16). Stub `check` kemudian digantikan oleh `check --only` (lihat bagian `check --only`).
- Fixture healthy lokal dengan start dan reset terdokumentasi (AC17–AC19); gate kualitas dan Commands di CLAUDE.md (AC20).

## Ringkasan AC P1

Semua P1-AC1 sampai P1-AC18 terpenuhi, dengan bukti nama test di handoff `.claude/work/P1-plan.md` (Log, entri phase gate). Ringkas:

- Check brute-force berurutan: 1 baseline + 8 probe, interval hanya di antara probe, tanpa retry (AC7–AC9).
- Verdict: healthy pass; unprotected dan accepts-invalid fail; akun sudah terkunci, baseline ditolak, probe 1 sudah protected = inconclusive; respons tak terklasifikasi, transport error, overlap matcher = error; cancel/deadline = not_run (AC1–AC6, AC13–AC16).
- Evidence per attempt (angka, enum, null; tanpa body/header/token) di report, golden P0 tidak berubah (AC12, AC17).
- Sentinel secret tidak muncul di report file, `Debug`, dan baris console (AC10–AC11).
- Fixture punya varian login rusak, `/__admin/stats`, dan dokumentasinya (AC18).

## Ringkasan AC P2

Semua P2-AC1 sampai P2-AC25 terpenuhi, dengan bukti nama test di handoff `.claude/work/P2-plan.md` (Log, entri phase gate). Ringkas:

- Check race: baca counter awal, lepas N action serentak (`join_all`, future lazy), tunggu `settle_ms`, baca counter akhir tepat sekali, tanpa retry (AC1, AC9, AC19).
- Verdict: delta 1 pass; delta 2 atau lebih fail `duplicate_side_effect`; delta 0 fail `action_not_executed`; counter awal bukan 0, outcome action ambigu, dispatch tidak lengkap, skew melebihi budget = inconclusive; counter hilang/bukan integer dan verifier gagal = error; cancel/deadline = not_run (AC2–AC7, AC12–AC18).
- Respons 202 ditolak saat `validate` (AC14).
- Evidence race (angka, enum, bool, null) di report; golden P0 dan brute_force tidak berubah (AC10, AC11).
- Sentinel secret tidak muncul di report file, `Debug`, console, stdout, dan stderr (AC20, AC23).
- Fixture: varian racy (sinkronisasi eksplisit, bukan timing), idempotent, `action_requests`/`verify_requests` di stats (AC8, AC25).
- CLI `check --only race` dan `validate --only race` (AC21, AC22, AC24).

## Cara mencoba

Semua command dijalankan dari root repo.

```sh
# Gate kualitas
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets            # 303 test, tanpa env var dan tanpa fixture berjalan

# Fixture healthy (terminal 1); reset dan detail ada di docs/fixture.md
cargo run --example fixture         # http://127.0.0.1:8080, Ctrl-C untuk berhenti
curl -X POST http://127.0.0.1:8080/__admin/reset   # reset state (alat dev)

# Validasi config contoh (terminal 2); nilai env ada di docs/fixture.md
export LOGIN_USER=demo LOGIN_VALID_PASSWORD=demo-pass LOGIN_INVALID_PASSWORD=wrong-pass
export TEST_ACTION_ID=act-1 TEST_AUTHORIZATION='Bearer fixture-token'
cargo run -q -- validate --config examples/core-checks.json   # exit 0, 117 request, 950 ms fixed waits

# Check brute force terhadap fixture lewat test
cargo test --test brute_force_fixture   # 17 test, fixture di port acak, tanpa env var
cargo test --test check_only_cli        # 21 test end-to-end CLI `check --only`, binary nyata
cargo test --test race_fixture          # 12 test check race terhadap fixture, port acak

# Varian rusak untuk dicoba manual (fixture berjalan): POST /broken/login/unprotected,
# POST /broken/login/accepts-invalid, GET /__admin/stats (detail di docs/fixture.md)

# `check` tanpa `--only` ditolak sampai P4: exit 2, config tidak dibaca, nol request
cargo run -q -- check --config examples/core-checks.json --out /tmp/result.json; echo $?
```

## Mencoba `check --only` (majukan sebagian P4)

Menjalankan satu check ke fixture lokal dan menulis report JSON. Saat ini `brute_force` dan `race`; `--only load` ditolak ("not available yet") sampai P3. Tanpa `--only`, `check` menolak dengan pesan `full suite available in P4` (pesan CLI berbahasa Inggris, sesuai DECISIONS P0 S9).

**Exit code run `--only` tidak pernah 0**: FAIL yang selesai = 1, PASS dan semua hasil lain (inconclusive, error, not_run, incomplete) = 2, cancel (Ctrl-C) = 130. PASS dibaca dari baris console dan dari `checks[].verdict` di report, bukan dari exit code. Report ditandai `"scope": "partial"`; check yang tidak dipilih tercatat `not_run` dengan reason `excluded_by_only`.

Config minimal: `examples/brute-force-only.json` (hanya `config_version`, `base_url`, `limits`, `brute_force`; bagian `race`/`load` tidak perlu ada). Isinya:

```json
{
  "config_version": 1,
  "base_url": "http://127.0.0.1:8080",
  "limits": {
    "request_timeout_ms": 5000,
    "suite_deadline_ms": 120000,
    "response_body_limit_bytes": 1048576
  },
  "brute_force": {
    "baseline_request": {
      "method": "POST", "path": "/login",
      "json": {"username": "${ENV:LOGIN_USER}", "password": "${ENV:LOGIN_VALID_PASSWORD}"}
    },
    "probe_request": {
      "method": "POST", "path": "/login",
      "json": {"username": "${ENV:LOGIN_USER}", "password": "${ENV:LOGIN_INVALID_PASSWORD}"}
    },
    "success_match": {"status_in": [200], "json_exists": ["/access_token"]},
    "rejection_match": {"status_in": [401]},
    "protection_match": {"status_in": [429]},
    "attempts": 8,
    "interval_ms": 100,
    "protect_by_attempt": 6,
    "protected_tail_count": 2
  }
}
```

Cara menjalankan (dari root repo; `export` harus di terminal yang sama dengan `cargo run -- check`; bila fixture bukan di port 8080, ubah `base_url` di salinan config):

```sh
cargo run --example fixture                          # terminal 1
# terminal 2:
export LOGIN_USER=demo LOGIN_VALID_PASSWORD=demo-pass LOGIN_INVALID_PASSWORD=wrong-pass
cargo run -q -- validate --config examples/brute-force-only.json --only brute_force   # exit 0, 9 request, 700 ms
cargo run -q -- check --config examples/brute-force-only.json --out /tmp/result.json --only brute_force; echo $?
# stdout: brute_force PASS first_protected=6 tail=3/2 attempts=8/8 reasons=protection_observed
# exit 2 (run parsial tidak pernah 0); verdict ada di console dan /tmp/result.json
curl -X POST http://127.0.0.1:8080/__admin/reset     # wajib sebelum mengulang; tanpa reset hasilnya INCONCLUSIVE

# Fixture rusak: harus FAIL (exit 1, reasons=protection_policy_not_met)
sed 's#"/login"#"/broken/login/unprotected"#g' examples/brute-force-only.json > /tmp/bf-broken.json
curl -X POST http://127.0.0.1:8080/__admin/reset
cargo run -q -- check --config /tmp/bf-broken.json --out /tmp/result.json --only brute_force; echo $?
```

Ctrl-C saat check berjalan: exit 130, report ditulis dengan `not_run` reason `cancelled`.

### Mencoba `check --only race`

Config: `examples/race-only.json` (hanya `config_version`, `base_url`, `limits`, `race`; 7 request, 250 ms fixed waits). Id action dan header auth datang dari env.

```sh
cargo run --example fixture                          # terminal 1
# terminal 2:
export TEST_ACTION_ID=act-1 TEST_AUTHORIZATION='Bearer fixture-token'   # nilai auth harus persis ini untuk fixture
curl -X POST http://127.0.0.1:8080/__admin/reset
cargo run -q -- validate --only race --config examples/race-only.json   # exit 0, 7 request, 250 ms
cargo run -q -- check --only race --config examples/race-only.json --out /tmp/race.json; echo $?
# stdout: race PASS delta=1 before=0 after=1 actions=5/5 skew=<n>/<budget>ms reasons=single_side_effect
# exit 2 (run parsial tidak pernah 0)
```

Reset fixture (atau pakai `TEST_ACTION_ID` baru) sebelum tiap run; tanpa itu hasilnya INCONCLUSIVE `initial_counter_not_zero`. Varian lain: salin config dan ganti `race.action_request.path`:

- `/broken/actions/{id}/apply-racy` untuk varian rusak: FAIL `duplicate_side_effect`, exit 1.
- `/idempotent/actions/{id}/apply` untuk varian idempotent: PASS dengan lima respons 200.

Verifier tetap `GET /actions/{id}`. Detail rute dan titik sinkronisasi ada di `docs/fixture.md`.

## Known limitations

- `check` hanya bisa `--only brute_force` atau `--only race`; suite lengkap (tanpa `--only`) datang di P4 dan `check` tanpa `--only` ditolak. Keputusan nasib `--only` setelah P4 diambil di P4. Penulis report atomic sudah ada tetapi belum dipanggil CLI.
- Fixture: healthy dan dua varian login rusak (P1). Varian racy dan idempotent ada sejak P2; varian slow ditambahkan di P3. Prelocked dibuat dari state healthy.
- Placeholder `${ENV:X}` di key `query` atau key objek body JSON masih dikirim sebagai literal (diam-diam); test `placeholder_in_query_or_json_key_is_accepted_known_limitation` mencatatnya dan harus dibalik saat diperbaiki.
- Jalur error key `json_equals` mencantumkan key tersebut (teks config milik pengguna, bukan nilai env).
- Cancel sebelum ada check akan exit 2, bukan 130; P4 harus menyisipkan check dengan reason `cancelled`. Check `fail` yang membawa reason pass menghasilkan exit 1 (bisa diperketat).
- `tls_failed` belum pernah dihasilkan (kegagalan TLS tercatat sebagai `connect_failed`); connect timeout dilaporkan sebagai `timeout`; drop/RST/respons sampah sebagai `other`.
- Timestamp report: presisi subsecond bisa bervariasi; potong ke detik di P4 bila perlu bentuk stabil.
- Tidak ada fsync direktori setelah rename report; mode file mengikuti umask (report tidak berisi secret).
- Aturan overlap `status_in` konservatif: irisan apa pun antar success/rejection/protection ditolak saat validasi.

- P1: proteksi yang hilang di tengah run lalu muncul lagi di ekor tetap PASS (PLAN §5 hanya menuntut attempt protected pertama tepat waktu dan ekor protected). Setelah probe 1 protected, semua probe tetap dikirim (workload tetap), jadi akun nyata terkunci lebih lama.
- P1: stop setelah semua probe selesai tanpa success tidak mengubah verdict; success + stop selalu `incomplete`. Jumlah probe yang salah tanpa stop = `not_run [not_started]`, tidak pernah PASS.
- P1: test integrasi hanya menegaskan batas bawah interval. Saran pengetatan test P1 sudah dikerjakan di P2 S1.

- P2: respons 202 ditolak saat `validate` (turunan PLAN §6, bukan kalimat eksplisit). Status "pending" yang hanya ditandai lewat body (200 + `{"status":"pending"}`) tidak terdeteksi; action dianggap sinkron.
- P2: dispatch memakai satu task (`join_all`), jadi skew yang terukur hanya selisih mikrodetik di sisi client dan tidak menjamin request tiba bersamaan di server. Jalur upgrade (`spawn` per action + `Barrier`) ada di komentar `ponytail:` di `src/checks/race.rs`.
- P2: transport error pada request action dilaporkan INCONCLUSIVE `action_outcome_ambiguous` + `transport_error`, bukan ERROR (tafsiran PLAN §6, bisa dibalik di DECISIONS).
- P2: action yang di-drop karena cancel/deadline mungkin tetap dieksekusi server; resource target butuh id baru atau reset sebelum run berikutnya.
- P2: test `r18` memakai pola bind-lalu-drop port; bila port dipakai ulang test lain, hasilnya merah, bukan PASS palsu. Header `X-Sentinel` tidak dipantulkan fixture, jadi buktinya lewat listener TCP mentah.
- P2: `evaluate` memberi `dispatch_incomplete` bila ada action tercatat tapi skew kosong (pengaman dari review S3/S4), supaya salah sambung di `run_with` tidak pernah jadi PASS.

- `check --only`: Ctrl-C yang datang sebelum handler SIGINT terdaftar (beberapa milidetik saat config dibaca) mematikan proses tanpa report; belum ada request yang terkirim saat itu.
- `check --only`: bila report gagal ditulis, baris console tetap mencetak verdict (mis. `PASS`); penandanya hanya di stderr dan exit 2. P4 dapat menambah penanda di console.
- `check --only`: precheck `--out` membandingkan path dengan `canonicalize`, jadi hardlink ke file config tidak terdeteksi; aman karena report ditulis lewat rename.
- `check --only`: cancel sebelum dispatch, cancel setelah check selesai, dan FAIL lalu deadline dibuktikan lewat unit test dan verifikasi manual reviewer, bukan test end-to-end otomatis (bergantung timing).

## Validasi API nyata

Belum dilakukan. Menunggu konfigurasi target dari pengguna setelah P4.

## Riwayat review pengguna

Belum ada.
