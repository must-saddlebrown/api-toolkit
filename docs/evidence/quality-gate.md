# Bukti gate kualitas (P0–P4)

Bukti untuk AC "gate" per fase (fmt, clippy `-D warnings`, `cargo test --all-targets` hijau; tanpa `unwrap`/`expect` produksi; tanpa dependency baru; tanpa test yang mengirim request ke target eksternal). Ditulis saat integrasi quality gates, berlaku untuk keadaan repo di commit `f48231a` (penutupan P4) ditambah berkas gate.

## Cara memeriksa ulang

```
scripts/verify.sh fast
```

Perintah ini menjalankan fmt, clippy, seluruh test, dan `scripts/ac-trace.sh`. Exit 0 berarti fmt, clippy `-D warnings`, dan 416 test hijau. Pada tiap commit, hook `.githooks/pre-commit` menjalankannya lagi.

## Tanpa `unwrap`/`expect` produksi

Pemeriksaan: untuk setiap `src/**/*.rs`, cari `.unwrap()` dan `.expect(` pada baris sebelum `#[cfg(test)]` pertama di berkas itu. Hasil: tidak ada baris yang cocok.

## Tanpa request ke `127.0.0.1:8080` atau target eksternal dari test

Acuan `127.0.0.1:8080` di `tests/` hanya berupa string `base_url` pada config contoh (`tests/fixture_proof.rs`, `tests/validate_cli.rs`) dan komentar. Test yang menjalankan binary memakai fixture pada port acak atau listener penghitung lokal (`tests/validate_cli.rs`, komentar "so a broken precheck cannot hit :8080"). Test `fixture_proof` tidak memakai env var dari luar.

## Dokumen yang diwajibkan AC gate

- Commands di `CLAUDE.md` diperbarui (P0-AC20).
- `docs/fixture.md` memuat varian broken login dan `/__admin/stats` (P1-AC18), varian race, reset, titik sinkronisasi, dan stats (P2-AC25), serta varian load (P3-AC26); `docs/fixture.openapi.yaml` ikut diperbarui.

## Batas bukti ini

Berkas ini bukti keadaan sekarang, bukan rekaman gate per fase saat fase itu ditutup; rekaman asli ada di Log `docs/loop-history/raw/P*-plan.md`. Pemeriksaan `unwrap` memakai heuristik "sebelum `#[cfg(test)]` pertama", jadi `unwrap` di berkas yang menaruh blok test di tengah tidak terdeteksi.
