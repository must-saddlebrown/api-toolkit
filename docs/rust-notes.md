# Catatan Rust

Satu bagian per konsep Rust baru yang dipakai di proyek: apa itu, mengapa dipakai di sini, dan di mana letaknya di kode. Ditambahkan saat close-out tiap fase.

## P0

### Library dan binary (`lib.rs` vs `main.rs`)
Satu package bisa punya library (`src/lib.rs`) dan binary (`src/main.rs`). Logika ada di library supaya folder `tests/` bisa memanggilnya sebagai crate `api_toolkit`; `main.rs` hanya pembungkus tipis. Nama package `api-toolkit` menjadi nama crate `api_toolkit` (tanda minus tidak sah di identifier). Lihat `src/lib.rs`, `src/main.rs`, bagian `[lib]` dan `[[bin]]` di `Cargo.toml`.

### Modul (`mod`) dan visibilitas (`pub`)
`mod config;` memberi tahu compiler untuk memuat `src/config.rs`; submodul seperti `src/config/validate.rs` dideklarasikan dengan `mod validate;` di dalam `config.rs`. Tanpa `pub`, item hanya terlihat di modulnya. Dipakai untuk memecah kode sesuai PLAN §13: `src/config.rs`, `src/http.rs`, `src/checks/mod.rs`.

### Cargo feature dan `[dev-dependencies]`
`default-features = false` mematikan fitur bawaan crate; kita hanya menyalakan yang dibutuhkan (`reqwest` dengan `rustls`, `json`). Dependency di `[dev-dependencies]` (axum, tempfile) hanya ikut build test/example, tidak masuk binary produk. Satu crate boleh muncul di dua bagian dengan fitur berbeda (tokio `test-util`, `signal`). Lihat `Cargo.toml`.

### serde: `derive`, `deny_unknown_fields`, `default`
`#[derive(Deserialize)]` membuat kode parsing otomatis. `#[serde(deny_unknown_fields)]` menolak field yang tidak dikenal (kontrak config PLAN §9); `#[serde(default)]` mengisi nilai bawaan; `#[serde(rename_all = "snake_case")]` membuat string enum stabil di JSON. Lihat `src/config.rs` (struct config) dan `src/report.rs` (enum report).

### `Deserialize` buatan sendiri (`Visitor`) dan `PhantomData`
serde diam-diam memakai nilai terakhir untuk key duplikat di `BTreeMap`. Agar duplikat ditolak, `UniqueMap<V>` mengimplementasikan `Deserialize` lewat `Visitor` dan `MapAccess`, lalu dipasang dengan `#[serde(deserialize_with = ...)]`. `PhantomData<V>` menandai tipe nilai tanpa menyimpannya. Lihat `src/config.rs` (`UniqueMap`, `UniqueMapVisitor`).

### Enum error dengan `thiserror`
`#[derive(thiserror::Error)]` plus `#[error("...")]` membuat `Display` dan `Error` otomatis; varian boleh membawa field. Library memakai error bertipe, hanya `main`/CLI yang memakai `anyhow`. Pesan error hanya memuat nama field, bukan nilai. Lihat `ConfigError` (`src/config.rs`), `UrlError` (`src/http/request.rs`), `TransportError` (`src/http/client.rs`), `ReportError` (`src/report.rs`).

### Operator `?`, `Result`, `map_err`, `anyhow::Context`
`?` mengembalikan error lebih awal dari fungsi; `map_err` mengubah tipe error; `with_context` (anyhow) menambah konteks di tepi program. Lihat `config::parse`, `cli::read_config`, `cli::validate_file`.

### Newtype dengan `Debug` manual (redaksi secret)
`Secret(String)` membungkus string dan menulis `Debug` sendiri yang hanya mencetak `Secret(<redacted>)`; sengaja tanpa `Display`, `Serialize`, atau `Clone`. Tipe lain yang memegangnya (`ResolvedRequest`) cukup memakai `#[derive(Debug)]` dan tetap aman. Lihat `src/config/resolve.rs` (`Secret`), `src/http/request.rs` (`ResolvedRequest`), `TransportResponse` di `src/http/client.rs` (Debug manual yang menyembunyikan body).

### `&str` vs `String` dan kepemilikan (ownership)
`resolve` meminjam `&Config` dan membangun salinan baru (`String`) yang sudah tersubstitusi; config mentah tidak berubah. Lihat `src/config/resolve.rs` dan `src/matchers.rs` (`&self`, `Option<&Value>`).

### Generic dengan batasan trait dan dependency injection
`F: Fn(&str) -> Option<String>` membuat pencarian env bisa diganti: produksi memakai `std::env::var`, test memakai closure palsu (tanpa menyentuh environment proses). `impl Write` membuat `cli::run` bisa dites dengan `Vec<u8>`. Lihat `resolve` di `src/config/resolve.rs` dan `run`, `validate_file` di `src/cli.rs`.

### `impl` di modul lain dan `impl From`
Metode `MatcherSpec::matches` ditulis di `src/matchers.rs` walau struct-nya ada di `src/config.rs` (boleh dalam satu crate). `impl From<TransportErrorCode> for TransportError` memungkinkan `.into()`. Lihat `src/matchers.rs`, `src/http/client.rs`.

### Enum `Copy`, `const ALL`, `match` exhaustive, dan golden test
Enum report punya `const ALL` dan `as_str()`. Test memakai `match` exhaustive tanpa cabang bawaan: menambah varian baru tidak akan compile sampai string-nya didaftarkan. Golden test membandingkan JSON persis dengan teks yang ditulis tangan. Lihat `src/report.rs` (`Verdict`, `ReasonCode`, `report_json_matches_golden`).

### `async`/`await`, `tokio::spawn`, `#[tokio::test]`
Fungsi `async fn` mengembalikan future yang dijalankan runtime tokio. `#[tokio::test]` menjalankan test async; `flavor = "multi_thread"` dipakai bila konkurensi benar-benar harus paralel (32 apply bersamaan). `tokio::spawn` menjalankan task; `tokio::time::timeout` membatasi waktu tunggu test. Lihat `src/http/client.rs` (`send` dan test), `tests/transport_fixture.rs`, `tests/fixture_smoke.rs`.

### Berbagi state: `Arc<Mutex<T>>` dan atomic
`Arc` membagi kepemilikan antar-thread, `Mutex` menjaga akses. Keputusan "apply pertama = 200, berikutnya = 409" dibuat atomik dengan menahan lock selama cek dan ubah, tanpa `.await` di tengah. Lock yang teracuni dipulihkan dengan `into_inner`. `static AtomicU64` dipakai untuk nama file sementara yang unik. Lihat `tests/support/fixture.rs`, `src/report.rs` (`write_report_atomic`), `src/http/client.rs` (test server).

### Handler dan extractor axum, graceful shutdown
Handler axum adalah fungsi `async` dengan extractor (`State`, `Path`, `Bytes`) yang mengembalikan `(StatusCode, Json<...>)`. Shutdown anggun memakai `oneshot` dan `with_graceful_shutdown`. Lihat `tests/support/fixture.rs`, `examples/fixture.rs` (Ctrl-C lewat `tokio::signal`).

### `#[path = "..."] mod`
Satu file (`tests/support/fixture.rs`) dipakai bersama oleh test dan example tanpa menjadikannya bagian produk. Lihat `examples/fixture.rs`.

### clap derive dan `ExitCode`
`#[derive(Parser)]` dan `#[derive(Subcommand)]` membangun CLI dari struct dan enum. `run` mengembalikan `u8`; `main` mengubahnya dengan `ExitCode::from`, sehingga exit code dapat dites tanpa memulai proses. Lihat `src/cli.rs`, `src/main.rs`.

### `let ... else`, `let` chain, dan combinator `Option`
`let Some(x) = ... else { return ... };` untuk keluar lebih awal; `if let ... && ...` (edisi 2024) untuk beberapa kondisi sekaligus; `Option::filter`, `is_none_or`, `ok_or_else`. Lihat `src/matchers.rs`, `src/config.rs` (`parse`), `src/report.rs`.

### Membaca respons bertahap (`chunk()`) dan batas ukuran
Body dibaca potong demi potong dan berhenti saat melewati batas, sehingga body tak terbatas tidak menghabiskan memori. Lihat `src/http/client.rs` (`send`).

### Test: `#[cfg(test)]`, `#[cfg(unix)]`, tabel kasus, proses nyata
Unit test inline di `#[cfg(test)] mod tests`; test khusus Unix (direktori read-only) memakai `#[cfg(unix)]`; test tabel memakai `Vec<(&str, fn(&mut Config), &str)>`; test integrasi menjalankan binary nyata lewat `env!("CARGO_BIN_EXE_api-toolkit")` dan `std::process::Command`. Lihat `src/config/validate.rs`, `src/report.rs`, `tests/validate_cli.rs`, `tests/support/cli.rs`.

## Catatan P1 (brute-force check)

### Closure generik yang mengembalikan `Future`
`run_with` menerima `F: FnMut(Kind) -> Fut` dengan `Fut: Future<Output = ...>`. Production memberi closure yang memanggil `http::send`; test memberi closure skrip. Jadi tidak perlu trait untuk test double. Lihat `src/checks/brute_force.rs` (`run_with`, `run`).

### `tokio::select!` (`biased`), `watch`, `std::future::pending`
Request berjalan sambil menunggu sinyal stop. `select!` mengambil yang selesai lebih dulu; `biased;` memeriksa stop dulu. Stop dikirim lewat `tokio::sync::watch`; bila pengirim di-drop, `pending()` membuat "tidak pernah stop". Request yang kalah di-drop (dibatalkan). Lihat `run_with`, `wait_stop`, `current_stop`, `src/checks/mod.rs` (`Stop`).

### `#[serde(skip_serializing_if = "Option::is_none")]`
Field `evidence` tidak muncul di JSON bila `None`, sehingga golden report P0 tetap sama. Lihat `src/report.rs` (`CheckResult`).

### Test dengan waktu virtual: `#[tokio::test(start_paused = true)]`
Waktu tokio maju otomatis saat semua task menunggu, jadi interval 100 ms tidak membuat test lambat dan hasilnya deterministik. Jangan dipakai di integration test dengan reqwest (timer-nya ikut maju palsu). Lihat tes `run_*` di `src/checks/brute_force.rs`.

### `Rc<RefCell<T>>`, `Pin<Box<dyn Future>>`, `impl FnMut`
State bersama satu thread untuk mencatat panggilan di test double; `Pin<Box<dyn Future>>` sebagai tipe kembalian closure skrip. Lihat `scripted` di tes `brute_force.rs`.

### `#[track_caller]`, `std::mem::forget`, `slice::windows`
`#[track_caller]` membuat panic assert menunjuk baris pemanggil (`assert_eval`). `mem::forget` menjaga `watch::Sender` tetap hidup di test. `windows(2)` membandingkan pasangan berurutan (`tests/brute_force_fixture.rs`, cek interval).

### Server TCP mentah di test
`tokio::net::TcpListener` plus `AsyncReadExt`/`AsyncWriteExt` untuk server skrip yang sengaja menggantung (timeout). Lihat `scripted_server` di `tests/brute_force_fixture.rs`.

## Catatan `check --only`

### Runtime tokio manual dan sinyal
`main` tidak async. `check_blocking` membangun runtime (`tokio::runtime::Builder`), mendaftarkan SIGINT (`tokio::signal::unix::signal`) sebelum request pertama, lalu `block_on(run_check(..))`. Fungsi inti `run_check` menerima future `cancel` lewat parameter `impl Future<Output = ()>`, sehingga test memberi future biasa, tanpa sinyal nyata. Lihat `src/cli.rs`.

### `select!` `biased` dengan guard `if`, `pin!`, `watch::send_replace`
Cancel dan deadline berlomba dengan check. `tokio::pin!` mem-pin future agar bisa di-poll berulang di loop; guard `if stop_sent.is_none()` membuat tiap sumber stop hanya mengirim sekali; `watch::Sender::send_replace` menyimpan stop pertama. `std::future::ready` dan `pending` dipakai sebagai cancel di test. Lihat `run_check`.

### Bagian config opsional: `Option<T>` di serde, `Selection`, `transpose`
`brute_force`, `race`, `load` menjadi `Option`; `Selection::{All, Only(CheckName)}` (enum `Copy` dengan data dan method `includes`) menentukan bagian mana yang divalidasi dan di-resolve. `Option::transpose` membalik `Option<Result<..>>` menjadi `Result<Option<..>>`, `Option::map_or` memberi default. Lihat `src/config.rs`, `src/config/validate.rs`, `src/cli.rs`.

### Let chain dan `canonicalize`
`if let (Ok(a), Ok(b)) = (..) && a == b` (edisi 2024) membandingkan path setelah `std::fs::canonicalize`, sehingga symlink ke config terdeteksi. Lihat `precheck_out`.

### Test end-to-end dengan binary nyata
`env!("CARGO_BIN_EXE_api-toolkit")` memberi path binary; `Command::env_clear().envs(..)` membuat env hanya berlaku untuk proses anak; `tokio::task::spawn_blocking` memanggil proses dari test async; `impl Drop` pada struct pembantu (`Hang`) membatalkan task server; `#[cfg(unix)]` untuk `kill -INT`. Lihat `tests/check_only_cli.rs`.
