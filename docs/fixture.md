# Fixture lokal (healthy, broken login, varian race)

Alat pengembangan, bukan fitur produk dan bukan mock server produk. Varian healthy, dua varian broken login (P1), varian racy dan idempotent untuk race (P2). Varian broken load (slow, flaky, hang) ada sejak P3.
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
| `GET /__admin/stats` | Alat dev. 200 `{"login_requests", "load_requests", "action_requests", "verify_requests"}` (semua angka). `login_requests` menghitung setiap POST yang sampai ke handler tiga rute login (termasuk body rusak 400; method selain POST dan body di atas batas default axum tidak dihitung); `load_requests` menghitung setiap `GET` ke empat rute load (`/critical`, `/broken/critical/slow`, `/broken/critical/flaky`, `/broken/critical/hang`), termasuk yang 401; `action_requests` menghitung setiap POST ke tiga rute apply (`/actions/{id}/apply`, `/broken/actions/{id}/apply-racy`, `/idempotent/actions/{id}/apply`), termasuk yang 401; `verify_requests` menghitung setiap `GET /actions/{id}`, termasuk 401. Request stats sendiri tidak dihitung. |
| `POST /actions/{id}/apply` | Wajib `Authorization` = `Bearer fixture-token`, jika tidak 401. Apply pertama untuk id itu: 200 dan `execution_count` jadi 1. Selanjutnya 409 tanpa menambah counter. Keputusan atomik (lock dipegang selama cek + tambah), jadi N request bersamaan menghasilkan tepat satu 200. |
| `POST /broken/actions/{id}/apply-racy` | Varian broken (check-then-act) dengan titik sinkronisasi eksplisit. Auth seperti healthy (tanpa header: 401). Bila counter id sudah > 0: 409. Bila counter 0: request mendaftar di gerbang (`tokio::sync::watch` per id) dan menunggu sampai minimal 2 request telah membaca 0 (maks 2 detik), baru menaikkan counter dan menjawab 200 `{"applied": true}`. Client yang benar-benar konkuren: minimal 2 kali 200 dan `execution_count >= 2` secara deterministik (bukan timing keberuntungan). Client sekuensial: gerbang timeout 2 detik, counter tetap 1, sehingga test duplikat MERAH (bukan PASS palsu). Check race harus FAIL `duplicate_side_effect`. |
| `POST /idempotent/actions/{id}/apply` | Varian healthy idempotent. Auth seperti healthy. Selalu 200 `{"applied": true}` dan counter id menjadi 1 (tepat satu side effect, tidak pernah lebih). Check race dengan N=5 harus PASS `single_side_effect` dengan lima 200 dan delta 1. |
| `GET /actions/{id}` | Wajib `Authorization`. 200 `{"id": "...", "execution_count": n}`, n awal 0. Id apa pun dibuat otomatis dengan count 0 saat pertama diakses. |
| `GET /critical` | Wajib `Authorization`. 200 `{"ok": true}` cepat. Setiap request dihitung di `load_requests`, termasuk 401. |
| `GET /broken/critical/slow` | Varian broken load. Auth seperti `/critical`. Tidur `SLOW_DELAY` = 300 ms lalu 200 `{"ok": true}`. Dengan `max_p95_ms=100`, check load FAIL `latency_exceeded`. |
| `GET /broken/critical/flaky` | Varian broken load. Auth seperti `/critical`. Bila nomor `load_requests` (setelah increment) kelipatan `FLAKY_EVERY` = 10: 500 `{"error": "flaky"}`, selain itu 200. Preflight adalah request ke-1, jadi 100 request load menghasilkan tepat 10 kegagalan. Berbasis counter, bukan timing. |
| `GET /broken/critical/hang` | Varian broken load. Auth seperti `/critical`. Bila nomor `load_requests` kelipatan 10: tidur `HANG_DELAY` = 3 detik lalu 200; selain itu 200 cepat. Dengan `request_timeout_ms` di bawah 3 detik request tersebut timeout (dihitung failed). |
| `POST /__admin/reset` | Alat dev. Mereset hitungan gagal login, semua counter action, state gerbang racy, semua counter load, dan counter `login_requests`, `action_requests`, `verify_requests` ke 0. 200 `{"reset": true}`. |
| rute lain | 404 JSON `{"error": "not_found"}`. |

Respons selalu JSON dengan body tetap (`{"error": ...}` untuk 400/401/404/409/429; stats berisi angka saja); tidak pernah memantulkan data request. Rute login (ketiganya) dengan body rusak atau bertipe salah dijawab `400 {"error":"bad_request"}`.

## Reset

```sh
curl -X POST http://127.0.0.1:8080/__admin/reset
```

atau restart proses fixture. Reset adalah alat pengembangan: tool `api-toolkit` tidak memanggilnya. Jalankan reset sebelum mengulang check (baseline login berjalan sebelum probe dan butuh hitungan gagal 0; race butuh counter awal 0).

## Varian race (P2)

Ketiga rute apply memakai map counter yang sama, dan verifier tetap `GET /actions/{id}`. Kasus lain dibuat lewat config, tanpa rute baru: counter hilang (`counter_pointer=/nope`), bukan integer (`/id` -> string), delta 0 (id action berbeda dari id verifier), status ambigu (`expected_action_statuses=[200]` pada rute healthy, 409 jadi ambigu), tanpa header auth (401).

Titik sinkronisasi racy: request pertama yang membaca counter 0 tidak menaikkan counter sebelum request kedua juga membaca 0. Karena itu duplikasi tidak bergantung pada kecepatan mesin. Timeout gerbang 2 detik harus lebih kecil daripada `request_timeout_ms` config. Reset menjatuhkan semua gerbang (request yang menunggu dilepas). Setiap run racy butuh id baru atau reset, karena counter > 0 membuat run berikutnya `initial_counter_not_zero`.

Bukti jumlah request: setelah satu run race sehat, `action_requests` = N dan `verify_requests` = 2 (tanpa retry).

## Varian prelocked (akun sudah terkunci)

Tidak ada rute khusus: prelocked adalah state healthy `/login` setelah >= 6 login salah. Cara membuatnya: kirim 6 login salah ke `/login` (atau jalankan check brute force dua kali tanpa reset). Baseline berikutnya dijawab 429 sehingga check menghasilkan INCONCLUSIVE `account_already_locked`. Dengan 5 login salah lebih dulu, baseline 200 dan probe 1 yang 429. Cek jumlah request yang sampai ke fixture dengan `GET /__admin/stats`.

## Varian load (P3)

Empat rute load memakai satu counter `load_requests` (lihat tabel). Reset mengosongkannya, dan aturan "setiap ke-10" dihitung dari counter itu, jadi jalankan reset sebelum mengulang check load. Check load mengirim 1 preflight + `requests` request: config contoh (100 request) menghasilkan `load_requests` = 101.

Mencoba `check --only load` (healthy):

```sh
cargo run --example fixture &              # atau terminal lain
export TEST_AUTHORIZATION='Bearer fixture-token'
curl -X POST http://127.0.0.1:8080/__admin/reset
cargo run -- validate --config examples/load-only.json --only load
cargo run -- check --config examples/load-only.json --out /tmp/load-report.json --only load
echo $?    # run parsial tidak pernah exit 0: PASS = 2, FAIL = 1
```

Varian broken: salin `examples/load-only.json`, ubah `load.request.path` ke `/broken/critical/slow` (dengan `max_p95_ms` 100), `/broken/critical/flaky` (FAIL `error_rate_exceeded`, 10 dari 100), atau `/broken/critical/hang` (dengan `limits.request_timeout_ms` 500 dan `requests` 20; dua request timeout). Reset di antara run. Cek jumlah request dengan `GET /__admin/stats`.
