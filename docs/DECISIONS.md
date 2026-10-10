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
