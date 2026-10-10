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
- Aturan validasi turunan (melengkapi PLAN §5/§8/§9): `protect_by_attempt` dalam 2..=attempts; `protected_tail_count` dalam 1..=attempts; overlap `status_in` antar success/rejection/protection ditolak saat validasi (konservatif); `initial_counter`=0 dan `expected_delta`=1 wajib (kontrak write-once §6); base_url harus origin http(s) tanpa userinfo/query/fragment; fixed waits = `(attempts-1)*interval_ms + settle_ms` ditolak bila melebihi `suite_deadline_ms`.
- Secret: tipe `Secret` dengan `Debug` teredaksi, tanpa `Display`/`Serialize`; pesan error hanya memuat nama env.
- Kontrak enum report v1 dikunci di P0 (`Verdict`, `CheckName`, `Completion`, `ReasonCode`, `Outcome`, `TransportErrorCode`), semua snake_case; daftar varian ada di handoff P0. Menambah varian bersifat aditif; mengubah/menghapus = naik `report_version`. `config_hash` = SHA-256 byte file config mentah.
