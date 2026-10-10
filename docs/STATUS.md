# Status pengembangan

Rencana aktif: `docs/PLAN.md` (Small Core Proof, 9 Oktober 2026). Keputusan setelah rencana: `docs/DECISIONS.md`. Rencana lama ada di `docs/archive/` dan tidak dijalankan.

| Fase | Isi | Status | Catatan |
| --- | --- | --- | --- |
| P0 | Kontrak CLI, runner minimum, fixture | selesai (10 Oktober 2026) | Phase gate PASS; 20 dari 20 AC terpenuhi; 111 test hijau |
| P1 | Login brute-force protection check | belum dimulai | Siap: `/phase P1` |
| P2 | Write-once race check | belum dimulai | |
| P3 | Parallel load check | belum dimulai | |
| P4 | Single-command proof + satu API nyata | belum dimulai | Checkpoint review pengguna |

## Fase aktif

Tidak ada. P0 selesai; P1 siap dimulai.

## Ringkasan AC P0

Semua P0-AC1 sampai P0-AC20 terpenuhi, dengan bukti berupa nama test atau output command di handoff `.claude/work/P0-plan.md` (Log, entri phase gate). Ringkas:

- Config v1 (`examples/core-checks.json`) parse; field tak dikenal, `config_version` selain 1, field wajib hilang, env hilang ditolak dengan nol request (AC1–AC9).
- Transport bersama: timeout, batas body streaming, tanpa redirect, tanpa retry, tanpa proxy dari environment, error disanitasi (AC10–AC11).
- Matcher `status_in`, `json_equals`, `json_exists` (AC12).
- Report v1: enum snake_case stabil dengan golden test, exit code sesuai PLAN §10, penulisan atomic (AC13–AC15).
- CLI `validate` dan stub `check` (selalu exit 2, tidak pernah 0) (AC5, AC16).
- Fixture healthy lokal dengan start dan reset terdokumentasi (AC17–AC19); gate kualitas dan Commands di CLAUDE.md (AC20).

## Cara mencoba

Semua command dijalankan dari root repo.

```sh
# Gate kualitas
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets            # 111 test, tanpa env var dan tanpa fixture berjalan

# Fixture healthy (terminal 1); reset dan detail ada di docs/fixture.md
cargo run --example fixture         # http://127.0.0.1:8080, Ctrl-C untuk berhenti
curl -X POST http://127.0.0.1:8080/__admin/reset   # reset state (alat dev)

# Validasi config contoh (terminal 2); nilai env ada di docs/fixture.md
export LOGIN_USER=demo LOGIN_VALID_PASSWORD=demo-pass LOGIN_INVALID_PASSWORD=wrong-pass
export TEST_ACTION_ID=act-1 TEST_AUTHORIZATION='Bearer fixture-token'
cargo run -q -- validate --config examples/core-checks.json   # exit 0, 117 request, 950 ms fixed waits

# Stub check: selalu exit 2 sampai P4, tidak mengirim request, tidak menulis file
cargo run -q -- check --config examples/core-checks.json --out /tmp/result.json; echo $?
```

## Known limitations

- `check` masih stub (exit 2); check sebenarnya dan penulisan report ke CLI datang di P1–P4. Penulis report atomic sudah ada tetapi belum dipanggil CLI.
- Hanya fixture healthy. Varian broken/idempotent/slow/prelocked ditambahkan di P1–P3.
- Placeholder `${ENV:X}` di key `query` atau key objek body JSON masih dikirim sebagai literal (diam-diam); test `placeholder_in_query_or_json_key_is_accepted_known_limitation` mencatatnya dan harus dibalik saat diperbaiki.
- Jalur error key `json_equals` mencantumkan key tersebut (teks config milik pengguna, bukan nilai env).
- Cancel sebelum ada check akan exit 2, bukan 130; P4 harus menyisipkan check dengan reason `cancelled`. Check `fail` yang membawa reason pass menghasilkan exit 1 (bisa diperketat).
- `tls_failed` belum pernah dihasilkan (kegagalan TLS tercatat sebagai `connect_failed`); connect timeout dilaporkan sebagai `timeout`; drop/RST/respons sampah sebagai `other`.
- Timestamp report: presisi subsecond bisa bervariasi; potong ke detik di P4 bila perlu bentuk stabil.
- Tidak ada fsync direktori setelah rename report; mode file mengikuti umask (report tidak berisi secret).
- Aturan overlap `status_in` konservatif: irisan apa pun antar success/rejection/protection ditolak saat validasi.

## Validasi API nyata

Belum dilakukan. Menunggu konfigurasi target dari pengguna setelah P4.

## Riwayat review pengguna

Belum ada.
