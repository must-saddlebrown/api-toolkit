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
