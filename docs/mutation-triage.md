# Triase mutasi

Perintah: `scripts/verify.sh mutants` (cargo-mutants 27.1.0; cakupan bawaan: `src/checks/**`, `src/matchers*`, `src/report*`).

Aturan triase: setiap mutan yang lolos (missed) atau timeout diperbaiki dengan test, atau dicatat di bawah sebagai equivalent dengan satu baris alasan. Mutan yang bisa mengubah non-PASS menjadi PASS tidak pernah diterima sebagai equivalent.

## Baseline (commit f48231a + berkas gate)

244 mutan: 215 caught, 6 missed, 20 unviable, 3 timeout.

Semua 9 mutan (6 missed + 3 timeout) diperbaiki dengan test lewat slice M1 dan M2 (lihat `docs/loop-history/QUALITY.md`). Tidak ada yang dinyatakan equivalent.

| Mutan | Keputusan |
| --- | --- |
| `src/checks/load.rs:68:54` `-`→`+`, `-`→`/` di `nearest_rank_ms` | Diperbaiki (M1): test untuk `pct` di luar 1..=100 |
| `src/checks/load.rs:263:19` `>`→`==`, `>`→`<` di `worker` (timeout) | Diperbaiki (M1): test batas jumlah request, gagal cepat |
| `src/checks/mod.rs:30:5` `current_stop` → `None` (timeout) | Diperbaiki (M1): test sinyal stop, gagal cepat |
| `src/report.rs:551:39` `\|\|`→`&&` di `partial_exit_code` | Diperbaiki (M2) |
| `src/report.rs:561:17` `\|\|`→`&&` di `partial_exit_code` | Diperbaiki (M2) |
| `src/report.rs:561:51` hapus `!` di `partial_exit_code` | Diperbaiki (M2) |
| `src/report.rs:615:23` guard `AlreadyExists` → `true` | Diperbaiki (M2) |

## Sesudah M1 dan M2

244 mutan: 224 caught, 0 missed, 20 unviable, 0 timeout (exit 0). Daftar equivalent: kosong.

Catatan: 20 mutan unviable (tidak terkompilasi) tidak diuji dan tidak dibuktikan apa pun tentang test.
