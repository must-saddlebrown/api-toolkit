# P4 — Single-command proof + satu API nyata

Status: S7 phase gate = PASS (reviewer 2026-10-10; 416 test; P4-AC7 = fixture proof selesai, validasi target nyata belum dilakukan). Berikutnya: main session close-out P4 (STATUS, DECISIONS, PLAN §3, rust-notes), lalu checkpoint pengguna.
Sumber: `docs/PLAN.md` §2, §3, §4, §8, §9, §10, §11 (P4), §12, §13, §14; `docs/DECISIONS.md` (P0 S7, P0 S9, "Majukan sebagian P4", "Penutupan `check --only`", penutupan P2, penutupan P3); `docs/STATUS.md` (Known limitations); `.claude/work/P3-plan.md` (format).

Kondisi awal (kode):
- `check` tanpa `--only` ditolak di `check_args` (`src/cli.rs:169-173`, pesan `full suite available in P4`). `--only brute_force|race|load` aktif.
- `Prepared` memegang satu `Selected` (`src/cli.rs:155-159`); `run_check` menjalankan satu check di dalam loop `select!` cancel/deadline yang sudah benar (`src/cli.rs:254-306`): satu timer `suite_deadline_ms`, satu `watch` `Stop`, SIGINT terdaftar sebelum request pertama (`src/cli.rs:233-249`).
- Console dicetak sebelum report ditulis; gagal tulis report hanya ditandai di stderr + exit 2/130 (`src/cli.rs:309-343`).
- `partial_report` sudah memotong timestamp ke detik dan menyusun urutan race, load, brute_force (`src/cli.rs:360-387`).
- `report::exit_code` sudah mengimplementasikan PLAN §10 untuk `Scope::Full` (exit 0 hanya bila tepat tiga check, semuanya `pass` + `complete` + reason pass, suite `complete`; cancel 130 diperiksa pertama; `excluded_by_only` di mode full → 2) (`src/report.rs:493-539`). `write_report_atomic` sudah ada (`src/report.rs:580`).
- `validate(cfg, Selection::All)` mewajibkan ketiga bagian dan menghitung budget gabungan (`src/config/validate.rs:65-72`); `resolve(cfg, Selection::All, …)` sudah ada. `validate` tanpa `--only` = mode suite lengkap.
- Setiap `run` check yang menerima `Stop` yang sudah aktif mengembalikan `not_run [stop]` tanpa request (P1 "stop sebelum baseline", P2 `run_check_race_cancel_before_dispatch_sends_nothing`, P3 L16). Ini yang dipakai untuk check yang belum jalan saat cancel/deadline.
- Test yang mengunci perilaku stub/penolakan dan HARUS diganti di P4 (kalau tidak, test akan menjalankan suite sungguhan, sebagian ke `127.0.0.1:8080`):
  - `src/cli.rs`: `check_is_2_and_writes_nothing` (596), `check_leaves_existing_out_file_untouched` (693, memakai `EXAMPLE` + env lengkap → akan mengirim request ke 8080), `check_without_only_gives_p4_message_and_reads_no_config` (724), `check_help_has_required_phrases` (901, teks help berubah).
  - `tests/validate_cli.rs`: bagian `check` di `validate_never_sends_requests` (103-117, config valid ke `CountingListener` → akan mengirim request), `check_stub_never_zero_and_keeps_existing_out` (125-158), bagian "check stub" di `sentinel_never_printed` (237-250).
  - `tests/check_only_cli.rs`: `check_without_only_message_names_all_three_checks` (1458).

File untracked `examples/api-checks.json`: bukan dari fase sebelumnya. Isinya identik dengan `examples/brute-force-only.json` (hanya `brute_force`, `/login`, matcher dan threshold PLAN §9, env `LOGIN_USER`/`LOGIN_VALID_PASSWORD`/`LOGIN_INVALID_PASSWORD`) kecuali `base_url` = `http://127.0.0.1:3000`. Tidak memuat `race`/`load`, jadi tidak bisa dipakai suite lengkap (ditolak `validate(All)`), hanya `--only brute_force`. Kemungkinan draf pengguna untuk target lokal lain; tidak dipakai, tidak diubah, tidak di-commit oleh slice mana pun. Lihat Pertanyaan terbuka Q1 (tidak memblokir).

## Scope

Masuk P4 (PLAN §11 P4: "perluas command `check --only` yang sudah ada … menjadi suite lengkap sebagai default; jangan membuat ulang parser, report, atau jalur cancel/deadline. Orchestrator menjalankan ketiganya, partial report/cancel, console summary, exit code, README satu command, sample config. Selesaikan fixture proof sebelum integrasi target nyata."):
- `src/cli.rs`: `check` tanpa `--only` = suite lengkap (§3 "`check` menjalankan ketiga check"), urutan race → load → brute_force (§3 "Urutan default"), check berikutnya tetap dicoba setelah FAIL/ERROR (§3 "Check yang FAIL tidak otomatis menghentikan check independen lain"), cancel/deadline global menandai sisa check NOT_RUN (§3, §8), report `scope: full` (§10), satu baris console per check (§10).
- Exit code suite PLAN §10 (sudah ada di `report::exit_code`; P4 mengujinya lewat suite nyata dan melengkapi tabel unit bila ada baris yang belum tercakup).
- Penanda console bila report gagal ditulis (§10 "Report-save failure tidak pernah memberi exit 0"; STATUS Known limitations).
- Contoh config varian rusak untuk demo manual (§11 P4 "sample config").
- Fixture proof end-to-end dengan satu perintah (§11 P4 AC1, AC4; §12 "positive/negative fixture evidence … dapat diulang dari state awal yang terdokumentasi").
- README satu command (§11 P4 AC6) dan daftar kebutuhan config target nyata (§14) tanpa mengarang target.

Di luar P4:
- Run API nyata itu sendiri: menunggu config dari pengguna (§14 "Harness tidak boleh mengarang endpoint, credential, business counter"). P4-AC7 hanya ditandai.
- Scope guard §2/§13: tanpa TUI, OpenAPI, DSL, JUnit, history, CVSS, AI, plugin. Tanpa dependency baru. Tanpa trait baru.
- Menjalankan check paralel satu sama lain (lihat Pilihan teknis).
- Carry-over yang tidak menimbulkan false PASS dan tidak disentuh: placeholder di key `query`/JSON, `tls_failed`, `fail` + reason pass → 1, SIGINT sebelum handler terdaftar, hardlink `--out`, fsync direktori.

## AC (atomik)

Penomoran mengikuti PLAN §11 P4: butir PLAN n → `P4-ACn`, sub-kriteria atomik `P4-ACn.m`. `P4-AC7` tetap validasi target nyata. AC tambahan dari keputusan P4 mulai `P4-AC8`.

| Id | Kriteria | Sumber |
| --- | --- | --- |
| P4-AC1.1 | `check --config <f> --out <r>` tanpa `--only` menjalankan ketiga check berurutan race → load → brute_force, satu `Transport`, satu `watch` `Stop`, satu timer deadline; setiap check dijalankan walaupun check sebelumnya FAIL/ERROR/INCONCLUSIVE. | §3, §11 P4 butir 1 |
| P4-AC1.2 | Suite lengkap memakai `Selection::All`: bagian yang hilang, config tidak valid, env hilang, atau `--out` tidak layak → exit 2, nol koneksi, `--out` tidak disentuh/dibuat, sentinel tidak tercetak. | §3 "Invalid konfigurasi seluruh suite menghasilkan nol request", §9 |
| P4-AC1.3 | Fixture healthy, `examples/core-checks.json` (base_url diganti port fixture): race PASS `[single_side_effect]`, load PASS `[thresholds_met]`, brute_force PASS `[protection_observed]`, semua `complete`. | §11 P4 butir 1 |
| P4-AC1.4 | Varian rusak di dalam suite menghasilkan verdict yang diharapkan: brute_force `/broken/login/unprotected` → FAIL `[protection_policy_not_met]`; `/broken/login/accepts-invalid` → FAIL dengan `invalid_credentials_accepted`; race `/broken/actions/{id}/apply-racy` → FAIL dengan `duplicate_side_effect`; load `/broken/critical/flaky` → FAIL `[error_rate_exceeded]`; load `/broken/critical/slow` + `max_p95_ms=100` → FAIL `[latency_exceeded]`. Suite dengan ketiganya rusak (`examples/core-checks-broken.json`) → tiga FAIL, exit 1. | §11 P4 butir 1, §11 P1–P3 |
| P4-AC1.5 | Report suite: `report_version` 1, `scope: "full"`, tepat tiga entri berurutan race, load, brute_force, masing-masing dengan evidence check-nya; tidak ada `excluded_by_only`; `completion` suite `complete` hanya bila ketiga check `complete`. | §10 |
| P4-AC1.6 | Console suite: stdout tepat tiga baris berurutan race, load, brute_force, masing-masing sama dengan `console_line` check itu; tidak ada catatan `partial run` di stderr. | §10 "satu baris per check" |
| P4-AC2.1 | FAIL di satu check tidak menyembunyikan check lain: satu varian rusak + dua healthy → check rusak FAIL, dua lainnya PASS dengan evidence lengkap; stats fixture membuktikan request ketiga check terkirim; exit 1. | §11 P4 butir 2, §3 |
| P4-AC2.2 | ERROR/INCONCLUSIVE di satu check tidak menghentikan check lain: race ERROR `counter_invalid` (pointer tidak ada) + load healthy PASS + brute_force rusak FAIL → ketiganya punya verdict sendiri, FAIL brute_force tetap terlihat di console dan report, exit 2. | §11 P4 butir 2, §10 "Definite FAIL pada satu check tetap terlihat walaupun suite exit 2" |
| P4-AC3.1 | Suite healthy lengkap → exit 0 (satu-satunya jalur 0). | §11 P4 butir 3, §10 |
| P4-AC3.2 | Minimal satu FAIL dan sisanya PASS `complete` → exit 1. | §10 |
| P4-AC3.3 | Ada INCONCLUSIVE/ERROR/NOT_RUN/incomplete di check mana pun → exit 2, termasuk bila ada FAIL lain (presedensi §10). | §10 |
| P4-AC3.4 | Cancel → exit 130 di titik mana pun: sebelum check pertama (ketiganya `not_run [cancelled]`, nol koneksi, report ditulis), di tengah check (check itu `not_run` dengan `cancelled`, check sesudahnya `not_run [cancelled]` dengan nol request, check sebelumnya mempertahankan verdict-nya). Menutup Known limitation "cancel sebelum ada check exit 2". | §3, §8, §10, DECISIONS P0 S7 |
| P4-AC3.5 | Satu deadline `suite_deadline_ms` untuk seluruh suite: check yang sedang jalan → `not_run` dengan `deadline_exceeded`, check sesudahnya `not_run [deadline_exceeded]` nol request, check yang sudah selesai mempertahankan verdict; exit 2. | §3, §8 |
| P4-AC3.6 | Tabel unit `exit_code` mencakup semua baris matriks E1–E12 di bawah. | §10 |
| P4-AC4.1 | Reset fixture + command yang sama → verdict dan reasons ketiga check sama dengan run pertama (healthy: exit 0 dua kali). | §11 P4 butir 4, §3 |
| P4-AC4.2 | Ulang tanpa reset tidak pernah PASS palsu: race INCONCLUSIVE `initial_counter_not_zero`, brute_force INCONCLUSIVE `account_already_locked`, exit 2. | §11 P4 butir 4, §3 |
| P4-AC5.1 | Report readable: JSON valid, `report_version` 1, `started_at`/`finished_at` RFC3339 UTC tanpa pecahan detik. | §10, §11 P4 butir 5 |
| P4-AC5.2 | Report atomic: `--out` yang sudah ada diganti utuh, tanpa file temp tersisa di direktori; bila tulis gagal, isi lama utuh. | §10 "Atomic report write" |
| P4-AC5.3 | Report dan console tersanitasi: sentinel unik di env yang dipakai ketiga check (body login, path param/header race, header load) muncul 0 kali di stdout, stderr, dan file report suite; kontrol positif: report memuat `"race"`, `"load"`, `"brute_force"`. | §10, CLAUDE.md Secrets |
| P4-AC5.4 | Gagal menulis report suite → exit 2 (130 bila cancelled), tidak pernah 0; stdout memuat penanda `report NOT WRITTEN: <path>` setelah baris check; stderr memuat error. | §10, STATUS Known limitations |
| P4-AC6.1 | README menjelaskan: setup state/akun (reset fixture, akun uji khusus untuk brute force, identitas terpisah untuk race/load, resource write-once baru), secret env (`${ENV:NAME}` dan daftar nama), arti setiap threshold, satu command, arti verdict/reason/exit code, dan mode `--only` sebagai run parsial. | §11 P4 butir 6, §3, §9 |
| P4-AC6.2 | README memuat daftar input yang harus disiapkan pengguna untuk API nyata (§14) beserta peringatan efek samping (lockout, side effect write-once, 1+N request load), tanpa endpoint/kredensial/counter karangan. | §14 |
| P4-AC7 | Run API nyata dengan report dan baseline yang sah setelah pengguna memberi target/config. **Status P4: fixture proof selesai, validasi target nyata belum dilakukan** (tidak bisa dibuktikan tanpa target pengguna; tidak boleh ditandai selesai oleh harness). | §11 P4 butir 7, §14 |
| P4-AC8 | `--only` tetap ada sebagai mode parsial dengan kontrak lama tidak berubah (scope `partial`, `excluded_by_only`, tidak pernah exit 0); semua test `tests/check_only_cli.rs` yang tidak terkait penolakan tetap hijau tanpa diubah. `--help` `check` menjelaskan kedua mode. | Keputusan P4 (lihat Pilihan teknis) |
| P4-AC9 | Fixture proof satu perintah: `cargo test --test fixture_proof` menjalankan binary nyata terhadap fixture di port acak dan membuktikan healthy lolos (exit 0) dan varian rusak gagal (exit 1), tanpa env var dan tanpa fixture yang sudah berjalan. Perintah ini tercantum di README dan Commands CLAUDE.md. | §11 P4, §12 |
| P4-AC10 | Gate: `cargo fmt --all -- --check`, clippy `-D warnings`, `cargo test --all-targets` hijau; tanpa `unwrap`/`expect` produksi baru; tanpa dependency baru; tidak ada test yang mengirim request ke `127.0.0.1:8080` atau target eksternal. | CLAUDE.md |

## Matriks exit code suite lengkap

Kolom verdict urutan race / load / brute_force. P = pass complete, F = fail complete, I = inconclusive, E = error, N = not_run. "Unit" = tabel `exit_code` di `src/report.rs`; "CLI" = `run_check` in-process (`src/cli.rs` tests); "Fix" = `tests/fixture_proof.rs` (binary + fixture).

| # | Kombinasi | Exit | Cakupan |
| --- | --- | --- | --- |
| E1 | P / P / P | 0 | Unit, Fix (healthy) |
| E2 | F di satu posisi, sisanya P (tiga varian posisi) | 1 | Unit, Fix (mixed) |
| E3 | F / F / F | 1 | Unit, Fix (broken) |
| E4 | F + I (mis. race I, brute_force F) | 2 | Unit, Fix (repeat tanpa reset + load flaky, opsional) |
| E5 | E + P + F | 2 | Unit, Fix (AC2.2) |
| E6 | P / P / N `[deadline_exceeded]` | 2 | Unit, CLI (deadline) |
| E7 | Ada `cancelled` di mana pun, termasuk bersama F | 130 | Unit, CLI (cancel), binary SIGINT |
| E8 | F tetapi `incomplete` (brute_force success + stop) | 2 | Unit |
| E9 | P dengan reason non-pass | 2 | Unit (sudah ada `exit_code_pass_reasons_table`) |
| E10 | `excluded_by_only` di `scope: full` | 2 | Unit (sudah ada) |
| E11 | Check hilang/duplikat, daftar kosong, suite `completion: incomplete` | 2 | Unit (sudah ada sebagian) |
| E12 | Report gagal ditulis (healthy) / gagal ditulis + cancel | 2 / 130 | CLI |
| E13 | Config tidak valid / env hilang / bagian hilang / `--out` tidak layak | 2, nol koneksi | CLI unit, binary |

## Pilihan teknis

| Topik | Keputusan | Alasan (satu baris) |
| --- | --- | --- |
| Nasib `--only` | Tetap ada sebagai mode parsial opsional dengan kontrak sama; tanpa `--only` = suite lengkap (default). | Berguna untuk target nyata (mis. menjalankan brute force terpisah karena lockout) dan sudah teruji; menghapusnya = kerja tambahan tanpa manfaat. |
| Eksekusi | Tiga check berurutan race → load → brute_force dalam satu future di loop `select!` yang sudah ada; `Prepared.selected` menjadi `Vec<Selected>` (satu elemen untuk `--only`, tiga untuk suite). Tanpa paralel antar check. | §3 menetapkan urutan (lockout terakhir) dan load paralel bisa mencemari latency/race; satu jalur kode untuk kedua mode. |
| Independensi | Hasil check sebelumnya tidak pernah membatalkan check berikutnya; hanya `Stop` yang menghentikan. | §3 "Check yang FAIL tidak otomatis menghentikan check independen lain". |
| Deadline | Satu timer `suite_deadline_ms` dari awal run untuk seluruh suite (sudah ada); tidak ada deadline per check. | §8 "suite deadline"; `validate(All)` sudah menolak fixed waits gabungan yang melebihi deadline. |
| Cancel/deadline untuk check yang belum jalan | Tetap memanggil `run` check itu dengan receiver yang `Stop`-nya sudah aktif, sehingga hasilnya `not_run [cancelled|deadline_exceeded]`, `incomplete`, nol request. | Memakai perilaku yang sudah teruji di P1–P3; otomatis menutup "cancel sebelum ada check exit 2" (exit jadi 130). |
| Receiver stop | `stop_rx.clone()` per check dari satu `watch::channel`. | Satu sinyal untuk semua; tanpa tipe baru. |
| Exit code | Pakai `report::exit_code` apa adanya (PLAN §10 untuk `Scope::Full`). Tidak memperketat `fail` + reason pass. | Sudah benar dan teruji sejak P0; perketatan tidak menutup jalur false PASS. |
| Report | `partial_report` digeneralisasi menjadi `build_report(scope, started, finished, hash, results)`: full = hasil apa adanya (urutan race, load, brute_force); partial = isi `excluded` seperti sekarang. Completion suite = `incomplete` bila ada hasil non-excluded yang `incomplete`. | Satu pembangun report, tanpa duplikasi. |
| Timestamp | Tetap dipotong ke detik untuk kedua scope (`replace_nanosecond(0)`), seperti `--only`. | Bentuk stabil; menutup Known limitation timestamp. |
| Console | Semua baris check dicetak setelah suite selesai, urutan race, load, brute_force, sebelum report ditulis. Catatan `partial run` di stderr hanya untuk `--only`. | Paling sedikit kode; cancel/deadline membuat run berakhir cepat. `ponytail:` cetak per check saat selesai bila suite nyata terasa lama. |
| Penanda gagal tulis report | Bila `write_report_atomic` atau pembangunan report gagal, cetak ke stdout `report NOT WRITTEN: <path>` setelah baris check (di samping error stderr yang sudah ada), berlaku untuk suite dan `--only`. Tanpa baris tambahan saat sukses. | Pengguna yang hanya membaca stdout tidak salah mengira PASS tersimpan; satu baris kode. |
| Help/pesan | `check --help`: tanpa `--only` ketiga check berjalan, exit 0 hanya bila ketiganya selesai dan PASS; `--only <check>` = run parsial yang tidak pernah exit 0; `Available: brute_force, race, load` tetap. Pesan `full suite available in P4` dihapus. | Teks lama menyatakan `--only` wajib. |
| Contoh config | Tambah `examples/core-checks-broken.json` = `core-checks.json` dengan path rusak (`/broken/login/unprotected`, `/broken/actions/{id}/apply-racy`, `/broken/critical/flaky`), dikunci test validate (117 request, 950 ms). `core-checks.json` tetap healthy. | Demo FAIL manual untuk checkpoint tanpa `sed`; satu file. |
| Fixture proof | Satu file integration test `tests/fixture_proof.rs` (tester): fixture in-process port 0, binary nyata, env injeksi. Perintah: `cargo test --test fixture_proof`. Tanpa script shell baru. | Deterministik, berjalan di gate, tidak butuh fixture manual atau env. |
| Format bukti fixture proof | Bukti = nama test + output `cargo test --test fixture_proof` di Log handoff, ditambah satu transkrip manual (fixture `cargo run --example fixture`, healthy lalu broken: tiga baris console + `echo $?`) yang disalin ke STATUS "Cara mencoba" saat close-out. | Bukti otomatis untuk reviewer, transkrip manusiawi untuk checkpoint pengguna. |
| Target nyata | README hanya berisi daftar input yang dibutuhkan (§14) dan aturan keselamatan; tanpa contoh config target. Rekomendasi: mulai dengan `validate`, lalu `--only` per check, baru suite. | §14 melarang mengarang target. |
| Test lama stub | Diganti (bukan dihapus tanpa pengganti) dengan test yang menegaskan jalur nol request P4 (config/env tidak valid) dan suite nyata hanya terhadap fixture port acak atau listener lokal. | Mencegah test mengirim request ke 8080 dan menjaga cakupan nol request. |
| Reason/report contract | Tidak ada `ReasonCode` baru, `report_version` tetap 1, golden tidak berubah. | Semua reason sudah ada sejak P0–P3. |

## Slices

| Id | Tujuan | File/modul | AC | Unit test (implementer, inline) | Integration test (tester, `tests/`) | Status |
| --- | --- | --- | --- | --- | --- | --- |
| S1 | Orkestrasi suite: `check_args` dengan `only: Option` → `Selection::All`/`Only`; `Prepared.selected: Vec<Selected>` urutan race, load, brute_force; `run_check` menjalankan berurutan dengan `stop_rx.clone()`; console semua baris; `build_report(scope, …)`; catatan partial hanya untuk `--only`; teks `--help`. Izin mekanis: ganti test stub yang tercantum di "Kondisi awal" (`src/cli.rs`, `tests/validate_cli.rs`, `tests/check_only_cli.rs:1458`) dengan versi P4: config tidak valid/env hilang/bagian hilang → 2, nol koneksi (`CountingListener`), `--out` lama utuh. | `src/cli.rs`, `tests/validate_cli.rs` (mekanis), `tests/check_only_cli.rs` (mekanis) | AC1.1, AC1.2, AC1.5, AC1.6, AC3.4, AC3.5, AC5.1, AC8 | `build_report` full: urutan, tanpa excluded, completion, detik bulat; `check_args` tanpa `--only` pada config tanpa `load` → 2 menyebut `load`, tanpa koneksi; `run_check` suite ke port tertutup: tiga baris console urutan benar, race `error` dan dua lainnya tetap dijalankan (load `error [preflight_failed, transport_error]`, brute_force `error [baseline_failed, transport_error]`), exit 2, report `scope: full`; cancel sebelum dispatch → ketiganya `not_run [cancelled]`, 0 koneksi, 130, report ditulis; cancel saat verifier race menggantung (tunggu koneksi teramati) → 130, load/brute_force `not_run [cancelled]`, tidak ada koneksi baru setelah cancel; deadline saat race menggantung (deadline 200 ms) → 2, ketiganya `not_run` dengan `deadline_exceeded`; `check --help` memuat frasa baru dan `Available: brute_force, race, load`; test `--only` lama tetap hijau | – | done |
| S2 | Penanda `report NOT WRITTEN: <path>` di stdout bila report gagal dibangun/ditulis (suite dan `--only`); lengkapi tabel `exit_code` sampai E1–E12 (tambah baris yang belum ada saja). | `src/cli.rs`, `src/report.rs` (`#[cfg(test)]`) | AC3.1–AC3.3, AC3.6, AC5.4 | `run_check` suite dengan direktori read-only (skip bila root): exit 2, stdout tiga baris + `report NOT WRITTEN`, isi lama utuh, satu file di direktori; versi `--only` yang ada diperbarui untuk penanda; tabel `exit_code` baris E1–E12 (verdict + exit persis) | – | done |
| S3 | Contoh `examples/core-checks-broken.json` (tiga path rusak, sisanya sama dengan `core-checks.json`); bagian "Suite lengkap" di `docs/fixture.md` (env, reset sebelum tiap suite, urutan, rute rusak per check). | `examples/core-checks-broken.json`, `docs/fixture.md`, `src/cli.rs` (`#[cfg(test)]`) | AC1.4 (config), AC6.1 (fixture doc) | `validate` contoh rusak → exit 0, `planned requests: 117`, `fixed waits: 950 ms`, nama env sama dengan contoh healthy; tidak ada nilai rahasia literal di file | – | done |
| S4 | Fixture proof verdict: binary nyata terhadap fixture port 0. | `tests/fixture_proof.rs` (baru) | AC1.3, AC1.4, AC1.5, AC1.6, AC2.1, AC2.2, AC3.1–AC3.3, AC4.1, AC4.2, AC9 | – | healthy `core-checks.json` → exit 0, tiga PASS reasons persis, stdout tiga baris berurutan, `scope: full`; `core-checks-broken.json` → exit 1, race FAIL `duplicate_side_effect`, load FAIL `[error_rate_exceeded]`, brute_force FAIL `[protection_policy_not_met]`; mixed tiga varian (satu rusak per posisi) → exit 1, dua lainnya PASS dengan evidence, stats fixture menunjukkan `login_requests`, `action_requests`, `load_requests` > 0; `accepts-invalid` → FAIL `invalid_credentials_accepted`; load slow + `max_p95_ms=100` → FAIL `[latency_exceeded]`; AC2.2 (race `counter_pointer` `/missing` → ERROR `counter_invalid`, load PASS, brute_force unprotected FAIL, exit 2, baris `brute_force FAIL` ada di stdout); healthy dua kali tanpa reset → run 2 exit 2, race `initial_counter_not_zero`, brute_force `account_already_locked`; reset (`POST /__admin/reset`) lalu run 3 → verdict/reasons sama dengan run 1, exit 0 | done |
| S5 | Fixture proof report dan cancel: sanitasi, atomic, SIGINT binary. | `tests/fixture_proof.rs` | AC3.4 (binary), AC5.1, AC5.2, AC5.3 | – | sentinel unik di `LOGIN_INVALID_PASSWORD`, `TEST_ACTION_ID`, `TEST_AUTHORIZATION` (nilai yang tetap diterima fixture, mis. sentinel di password salah dan id action; auth tetap `Bearer fixture-token` sehingga sentinel auth diuji via `--only` P3 yang sudah ada) → 0 kemunculan di stdout, stderr, report, kontrol positif nama check ada; `--out` berisi `OLD` → diganti JSON valid, `report_version` 1, timestamp tanpa pecahan detik, tidak ada file `.tmp-` tersisa; SIGINT: listener yang menggantung pada request pertama (pola `tests/check_only_cli.rs:705`, tunggu koneksi teramati, bukan sleep) → exit 130, report ketiganya ada, check yang belum jalan `not_run [cancelled]` | done |
| S6 | Dokumentasi: README ditulis ulang untuk suite lengkap (setup state/akun, secret env, threshold, satu command, arti verdict/reason/exit, `--only`), bagian "Menjalankan terhadap API nyata" berisi daftar input §14 dan peringatan; Commands di `CLAUDE.md` mendapat `cargo test --test fixture_proof`. | `README.md`, `CLAUDE.md` (baris Commands saja) | AC6.1, AC6.2, AC9 (dokumentasi) | – | – (reviewer membaca terhadap checklist AC6) | done |
| S7 | Phase gate dan bukti: gate tiga perintah, `cargo test --test fixture_proof`, transkrip manual healthy + broken dengan fixture `cargo run --example fixture` (tiga baris console + exit code), penandaan AC7. STATUS/DECISIONS/PLAN diperbarui main session saat close-out. | – (reviewer, main session) | AC7 (penandaan), AC10, semua AC (bukti) | – | full suite | in_progress |

Cakupan AC: AC1.1 S1; AC1.2 S1; AC1.3 S4; AC1.4 S3/S4; AC1.5 S1/S4; AC1.6 S1/S4; AC2.1 S4; AC2.2 S4; AC3.1–3.3 S2/S4; AC3.4 S1/S5; AC3.5 S1; AC3.6 S2; AC4.1 S4; AC4.2 S4; AC5.1 S1/S5; AC5.2 S5 (+ unit atomic P0); AC5.3 S5; AC5.4 S2; AC6.1 S3/S6; AC6.2 S6; AC7 S7 (penandaan saja); AC8 S1; AC9 S4/S6; AC10 setiap slice dan S7.

Cakupan matriks: E1–E12 unit di S2; E1–E3, E5 Fix S4; E4 Fix S4 (repeat tanpa reset); E6, E7, E12 CLI S1/S2; E7 binary S5; E13 S1.

## Pertanyaan terbuka

- Q1 (tidak memblokir): `examples/api-checks.json` (untracked) berisi config brute_force saja ke `http://127.0.0.1:3000`. Apakah ini draf config target nyata Anda? Rekomendasi: biarkan untracked dan jangan dipakai di P4; bila memang target nyata, lengkapi bagian `race` dan `load` (atau jalankan `--only brute_force`) setelah P4, dan putuskan apakah file ini di-commit (aman: hanya berisi `${ENV:…}`, tanpa nilai rahasia) atau ditambahkan ke `.gitignore`.

## Draf entri DECISIONS (untuk close-out main session)

`## 2026-10-10 — Pilihan teknis P4` — Detail: `.claude/work/P4-plan.md`. Terdampak: PLAN §3, §8, §9, §10, §11 (P4).
- `check` tanpa `--only` menjalankan suite lengkap berurutan race → load → brute_force; `--only` tetap sebagai run parsial dengan kontrak lama.
- Satu deadline suite dan satu `Stop`; check yang belum jalan saat cancel/deadline tetap dipanggil dan langsung `not_run [stop]` tanpa request (cancel sebelum check pertama kini 130).
- Exit code memakai `report::exit_code` apa adanya; timestamp dipotong ke detik untuk kedua scope.
- Console dicetak setelah suite selesai; gagal tulis report menambah `report NOT WRITTEN: <path>` di stdout.
- `examples/core-checks-broken.json` untuk demo FAIL; fixture proof = `cargo test --test fixture_proof`.
- PLAN §3/§9/§11 diperbarui: hapus "Sampai P4, `check` wajib memakai `--only`"; `--only` menjadi opsional.

## Risiko dan hal yang diuji

- Test lama yang menjalankan `check` tanpa `--only` dengan config valid akan mengirim request (sebagian ke `127.0.0.1:8080`). S1 wajib menggantinya; reviewer grep `"check"` tanpa `--only` di `src/cli.rs` tests dan `tests/` untuk memastikan semuanya memakai fixture port acak, port tertutup, atau `CountingListener`.
- False PASS lewat check yang tidak dijalankan: `exit_code` mensyaratkan tepat tiga check `pass` + `complete`; S1 memastikan check yang tidak jalan tetap punya entri `not_run`, bukan hilang dari report.
- Cancel di antara dua check: receiver di-clone dari channel yang sama, jadi check berikutnya melihat `Stop` yang sudah aktif. Diuji di S1 (cancel saat race → load/brute_force nol koneksi).
- Durasi `fixture_proof`: tiap suite healthy ±2–3 s (950 ms fixed waits + 101 request load); sekitar 8–10 run suite. Bila terlalu lama, kurangi `load.requests` di salinan config test (bukan di contoh).
- Fixture flaky berbasis counter `load_requests`: tiap run suite di test memakai fixture baru atau reset, kalau tidak jumlah 500 bergeser.
- Racy gate timeout 2 s harus lebih kecil dari `request_timeout_ms` (5000 di contoh): aman.
- Brute force berjalan terakhir dan fixture memakai counter login yang terpisah dari action/load: tidak ada pencemaran antar check di fixture. Di API nyata, limiter bersama tetap mungkin (§3); README menyebutnya.
- Console dicetak di akhir: pada suite nyata yang lama tidak ada output sampai selesai (diterima, `ponytail:` di kode).

## Konsep Rust baru (untuk `docs/rust-notes.md`)

- `Vec` berisi enum (`Vec<Selected>`) yang diiterasi berurutan di dalam satu blok `async`, dengan `await` per elemen.
- `watch::Receiver::clone()`: beberapa konsumen membaca satu sinyal yang sama.

## Log

(kosong; verdict per slice dan phase gate ditulis di sini)

### 2026-10-10 — S1 review ronde 1: FIX (reviewer)

Gate: fmt exit 0, clippy exit 0, `cargo test --all-targets` 398 lulus / 0 gagal. Kode produksi benar. Run live binary terhadap fixture port 0: healthy exit 0; tanpa reset exit 2 (race `initial_counter_not_zero`, bf `account_already_locked`); tiga rusak exit 1; campuran exit 1; race ERROR + bf FAIL exit 2; deadline di tengah load exit 2 (race PASS tetap); SIGINT di tengah race/load/bf dan SIGINT ganda selalu 130, check sesudahnya punya 0 request, check sebelumnya mempertahankan verdict; sentinel muncul 0x; `--out` lama diganti tanpa file temp; `--only race|load` tetap partial dan exit 2.

Mutan (salinan /tmp): "stop melewati check berikutnya" -> 6 merah; "baris load memakai console bf" -> merah; "completion hanya dari check terakhir" -> merah; **"stop menimpa verdict check sebelumnya jadi not_run" -> LOLOS** (semua test cancel/deadline S1 menghentikan saat race, sehingga belum ada check yang selesai).

Temuan FIX:
1. `src/cli.rs` tests (setelah `run_check_full_deadline_while_race_hangs_is_2_all_not_run_deadline`): bagian AC3.4/AC3.5 "check sebelumnya mempertahankan verdict-nya" belum punya test. Tambahkan dua test yang memakai `preflight_ok_then_hang()` (race verifier dapat 200 `{"ok":true}` -> race ERROR `[counter_invalid]` `complete`; preflight load lalu menggantung): (a) cancel setelah `count >= 2` -> 130, race `error [counter_invalid] complete`, load `not_run` dengan `cancelled`, bf `[cancelled]`, `count == 2`; (b) `suite_deadline_ms` 400 -> 2, race tetap `error complete`, load `reasons[0] == deadline_exceeded`, bf `[deadline_exceeded]`, `count == 2`. Reviewer sudah menjalankan versi ini: hijau 5/5 di kode sekarang, merah pada mutan.

Saran (tidak memblokir): `tests/validate_cli.rs::check_full_invalid_input_is_2_and_keeps_existing_out` kasus kedua (`--out` parent tidak ada, `DEV_ENV`) memakai config contoh ke 127.0.0.1:8080; bila precheck `--out` rusak, test ini akan mengirim request ke 8080. Ganti `base_url` ke `CountingListener`/port tertutup atau tambahkan assert stderr `parent directory`.

### 2026-10-10 — S1 review ronde 2: PASS (reviewer)

- Kode produksi tidak berubah sejak ronde 1: `diff` antara snapshot `git diff` ronde 1 dan ronde 2 hanya berisi dua test baru di `src/cli.rs` dan perubahan di `tests/validate_cli.rs`.
- Temuan 1 tertutup: `run_check_full_cancel_after_race_complete_keeps_race_verdict` dan `run_check_full_deadline_after_race_complete_keeps_race_verdict` sesuai spesifikasi (race `error`/`counter_invalid`/`complete`, load dan bf `not_run` dengan `cancelled`/`deadline_exceeded`, `count == 2`). Mutan "check yang sudah selesai ditimpa jadi `not_run` setelah stop" (salinan /tmp) membuat 3 test merah: kedua test baru, ditambah `run_check_load_cancel_while_requests_in_flight_is_130`. Pada kode asli kedua test hijau 5/5, 10/10 berturut-turut, dan 4 binary paralel × 32 thread di bawah beban 4× `yes` (semua hijau).
- Saran tertutup: kasus kedua kini memakai `CountingListener` dan assert `parent directory` serta `listener.finish() == 0`.
- Gate: fmt exit 0, clippy exit 0, `cargo test --all-targets` 400 lulus / 0 gagal.
- 8080: listener penghitung python di 127.0.0.1:8080 selama seluruh `cargo test --all-targets` (kontrol positif 1 koneksi) mencatat 0 koneksi dari test. Hanya dua komentar di `tests/validate_cli.rs` yang menyebut 8080.

### 2026-10-10 — S2 review ronde 1: FIX (reviewer)

Gate: fmt exit 0, clippy exit 0, `cargo test --all-targets` 402 lulus / 0 gagal. Listener penghitung di 127.0.0.1:8080 (kontrol positif 1) mencatat 0 koneksi selama seluruh test. Kode produksi benar: penanda `report NOT WRITTEN: <path>` dicetak di dua jalur gagal (`build_report` dan `write_report_atomic`), hanya path (tanpa body/secret), sesudah baris check, tidak dicetak saat sukses. Snapshot diff ronde 1: `/tmp/p4s2_r1.diff`.

Mutan pada `exit_code` (dibatasi ke badan `exit_code`, salinan /tmp): 13 dari 14 tertangkap (E1, E2, E3, E4, E5, E6, E7 tanpa 130, E7 cancel hanya di check pertama, E8, E9, E11 suite incomplete, E11 satu-per-check). Mutan pada penanda/exit di `run_check`: penanda dihapus, penanda ke stderr, gagal tulis jadi exit 0, penanda juga dicetak saat sukses, semuanya tertangkap.

Lolos:
- **E10**: hapus `if has_excluded_reason { return 2; }` dari `exit_code` -> semua test hijau. Test "sudah ada" (`exit_code_partial_table`, akhir) memakai race/load `not_run incomplete`, yang sudah memberi 2 lewat `not_conclusive`, jadi cabang E10 tidak pernah diuji sendiri.
- **E12 cancel**: ubah `return if cancelled { 130 } else { EXIT_INVALID }` (jalur write gagal, `src/cli.rs:362`) menjadi `return EXIT_INVALID` -> semua test hijau. Baris matriks E12 "gagal ditulis + cancel = 130" tidak punya test.

Temuan FIX:
1. `src/report.rs` tests: tambah baris E10 di `exit_code_full_suite_matrix`: `suite((Fail, Complete, excl), p, p)` dengan `excl = &[ReasonCode::ExcludedByOnly]` -> 2 (cabang lain akan memberi 1). Reviewer menguji ini: hijau di kode asli, merah pada mutan E10.
2. `src/cli.rs` tests: tambah `run_check_full_cancel_and_report_write_failure_is_130_with_marker` = pola test gagal-tulis suite (direktori 0o555, skip bila root) dengan `std::future::ready(())` sebagai cancel: exit 130, stdout 4 baris dengan baris terakhir `report NOT WRITTEN: <path>`, isi `OLD` utuh, satu file di direktori. Reviewer menguji ini: hijau di kode asli, merah pada mutan E12 cancel.

Bukan temuan: `examples/api-checks.json` yang untracked (mtime 12:33, sebelum S2, tanpa referensi di kode/test, secret hanya `${ENV:..}`) bukan bagian dari S2; main session memutuskan apakah file ini di-commit atau dibuang.

### 2026-10-10 — S2 review ronde 2: PASS (reviewer)

- Diff-of-diffs (`/tmp/p4s2_r1.diff` vs diff sekarang): hanya dua hunk test baru (baris E10 di `report::tests::exit_code_full_suite_matrix`, test `cli::tests::run_check_full_cancel_and_report_write_failure_is_130_with_marker`); kode produksi tidak berubah.
- Mutan (salinan /tmp, kontrol hijau dulu): hapus cabang `if has_excluded_reason { return 2; }` -> `exit_code_full_suite_matrix` FAILED (20 lulus/1 gagal); `return if cancelled { 130 } else { EXIT_INVALID }` di jalur write gagal (`src/cli.rs:362`) -> `return EXIT_INVALID` -> test cancel+write-fail FAILED (61 lulus/1 gagal). Bukan error kompilasi. Salinan dipulihkan, `diff -r src` identik.
- Gate: fmt exit 0, clippy `-D warnings` exit 0, `cargo test --all-targets` 403 lulus / 0 gagal; 5 run tambahan semuanya exit 0 tanpa FAILED (tidak ada flaky).
- 8080: listener penghitung di 127.0.0.1:8080 (kontrol positif 1 koneksi) selama 6 run penuh: tetap 1, jadi 0 koneksi dari test.
- Catatan kecil (bukan blocking): jalur `build_report` gagal (`src/cli.rs:356`) dengan cancel tetap tanpa test; jalur ini tidak bisa dipicu dari luar tanpa test double, jadi diterima.
- Status slice di tabel tidak diubah reviewer (sesuai instruksi).

### 2026-10-10 — S3 review ronde 1: PASS (reviewer)

- Config: diff `json.tool` `examples/core-checks.json` vs `examples/core-checks-broken.json` = tepat 4 baris path (login baseline+probe -> `/broken/login/unprotected`, race action -> `/broken/actions/{id}/apply-racy`, load -> `/broken/critical/flaky`). Verify race tetap `/actions/{id}`. Tidak ada literal `demo`/`demo-pass`/`wrong-pass`/`fixture-token`/`act-1`; semua secret lewat `${ENV:..}`.
- `validate` biner nyata (env dev): exit 0, `planned requests: 117`, `fixed waits: 950 ms`, lima nama env sama dengan healthy.
- Live (fixture `--port 0`, base_url di salinan config, reset sebelum tiap run): healthy exit 0, tiga PASS complete (`single_side_effect`, `thresholds_met`, `protection_observed`), urutan stdout race -> load -> brute_force, `scope: full`; rusak exit 1, race FAIL `duplicate_side_effect` (delta 3), load FAIL `error_rate_exceeded` (500:10 dari 100), brute_force FAIL `protection_policy_not_met`; stats 5/101/9/2. Diulang sekali (healthy lagi exit 0). Tanpa reset: healthy exit 2 (race `initial_counter_not_zero`, brute_force `account_already_locked`). Varian di dokumen (`accepts-invalid` + `slow` dengan `max_p95_ms` 100): FAIL `invalid_credentials_accepted` dan FAIL `latency_exceeded`. Tidak ada nilai rahasia di stdout/stderr/report. Artefak /tmp dihapus.
- Dokumen `docs/fixture.md` "Suite lengkap (P4)": rute, lima env, urutan, aturan exit, reset cocok dengan `tests/support/fixture.rs` dan `src/cli.rs`.
- Gate: fmt exit 0, clippy `-D warnings` exit 0, `cargo test --all-targets` 404 lulus / 0 gagal (`cli::tests::validate_broken_example_ok_and_matches_healthy_env` ok). Listener penghitung 127.0.0.1:8080 (kontrol positif 1) tetap 1 selama run penuh: 0 koneksi dari test.
- Saran (tidak blocking): kalimat "Tanpa reset ... brute_force INCONCLUSIVE `account_already_locked`" hanya benar untuk suite healthy; suite rusak tanpa reset memberi race INCONCLUSIVE + brute_force tetap FAIL (rute unprotected tanpa state), exit 2. Judul/intro `docs/fixture.md` belum menyebut P4 (bisa dirapikan di S6).
- Status slice di tabel tidak diubah reviewer.

### 2026-10-10 — S4 review ronde 1: FIX (reviewer)

Gate: fmt exit 0, clippy `-D warnings` exit 0, `cargo test --all-targets` 412 lulus / 0 gagal (`fixture_proof` 8/8, ±3,1 s). Listener penghitung di 127.0.0.1:8080 (kontrol positif 1) tetap 1 selama run penuh: 0 koneksi dari test. `fixture_proof` 6x berurutan dan 4 binary paralel di bawah 4x `yes`: semuanya hijau. sha256 `examples/*.json` sama sebelum/sesudah; `src/` tidak berubah. Live (fixture port 0, 3 putaran healthy/broken/accepts-invalid/race-error): reasons persis `[duplicate_side_effect]`, `[invalid_credentials_accepted]`, `[counter_invalid]`; 0 kemunculan `demo`/`wrong-pass`/`act-1`/`fixture-token` di stdout/stderr/report, kontrol nama check ada.

Mutan (salinan /tmp, kontrol hijau, tanpa error kompilasi), terhadap `--test fixture_proof`: tertangkap = exit selalu 0 (6 merah), FAIL→2 (4), ERROR/INCONCLUSIVE tidak dihitung (2), cek FAIL sebelum not_conclusive (1), urutan dibalik (7), `break` setelah FAIL (3), check sesudah FAIL jadi `not_run` (3), reset fixture no-op (4), evidence race dibuang (3), console dibalik (7), catatan `partial run` selalu (7).

Lolos:
- `CheckName::Load`/`Race` di console memakai `brute_force::console_line` **hanya untuk scope full**: seluruh `cargo test --all-targets` hijau. `assert_lines` hanya mengecek prefix `"{name} {VERDICT}"`; test healthy hanya `ends_with("reasons=...")`. AC1.6 ("masing-masing sama dengan `console_line` check itu") tidak terkunci untuk suite.
- Reason tambahan pada FAIL race (`duplicate_side_effect` + lain) dan bf accepts-invalid lolos di `fixture_proof` (assert `contains`). Live menunjukkan reasons persis satu elemen, jadi assert persis aman.
- Completion suite selalu `complete`: lolos di `fixture_proof` (hanya jalur complete), tetapi tertangkap 12 test unit `cli.rs` → bukan temuan.

Temuan FIX (tester, `tests/fixture_proof.rs`):
1. AC1.6: di `assert_lines` (atau test healthy + race-error) tambahkan penanda format per check: baris race memuat ` delta=` dan ` skew=`, baris load memuat ` error_rate=` dan ` max_in_flight=`, baris brute_force memuat ` first_protected=`. Harus merah pada mutan "Load/Race full → `brute_force::console_line`".
2. AC1.4/AC2.2: ganti `contains` dengan `assert_entry` persis: race broken `["duplicate_side_effect"]` `complete` (broken + mixed posisi 0), bf accepts-invalid `["invalid_credentials_accepted"]` `complete`, race ERROR `["counter_invalid"]` `complete`.

Bukan temuan: perbandingan run 1 vs run 3 (`summary()`) tidak bisa kosong, karena `assert_healthy` lebih dulu mengunci tiga entri persis di kedua run. `config_examples_are_not_modified_by_these_tests` hanya mengecek `base_url` (bukan hash); diterima karena test lain membaca contoh read-only.

### 2026-10-10 — S4 review ronde 2: PASS (reviewer)

- Perbaikan tester terverifikasi di `tests/fixture_proof.rs`: `assert_lines` (baris 153–174) kini mengecek token per check (race ` delta=`/` skew=`, load ` error_rate=`/` max_in_flight=`, brute_force ` first_protected=`); semua `contains` diganti `assert_entry` persis (race broken + mixed posisi rusak, bf accepts-invalid, race ERROR `["counter_invalid"]`, completion `complete`). `git diff --quiet -- src/ examples/ Cargo.*` bersih.
- Mutan (salinan /tmp, `--test fixture_proof`, kontrol hijau sebelum/sesudah, 0 error kompilasi, `diff -r src` identik setelah pulih): Load full → `brute_force::console_line` = 7 merah (`should contain ' error_rate='`); Race full → bf line = 7 merah (`' delta='`); FAIL race + reason ekstra = 2 merah (broken, mixed); bf accepts-invalid + reason ekstra = 1 merah; race ERROR + reason ekstra = 1 merah. Semua di asersi `reasons` (baris 179) atau token, bukan kebetulan.
- Gate: fmt exit 0, clippy `-D warnings` exit 0, `cargo test --all-targets` 412 lulus / 0 gagal. `fixture_proof` 5x berurutan (8/8, ±3,1 s) dan 4 binary paralel `--test-threads 8` di bawah 4x `yes`: semua hijau.
- 8080: listener penghitung (kontrol positif 1) tetap 1 sepanjang semua run: 0 koneksi dari test. sha256 `examples/*.json` sama sebelum/sesudah.
- Status slice di tabel tidak diubah reviewer.

### 2026-10-10 — S5 review ronde 1: FIX (reviewer)

Gate: fmt exit 0, clippy `-D warnings` exit 0, `cargo test --all-targets` 416 lulus / 0 gagal (`fixture_proof` 12/12, ±3,2 s). `fixture_proof` 5x berurutan + 2 putaran × 4 binary paralel `--test-threads 12` di bawah 4x `yes`: semua hijau. Listener penghitung 127.0.0.1:8080 (kontrol positif 1) tetap 1: 0 koneksi dari test. sha256 `examples/*.json` sama; `src/` dan `examples/` tidak berubah (hanya `tests/fixture_proof.rs`). Snapshot diff: `/tmp/p4s5_r1.diff`.

Mutan (salinan /tmp `/tmp/s5m`, kontrol hijau sebelum/sesudah, tanpa error kompilasi, `diff -r src` identik): tertangkap = rename→copy (temp tersisa), timestamp pecahan, `--out` lama tidak diganti, handler SIGINT tidak dipakai, SIGINT tidak didaftarkan (SIGUSR1), cancel→exit 2, `Stop::Cancelled`→`deadline_exceeded` global, check berikutnya pakai channel stop baru, nilai env di stderr / di report (`tool_version` = body probe) / di baris console race (full saja), body tidak terkirim, id dibuang dari URL di wire (kontrol positif wire merah).

Lolos (seluruh `--test fixture_proof` hijau):
- **M2 tulis non-atomic** (`fs::rename` diganti `fs::write(path)` langsung, temp dihapus): `--out` diganti, JSON valid, tidak ada temp → test AC5.2 hijau. "Atomic" tidak dibuktikan oleh test S5 (unit P0 `atomic_write_*` juga hijau pada mutan ini).
- **M8 load mengirim preflight setelah cancel** (lalu tetap `not_run [cancelled]`): hijau. Asersi "tidak ada koneksi baru setelah cancel" diambil SETELAH proses keluar (`after_exit` + sleep 300 ms) → trivial, proses mati tidak bisa membuka koneksi.
- **M7b reason race saja salah** (`deadline_exceeded` saat cancel): hijau, karena test hanya mengecek `checks[1..2]`; race cukup `!= pass` + `incomplete`.

Temuan FIX (tester, `tests/fixture_proof.rs`):
1. `sigint_on_full_suite_…` (baris 542): simpan `let seen_at_sigint = h.seen();` tepat sebelum `kill -INT`; setelah exit assert `seen_at_sigint == 1` dan `h.seen() == seen_at_sigint`. Asersi `after_exit` + sleep boleh dihapus (trivial).
2. Test yang sama: assert `checks[0]` `verdict == "not_run"` dan `reasons == ["cancelled"]` (race menggantung di verify-before).
3. `out_with_old_content_…`: (`#[cfg(unix)]`) ambil `MetadataExt::ino` `--out` setelah menulis `OLD`, dan sesudah run assert inode berbeda (rename membuat file baru; tulis di tempat mempertahankan inode).
4. Label komentar: test sentinel adalah AC5.3 (bukan "AC10"), test SIGINT adalah AC3.4 (bukan AC5.3).

Reviewer sudah menjalankan versi 1–3 di salinan /tmp: hijau 5/5 di kode asli; M2 merah (`assert_ne` inode), M7b merah (`["deadline_exceeded"]` vs `["cancelled"]`), M8 merah (`2` vs `1`).

Bukan temuan: sentinel `TEST_AUTHORIZATION` tidak diuji di suite (sesuai rencana S5, ditutup oleh `--only` P3). Kontrol positif wire bermakna (mutan id dibuang dari URL membuatnya merah). Catatan alat: `shutil.copytree` memulihkan mtime lama, sehingga cargo memakai binary mutan yang basi; `touch` file setelah restore.

### 2026-10-10 — S5 review ronde 2: PASS (reviewer)

Perubahan sejak ronde 1 (`diff /tmp/p4s5_r1.diff` vs diff sekarang): hanya `tests/fixture_proof.rs`: helper `inode()`, `assert_ne!` inode `--out` (`#[cfg(unix)]`), `seen_at_sigint` diambil sebelum `kill -INT` + `assert_eq!(…, 1)`, loop `[0, 1, 2]` dengan `not_run` + `["cancelled"]`, asersi akhir `h.seen() == seen_at_sigint`, label AC3.4/AC5.3. `src/`, `examples/`, `Cargo.*` tidak berubah (`git diff --quiet` exit 0); sha256 `examples/*.json` sama dengan ronde 1.

Mutan (salinan /tmp, `touch` setelah restore, `diff -r src` identik, kontrol hijau sebelum dan sesudah 12/12):
- M2 tulis non-atomic (`fs::rename` → hapus temp + `fs::write(path)`): MERAH, `fixture_proof.rs:471` "--out was written in place, not renamed" (inode sama 493109).
- M7b reason stop race = `deadline_exceeded`: MERAH, `:605` `["deadline_exceeded"]` vs `["cancelled"]`.
- M8 load mengirim preflight setelah cancel: MERAH, `:619` "a connection was opened after cancel" `2` vs `1`.

Gate: fmt exit 0, clippy `-D warnings` exit 0, `cargo test --all-targets` 416 lulus / 0 gagal. `fixture_proof` 5x berurutan 12/12 (±3,2 s); 2 putaran × 4 binary paralel `--test-threads 12` di bawah 4x `yes`: semua rc 0. Listener penghitung 127.0.0.1:8080: kontrol positif 1, akhir tetap 1 → 0 koneksi dari test.

Catatan (bukan blocker): `assert_eq!(seen_at_sigint, 1)` bergantung pada race yang menggantung di request pertama (verify-before); jika urutan full suite berubah, test ini ikut harus diubah, dan itu memang diinginkan. Di non-unix, cek atomic dilewati (`inode` = 0), sedangkan rencana hanya mewajibkan Linux.

### 2026-10-10 — S6 review ronde 1: FIX (reviewer)

Gate: fmt exit 0, clippy `-D warnings` exit 0, `cargo test --all-targets` 416 lulus / 0 gagal (`fixture_proof` 12/12). Diff hanya `README.md` + satu baris `CLAUDE.md`; `src/`, `tests/`, `examples/`, `docs/` tidak berubah. `examples/api-checks.json` tidak dirujuk.

Live (fixture `--port 8099`, salinan config dengan `base_url` diganti, env README): `validate` exit 0 (117 request, 950 ms, 5 nama env); healthy exit 0, tiga baris console identik dengan contoh README (termasuk `p95=3`); healthy tanpa reset exit 2 (race `initial_counter_not_zero`, brute_force `account_already_locked`, load PASS); broken setelah reset exit 1 (`duplicate_side_effect`, `error_rate_exceeded`, `protection_policy_not_met`); `--only race` exit 2, `scope: partial`, dua `excluded_by_only`, catatan stderr; `--only load` dengan `HTTP_PROXY`/`ALL_PROXY` palsu tetap PASS (proxy diabaikan); direktori read-only → tiga baris + `report NOT WRITTEN: <path>`, exit 2, isi lama `OLD` utuh; env hilang → exit 2, `--out` tidak dibuat; `validate --only brute_force` tanpa env race exit 0; stats fixture 101 load request (1+N).

Kode vs README: `exit_code` baris 0/1/2 cocok; reason per check cocok dengan `evaluate` (`overlapping_rules` hanya ERROR/ekstra FAIL; `workload_incomplete` INCONCLUSIVE tanpa stop, NOT_RUN dengan stop; `baseline_failed` INCONCLUSIVE/ERROR); `connect_timeout_ms` default 3000 (`Limits::default`); `no_proxy()`, `Policy::none`, `pool_max_idle_per_host(0)`; SIGINT didaftarkan di `check_blocking` setelah `check_args`, jadi Ctrl-C sebelum itu mematikan proses tanpa report dan tanpa request (Keterbatasan baris 233 benar). Batas threshold (2..=100, 2..=32, ≤1000, ≤32, 0..=1, 202 ditolak, tail 1..=attempts-1) cocok dengan `validate.rs`.

Temuan FIX (implementer, `README.md`):
1. Baris 69, exit 130 "di titik mana pun": salah. (a) Ctrl-C sebelum handler: proses mati karena sinyal (shell memang menampilkan 130, tetapi tanpa report, lihat baris 233). (b) Ctrl-C setelah check terakhir selesai (saat console/report ditulis) diserap handler tokio tapi `cancel` tidak lagi di-poll setelah `break r` (`src/cli.rs` loop `select!`), jadi exit mengikuti verdict (bisa 0/1/2). Ganti dengan: 130 bila Ctrl-C datang saat suite berjalan, termasuk bila report gagal ditulis; lihat Keterbatasan untuk Ctrl-C sangat awal.

Sebaiknya diperbaiki (tidak blocking):
2. Baris 25 (dan baris 104 untuk deadline): "menjadi `not_run [cancelled]`" tidak persis: load yang terhenti = `not_run [cancelled, workload_incomplete]`; brute_force yang sudah melihat kredensial salah diterima tetap FAIL `invalid_credentials_accepted` + `cancelled` (incomplete). Tulis "`not_run` dengan reason `cancelled`/`deadline_exceeded` (load juga `workload_incomplete`); FAIL yang sudah terbukti tetap FAIL".
3. Baris 106: hanya `connect_timeout_ms` disebut opsional, padahal seluruh `limits` opsional dengan default (5000, 120000, 1048576).

Saran: baris 48 verdict `error` menyebut "Config", padahal config tidak valid tidak pernah menghasilkan verdict (exit 2 sebelum run); baris 105 "lebih besar dianggap error" = transport error `body_too_large` (di load dihitung sampel gagal, bukan ERROR); baris 212 base URL boleh punya base path; `config_invalid`/`report_write_failed` ada di enum tetapi tidak pernah dihasilkan (penghilangan dari tabel benar).

CLAUDE.md baris Commands: akurat (perintah jalan, 12/12, bahasa Inggris). Tautan/jalur di README semua ada.

### 2026-10-10 — S6 review ronde 2: PASS (reviewer)

Gate: fmt exit 0, clippy `-D warnings` exit 0, `cargo test --all-targets` 416 lulus / 0 gagal. Diff: `README.md` + satu baris `CLAUDE.md` dari ronde 1; `src/`, `tests/`, `examples/`, `docs/` tidak berubah (`examples/api-checks.json` untracked, bukan bagian slice, tidak dirujuk).

Temuan ronde 1:
1. Exit 130 (baris 69, 236): ✅ cocok dengan `src/cli.rs:295-308` (cancel hanya di-poll sampai `break r`) dan `:356/:362` (130 pada build/write gagal). Live: SIGINT di tengah brute_force (suite dan `--only`) → rc 130.
2. Reason cancel/deadline (baris 25, 104): ✅ load `[s.reason(), workload_incomplete]` (`load.rs:193-197`); brute_force FAIL + stop reason + incomplete (`brute_force.rs:179-188`, test `m22_cancel_after_success_probe_is_still_fail`). Live (rute `/broken/login/accepts-invalid`, SIGINT setelah 3 login): `brute_force FAIL ... reasons=invalid_credentials_accepted,cancelled`, report `fail` incomplete, tidak ada login setelah SIGINT.
3. `limits` opsional (baris 108): ✅ `#[serde(default)] pub limits` + `Limits::default` = 3000/5000/120000/1048576.
Saran (Config, base path): ✅.

Sebaiknya diperbaiki (tidak blocking, satu kalimat masing-masing, bisa langsung di main session):
- Baris 105 "di load dihitung sampel gagal, bukan verdict ERROR": menyesatkan (saran ronde 1 saya sendiri kurang tepat). Preflight memakai request yang sama, jadi body yang selalu terlalu besar membuat load ERROR `preflight_failed, transport_error`. Live: `limits.response_body_limit_bytes: 1`, `--only load` → `load ERROR ... reasons=preflight_failed,transport_error`. Usul: "(di load: pada preflight = ERROR `preflight_failed`; pada sampel = sampel gagal)".
- Baris 69 "Ctrl-C yang sangat awal ... tidak menghasilkan 130": proses mati karena SIGINT, jadi `echo $?` di shell tetap mencetak 130 (dibuktikan: rc -2 di python, `shell_status=130` di bash), hanya saja tanpa report. Usul: "Ctrl-C sangat awal mematikan proses (shell menampilkan 130) tanpa report; Ctrl-C setelah semua check selesai tidak menghasilkan 130".
- Minor: load yang dibatalkan sebelum preflight menjawab = `not_run [cancelled]` saja (`load.rs:163-168`), tanpa `workload_incomplete`.

### 2026-10-10 — S7 phase gate P4: PASS (reviewer)

Gate (HEAD f94b05e, pohon kerja bersih kecuali `examples/api-checks.json` untracked yang tidak dirujuk): `cargo fmt --all -- --check` exit 0; `cargo clippy --all-targets --all-features -- -D warnings` exit 0; `cargo test --all-targets` exit 0, 416 lulus / 0 gagal; `cargo test --test fixture_proof` exit 0, 12/12 (±3,15 s), diulang 3x lagi 12/12. Listener penghitung di 127.0.0.1:8080 selama semua run: kontrol positif 1, akhir 1 → 0 koneksi dari test. Tanpa `unwrap`/`expect` produksi (awk sampai `#[cfg(test)]`: 0). `Cargo.toml`/`Cargo.lock` tidak berubah sejak b1aa59d (tanpa dependency baru).

Transkrip manual (fixture `cargo run --example fixture`, port 8080, env dev dari `docs/fixture.md`; path `--out` di direktori temp, disingkat):

```
$ curl -s -X POST http://127.0.0.1:8080/__admin/reset
{"reset":true}
$ ./target/debug/api-toolkit validate --config examples/core-checks.json
config OK
planned requests: 117
fixed waits: 950 ms
environment variables present: LOGIN_INVALID_PASSWORD, LOGIN_USER, LOGIN_VALID_PASSWORD, TEST_ACTION_ID, TEST_AUTHORIZATION
exit=0
$ ./target/debug/api-toolkit check --config examples/core-checks.json --out result-healthy.json
race PASS delta=1 before=0 after=1 actions=5/5 skew=1/100ms reasons=single_side_effect
load PASS error_rate=0/0.01 p95=3/500ms attempted=100/100 samples=100 max_in_flight=10/10 timeouts=0 status=200:100 errors=none reasons=thresholds_met
brute_force PASS first_protected=6 tail=3/2 attempts=8/8 reasons=protection_observed
exit=0
$ curl -s -X POST http://127.0.0.1:8080/__admin/reset
{"reset":true}
$ ./target/debug/api-toolkit check --config examples/core-checks-broken.json --out result-broken.json
race FAIL delta=2 before=0 after=2 actions=5/5 skew=1/100ms reasons=duplicate_side_effect
load FAIL error_rate=0.1/0.01 p95=3/500ms attempted=100/100 samples=100 max_in_flight=10/10 timeouts=0 status=200:90,500:10 errors=none reasons=error_rate_exceeded
brute_force FAIL first_protected=none tail=0/2 attempts=8/8 reasons=protection_policy_not_met
exit=1
$ curl -s http://127.0.0.1:8080/__admin/stats
{"action_requests":5,"load_requests":101,"login_requests":9,"verify_requests":2}
```

stderr kosong pada kedua run. Report: healthy `report_version` 1, `scope: full`, `completion: complete`, `started_at` `2026-10-10T14:47:57Z` (tanpa pecahan detik), race/load/brute_force `pass` `complete`; broken `fail` `complete` dengan reasons persis di atas. `demo-pass`/`wrong-pass`/`act-1`/`fixture-token` 0x di stdout/stderr/report. Fixture dihentikan (SIGINT), 0 listener 8080 tersisa, direktori temp dihapus.

Sentinel (python -I, binary nyata, fixture `--port 0`): (A) sentinel di `LOGIN_INVALID_PASSWORD` + `TEST_ACTION_ID`, suite PASS exit 0 → 0 kemunculan di stdout+stderr+report, report memuat `"race"`, `"load"`, `"brute_force"`; (B) kelima env = sentinel → exit 2 (race ERROR `verifier_failed`, load INCONCLUSIVE `target_not_ready`, bf INCONCLUSIVE `baseline_failed`) → 0 kemunculan; (C) kontrol positif wire: recorder TCP mentah sebagai base_url → 3 koneksi, sentinel 5x di wire (termasuk `TEST_AUTHORIZATION` dan `TEST_ACTION_ID`), 0x di stdout/stderr/report, exit 2.

False PASS lintas-slice: `run` (`src/cli.rs`) mengembalikan 0 hanya dari clap `--help`/`--version`, `validate` sukses, atau `report::exit_code` setelah report berhasil ditulis (`src/cli.rs:364`); jalur build/tulis gagal → 2/130 (`:356`, `:362`). `exit_code` (`src/report.rs:501-539`) memberi 0 hanya bila `scope: full`, tanpa `cancelled`/`excluded_by_only`, tepat satu entri per `CheckName`, semua `pass` + reason pass + `complete`, suite `complete`; `partial_exit_code` tidak pernah 0. Check yang belum jalan saat stop tetap dipanggil (loop `for selected` tanpa `break`) dan menghasilkan entri `not_run`, jadi tidak ada check hilang. Mutan lintas slice yang relevan sudah tertangkap di S1–S5 (stop melewati check, verdict ditimpa setelah stop, E10, cancel+write gagal, console scope full, atomic, preflight setelah cancel).

Tabel AC, matriks E1–E13, limitations, dan konsep Rust: di laporan reviewer ke main session (ringkas: semua AC met kecuali AC7 = fixture proof selesai, validasi target nyata belum dilakukan).
