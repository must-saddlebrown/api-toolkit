# Findings dari run target nyata

Temuan dari run `check` terhadap API di luar fixture. Setiap temuan: bukti dari report, tafsiran, keputusan, status. Nilai secret tidak pernah dicatat di sini. Keputusan teknis: `docs/DECISIONS.md` (2026-10-14). Rencana kerja: `.claude/work/Q2-Q3-plan.md`.

Sumber bukti (belum di-commit): config `examples/ninja-checks.json`, `examples/mustex-load.json`, `examples/api-checks.json`, `examples/fail-checks.json`; report di `examples/results/*.json`.

## F-001 Race: ERROR padahal oracle hanya belum tersedia

Status: diputuskan, dikerjakan di fase Q2.

| Report | verify_before | counter_before | actions_dispatched | Verdict lama |
| --- | --- | --- | --- | --- |
| ninja-checks | status 200, tanpa error | null | 0 | ERROR counter_invalid |
| mustex-load | status 401, tanpa error | null | 0 | ERROR verifier_failed |

- ninja: verifier `GET /api/articles` cocok `verify_match`, tetapi pointer `/execution_count` tidak menunjuk apa pun (endpoint daftar artikel, bukan counter write-once). Kode lama menyatukan body non-JSON, pointer absent, dan nilai non-integer menjadi `counter_invalid`.
- mustex: verifier 401 (token tidak diterima) -> respons tidak cocok `verify_match` -> ERROR.
- Pada keduanya nol action terkirim; tidak ada side effect dan tidak ada false PASS. Masalahnya salah klasifikasi: prasyarat oracle belum terpenuhi = INCONCLUSIVE menurut PLAN §4.
- Keputusan: tabel batas di PLAN §6; reason baru `baseline_unreadable` dan `counter_not_available`; hanya verifier sebelum action yang direklasifikasi.

## F-002 Brute force: limiter berbasis waktu tidak terjangkau mode sequential

Status: diputuskan, dikerjakan di fase Q3 (setelah Q2).

| Report | attempts | interval_ms | elapsed probe | jarak mulai antar probe | Respons probe | Verdict |
| --- | --- | --- | --- | --- | --- | --- |
| ninja-checks | 8 | 100 | 169-198 ms | 272-300 ms (±285) | 8x 401 | FAIL protection_policy_not_met |
| mustex-load | 15 | 50 | 240-274 ms | 292-325 ms (±306) | 15x 400 | FAIL protection_policy_not_met |
| fail-checks | 8 | 200 | 28-43 ms | ±233 ms | 8x 401 | FAIL protection_policy_not_met |
| api-checks | 8 | 200 | 3-37 ms | ±207-239 ms | 401 x5 lalu 429 mulai attempt 6 | PASS |

- `interval_ms` hanya jeda antara selesainya probe k dan mulai probe k+1. Latency tinggi membuat jarak mulai 2,5-6x lebih besar dari `interval_ms`; laju efektif ±3,3-3,5 login gagal per detik pada dua target nyata.
- Report TIDAK membuktikan adanya limiter berbasis waktu; hasilnya konsisten dengan "limiter waktu tidak terpicu" dan dengan "memang tidak ada proteksi". FAIL sequential berarti "kebijakan terkonfigurasi tidak terlihat pada pacing ini", bukan "API tanpa proteksi".
- Risiko yang dihadapi adalah false FAIL, bukan false PASS. Mode burst baru harus menjaga semua jalur tak terbukti (window tidak tercapai, semua probe protected, probe tak terklasifikasi) tetap INCONCLUSIVE/ERROR/NOT_RUN.
- Keputusan: `brute_force.mode = "sequential" | "burst"`; semantik di PLAN §5.

## F-003 Config contoh berisi secret literal (catatan operasional)

Status: terbuka, menunggu pengguna.

`examples/ninja-checks.json` dan `examples/mustex-load.json` (dan `examples/api-checks.json` bila diisi) memuat nilai header `Authorization` serta, pada mustex, kredensial login sebagai literal, bukan `${ENV:NAME}`. Report hasil tidak berisi secret (hanya angka, enum, null). Jangan di-commit; ganti ke `${ENV:...}` atau tambahkan ke `.gitignore`; pertimbangkan merotasi token/password. Tidak ada perubahan kode yang diperlukan.
