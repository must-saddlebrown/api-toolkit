# Kualitas: gate lokal, jejak AC, baseline mutasi

Ringkasan cara repo ini menjaga agar "selesai" berarti terbukti, bukan hanya terkompilasi. Semua gate di bawah berjalan lokal.

## Loop

| Peran | Tugas |
| --- | --- |
| Sesi utama (orkestrator) | Memilih tier, mendelegasikan, mencatat verdict di `docs/loop-history/<fase>.md`, commit |
| `analis` | Akar masalah, keputusan, AC atomik, slice. Tidak mengedit source |
| `implementer` | Menerapkan satu slice beserta unit test inline |
| `reviewer` | Memeriksa slice atau phase gate terhadap AC dengan bukti. Verdict PASS / FIX / REANALYZE |
| `tester` | Test integrasi di `tests/` dan menjalankan suite |

- FIX → `implementer` lalu `reviewer` lagi. REANALYZE → `analis`. Maksimum 2 putaran review per tugas.
- Setelah 2 putaran tanpa PASS, slice ditandai `blocked`, keadaan ditulis di handoff, dan pengguna diberi laporan. Tidak ada putaran ke-3.
- Setiap verdict reviewer ditulis satu baris di `docs/loop-history/`; AC yang lolos masuk `docs/ac-trace.md`.

## Gate

| Gate | Memeriksa | Ditegakkan oleh | Cara melewatinya |
| --- | --- | --- | --- |
| `scripts/verify.sh fast` | fmt, clippy `-D warnings`, seluruh test, jejak AC | Instruksi di CLAUDE.md (definisi "selesai") | Tidak dijalankan sama sekali |
| `.githooks/pre-commit` | Menjalankan `verify.sh fast` pada setiap commit | Hook git (`core.hooksPath=.githooks`) | `git commit --no-verify`, mengubah `core.hooksPath`, atau clone baru yang belum mengaktifkan hook. Dua yang pertama ditolak oleh deny rule bagi Claude, tidak bagi manusia |
| `implementer-stop-gate.sh` | Implementer tidak boleh melapor selesai saat `verify.sh fast` gagal | Hook Claude Code `SubagentStop` | Percobaan kedua selalu diizinkan (mencegah loop tak berujung); kegagalan lalu ditulis ke `.claude/work/gate-failed.txt` dan harus ditangkap reviewer atau hook git |
| `scripts/ac-trace.sh` | Tiap AC punya bukti: nama test ada dan tidak `#[ignore]`, atau berkas `manual:` ada | Bagian dari `verify.sh fast` | Menulis nama test yang ada tetapi tidak menguji AC itu; bukti `manual:` hanya diperiksa keberadaan berkasnya |
| Deny rule di `.claude/settings.json` | Agen tidak boleh mengedit skrip gate, `.githooks/`, `.claude/hooks/`, settings, dan tidak boleh memakai `--no-verify` atau mengubah `core.hooksPath` | Permission Claude Code | Aturan `Edit(...)` mencakup alat Edit; perintah shell yang menulis berkas (mis. `sed -i`) tidak dijamin tercakup. Manusia bebas mengubah semuanya |
| `scripts/verify.sh mutants` | Test benar-benar mendeteksi perubahan perilaku di `checks`, `matchers`, `report` | Instruksi di skill `/phase` (phase gate) | Tidak dijalankan; tidak ada hook yang memaksa |
| Aturan "no false PASS" | Jalur yang tak bisa membuktikan oracle berakhir INCONCLUSIVE/ERROR/NOT_RUN | Instruksi CLAUDE.md, test, perhatian khusus reviewer | Bergantung pada kualitas test dan penilaian reviewer |

## Metrik loop

Keluaran `scripts/loop-metrics.sh` (dari `docs/loop-history/*.md`):

| Fase | Slice | PASS putaran 1 | FIX | REANALYZE | Blocked | Gate non-PASS | Escaped |
| --- | --- | --- | --- | --- | --- | --- | --- |
| P0 | 10 | 8/10 (80%) | 2 | 0 | 0 | 0 | 0 |
| P1 | 5 | 4/5 (80%) | 1 | 0 | 0 | 0 | 0 |
| P2 | 8 | 8/8 (100%) | 0 | 0 | 0 | 0 | 0 |
| P3 | 8 | 4/8 (50%) | 4 | 0 | 0 | 0 | 0 |
| P4 | 8 | 3/8 (38%) | 5 | 0 | 0 | 0 | 0 |
| **Total** | 39 | 27/39 (69%) | 12 | 0 | 0 | 0 | 0 |

Catatan: P4 mencakup slice S1–S6 dan M1–M2. Riwayat P0–P4 disusun ulang dari Log handoff (`docs/loop-history/raw/`), bukan dicatat saat kejadian. Perbaikan kecil sesudah PASS di P0 (S3–S6, S8) tidak dihitung sebagai putaran karena tidak ada verdict FIX; angka "PASS putaran 1" P0 karenanya agak optimistis. Slice S11 P0 (tolak `race.concurrency < 2`) diusulkan phase gate P0 tetapi tidak pernah dikerjakan, jadi tidak tercatat.

## Mutasi

cargo-mutants 27.1.0, cakupan bawaan (`src/checks/**`, `src/matchers*`, `src/report*`):

| | Caught | Missed | Unviable | Timeout | Total |
| --- | --- | --- | --- | --- | --- |
| Sebelum (baseline) | 215 | 6 | 20 | 3 | 244 |
| Sesudah M1 + M2 | 224 | 0 | 20 | 0 | 244 |

Sembilan mutan yang lolos diperbaiki dengan unit test saja; kode produksi tidak berubah. Tidak ada mutan yang dinyatakan equivalent (`docs/mutation-triage.md`). Dua test M1 memakai `std::process::exit(101)` karena mutan batas worker membuat loop sibuk tanpa menyerahkan giliran sehingga timeout biasa tidak sempat jalan; pada kode benar cabang itu tidak pernah dieksekusi.

Tambahan di luar cakupan bawaan: `src/config/validate.rs` (86 mutan) diukur atas permintaan pengguna. Sebelum: 82 caught, 1 missed, 3 unviable. Sesudah slice M3: 83 caught, 0 missed, 3 unviable, 0 timeout. Berkas ini belum masuk cakupan bawaan `scripts/verify.sh`; jalankan `scripts/verify.sh mutants --file 'src/config/validate.rs'`.

## Cakupan AC

`docs/ac-trace.md`: 114 baris AC (P0–P4).

- Bukti test: 106 baris.
- Bukti `manual:`: 8 baris (P0-AC18, P0-AC20, P1-AC18, P2-AC25, P3-AC26, P4-AC6a, P4-AC6b, P4-AC10). Ini AC tentang dokumen dan gate; bukti `manual:` hanya diperiksa keberadaan berkasnya, bukan isinya. Isi `docs/evidence/quality-gate.md` ditulis sekarang, bukan rekaman saat fase ditutup.
- P4-AC1.1 … P4-AC5.4 ditulis `P4-AC1a` … `P4-AC5d` karena `ac-trace.sh` hanya mengenali id berbentuk `AC<n>[a-z]`.
- P0-AC16 ("`check` pada P0 tidak pernah exit 0") sudah digantikan perilaku P2–P4; buktinya hanya menunjukkan `check` tidak exit 0 pada input tidak valid. Perlu ditandai digantikan.
- Beberapa baris hanya sebagian tertutup test (mis. TLS di P0-AC11, bind loopback di P0-AC17, "mencakup connect" di P3-AC12); keterangannya ada di kolom Keterangan.
- P4-AC7 (run API nyata) tidak ada di tabel. Terbuka, lihat di bawah.

## Keterbatasan

- Gate bersifat lokal. Manusia tetap bisa melewati hook git (`--no-verify`, mengubah `core.hooksPath`); CI belum ada sampai server git tersedia.
- Reviewer adalah LLM. Verdict-nya bisa keliru; gate mekanis (fmt, clippy, test, jejak AC, mutasi) membatasi kerusakan tetapi tidak menggantikan penilaian.
- `ac-trace.sh` membuktikan bahwa test yang dirujuk ada, tidak di-ignore, dan lulus. Ia tidak membuktikan bahwa test itu memang menguji AC yang dituju; itu tugas reviewer.
- Mutasi bawaan hanya mencakup `checks`, `matchers`, `report`; `config/validate.rs` diukur terpisah. `config/resolve.rs`, `http`, dan `cli` tidak diukur. Mutan unviable (20) tidak membuktikan apa-apa.
- Kebenaran oracle (aturan verdict di `docs/PLAN.md`) bergantung pada rencana dan pengguna. Test yang lulus membuktikan kode sesuai rencana, bukan bahwa rencana benar.
- Semua bukti berasal dari fixture lokal. **P4-AC7 (validasi terhadap API nyata) terbuka**: belum ada laporan run nyata, jadi barisnya sengaja tidak ada di `docs/ac-trace.md`.

## Verifikasi ulang oleh pihak ketiga

```
git checkout <commit>
git config core.hooksPath .githooks   # opsional, mengaktifkan pre-commit
scripts/verify.sh fast                # fmt, clippy, test, jejak AC; exit 0 = lulus
scripts/verify.sh mutants             # opsional, ±10–15 menit, butuh cargo-mutants
scripts/loop-metrics.sh               # ringkasan loop
```

Keluaran terakhir `verify.sh` tersimpan di `target/verify/` (`last.txt` memuat hash tree dan commit).
