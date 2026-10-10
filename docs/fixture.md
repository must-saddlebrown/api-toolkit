# Fixture lokal (healthy)

Alat pengembangan, bukan fitur produk dan bukan mock server produk. Hanya varian healthy; varian broken/slow/prelocked ditambahkan di P1-P3.
Kode: `tests/support/fixture.rs`, dijalankan lewat `examples/fixture.rs`.

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
| `POST /actions/{id}/apply` | Wajib `Authorization` = `Bearer fixture-token`, jika tidak 401. Apply pertama untuk id itu: 200 dan `execution_count` jadi 1. Selanjutnya 409 tanpa menambah counter. Keputusan atomik (lock dipegang selama cek + tambah), jadi N request bersamaan menghasilkan tepat satu 200. |
| `GET /actions/{id}` | Wajib `Authorization`. 200 `{"id": "...", "execution_count": n}`, n awal 0. Id apa pun dibuat otomatis dengan count 0 saat pertama diakses. |
| `GET /critical` | Wajib `Authorization`. 200 `{"ok": true}` cepat. |
| `POST /__admin/reset` | Alat dev. Mereset hitungan gagal login, semua counter action, dan counter load ke 0. 200 `{"reset": true}`. |
| rute lain | 404 JSON `{"error": "not_found"}`. |

Respons selalu JSON dengan body tetap (`{"error": ...}` untuk 400/401/404/409/429); tidak pernah memantulkan data request. `POST /login` dengan body rusak atau bertipe salah dijawab `400 {"error":"bad_request"}`.

## Reset

```sh
curl -X POST http://127.0.0.1:8080/__admin/reset
```

atau restart proses fixture. Reset adalah alat pengembangan: tool `api-toolkit` tidak memanggilnya. Jalankan reset sebelum mengulang check (baseline login berjalan sebelum probe dan butuh hitungan gagal 0; race butuh counter awal 0).
