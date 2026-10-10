# Fixture lokal (healthy dan broken login)

Alat pengembangan, bukan fitur produk dan bukan mock server produk. Varian healthy plus dua varian broken login (P1). Varian broken race/load ditambahkan di P2-P3.
Kode: `tests/support/fixture.rs`, dijalankan lewat `examples/fixture.rs`.
Spesifikasi OpenAPI semua rute: [fixture.openapi.yaml](fixture.openapi.yaml) (bisa dibuka di Swagger Editor atau `npx @redocly/cli preview-docs docs/fixture.openapi.yaml`).

## Start

```sh
cargo run --example fixture                # port 8080
cargo run --example fixture -- --port 9090 # port lain (0 = acak, alamat dicetak saat start)
```

Hanya bind ke `127.0.0.1` (loopback), tidak pernah `0.0.0.0`/`::`. Berhenti dengan Ctrl-C (graceful shutdown). Server tidak mencatat request, header, atau body.

## Env var untuk config contoh (`examples/core-checks.json`)

Nilai di bawah adalah konstanta dev publik, bukan secret. Hanya untuk fixture; target nyata memakai nilai milik pengguna.

```sh
export LOGIN_USER=demo
export LOGIN_VALID_PASSWORD=demo-pass
export LOGIN_INVALID_PASSWORD=wrong-pass
export TEST_ACTION_ID=act-1
export TEST_AUTHORIZATION='Bearer fixture-token'
```

Jika memakai `--port` selain 8080, ubah `base_url` di salinan config.

## Endpoint dan perilaku healthy

| Endpoint | Perilaku |
| --- | --- |
| `POST /login` JSON `{"username","password"}` | Kredensial benar dan hitungan gagal < 6: 200 `{"access_token": "fixture-access-token"}`. Kredensial salah: 401 untuk kegagalan ke-1 sampai ke-5, 429 mulai kegagalan ke-6 (proteksi pada attempt 6, sesuai `protect_by_attempt` contoh). Login benar TIDAK mereset hitungan gagal; setelah hitungan >= 6, login benar pun 429. Hanya reset admin yang menghapus hitungan. |
| `POST /broken/login/unprotected` | Varian broken: tanpa proteksi. Kredensial benar 200 token; salah selalu 401, tidak pernah 429, tanpa state. Body rusak 400. |
| `POST /broken/login/accepts-invalid` | Varian broken: username `demo` dengan password apa pun dijawab 200 token (kredensial salah diterima). Username lain 401. Body rusak 400. |
| `GET /__admin/stats` | Alat dev. 200 `{"login_requests": n, "load_requests": m}`. `login_requests` menghitung setiap POST yang sampai ke handler tiga rute login (termasuk body rusak 400; method selain POST dan body di atas batas default axum tidak dihitung); `load_requests` menghitung `GET /critical` yang terotorisasi. Request stats sendiri tidak dihitung. |
| `POST /actions/{id}/apply` | Wajib `Authorization` = `Bearer fixture-token`, jika tidak 401. Apply pertama untuk id itu: 200 dan `execution_count` jadi 1. Selanjutnya 409 tanpa menambah counter. Keputusan atomik (lock dipegang selama cek + tambah), jadi N request bersamaan menghasilkan tepat satu 200. |
| `GET /actions/{id}` | Wajib `Authorization`. 200 `{"id": "...", "execution_count": n}`, n awal 0. Id apa pun dibuat otomatis dengan count 0 saat pertama diakses. |
| `GET /critical` | Wajib `Authorization`. 200 `{"ok": true}` cepat. |
| `POST /__admin/reset` | Alat dev. Mereset hitungan gagal login, semua counter action, counter load, dan counter `login_requests` ke 0. 200 `{"reset": true}`. |
| rute lain | 404 JSON `{"error": "not_found"}`. |

Respons selalu JSON dengan body tetap (`{"error": ...}` untuk 400/401/404/409/429; stats berisi angka saja); tidak pernah memantulkan data request. Rute login (ketiganya) dengan body rusak atau bertipe salah dijawab `400 {"error":"bad_request"}`.

## Reset

```sh
curl -X POST http://127.0.0.1:8080/__admin/reset
```

atau restart proses fixture. Reset adalah alat pengembangan: tool `api-toolkit` tidak memanggilnya. Jalankan reset sebelum mengulang check (baseline login berjalan sebelum probe dan butuh hitungan gagal 0; race butuh counter awal 0).

## Varian prelocked (akun sudah terkunci)

Tidak ada rute khusus: prelocked adalah state healthy `/login` setelah >= 6 login salah. Cara membuatnya: kirim 6 login salah ke `/login` (atau jalankan check brute force dua kali tanpa reset). Baseline berikutnya dijawab 429 sehingga check menghasilkan INCONCLUSIVE `account_already_locked`. Dengan 5 login salah lebih dulu, baseline 200 dan probe 1 yang 429. Cek jumlah request yang sampai ke fixture dengan `GET /__admin/stats`.
