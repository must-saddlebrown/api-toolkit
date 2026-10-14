# Decision log

Format: tanggal, keputusan, alasan, bagian rencana yang terdampak. Entri terbaru di bawah. Jika bertentangan dengan `docs/PLAN.md`, entri di sini yang berlaku sampai rencana diperbarui.

## 2026-10-08 — Orkestrasi agen

- Agen yang dipakai: agen user-level yang sudah ada (Explore, researcher, analis, implementer, reviewer, tester; deep-engineer manual). Tidak ada agen baru.
- Prosedur fase ada di skill `/phase`; aturan proyek di `CLAUDE.md`.
- Unit test inline Rust ditulis implementer; tester menulis integration test di `tests/`, karena guard tester hanya mengizinkan path test.

## 2026-10-09 — Ganti rencana ke Small Core Proof

Keputusan pengguna. Rencana aktif menjadi `docs/PLAN.md` (Small Core Proof). Rencana flow builder revisi 1 dipindah ke `docs/archive/PLAN-flow-builder-rev1.md` sebagai historis.

- v0.1 adalah CLI dengan tiga check: login brute-force protection, write-once race, parallel load. Fase P0–P4 menggantikan F0–F6.
- Ditunda ke roadmap (PLAN §13): TUI, OpenAPI import, flow builder, node `concat`/`template`, assertion array, JUnit, run history, bundle export, `.env` lokal. Keputusan revisi 1 terkait fitur-fitur ini tidak berlaku untuk v0.1.
- Secret hanya dari environment variable melalui `${ENV:NAME}` (PLAN §9).
- Checkpoint pengguna utama setelah P4, bukan per fase.
- Kode dari rencana lama, jika ada, dipakai ulang hanya jika membantu core proof; inventaris dibuat sekali di `.claude/work/migration.md` (PLAN §14).

## 2026-10-10 — Pilihan teknis P0

Detail dan slice: `.claude/work/P0-plan.md`. Versi diambil dari crates.io pada 2026-10-10. Terdampak: PLAN §2 (kandidat dependency), §8, §9, §10, §11 (P0), §13.

- Package: satu package, `src/lib.rs` + `src/main.rs` tipis, binary `api-toolkit`, edition 2024, `rust-version = "1.88"`. Alasan: integration test di `tests/` butuh library; MSRV tertinggi dependency adalah 1.88 (`time`).
- Dependency: clap 4.6 (derive), tokio 1.53 (`rt-multi-thread`, `macros`, `time`, `net`, `sync`; dev `test-util`), reqwest 0.13 (`default-features=false`, `rustls`, `json`), serde 1 + serde_json 1, thiserror 2 (library), anyhow 1 (hanya main/cli), sha2 0.11 (config hash), time 0.3 (`formatting`, timestamp RFC3339 UTC). Dev: axum 0.8 (fixture), tempfile 3. Alasan: kandidat PLAN §2, seperlunya. Risiko: reqwest `rustls` memakai aws-lc-rs (butuh toolchain C); dibuktikan di slice S1, fallback dicatat di sini bila gagal. Nama feature TLS di reqwest 0.13 adalah `rustls`, bukan `rustls-tls`.
- Tanpa crate `uuid`: run ID = `{unix_ms}-{8 hex pertama config_hash}`. Alasan: YAGNI; ditambah bila run paralel dibutuhkan.
- Config: JSON saja; `deny_unknown_fields` di semua struct; dua tahap (`Config` mentah, lalu `ResolvedConfig` setelah env disubstitusi lewat fungsi lookup yang diinjeksi agar test tidak mengubah env proses). `limits` mendapat `connect_timeout_ms` opsional (default 3000) karena PLAN §8 menyatakan semua timeout dapat dikonfigurasi. `json_equals` berbentuk objek `{pointer: nilai}`, `json_exists` array pointer.
- Aturan validasi turunan (melengkapi PLAN §5/§8/§9): `protect_by_attempt` dalam 2..=attempts; `protected_tail_count` dalam 1..=attempts-1 (attempt 1 harus rejection, PLAN §5); overlap `status_in` antar success/rejection/protection ditolak saat validasi (konservatif); `initial_counter`=0 dan `expected_delta`=1 wajib (kontrak write-once §6); base_url harus origin http(s) tanpa userinfo/query/fragment; fixed waits = `(attempts-1)*interval_ms + settle_ms` ditolak bila melebihi `suite_deadline_ms`.
- Secret: tipe `Secret` dengan `Debug` teredaksi, tanpa `Display`/`Serialize`; pesan error hanya memuat nama env.
- Kontrak enum report v1 dikunci di P0 (`Verdict`, `CheckName`, `Completion`, `ReasonCode`, `Outcome`, `TransportErrorCode`), semua snake_case; daftar varian ada di handoff P0. Menambah varian bersifat aditif; mengubah/menghapus = naik `report_version`. `config_hash` = SHA-256 byte file config mentah.

## 2026-10-10 — Interpretasi validasi config (P0 S3)

Detail: `.claude/work/P0-plan.md`. Terdampak: PLAN §5, §8, §9. Reviewer PASS.

- `RequestBudget { requests, fixed_wait_ms }`: requests = 1 + attempts (brute force) + 2 + concurrency (race: dua pembacaan verifier + N action) + 1 + load requests (preflight + load). Contoh §9: 117 request, 950 ms fixed wait.
- Fixed waits memakai aritmetika `u128`; yang tidak muat `u64` atau melebihi `suite_deadline_ms` ditolak (tidak ada saturate/wrap). Sama dengan deadline diterima, hanya lebih besar yang ditolak.
- Batas numerik PLAN §8: probe ≤ 100, race ≤ 32, load requests ≤ 1000, load concurrency ≤ 32. Batas lain (timeout, body, deadline) hanya wajib > 0 karena PLAN tidak menetapkan batas atas.
- `max_error_rate` dalam 0..=1 (NaN/inf ditolak). Kode status dalam 100..=599; `status_in` kosong ditolak. `json_equals`/`json_exists` yang ada tetapi kosong ditolak (menghilangkannya sah).
- `protected_tail_count` dalam 1..=attempts-1 (koreksi dari entri 2026-10-10 sebelumnya: attempt 1 harus rejection, jadi tail tidak boleh mencakup semua attempt).
- JSON Pointer harus diawali `/`, `~` diikuti `0`/`1`. `json: null` ditolak (tanpa body = hilangkan field). Key duplikat di `path_params`/`query`/`headers`/`json_equals` ditolak saat parse; nama header yang beda huruf besar/kecil ditolak saat validate. Pesan error hanya memuat nama field/rule, tidak pernah nilai.
- `base_url` harus literal `scheme://host` (http/https), tanpa userinfo/query/fragment/spasi/karakter kontrol; base path boleh. `path` request diawali satu `/`, tanpa `//` di awal, `://`, `?`, `#`, `\`, karakter kontrol. Nama placeholder harus sama persis dengan key `path_params` dua arah.
- `validate` berhenti di masalah pertama.
- Diteruskan ke S4: URL dibangun dari bagian yang sudah divalidasi dengan encoding segmen path; S4 wajib punya test untuk path/base_url aneh. Saran reviewer yang belum diambil: `race.concurrency = 1` masih diterima; method huruf kecil dan header `Host`/`Content-Length` masih diterima.

## 2026-10-10 — Resolusi env dan pembangunan URL (P0 S4)

Detail: `.claude/work/P0-plan.md`. Terdampak: PLAN §8, §9. Reviewer PASS.

- Env var yang ada tetapi kosong dianggap hilang (konservatif: kredensial/id kosong hampir pasti salah konfigurasi).
- Placeholder = string utuh berawalan `${ENV:` dan berakhiran `}`; nama `[A-Za-z_][A-Za-z0-9_]*`. Bentuk lain yang mengandung `${ENV:` (`${ENV:}`, `${ENV:A B}`, `${ENV:A}${ENV:B}`) = `BadPlaceholder` (path field saja). `prefix-${ENV:X}`, `${env:x}` tetap literal sesuai PLAN §9 (tidak ada interpolasi); nilai env yang berisi `${ENV:Y}` tidak di-resolve ulang.
- `Secret`: manual `Debug` teredaksi, tanpa `Clone`/`Display`/`Serialize`; satu-satunya akses lewat `expose()`. `ResolvedRequest` menyimpan URL, nilai header, dan body sebagai `Secret`.
- Nilai header hasil resolve yang memuat CR, LF, atau NUL ditolak di `resolve` (nama field saja). Aturan persis itu, tidak lebih lebar.
- URL: `percent-encoding 2.3` jadi dependency langsung (sudah ada di tree via reqwest); parsing lewat `reqwest::Url`. Semua karakter selain `A-Za-z0-9-._~` di-encode. Segmen `.`/`..` ditolak (`DotSegment`); `%` di nilai di-encode (`%2e%2e` jadi `%252e%252e`). Hasil dicek ulang: scheme, host, port, jumlah segmen sama dengan base. Nilai path param kosong dan key query kosong ditolak.
- `resolve` tidak memanggil `validate`; S9 wajib memanggil `validate` dulu. Saran yang belum diambil: `BadPlaceholder` dan `MissingEnv` dilaporkan terpisah (dua run); `validate` bisa memberi peringatan untuk placeholder yang tampak typo (`${env:x}`).

## 2026-10-10 — Semantik matcher (P0 S5)

Detail: `.claude/work/P0-plan.md`. Terdampak: PLAN §4, §9. Reviewer PASS.

- `impl MatcherSpec { matches(status, Option<&Value>) -> bool }` di `src/matchers.rs`; tanpa tipe wrapper dan tanpa trait. Hanya mengembalikan bool, tidak pernah mencetak isi body.
- Semua kondisi yang ada di-AND. `status_in` kosong = tidak cocok. Pointer yang menunjuk ke `null` dianggap ada; expected `null` di `json_equals` hanya cocok dengan `null` eksplisit, bukan key yang hilang. Kesetaraan mengikuti serde_json: `1` ≠ `1.0` ≠ `"1"`.
- Body `None` (tidak ada / bukan JSON / terpotong) dengan kondisi `json_*` yang ada, termasuk container kosong, = tidak cocok (tanpa false PASS). Pointer tidak valid, indeks di luar jangkauan, atau body non-object = tidak cocok, tanpa panic.
- Pemanggil P1–P3 harus mengirim `None` untuk body non-JSON, terpotong, atau melebihi batas, bukan `Some(Null)`.

## 2026-10-10 — Transport bersama (P0 S6)

Detail: `.claude/work/P0-plan.md`. Terdampak: PLAN §4, §8, §10. Reviewer PASS (plus satu ronde perbaikan).

- Client reqwest 0.13: redirect `Policy::none()`, tanpa `.retry()`, TLS verification aktif, `.no_proxy()` (env `HTTP(S)_PROXY` tidak boleh membelokkan trafik), `pool_max_idle_per_host(0)` (tanpa reuse koneksi, jadi koneksi basi tidak memicu percobaan kedua tersembunyi; longgarkan di P3 bila load butuh pooling). Satu request = satu koneksi. Nilai header ditandai `set_sensitive`.
- reqwest tanpa fitur dekompresi (hanya `json`, `rustls`), tidak ada `Accept-Encoding`: batas body berlaku pada byte mentah, tidak ada decompression bomb.
- Body dibaca streaming via `chunk()`; lebih dari `max_body_bytes` = `body_too_large` (tepat di batas diterima). `content_length()` yang diketahui dan melebihi batas ditolak sebelum body dibaca. Tidak ada flag `truncated`: body kosong / bukan JSON = `None`; oversize = error. Pemanggil P1–P3 mengirim `None` ke matcher untuk body non-JSON.
- `timeout` total mencakup pembacaan body (slow-drip kena timeout). Connect timeout dilaporkan sebagai `timeout`.
- `TransportErrorCode` (di `src/report.rs`, sebelum S7): `timeout`, `connect_failed`, `tls_failed`, `body_too_large`, `body_read_failed`, `request_invalid`, `other`; snake_case. `tls_failed` belum pernah dihasilkan (reqwest tidak membedakan dari kegagalan connect; jatuh ke `connect_failed`). Tambahkan hanya bila ada sinyal andal. `TransportError` hanya membawa kode: tanpa URL, tanpa teks error reqwest, tanpa `source()`.
- Method harus huruf besar (ditolak di `validate`; transport juga meng-uppercase sebagai pertahanan). Header `Host`, `Content-Length`, `Transfer-Encoding`, `Connection` ditolak di `validate` (tanpa memandang huruf besar/kecil). `Content-Type` buatan pengguna diizinkan; transport hanya menambah `application/json` bila ada body dan tidak ada `Content-Type` pengguna.
- Tidak ada test connect-timeout (tidak bisa andal di loopback tanpa alamat non-routable); dicatat dengan komentar `ponytail:`.

## 2026-10-10 — Report model, exit code, dan penulisan atomic (P0 S7)

Detail: `.claude/work/P0-plan.md`. Terdampak: PLAN §4, §10. Reviewer PASS di ronde 2 (ronde 1 FIX: duplikat check, reason non-pass pada pass, temp file tanpa `create_new`).

- Enum report v1 terkunci di `src/report.rs` (`Verdict`, `CheckName`, `Completion`, `ReasonCode` 29 varian, `Outcome`, `TransportErrorCode`), masing-masing dengan `ALL` dan `as_str()`. Golden test memakai `match` exhaustive: varian baru tidak compile sampai stringnya ditambahkan. Dua jalur (serde dan `as_str`) diuji sama.
- Report v1: `report_version` (konstanta 1), `run_id` = `{unix_ms}-{8 hex pertama config_hash}`, `tool_version`, `config_hash` (SHA-256 hex byte file config mentah), `started_at`/`finished_at` (RFC3339 UTC), `completion`, `checks`. Tidak ada field body/header/secret. JSON pretty + newline akhir. Timestamp diinjeksikan (jam bisa dikontrol test); presisi subsecond bisa bervariasi, P4 sebaiknya memotong ke detik bila bentuk stabil dibutuhkan.
- `exit_code(&Report)` mengikuti PLAN §10 (cancel → error/inconclusive/incomplete/not_run → fail → pass). Exit 0 hanya bila tepat tiga check (satu per `CheckName`), semuanya `pass` + `complete`, alasan kosong atau hanya `protection_observed`/`single_side_effect`/`thresholds_met`, dan suite `complete`. Selain itu 2 (daftar kosong/parsial/duplikat, alasan non-pass, dll). Cancel = ada check dengan `ReasonCode::Cancelled` (tanpa flag suite) → 130, diperiksa lebih dulu.
- Untuk P4: cancel sebelum ada check harus tetap menyisipkan check yang membawa `cancelled`, atau exit code jatuh ke 2 bukan 130. Check `fail` yang membawa alasan pass (kontradiktif) menghasilkan 1; bisa diperketat.
- `write_report_atomic`: temp `.<nama>.tmp-<pid>-<counter>` (counter `AtomicU64`) di direktori yang sama, dibuka `create_new` (tidak mengikuti symlink), fsync, lalu rename; retry maksimal 8 saat `AlreadyExists`; cleanup hanya menghapus temp milik panggilan itu. `ReportError::Io` hanya memuat path dan `io::ErrorKind` (tanpa pesan OS, tanpa isi report). Belum: fsync direktori setelah rename; mode file mengikuti umask (report tidak berisi secret).

## 2026-10-10 — Fixture healthy (P0 S8)

Detail: `.claude/work/P0-plan.md`, `docs/fixture.md`. Terdampak: PLAN §9, §11 (P0). Reviewer PASS (plus satu ronde perbaikan kecil).

- Fixture = alat pengembangan, bukan produk: kode di `tests/support/fixture.rs` (axum 0.8, dev-dependency; `cargo tree -e normal` tidak memuat axum), dipakai test lewat `mod support;` dan `examples/fixture.rs` lewat `#[path]`. Bind hanya `127.0.0.1` (default port 8080, `--port`). `tokio` dev-dependency mendapat feature `signal` (Ctrl-C di example).
- Healthy login: kredensial salah → 401 untuk kegagalan ke-1..5, 429 mulai kegagalan ke-6; kredensial benar → 200 `{"access_token"}` selama hitungan gagal < 6, lalu 429. Login benar TIDAK mereset hitungan; hanya `POST /__admin/reset`. Run kedua tanpa reset membuat baseline 429 → brute force INCONCLUSIVE `account_already_locked` (sesuai PLAN §5).
- Action write-once: apply pertama → 200 dan counter +1, berikutnya → 409 tanpa increment; keputusan atomik (lock dipegang melewati cek+increment, tanpa `.await`). Id dibuat otomatis dengan count 0 saat pertama diakses. `GET /actions/{id}` → `{id, execution_count}`. `/critical` → 200. Authorization wajib untuk action dan critical.
- Reset (`/__admin/reset`) mencakup hitungan gagal login, semua counter action, counter load. Reset hanyalah alat dev; `api-toolkit` tidak memanggilnya.
- Respons fixture berisi JSON tetap dan tidak pernah memantulkan body/header request (body login yang salah bentuk → 400 `{"error":"bad_request"}`). Fixture tidak mencatat request.
- Varian broken/idempotent/slow/prelocked ditambahkan di P1–P3 saat dibutuhkan. Counter `load_requests` dihitung tetapi belum dibaca (dipakai P3).
- Catatan untuk S10 dan seterusnya: test konkurensi memakai `flavor = "multi_thread"`; client test memakai `.no_proxy()`.

## 2026-10-10 — CLI validate dan check (P0 S9)

Detail: `.claude/work/P0-plan.md`. Terdampak: PLAN §3, §9, §10. Reviewer PASS di ronde 2 (ronde 1 FIX: `validate` mencetak nama env dari pemindaian JSON mentah, bukan dari yang benar-benar di-resolve).

- `api-toolkit validate --config <path>`: baca file → `parse` → `validate` → `resolve` dengan lookup env yang diinjeksi; tidak pernah membuat `Transport` (0 koneksi, dibuktikan dengan listener penghitung). Sukses (exit 0) mencetak jumlah request terencana, fixed waits (ms), dan NAMA env yang benar-benar dicari `resolve` (`ResolvedRequests.env_names`, terurut, tanpa duplikat); tidak pernah nilai. Semua error → stderr `error: <Display>` + exit 2; env hilang dilaporkan sekaligus. Pesan CLI berbahasa Inggris. Exit code: 0 valid, 2 untuk semua error termasuk salah pakai (clap). `--help`/`--version` 0.
- `api-toolkit check --config --out`: stub P0, tidak memuat config, tidak menulis/menyentuh file `--out`, pesan not-implemented-until-P4, selalu exit 2 (tidak pernah 0).
- `run(args, env_lookup, out, err) -> u8`; `main.rs` tipis (`ExitCode::from`). `anyhow` hanya di cli/main. Error tulis stdout (EPIPE) tidak panic (exit 2).
- File config dibatasi 1 MiB (dibaca maksimal 1 MiB + 1 byte); lebih besar → exit 2 menyebut path dan batas.
- Placeholder `${ENV:NAME}` utuh di posisi yang TIDAK di-resolve (base_url, path, kunci/nilai string `json_equals`, pointer `json_exists`, `counter_pointer`, semua matcher) ditolak (`ConfigError::Invalid`, hanya path field) agar tidak diam-diam menjadi literal. Literal yang kebetulan mirip placeholder di `json_equals` ikut ditolak; jalan keluarnya bentuk awalan/akhiran (`x${ENV:ok}`) yang tetap literal sesuai PLAN §9.
- Catatan: jalur error untuk key `json_equals` mencantumkan key itu (teks config milik pengguna, bukan nilai env).
- Belum (carry-over ke phase gate): placeholder di key `query` dan key objek body json masih diam-diam literal; `--config` berupa FIFO bisa menggantung (cukup cek `is_file`); tes batas tepat 1 MiB.

## 2026-10-10 — Penutupan P0: race.concurrency minimal 2 dan config harus file biasa

Detail: `.claude/work/P0-plan.md` (Log, entri phase gate). Terdampak: PLAN §6, §8. Phase gate reviewer PASS; item ini dari rekomendasi gate.

- `race.concurrency` harus ≥ 2 (batas atas tetap 32, PLAN §8). Satu request hanya bisa menghasilkan delta 1, sehingga "race" dengan concurrency 1 bisa PASS tanpa membuktikan apa pun. Ditolak di `validate` dengan path `race.concurrency`; boundary test 1 ditolak dan 2 diterima.
- `--config` harus file biasa: FIFO, device, atau direktori ditolak di awal (`metadata().is_file()`), karena membacanya bisa menggantung selamanya.
- Carry-over yang sengaja belum dikerjakan (tidak menimbulkan false PASS; tercatat di STATUS Known limitations): placeholder di key `query`/key objek body JSON masih literal; jalur error key `json_equals`; cancel sebelum ada check exit 2 (P4); `fail` + reason pass exit 1; `tls_failed` belum dihasilkan.

## 2026-10-10 — Pilihan teknis P1

Detail dan slice: `.claude/work/P1-plan.md`. Terdampak: PLAN §3, §4, §5, §8, §10, §11 (P1), §13.

- Struktur: semua logika brute force di `src/checks/brute_force.rs` sebagai fungsi murni `classify` dan `evaluate`, loop async `run_with` dengan closure pengirim (test double tanpa trait), `run` yang membungkus `http::send`, dan `console_line`. Alasan: verdict matrix teruji tanpa jaringan; aturan "tanpa trait sampai ada implementasi kedua".
- Klasifikasi: jumlah matcher yang cocok 0 → `unexpected`, 1 → kategori itu, >1 → `unexpected` + `overlapping_rules` (ERROR, PLAN §4). Transport error → `transport_error` dengan kode tersanitasi.
- Workload tetap: setelah baseline sukses, semua `attempts` probe selalu dikirim (tidak berhenti dini), kecuali cancel/deadline. Baseline tidak sukses → nol probe. Alasan: jumlah request sama dengan `RequestBudget` dan evidence lengkap; tidak menambah login gagal pada akun yang precondition-nya sudah gagal.
- Interval: `tokio::time::sleep(interval_ms)` antara selesainya probe k dan dispatch probe k+1; tanpa tunggu baseline→probe 1 dan setelah probe terakhir. Alasan: konsisten dengan fixed waits P0 `(attempts-1)*interval_ms`.
- Presedensi verdict: stop sebelum baseline → `not_run`; baseline protection → `inconclusive` `account_already_locked`; baseline rejection → `inconclusive` `baseline_failed`; baseline unexpected/transport → `error` `baseline_failed` (+ `transport_error`); probe success → `fail` `invalid_credentials_accepted` + reason tambahan (juga saat terhenti); stop → `not_run`; probe tak terklasifikasi → `error` `probe_unclassifiable` (+ `transport_error`); probe 1 protected → `inconclusive` `account_already_locked`; sisanya PASS `protection_observed` atau FAIL `protection_policy_not_met`. Tidak ada `ReasonCode` baru.
- Policy tail: PASS hanya bila attempt protected pertama ≤ `protect_by_attempt` dan `protected_tail_count` attempt terakhir protected. Celah proteksi di tengah tidak dinilai (PLAN §5 tidak menuntut kontinuitas); dicatat sebagai keterbatasan.
- Cancel/deadline: `checks::Stop { Cancelled, DeadlineExceeded }` lewat `tokio::sync::watch::Receiver<Option<Stop>>`; dicek sebelum baseline, `send` dan `sleep` di-`select!` dengan prioritas stop. Request in-flight di-drop dan tidak dicatat sebagai attempt (tetap dihitung di `attempts_dispatched`). Hasil: completion `incomplete`, reason `cancelled`/`deadline_exceeded`. Pemicu (Ctrl-C, deadline global) datang di P4. Tanpa dependency baru.
- Evidence report (aditif, `report_version` tetap 1): `CheckResult.evidence: Option<BruteForceEvidence>` dengan `skip_serializing_if = "Option::is_none"` (golden P0 tidak berubah). Isi: threshold terkonfigurasi, `attempts_planned`, `attempts_dispatched`, `first_protected_attempt`, `protected_tail_observed`, `baseline` dan `attempts` (nomor, outcome, status, error code, `offset_ms`, `elapsed_ms`). Hanya angka, enum, dan null; tanpa body/header/token/URL. P2 akan mengubah tipe field menjadi enum untagged tanpa mengubah JSON.
- Timing evidence diukur di loop check dengan `tokio::time::Instant` (bisa di-pause di unit test), bukan `TransportResponse.elapsed`.
- Fixture: rute broken terpisah di proses yang sama, `POST /broken/login/unprotected` (salah selalu 401) dan `POST /broken/login/accepts-invalid` (password apa pun untuk `demo` → 200 token). Prelocked = state healthy setelah 6 login salah. `GET /__admin/stats` → `{login_requests, load_requests}` untuk bukti jumlah attempt dan tanpa retry; reset mengosongkan. Semua tetap alat dev.
- Determinisme test: unit `run_with` memakai `#[tokio::test(start_paused = true)]` dan assert offset virtual tepat; integration terhadap fixture tidak memakai pause (timer reqwest akan maju palsu) dan hanya assert batas bawah interval.

## 2026-10-10 — Penutupan P1: semantik stop, workload tidak lengkap, console

Detail: `.claude/work/P1-plan.md` (Log, entri phase gate). Terdampak: PLAN §5, §10 (PLAN sudah disamakan). Phase gate reviewer PASS; item ini dari rekomendasi gate.

- Jumlah probe tidak sama dengan `attempts` setelah baseline sukses, tanpa success dan tanpa stop, tidak pernah PASS: hasilnya `not_run` dengan reason `not_started` dan completion `incomplete`. Dengan stop, reason-nya reason stop. Baseline yang hilang tanpa stop juga `not_run [not_started]`. Ditemukan reviewer S2 sebagai false PASS dan ditutup di ronde 2.
- Stop (cancel/deadline) yang datang setelah semua probe selesai dan tanpa success tidak mengubah verdict: tetap verdict normal dengan completion `complete`, karena oracle sudah terbukti (dikunci `stop_after_all_probes_finished_keeps_normal_verdict`). Success + stop selalu `incomplete` dan membawa reason stop, tetap FAIL.
- `protected_tail_observed` adalah jumlah protected berurutan yang sebenarnya (tanpa cap); syarat PASS `>= protected_tail_count`.
- Request in-flight saat stop di-drop: dihitung di `attempts_dispatched`, tidak pernah menjadi attempt di evidence. Console `attempts=` memakai jumlah attempt tercatat, bukan yang di-dispatch.
- Console: `brute_force PASS first_protected=6 tail=3/2 attempts=8/8`. "Matched rule" PLAN §10 diwakili `outcome` per attempt (1:1). PLAN §5 dan §10 disamakan dengan perilaku ini; `protected_tail_count` berarti "minimal N attempt terakhir protected" (bukan selalu dua).
- Celah proteksi di tengah run (M16: protected, lalu sempat tidak protected, lalu protected lagi di ekor) tetap PASS karena PLAN §5 hanya menuntut attempt protected pertama tepat waktu dan ekor protected. Keterbatasan, bukan bug.
- Test double check = closure pengirim (tanpa trait). Test unit memakai `tokio::time::pause`; integration test tidak (timer reqwest akan maju palsu) dan hanya menegaskan batas bawah interval.
- Varian prelocked tidak punya rute: dibuat dari state healthy (6 login salah, atau dua run tanpa reset).
- Saran non-blocking S5 yang dicatat, belum dikerjakan: `.all()` pada list kosong di `tests/brute_force_fixture.rs` (assert `attempts.len() == 8`), interval integration diukur dari start probe (sudah dikunci unit dari akhir probe), `tail_longer_than_protection_fails` hanya assert verdict, `run_cfg` hanya cek SENTINEL di Debug. Rencana: satu slice kecil pengetatan test di awal P2.

## 2026-10-10 — Majukan sebagian P4: `check --only`

Keputusan pengguna. Detail, AC (PA-AC1–PA-AC22), matriks, dan slice: `.claude/work/only-flag-plan.md`. Terdampak: PLAN §3, §8, §9, §10, §11 (P4).

- `api-toolkit check --config <file> --out <file> --only <check>` tersedia sebelum P2. Sampai P4 `--only` wajib; tanpa `--only`, `check` menolak (exit 2, config tidak dibaca, nol request, `--out` tidak disentuh) dengan pesan bahwa suite lengkap tersedia di P4. Hanya `brute_force` yang bisa dijalankan; `--only race` (P2) dan `--only load` (P3) ditolak "not available yet", nama tak dikenal ditolak; semuanya exit 2 dan nol request. `validate --only <check>` memvalidasi bagian itu saja; `validate` tanpa `--only` tetap mode suite lengkap.
- Config: `brute_force`, `race`, `load` menjadi `Option` di `Config`; kewajiban "ketiga bagian ada" pindah dari `parse` ke `validate(cfg, Selection::All)` (pesan menyebut nama bagian). Field wajib di dalam bagian tetap wajib saat parse; `deny_unknown_fields` tetap berlaku untuk seluruh file. Pada `--only`, bagian lain yang ada tetap harus berbentuk benar (field tak dikenal, tipe salah, key duplikat ditolak) tetapi tidak divalidasi semantik, tidak di-resolve (env-nya tidak wajib), dan tidak masuk `RequestBudget`/fixed waits. Alasan: typo tidak pernah diam-diam diabaikan; env race/load tidak boleh menghalangi run brute_force.
- Report (aditif, `report_version` tetap 1): field `scope` (`full`/`partial`) setelah `completion`, selalu ditulis; `ReasonCode::ExcludedByOnly` (`excluded_by_only`), total 30 varian. Check yang tidak dipilih = `not_run`, `["excluded_by_only"]`, `incomplete`, tanpa evidence. Urutan entri mengikuti urutan default PLAN §3 (race, load, brute_force). `completion` suite mengikuti check terpilih. Timestamp dipotong ke detik. Golden test diperbarui.
- Exit code run parsial tidak pernah 0: FAIL selesai → 1; PASS dan hasil lain → 2; ada `cancelled` → 130. Presedensi PLAN §10 tetap (cancel → incomplete/error/inconclusive → fail), entri `excluded_by_only` diabaikan; jadi FAIL yang terhenti deadline → 2. Mode full tidak berubah, dan `excluded_by_only` di mode full → 2. Alasan: PLAN §9 melarang skip flag yang membuat command tampil sukses; PASS parsial dibaca dari console dan report, dijelaskan di `--help`.
- Cancel dan deadline: CLI membuat runtime tokio, mendaftarkan SIGINT sebelum request pertama (tokio feature `signal` dipindah ke dependency normal, tanpa crate baru), dan memasang timer `suite_deadline_ms`; keduanya mengirim `Stop` lewat `watch` yang sudah ada dari P1. Cancel sebelum dispatch tetap menjalankan `brute_force::run` yang langsung mengembalikan `not_run [cancelled]`, sehingga exit 130 (menutup catatan P0 S7). Cancel setelah check selesai tidak mengubah verdict.
- `--out`: dicek sebelum request pertama (direktori induk harus ada, `--out` bukan direktori, bukan file config yang sama); bila gagal exit 2 tanpa request. File yang sudah ada ditimpa atomic. Gagal menulis report: baris console tetap tercetak, exit 2 (130 bila cancelled), isi lama utuh.
- Console: satu baris `brute_force::console_line` di stdout, satu catatan partial di stderr. Tanpa nilai env, body, atau header.
- Contoh config parsial: `examples/brute-force-only.json` (bagian brute_force PLAN §9, base_url fixture), dikunci test (9 request, 700 ms fixed waits).
- Urutan checkpoint: bagian P4 ini dikerjakan sebelum P2, jadi menyimpang dari urutan PLAN §11 dan CLAUDE.md ("checkpoint utama setelah P4"). Yang TIDAK berubah: checkpoint review pengguna utama tetap setelah P4 fixture proof (ketiga check), validasi API nyata tetap setelah P4 dengan config dari pengguna, scope v0.1 (PLAN §2) tetap, P2/P3 tetap berikutnya. Mencoba `--only` terhadap fixture bukan checkpoint dan bukan validasi target nyata.
- P4 memperluas command ini menjadi suite lengkap sebagai default (bukan membuat ulang parser, report, atau jalur cancel/deadline). Usulan: `--only` tetap ada sebagai mode parsial opsional dengan kontrak yang sama; diputuskan final di P4.

## 2026-10-10 — Penutupan `check --only`

Detail: `.claude/work/only-flag-plan.md` (Log, entri phase gate). Terdampak: PLAN §10 (sudah disamakan). Phase gate reviewer PASS (PA-AC1–PA-AC22, 229 test).

- Pesan penolakan tanpa `--only` berbahasa Inggris (`full suite available in P4`), mengikuti keputusan pesan CLI P0 S9; frasa Indonesia "suite lengkap tersedia di P4" ada di STATUS dan README. Mengubah bahasa pesan CLI = keputusan baru.
- Keterbatasan yang diterima: SIGINT sebelum handler terdaftar mematikan proses tanpa report (belum ada request); baris console tetap mencetak verdict bila report gagal ditulis (penanda hanya stderr dan exit 2); precheck `--out` tidak mendeteksi hardlink ke config (aman karena report ditulis lewat rename); nasib `--only` setelah P4 diputuskan di P4.
- Cakupan manual, bukan end-to-end otomatis: cancel sebelum dispatch (unit `run_check_cancel_before_dispatch_is_130_with_report`), cancel setelah check selesai, FAIL lalu deadline, cancel + direktori read-only. Alasan: bergantung timing; exit dihitung dari report yang sama dengan baris console.
- Test unit `run_check_cancel_while_request_hangs_is_130` sempat flaky (cancel dengan tebakan 150 ms datang sebelum koneksi diterima). Diperbaiki dengan menunggu koneksi pertama teramati. Pelajaran: test yang butuh "request sedang berjalan" menunggu kondisi teramati, bukan sleep.
- Saran test tidak memblokir, belum dikerjakan: perketat kata kunci `brute_force` di `tests/check_only_cli.rs` ke pesan lengkap; kill proses anak sebelum assert pada kasus cancel yang gagal; port bebas yang di-bind lalu di-drop bisa dipakai ulang test paralel (gagal merah, bukan false PASS).

## 2026-10-10 — Pilihan teknis P2

Detail dan slice: `.claude/work/P2-plan.md`. Terdampak: PLAN §3, §4, §6, §8, §9, §10, §11 (P2), §13.

- Struktur: semua logika race di `src/checks/race.rs` sebagai fungsi murni `read_verifier`, `action_allowed`, `skew_ms`, `evaluate`, loop async `run_with` dengan closure pengirim `Fn(Kind) -> Fut` (`Kind { VerifyBefore, Action, VerifyAfter }`), `run` yang membungkus `http::send`, dan `console_line`. Helper stop (`current_stop`, `wait_stop`) dipindah ke `src/checks/mod.rs`. Alasan: pola P1, matrix teruji tanpa jaringan, tanpa trait. `Fn` (bukan `FnMut`) karena N future meminjam closure bersamaan.
- Verifier: `Err` → `verifier_failed` + `transport_error`; tidak cocok `verify_match` → `verifier_failed`; pointer hilang, body bukan JSON, atau nilai bukan integer ≥ 0 (`Value::as_u64`) → `counter_invalid`; semua ERROR. Before ≠ 0 → INCONCLUSIVE `initial_counter_not_zero`. Kegagalan di before = nol action.
- Action: diizinkan hanya bila respons ada dan status ada di `expected_action_statuses`. Status lain dan transport error pada action = ambigu → INCONCLUSIVE `action_outcome_ambiguous` (+ `transport_error`). Alasan: action yang timeout mungkin sudah atau belum dieksekusi; PLAN §6 menaruh outcome ambigu di INCONCLUSIVE. Body action tidak dibaca.
- 202 ditolak di `validate` untuk `race.expected_action_statuses`. Alasan: PLAN §6, respons 202/pending tidak boleh menjadi delta1 PASS. Status pending yang ditandai lewat body tetap tidak terdeteksi (keterbatasan, aksi sinkron).
- Presedensi verdict: before hilang → `not_run`; before tidak valid → error/inconclusive; after hilang (stop) → `not_run`; after tidak valid → error; delta > 1 → FAIL `duplicate_side_effect` + reason tambahan (mengalahkan skew/ambigu); jumlah action ≠ N → INCONCLUSIVE `dispatch_incomplete`; ambigu → INCONCLUSIVE; delta 0 → FAIL `action_not_executed`; skew > budget → INCONCLUSIVE `dispatch_skew_exceeded`; delta 1 → PASS `single_side_effect`. Reason tambahan selalu berurutan `action_outcome_ambiguous`, `transport_error`, `dispatch_incomplete`, `dispatch_skew_exceeded`. Tidak ada `ReasonCode` baru.
- Barrier: N future action dibuat dulu (lazy, belum ada byte terkirim), lalu dijalankan bersama dengan `futures_util::future::join_all` di satu task; tiap future mencatat waktu dispatch saat pertama di-poll. Tanpa `spawn`, tanpa `tokio::sync::Barrier`. Alasan: paling sedikit kode; `spawn` butuh `'static` padahal `Secret` sengaja tanpa `Clone`. Dependency langsung baru `futures-util 0.3` (`default-features = false`, `alloc`), sudah ada di tree lewat reqwest/hyper.
- Skew = max − min waktu dispatch semua action yang terkirim, dibulatkan ke atas ke ms; `> max_dispatch_skew_ms` berlebih, sama dengan budget masih lolos. Verdict skew diuji dengan array offset yang diketahui; integrasi memakai budget 1000 ms.
- Settle: `sleep(settle_ms)` setelah semua action selesai, lalu tepat satu verifier after. Stop (`watch`, pola P1) di-`select!` di before, tiap action, settle, dan after; action yang di-drop dihitung di `actions_dispatched`, tidak dicatat di `actions`. Stop setelah after selesai tidak mengubah verdict.
- Evidence report (aditif, `report_version` tetap 1): `CheckResult.evidence: Option<Evidence>`, `#[serde(untagged)] enum Evidence { BruteForce, Race }`; JSON brute_force identik. `RaceEvidence`: threshold terkonfigurasi (`concurrency`, `expected_action_statuses`, `max_dispatch_skew_ms`, `settle_ms`), `actions_dispatched`, `dispatch_skew_ms`, `counter_before`, `counter_after`, `delta`, `verify_before`/`verify_after` (status, error, offset, elapsed), `actions` (nomor, status, error, `allowed`, offset, elapsed). Hanya angka, enum, bool, null.
- Console: `race PASS delta=1 before=0 after=1 actions=5/5 skew=0/100ms reasons=single_side_effect` (`none` untuk null).
- Fixture: `POST /broken/actions/{id}/apply-racy` dengan titik sinkronisasi eksplisit: request yang membaca counter 0 menunggu (gerbang `tokio::sync::watch` per id, timeout 2 s) sampai minimal 2 request membaca 0, baru menaikkan counter. Hasilnya delta ≥ 2 deterministik bila client benar-benar konkuren; bila tidak, gerbang timeout dan test merah, bukan PASS. `POST /idempotent/actions/{id}/apply` selalu 200 dengan satu increment. Verifier tetap `GET /actions/{id}` (map bersama). `/__admin/stats` menambah `action_requests` dan `verify_requests`; reset mengosongkan semua state baru. Kasus missing/noninteger/delta 0 dibuat lewat config, tanpa rute baru.
- CLI: `--only race` aktif di `check` dan `validate`; `--only load` tetap ditolak sampai P3. `Prepared` memegang `enum Selected { BruteForce, Race }`; jalur cancel/deadline/report/exit `--only` tidak berubah. Contoh config `examples/race-only.json` (7 request, 250 ms fixed waits).

## 2026-10-10 — Penutupan P2

Detail: `.claude/work/P2-plan.md` (Log, entri phase gate). Terdampak: PLAN §3 (daftar check yang ditolak `--only` kini hanya `load`). Phase gate reviewer PASS (P2-AC1–AC25, 303 test).

- Pengaman tambahan di `evaluate`: action tercatat tetapi skew kosong → INCONCLUSIVE `dispatch_incomplete`, tidak pernah PASS. Alasan: bila `run_with` salah menyambung nilai skew, hasilnya tidak boleh menjadi false PASS (temuan review S3, dikunci test S4).
- Pengetatan test P1 dan `--only` dikerjakan di P2 S1. Dua test deadline unit (race dan brute_force) dan satu test deadline di `tests/check_only_cli.rs` melonggarkan assert jumlah koneksi menjadi `<= 1` / dihapus karena flaky di bawah beban berat; jaminan yang dipertahankan: exit 2, `not_run [deadline_exceeded]`, `incomplete`, tidak ada action terkirim. Pelajaran sama dengan `check --only`: test yang butuh "request sedang berjalan" menunggu kondisi teramati, bukan timing.
- `check_help_has_required_phrases` kini mengunci `Available: brute_force, race`.
- Keterbatasan yang diterima: dispatch satu task (skew mikrodetik), status pending lewat body tidak terdeteksi, action yang di-drop saat stop mungkin tetap dieksekusi server (resource butuh id baru/reset).
- Tafsiran yang bisa dibalik: transport error pada request action → INCONCLUSIVE `action_outcome_ambiguous` + `transport_error` (bukan ERROR). Membaliknya cukup mengubah satu baris di `evaluate` dan baris R15 di matrix handoff.
- Saran test yang belum dikerjakan, tidak memblokir: satu test `evaluate` untuk kombinasi delta>1 + transport error pada action (AC16), tambahan assert di test healthy (angka skew di console sama dengan `evidence.dispatch_skew_ms`), dan `planned requests: 7` di test `validate` binary nyata.

## 2026-10-10 — Pilihan teknis P3

Detail dan slice: `.claude/work/P3-plan.md`. Terdampak: PLAN §3, §4, §7, §8, §9, §10, §11 (P3), §13.

- Struktur: semua logika load di `src/checks/load.rs` sebagai fungsi murni `classify`, `nearest_rank_ms`, `ceil_ms`, `evaluate`, loop async `run_with` dengan closure pengirim `Fn(Kind) -> Fut` (`Kind { Preflight, Load(u32) }`, nomor request 1..=N), `run` yang membungkus `http::send`, dan `console_line`. Alasan: pola P1/P2, matrix teruji tanpa jaringan, tanpa trait.
- Bounded parallelism: worker pool eksplisit dalam satu task, `min(concurrency, requests)` future worker dijalankan `futures_util::future::join_all`; nomor berikutnya diambil dari `AtomicU32::fetch_add` bersama; tiap worker mengirim request berikutnya hanya setelah sebelumnya selesai (closed-loop). Tanpa `spawn`, `Semaphore`, atau dependency baru. Alasan: harfiah "closed-loop worker pool" PLAN §7; `spawn` butuh `'static` padahal `Secret` tanpa `Clone`.
- Concurrency aktual: `in_flight`/`max_in_flight` atomik di sekitar `send`; `evaluate` memperlakukan `max_in_flight > min(concurrency, requests)`, `attempted > dispatched`, atau `dispatched > requests` sebagai ERROR `runner_error`.
- Outcome: success = respons cocok `response_match`; failed = status lain, mismatch JSON (termasuk body bukan JSON), transport error, timeout. `error_rate = failed / attempted` (`f64`, tanpa epsilon; sama dengan threshold lolos). p95/p50/max nearest-rank (`rank = ceil(pct·n/100)`) atas semua sampel tercatat termasuk timeout/error, durasi dibulatkan ke atas ke ms berbasis nanodetik; sama dengan `max_p95_ms` lolos.
- Preflight: tepat satu request sebelum load, tidak masuk sampel. Tidak cocok → INCONCLUSIVE `target_not_ready`; transport error (termasuk timeout) → ERROR `preflight_failed` + `transport_error`; keduanya nol request load.
- Presedensi verdict: invarian runner → `error [runner_error]`; preflight hilang → `not_run`; preflight transport → error; preflight mismatch → inconclusive; sampel < N dengan stop → `not_run [stop, workload_incomplete]`, tanpa stop → `inconclusive [workload_incomplete]` (completion `incomplete`); workload lengkap → FAIL `[error_rate_exceeded, latency_exceeded]` (yang berlaku, urutan tetap) atau PASS `[thresholds_met]`. Stop setelah N tercatat tidak mengubah verdict. Tidak ada `ReasonCode` baru.
- Stop saat load = NOT_RUN `[stop, workload_incomplete]` (PLAN §3/§4, preseden P1/P2), bukan INCONCLUSIVE seperti tulisan lama PLAN §7. Keputusan pengguna 2026-10-10 (pertanyaan Q1); PLAN §7 sudah disamakan. Exit code sama di kedua tafsiran.
- Pooling transport tetap mati (`pool_max_idle_per_host(0)`): setiap request load membuka koneksi baru, durasi mencakup connect (PLAN §7). Alasan: tidak ada retry tersembunyi pada koneksi basi.
- Evidence report (aditif, `report_version` tetap 1): `Evidence::Load(LoadEvidence)` berisi threshold terkonfigurasi, `preflight` (`VerifyEvidence`), `dispatched`, `attempted`, `succeeded`, `failed`, `timed_out`, `error_rate`, `p50_ms`, `p95_ms`, `max_ms`, `max_in_flight`, `load_elapsed_ms`, `throughput_rps`, `status_counts` (`BTreeMap<u16, u32>`), `error_counts` (`BTreeMap<TransportErrorCode, u32>`). Agregat saja, tanpa daftar per-request. `Eq` dilepas dari `Evidence`, `CheckResult`, `Report` karena `f64`; `TransportErrorCode` mendapat `Ord`. JSON lama identik.
- Console: `load PASS error_rate=0/0.01 p95=3/500ms attempted=100/100 samples=100 max_in_flight=10/10 timeouts=0 status=200:100 errors=none reasons=thresholds_met` (`none` untuk null/kosong).
- Fixture: `GET /broken/critical/slow` (delay tetap 300 ms), `GET /broken/critical/flaky` (setiap request load ke-10 → 500, berbasis counter), `GET /broken/critical/hang` (setiap ke-10 tidur 3 s). `load_requests` kini menghitung semua request ke keempat rute load termasuk 401; reset mengosongkannya. Alasan: error berbasis counter deterministik; delay hanya memberi batas bawah, sehingga mesin lambat tidak pernah membalik FAIL menjadi PASS. Test integrasi PASS memakai `max_p95_ms` longgar.
- CLI: `--only load` aktif di `check` dan `validate`; `Selected::Load`; `--help` dan pesan memuat `brute_force, race, load`. Contoh config `examples/load-only.json` (101 request, 0 ms fixed waits).

## 2026-10-10 — Penutupan P3

Detail: `.claude/work/P3-plan.md` (Log, entri phase gate). Terdampak: PLAN §3 (daftar check yang ditolak `--only` kini hanya nama tak dikenal), §7. Phase gate reviewer PASS (P3-AC1–AC26, L1–L26, 393 test).

- Entri 2026-10-09/10 yang menyebut `--only load` ditolak sampai P3 (mis. "Majukan sebagian P4") sudah tidak berlaku: `--only brute_force|race|load` semuanya aktif.
- Keterbatasan yang diterima: `throughput_rps` hanya di report (informatif, `null` bila elapsed 0); preflight memakai `load.request` yang sama dengan load (target nyata menerima 1+N request, endpoint non-idempoten ikut terkena); Ctrl-C dan deadline sama-sama NOT_RUN (bedanya reason dan exit 130/2); request in-flight yang di-drop saat stop mungkin sudah sampai server; dispatch satu task (`join_all`, maks 32 worker); pooling mati sehingga p95 API nyata mencakup handshake; fixture flaky/hang berbasis counter bersama `load_requests` (reset wajib antar run, run paralel saling mengganggu); hang pada counter kelipatan 10 dan butuh `request_timeout_ms` < 3 s.
- `load_requests` kini menghitung semua request rute load termasuk 401 (semantik berubah dari P1/P2).
- Test e2e deadline load bergantung pada preflight dan 8 request cepat selesai dalam 1,5 s; di mesin sangat sibuk hasilnya merah, bukan PASS palsu.
- Kerja tambahan di gate: test `check_without_only_message_names_all_three_checks`, penguatan test sentinel (needle unik per kanal, request line dan nilai header di wire dibandingkan persis), dan test e2e deadline dengan reasons persis.
- Belum dikerjakan, tidak memblokir: PASS e2e CLI memakai `max_p95_ms` 500 dari contoh (p95 nyata ± 3 ms), bukan 5000.


## 2026-10-10 — Pilihan teknis P4

Detail dan slice: `.claude/work/P4-plan.md`. Terdampak: PLAN §3 (kalimat "Sampai P4, `check` wajib memakai `--only`" diganti), §8, §9, §10, §11 (P4).

- `check` tanpa `--only` menjalankan suite lengkap berurutan race → load → brute_force (brute_force terakhir karena bisa mengunci akun). `--only` tetap sebagai run parsial dengan kontrak lama (`scope: partial`, tidak pernah exit 0).
- FAIL, ERROR, atau INCONCLUSIVE satu check tidak menghentikan check berikutnya; hanya cancel/deadline yang menghentikan. Satu `Transport`, satu sinyal stop (`watch`), satu timer `suite_deadline_ms` untuk seluruh suite.
- Check yang belum jalan saat cancel/deadline tetap dipanggil dan langsung `not_run` tanpa request, sehingga report selalu memuat tiga entri. Akibatnya cancel sebelum check pertama kini exit 130 (sebelumnya 2).
- Exit code memakai `report::exit_code` apa adanya (PLAN §10): 0 hanya bila tiga check `pass` + `complete` + reason pass; matriks E1–E13 dikunci test.
- Timestamp report dipotong ke detik untuk kedua scope.
- Console dicetak setelah seluruh suite selesai (catatan `ponytail:` di `src/cli.rs`). Bila report gagal dibangun/ditulis, stdout menambah baris `report NOT WRITTEN: <path>`; exit 2 (130 bila cancel); isi `--out` lama utuh.
- Bukti fixture: `examples/core-checks-broken.json` (login unprotected, race apply-racy, load flaky: FAIL deterministik) dan `cargo test --test fixture_proof` (12 test, biner nyata, fixture di port acak, tanpa env var).
- Tidak ada dependency baru, `ReasonCode` baru, atau perubahan `report_version`.

## 2026-10-10 — Penutupan P4 (checkpoint pengguna)

Detail: `.claude/work/P4-plan.md` (Log, entri phase gate). Phase gate reviewer PASS (416 test). Terdampak: PLAN §3.

- Entri "Majukan sebagian P4" dan catatan lama yang menyebut `check` tanpa `--only` ditolak tidak berlaku lagi.
- **P4-AC7 (validasi target nyata) belum dilakukan**: fixture proof selesai, menunggu config target dari pengguna. Checkpoint pengguna terbuka; jangan ditandai selesai tanpa report dari run nyata.
- Keterbatasan baru: Ctrl-C sebelum handler terdaftar mematikan proses tanpa report; Ctrl-C setelah check terakhir selesai mengikuti verdict (bukan 130); load yang dibatalkan sebelum preflight dijawab hanya `not_run [cancelled]` tanpa `workload_incomplete`; brute_force yang sudah FAIL lalu dibatalkan tetap FAIL + `cancelled`/`incomplete` (exit 130); jalur `build_report` gagal + cancel tanpa test; test SIGINT `seen_at_sigint == 1` bergantung pada urutan race → load → brute_force; cek atomic (inode) hanya di unix.
- `examples/api-checks.json` (untracked, brute_force saja ke `127.0.0.1:3000`) bukan bagian P4; keputusan di-commit atau di-`.gitignore` menunggu pengguna.

## 2026-10-14 — Q2: batas ERROR/INCONCLUSIVE race

Detail: `.claude/work/Q2-Q3-plan.md`, `docs/findings.md` F-001. Terdampak: PLAN §4, §6, §10 (README).

- Verifier before dipisah menjadi empat kelas (tabel V1–V7 di PLAN §6). Mismatch `verify_match` → INCONCLUSIVE `baseline_unreadable`; pointer absent → INCONCLUSIVE `counter_not_available`. Transport error, body bukan JSON, dan nilai bukan integer >= 0 (termasuk `null`) tetap ERROR. V2 mendahului V3.
- Verifier after TIDAK berubah (ERROR). Alasan: action sudah terkirim, "oracle belum tersedia" tidak berlaku.
- Kontrak report: `ReasonCode` +2 (`baseline_unreadable`, `counter_not_available`), total 32. Aditif; `report_version` tetap 1. Tidak ada file schema atau dokumen kontrak terpisah; yang ikut berubah: `ReasonCode::ALL` dan dua `as_str` exhaustive di `src/report.rs`, daftar string di `tests/report_contract.rs`, tabel reason di README. Input yang sama kini ERROR → INCONCLUSIVE (exit code tetap 2). Bila ada konsumen eksternal yang mem-parse `counter_invalid`/`verifier_failed`, naikkan `report_version` ke 2 (menunggu konfirmasi pengguna).
- Tidak ada jalur baru menuju PASS (kedua kelas baru berhenti sebelum action; dikunci test exhaustif).
- Fixture: 4 rute verifier rusak `GET /broken/actions/{id}/verify-{denied,not-json,no-counter,bad-counter}`.
