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
