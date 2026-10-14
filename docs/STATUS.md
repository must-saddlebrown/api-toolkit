# Status pengembangan

Rencana aktif: `docs/PLAN.md` (Small Core Proof, 9 Oktober 2026). Keputusan setelah rencana: `docs/DECISIONS.md`. Rencana lama ada di `docs/archive/` dan tidak dijalankan.

| Fase | Isi | Status | Catatan |
| --- | --- | --- | --- |
| P0 | Kontrak CLI, runner minimum, fixture | selesai (10 Oktober 2026) | Phase gate PASS; 20 dari 20 AC terpenuhi; 111 test hijau |
| P1 | Login brute-force protection check | selesai (10 Oktober 2026) | Phase gate PASS; 18 dari 18 AC terpenuhi; 185 test hijau |
| P2 | Write-once race check | selesai (10 Oktober 2026) | Phase gate PASS; 25 dari 25 AC terpenuhi; 303 test hijau |
| P3 | Parallel load check | selesai (10 Oktober 2026) | Phase gate PASS; 26 dari 26 AC terpenuhi; 393 test hijau |
| P4 | Single-command proof + satu API nyata | selesai (14 Oktober 2026) | Phase gate PASS (10 Oktober); fixture proof selesai (416 test hijau); P4-AC7 dikonfirmasi pengguna 14 Oktober 2026, bukti `docs/real-api/2026-10-14.md` |
| Q2 | Batas ERROR/INCONCLUSIVE race (verifier before) | selesai (S1–S5, gate reviewer PASS, mutasi 0 lolos) | AC P2-AC26–34 (9 baris) di `docs/ac-trace.md`; mutasi race.rs + report.rs: 110 mutan, 101 tertangkap, 9 unviable, 0 lolos |
| Q3 | Mode burst brute_force (`window_ms`, `evaluate_burst`, `run_burst_with`) | selesai (S1–S6, gate reviewer PASS, mutasi 0 lolos) | AC P1-AC19–32 (14 baris) di `docs/ac-trace.md`; `verify.sh fast` exit 0; mutasi 277 mutan/253 tertangkap/24 unviable/0 lolos |
| Q4 | Mode baseline brute_force (`baseline = "require_success"\|"none"`, `evaluate_none`, peer IP) | S1–S8 dikerjakan (implementer S1–S6, tester S7, dokumen S8); **gate reviewer PASS 2026-10-14** | AC P1-AC33–46 (14 baris) di `docs/ac-trace.md`; `verify.sh fast` exit 0; mutasi 288 (265 caught, 2 missed non-blocking di `config.rs` plumbing pra-Q4, 21 unviable) |

## Fase aktif

P0–P4 selesai. Checkpoint P4 ditutup pengguna pada 14 Oktober 2026 (P4-AC7 terkonfirmasi, lihat "Validasi API nyata"). Tidak ada pekerjaan fase yang tersisa; yang terbuka hanya keputusan pengguna (lihat DECISIONS 2026-10-14 "Penutupan checkpoint P4").

Q2: perubahan klasifikasi verifier before di race (DECISIONS 2026-10-14, `docs/findings.md` F-001) sudah dikerjakan; gate reviewer PASS dan mutasi 0 lolos. Tidak mengubah status checkpoint P4.

Q3: mode burst brute_force (DECISIONS 2026-10-14, `docs/findings.md` F-002) S1–S6 sudah dikerjakan (config/validate mode-aware, `evaluate_burst`, `run_burst_with`, `Evidence::BruteForceBurst`, console burst, fixture `/windowed/login` dan `/broken/login/always-protected`, 16 test integrasi/CLI baru). Gate reviewer PASS (2026-10-14); mutasi `scripts/verify.sh mutants` (brute_force.rs, checks/mod.rs, race.rs, validate.rs, report.rs): 277 mutan, 253 tertangkap, 24 unviable, 0 lolos. Tidak mengubah status checkpoint P4.

Q4: mode baseline brute_force (DECISIONS 2026-10-14, "Q4: mode baseline brute_force") S1–S8 sudah dikerjakan: `Baseline` enum dan `baseline_request: Option` di `config.rs`/`validate.rs`/`resolve.rs`, `remote_addr` di `TransportResponse`, `evaluate_none`/`evaluate_burst_none` di `brute_force.rs`, wiring `run_with`/`run_burst_with`/`console_line`/`cli.rs`, fixture `/per-ip/login` dengan `ConnectInfo<SocketAddr>`, contoh `examples/brute-force-baseline-none.json`, dan test integrasi (`q1`–`q6` di `tests/brute_force_fixture.rs`, dua test di `tests/validate_cli.rs`). `verify.sh fast` exit 0. **Gate reviewer PASS 2026-10-14**: 14 AC P1-AC33–46 diverifikasi lewat test nyata, jalur false-PASS (`endpoint_not_confirmed` vs `probe_unclassifiable`, never-pass exhaustif, redaksi `peer_ip`) diperiksa, `report_version` tetap 1 dikonfirmasi di kode. Mutasi (`src/checks/brute_force.rs`, `src/config/validate.rs`, `src/config.rs`, `src/report.rs`): 288 mutan, 265 caught, 2 missed, 21 unviable. Dua mutan yang lolos (`config.rs:230` teks diagnostik serde, `config.rs:278` tukar `ConfigError::Syntax`/`Schema`) adalah plumbing parse pra-Q4 yang baru masuk scope karena gate ini pertama kali memasukkan seluruh `config.rs`; bukan jalur oracle, config tetap ditolak di keduanya — diterima sebagai keterbatasan non-blocking. Tidak mengubah status checkpoint P4.

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

## Ringkasan AC P3

Semua P3-AC1 sampai P3-AC26 terpenuhi, dengan bukti nama test di handoff `.claude/work/P3-plan.md` (Log, entri phase gate). Ringkas:

- Check load: satu preflight (tidak masuk sampel), lalu `requests` request lewat worker pool tertutup `min(concurrency, requests)` dalam satu task (`join_all`), tanpa retry dan tanpa redirect (AC1, AC6, AC11, AC12).
- Verdict: healthy pass; slow `latency_exceeded` dan flaky `error_rate_exceeded` fail; preflight tidak cocok = inconclusive `target_not_ready`; preflight transport error = error `preflight_failed`; sampel kurang tanpa stop = inconclusive `workload_incomplete`; cancel/deadline = not_run `[stop, workload_incomplete]`; invarian runner rusak = error `runner_error` (AC2–AC5, AC7–AC10, AC13–AC16).
- p95 nearest-rank atas semua sampel termasuk timeout dan error; error rate tepat sama dengan batas lolos tanpa epsilon; tidak ada jalur NaN menjadi PASS (AC5, AC13–AC15).
- Evidence load (agregat: histogram status, `error_counts`, p50/p95/max, throughput) di report; golden P0, brute_force, race tidak berubah (AC17). Baris console sesuai PLAN §10 (AC18).
- Sentinel secret tidak muncul di report file, `Debug`, console, stdout, dan stderr; sentinel terbukti terkirim di wire lewat listener TCP mentah (AC19, AC22).
- Fixture: `/critical`, `/broken/critical/{slow,flaky,hang}` (flaky dan hang setiap request ke-10 berdasarkan counter); `load_requests` menghitung semua request rute load termasuk 401 (AC25).
- CLI `check --only load` dan `validate --only load`; contoh `examples/load-only.json` (101 request, 0 ms fixed waits) (AC20, AC21, AC23, AC24).

## Ringkasan AC P4

Semua AC P4 terpenuhi dengan bukti nama test di handoff `.claude/work/P4-plan.md` (Log, entri phase gate), **kecuali P4-AC7: fixture proof selesai, validasi target nyata belum dilakukan.** Ringkas:

- `check` tanpa `--only` menjalankan race → load → brute_force berurutan, satu deadline suite, tiga baris console, report `scope: full` (AC1, AC8).
- Satu check FAIL/ERROR/INCONCLUSIVE tidak menghentikan yang lain; exit code sesuai PLAN §10 (matriks E1–E13): 0 hanya bila tiga PASS complete, 1 bila ada FAIL tanpa kondisi exit-2, 2 untuk sisanya, 130 untuk cancel (AC2, AC3).
- Cancel/deadline: check yang belum jalan menjadi `not_run` tanpa request; cancel sebelum check pertama kini 130 (AC3.4, AC3.5).
- Ulang tanpa reset tidak pernah PASS palsu (race `initial_counter_not_zero`, brute_force `account_already_locked`, exit 2); setelah reset hasil sama dengan run pertama (AC4).
- Report ditulis atomic (inode berubah, tanpa file temp), timestamp tanpa pecahan detik; gagal tulis → `report NOT WRITTEN: <path>` di stdout, exit 2/130, isi lama utuh (AC5.1, AC5.2, AC5.4).
- Sentinel secret tidak muncul di stdout, stderr, report; terbukti terkirim di wire lewat listener TCP mentah (AC5.3).
- `examples/core-checks-broken.json`, README untuk suite lengkap, daftar input API nyata (PLAN §14), `docs/fixture.md` (AC1.4, AC6, AC9).

## Cara mencoba

Semua command dijalankan dari root repo.

```sh
# Gate kualitas
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets            # 416 test, tanpa env var dan tanpa fixture berjalan

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
cargo test --test load_fixture          # 13 test check load terhadap fixture, port acak

# BUKTI FIXTURE PROOF (satu perintah): suite lengkap, healthy dan broken, biner nyata, fixture di port acak
cargo test --test fixture_proof         # 12 test, tanpa env var dan tanpa fixture berjalan

# Varian rusak untuk dicoba manual (fixture berjalan): POST /broken/login/unprotected,
# POST /broken/login/accepts-invalid, GET /__admin/stats (detail di docs/fixture.md)

# Suite lengkap manual (fixture berjalan, env di atas ada; reset sebelum tiap suite)
curl -X POST http://127.0.0.1:8080/__admin/reset
cargo run -q -- check --config examples/core-checks.json --out /tmp/result.json; echo $?          # healthy: 3 PASS, exit 0
curl -X POST http://127.0.0.1:8080/__admin/reset
cargo run -q -- check --config examples/core-checks-broken.json --out /tmp/result.json; echo $?   # rusak: 3 FAIL, exit 1
# stdout (healthy): race PASS ... reasons=single_side_effect / load PASS ... reasons=thresholds_met / brute_force PASS ... reasons=protection_observed
# stdout (rusak):   duplicate_side_effect / error_rate_exceeded / protection_policy_not_met
```

## Mencoba `check --only` (majukan sebagian P4)

Menjalankan satu check ke fixture lokal dan menulis report JSON. Saat ini `brute_force`, `race`, dan `load`; `--only load` aktif sejak P3. Tanpa `--only`, `check` menjalankan suite lengkap (lihat "Cara mencoba").

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

### Mencoba `check --only load`

Config: `examples/load-only.json` (hanya `config_version`, `base_url`, `limits`, `load`; `GET /critical`, 100 request, concurrency 10; 101 request terencana, 0 ms fixed waits). Auth dari env.

```sh
cargo run --example fixture                          # terminal 1
# terminal 2:
export TEST_AUTHORIZATION='Bearer fixture-token'     # nilai harus persis ini untuk fixture
curl -X POST http://127.0.0.1:8080/__admin/reset
cargo run -q -- validate --only load --config examples/load-only.json   # exit 0, 101 request, 0 ms
cargo run -q -- check --only load --config examples/load-only.json --out /tmp/load.json; echo $?
# stdout: load PASS error_rate=0/0.01 p95=<n>/500ms attempted=100/100 samples=100 max_in_flight=10/10 ... reasons=thresholds_met
# exit 2 (run parsial tidak pernah 0)
```

Reset fixture sebelum tiap run (counter flaky dan hang ikut `load_requests`). Varian lain: salin config dan ganti `load.request.path`: `/broken/critical/slow` (turunkan `max_p95_ms` ke 100: FAIL `latency_exceeded`, exit 1), `/broken/critical/flaky` (FAIL `error_rate_exceeded`, exit 1), `/broken/critical/hang` (setiap ke-10 tidur 3 s; pakai `request_timeout_ms` < 3000, mis. 500, untuk melihat timeout). Detail ada di `docs/fixture.md`.

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

- `check` tanpa `--only` menjalankan suite lengkap (P4); `--only brute_force|race|load` tetap ada sebagai run parsial (DECISIONS P4).
- Fixture: healthy dan dua varian login rusak (P1). Varian racy dan idempotent ada sejak P2; varian load slow, flaky, dan hang sejak P3. Prelocked dibuat dari state healthy.
- Placeholder `${ENV:X}` di key `query` atau key objek body JSON masih dikirim sebagai literal (diam-diam); test `placeholder_in_query_or_json_key_is_accepted_known_limitation` mencatatnya dan harus dibalik saat diperbaiki.
- Race (Q2): respons verifier before yang tidak cocok `verify_match` dievaluasi sebelum body, jadi body non-JSON dengan matcher `json_*` menjadi INCONCLUSIVE `baseline_unreadable`, bukan ERROR `counter_invalid`. Verifier after tetap ERROR (action sudah terkirim). Test fixture untuk after yang rusak hanya di unit `run_with`, tidak ada rute fixture.
- Jalur error key `json_equals` mencantumkan key tersebut (teks config milik pengguna, bukan nilai env).
- Check `fail` yang membawa reason pass menghasilkan exit 1 (bisa diperketat). Cancel sebelum check pertama kini exit 130 (check tetap tercatat `not_run [cancelled]`).
- `tls_failed` belum pernah dihasilkan (kegagalan TLS tercatat sebagai `connect_failed`); connect timeout dilaporkan sebagai `timeout`; drop/RST/respons sampah sebagai `other`.
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

- Q3 (mode burst brute_force): `window_ms` mengukur selisih waktu dispatch di sisi client dalam satu task (`join_all`), bukan waktu kedatangan di server; limiter tidak dijamin melihat probe dalam jendela yang sama. FAIL burst (`P == 0`) bukan bukti tanpa proteksi: limiter dengan ambang di atas N (maks 32) atau limiter per-menit tidak akan terpicu. `P == N` selalu INCONCLUSIVE `account_already_locked`, yang bisa menutupi limiter yang sangat ketat (mis. 1 request/detik di mana baseline sudah memakai jatah); tidak ada field `min_protected` (YAGNI, lihat DECISIONS C2).

- Q4 (mode baseline brute_force): kalimat caveat "FAIL `protection_not_triggered` bukan bukti tanpa proteksi" hanya ada di dokumen (PLAN §5, README, `docs/fixture.md`), bukan di teks reason atau console/report runtime (sama seperti caveat `protection_policy_not_met`/burst sebelumnya; tester menandai ini saat menulis `q3`/`q4` di `tests/brute_force_fixture.rs`). `peer_ip`/`peer_family` hanya diisi dari respons yang benar-benar diterima (`reqwest::Response::remote_addr()`); pada transport error (termasuk seluruh kasus `endpoint_not_confirmed`) keduanya tetap `null`, jadi kasus endpoint tak terkonfirmasi tidak pernah membawa bukti peer. Fixture `/per-ip/login` mengunci per IP peer; di semua test yang memakainya request datang dari `127.0.0.1` saja, jadi limiter berperilaku seperti hitungan global (disengaja, membuktikan baris PASS deterministik) dan famili selalu `v4`.

- P3: `throughput_rps` hanya di report (informatif, `null` bila elapsed 0), tidak di console.
- P3: preflight memakai `load.request` yang sama dengan load, jadi target nyata menerima 1+N request; endpoint non-idempoten ikut terkena efeknya.
- P3: Ctrl-C dan deadline sama-sama NOT_RUN `[stop, workload_incomplete]` (bedanya reason dan exit 130/2). Request in-flight yang di-drop saat stop mungkin sudah sampai server; dihitung di `dispatched`, bukan `attempted`.
- P3: dispatch satu task (`join_all`, maksimal 32 worker); pooling transport mati, jadi tiap request membuka koneksi baru dan p95 API nyata mencakup handshake (lebih tinggi daripada klien keep-alive).
- P3: fixture flaky dan hang berbasis counter bersama `load_requests` (termasuk 401): reset wajib antar run dan run paralel ke satu fixture saling mengganggu. Hang jatuh pada kelipatan 10 (preflight = 1) dan butuh `request_timeout_ms` < 3 s; shutdown fixture bisa menunggu handler yang tidur.
- P3: report load hanya agregat (tanpa daftar per-request); p50/p95/max dibulatkan ke atas ke ms.
- P3: test e2e deadline load (`load_deadline_during_load_is_not_run_deadline_exceeded_workload_incomplete`) bergantung pada preflight dan 8 request cepat selesai dalam 1,5 s; di mesin sangat sibuk hasilnya merah, bukan PASS palsu. PASS e2e CLI memakai `max_p95_ms` 500 dari contoh (p95 nyata ± 3 ms).

- `check --only`: Ctrl-C yang datang sebelum handler SIGINT terdaftar (beberapa milidetik saat config dibaca) mematikan proses tanpa report; belum ada request yang terkirim saat itu.
- Bila report gagal ditulis, baris console tetap mencetak verdict; stdout menambah `report NOT WRITTEN: <path>`, stderr memuat error, exit 2 (130 bila cancel).
- `check --only`: precheck `--out` membandingkan path dengan `canonicalize`, jadi hardlink ke file config tidak terdeteksi; aman karena report ditulis lewat rename.
- `check --only`: cancel sebelum dispatch, cancel setelah check selesai, dan FAIL lalu deadline dibuktikan lewat unit test dan verifikasi manual reviewer, bukan test end-to-end otomatis (bergantung timing).

- P4: Ctrl-C sebelum handler terdaftar mematikan proses tanpa report (shell tetap melihat status 130); Ctrl-C setelah check terakhir selesai mengikuti verdict, bukan 130.
- P4: console dicetak setelah seluruh suite selesai (tidak ada output sampai selesai pada suite nyata yang lama; catatan `ponytail:` di `src/cli.rs`).
- P4: load yang dibatalkan sebelum preflight dijawab hanya `not_run [cancelled]` (tanpa `workload_incomplete`); brute_force yang sudah FAIL lalu dibatalkan tetap FAIL + `cancelled`, `incomplete` (exit 130).
- P4: jalur `build_report` gagal + cancel tidak punya test; test SIGINT binary (`seen_at_sigint == 1`) bergantung pada urutan race → load → brute_force; cek atomic lewat inode hanya di unix.
- P4: `examples/api-checks.json` (untracked, brute_force saja ke `127.0.0.1:3000`) bukan bagian P4 dan tidak dipakai.

## Validasi API nyata

**P4-AC7: selesai, dikonfirmasi pengguna 14 Oktober 2026.** Bukti: `docs/real-api/2026-10-14.md` (daftar run dan verdict dari `examples/results/`). Kebutuhan yang muncul dari run nyata sudah ditangani di Q2–Q4. Catatan di bawah dipertahankan sebagai panduan run nyata berikutnya.

Yang harus pengguna sediakan (PLAN §14; detail dan peringatan efek samping di `README.md`, bagian "Menjalankan terhadap API nyata"):

- base URL target;
- login: request baseline (kredensial valid) dan probe (kredensial salah), serta policy (`success_match`, `rejection_match`, `protection_match`, `attempts`, `protect_by_attempt`, `protected_tail_count`, `interval_ms`) dan akun uji yang boleh terkunci;
- resource write-once yang segar (belum pernah dieksekusi) dengan request action, request verifier, dan JSON pointer counter (awal harus 0);
- endpoint load, `requests`, `concurrency`, `max_error_rate`, `max_p95_ms`, dan `response_match` untuk preflight;
- nama env untuk semua secret (`${ENV:NAME}`).

Jangan mengarang target atau kredensial. Brute-force bisa mengunci akun, race mengubah state sekali, load mengirim 1+N request tanpa retry.

## Riwayat review pengguna

- 2026-10-14, P4: P4-AC7 (validasi API nyata) dikonfirmasi selesai dan bekerja dengan baik oleh pengguna. Checkpoint P4 ditutup. Bukti: `docs/real-api/2026-10-14.md`.

## Riwayat pekerjaan

- 2026-10-14: Q2 (race, batas ERROR/INCONCLUSIVE) S1–S5 dikerjakan: ReasonCode +2 (32), `read_verifier` dan pemetaan before/after dipisah, 4 rute fixture verifier rusak, test integrasi, 9 baris AC baru. Gate reviewer PASS; mutasi 0 lolos.
- 2026-10-14: Q3 (brute_force, mode burst) S1–S6 dikerjakan: `BruteForceMode`/`Option` tiga field sequential di `config.rs`, aturan per mode dan budget di `validate.rs`, `dispatch_spread_ms` dipindah ke `checks::mod`, `evaluate_burst` + `run_burst_with` + `Evidence::BruteForceBurst` + console burst di `brute_force.rs`/`report.rs`, fixture `/windowed/login` dan `/broken/login/always-protected`, contoh `examples/brute-force-burst.json`, 16 test integrasi/CLI baru (b20–b31, `validate_burst_*`, `burst_only_brute_force_is_pass_with_partial_scope`). 14 baris AC baru (`P1-AC19`–`P1-AC32`). Gate reviewer PASS; mutasi 277 mutan/253 tertangkap/24 unviable/0 lolos; satu perbaikan non-blocking dari review (pesan error `attempts` burst di `validate.rs` kini menyatakan batas 2..=32, bukan 1..=32).
- 2026-10-14: Q4 (brute_force, mode baseline `"require_success"|"none"`) S1–S8 dikerjakan. Tabrakan nama ditemukan dan diselesaikan di tengah S1 (lihat DECISIONS): field evidence mode baru diberi nama `baseline_mode`, bukan `baseline` (key itu sudah dipakai evidence attempt baseline); perubahan tetap aditif, `report_version` 1. `ReasonCode` +2 (34): `endpoint_not_confirmed` (inconclusive), `protection_not_triggered` (fail, menggantikan `protection_policy_not_met` hanya di jalur none). `Baseline` enum dan `baseline_request: Option<RequestSpec>` di `config.rs`; cabang validasi + budget `attempts`-saja di `validate.rs`; `BruteForceRequests.baseline: Option<ResolvedRequest>` di `resolve.rs`. `TransportResponse.remote_addr` dari `reqwest::Response::remote_addr()` (satu baris, tanpa dependency baru). Dua fungsi murni baru `evaluate_none`/`evaluate_burst_none` di `brute_force.rs` (tidak menyentuh `evaluate`/`evaluate_burst`), lalu `run_with`/`run_burst_with` melewati baseline sepenuhnya saat `baseline = "none"` dan menangkap `peer_ip`/`peer_family`; `console_line` menambah token `baseline=none` hanya di jalur itu. Fixture `/per-ip/login` (limiter hitungan per-IP, `ConnectInfo<SocketAddr>`, `PER_IP_LIMIT=5`) plus `examples/brute-force-baseline-none.json`. Tester (S7) menambah `q1`–`q6` di `tests/brute_force_fixture.rs` dan dua test di `tests/validate_cli.rs`, termasuk kontrol negatif redaksi `peer_ip` (`q6`). 14 baris AC baru (`P1-AC33`–`P1-AC46`) di `docs/ac-trace.md`. `verify.sh fast` exit 0. **Gate reviewer PASS**: mutasi 288 (265 caught, 2 missed non-blocking di `config.rs` plumbing pra-Q4, 21 unviable).
