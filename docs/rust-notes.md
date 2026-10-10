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

## Catatan P2 (race check)

### `#[serde(untagged)]` pada enum
`Evidence` adalah enum `{ BruteForce, Race }`. Dengan `#[serde(untagged)]`, JSON hanya berisi isi varian tanpa nama varian, jadi report brute_force tetap identik dengan golden P0. Aman karena tipenya hanya di-serialize, tidak pernah dibaca balik. Lihat `src/report.rs` (`Evidence`).

### Future itu malas (lazy) dan `join_all`
Memanggil fungsi `async` hanya membuat future; tidak ada byte terkirim sampai future itu di-poll. `futures_util::future::join_all` mem-poll N future sekaligus dalam satu task, jadi N action dilepas bersamaan tanpa `spawn` dan tanpa `Barrier`. Tiap future mencatat waktu dispatch pada poll pertama. Keterbatasannya (skew hanya mikrodetik) ada di komentar `ponytail:` dan jalur upgradenya `spawn` + `tokio::sync::Barrier`. Lihat `src/checks/race.rs` (`run_with`).

### `Fn` versus `FnMut`
Closure pengirim di `race::run_with` bertipe `Fn(Kind) -> Fut` karena N future meminjam closure yang sama secara bersamaan (hanya baca). `brute_force::run_with` memakai `FnMut` karena dipanggil berurutan dan boleh mengubah state. Lihat `src/checks/race.rs`, `src/checks/brute_force.rs`.

### Blok berlabel `'run: { ... break 'run; }`
Alur "before -> action -> settle -> after" punya banyak jalan keluar dini (stop, counter bukan 0). Blok berlabel memungkinkan `break 'run` keluar dari seluruh rangkaian tanpa fungsi bantu tambahan. Lihat `run_with`.

### `tokio::sync::watch` dan `select!`
Stop (cancel/deadline) dikirim lewat `watch`; tiap future memegang `Receiver::clone` dan berlomba dengan request memakai `select! { biased; .. }`. Di fixture, `watch::Sender::send_modify` dan `Receiver::wait_for` dipakai sebagai gerbang sinkronisasi eksplisit untuk varian racy: request yang membaca counter 0 menunggu sampai minimal dua request membaca 0 (timeout 2 detik). Lihat `src/checks/mod.rs` (`current_stop`, `wait_stop`) dan `tests/support/fixture.rs` (`apply_racy`).

### Melepas `MutexGuard` sebelum `.await`
Guard kunci tidak boleh ditahan melintasi `.await` (risiko deadlock dan future tidak `Send`). Blok `{ ... }` mengembalikan hanya data yang dibutuhkan sehingga guard dilepas sebelum menunggu. Lihat `apply_racy` di `tests/support/fixture.rs`.

### Mengurai nilai JSON dengan aman
`Value::pointer(..).and_then(Value::as_u64)` hanya menerima bilangan bulat non-negatif; `"1"`, `1.5`, `true`, `null`, dan `-1` jatuh ke `counter_invalid`. `u128::div_ceil` membulatkan skew ke atas dalam milidetik, `Option::is_some_and` dan `matches!(x, Ok(r) if ..)` (pola dengan guard) mempersingkat pengecekan. `let ... else` memberi jalan keluar dini yang rapi. Lihat `src/checks/race.rs` (`read_verifier`, `skew_ms`, `evaluate`).

### Visibilitas `pub(crate)`
`current_stop` dan `wait_stop` dipakai bersama check brute_force dan race tetapi tidak perlu terlihat di luar crate. Lihat `src/checks/mod.rs`.

### Enum dengan data per varian di CLI
`Selected` membawa varian check yang sedang dijalankan; `match &prepared.selected` memilih check dan `Option::take_if` memindahkan hasil ke slot yang sesuai saat menyusun report partial. Lihat `src/cli.rs`.

### Alat bantu test
`Cell`/`RefCell` untuk state bersama di test satu thread, `Arc<AtomicUsize>` untuk menghitung koneksi, `TcpStream::peek` untuk membedakan GET dari POST tanpa mengonsumsi byte, `Child::kill`/`wait` untuk membersihkan proses anak sebelum `panic!`, dan `JoinHandle::abort` di `Drop` untuk menutup listener. Test dengan `tokio::time::pause` memberi clock virtual untuk unit test; integration test tidak memakainya. Lihat `src/checks/race.rs` (`mod tests`), `src/cli.rs` (`mod tests`), `tests/check_only_cli.rs`, `tests/race_fixture.rs`.

## Catatan P3 (load check)

### `f64` hanya `PartialEq`, tanpa `Eq`
`f64` punya NaN yang tidak sama dengan dirinya sendiri, jadi `derive(Eq)` tidak bisa dipakai pada struct yang memuat `f64`. Karena `LoadEvidence` memuat `error_rate` dan `throughput_rps`, `Eq` ikut dilepas dari `Evidence`, `CheckResult`, dan `Report`; hanya `PartialEq` yang tersisa. Lihat `src/report.rs` dan `Evaluation` di `src/checks/load.rs`. serde_json menulis NaN/inf sebagai `null` tanpa error, jadi `error_rate` hanya dihitung bila `attempted > 0` (`bool::then`).

### `BTreeMap` dan enum `Ord` sebagai kunci
`status_counts: BTreeMap<u16, u32>` dan `error_counts: BTreeMap<TransportErrorCode, u32>` menghasilkan objek JSON dengan kunci terurut dan stabil (golden test tidak goyah). Kunci enum butuh `derive(PartialOrd, Ord)`. Diisi dengan `entry(k).or_insert(0)`. Lihat `src/report.rs` dan `src/checks/load.rs`.

### Atomik: `AtomicU32` dan `Ordering::Relaxed`
Worker bersama mengambil nomor request berikutnya dengan `fetch_add(1)`; jumlah request yang sedang berjalan naik dengan `fetch_add`, turun dengan `fetch_sub`, dan nilai tertinggi dicatat dengan `fetch_max`. `Ordering::Relaxed` cukup karena yang dibutuhkan hanya hitungan yang benar, bukan urutan antar variabel. Lihat `worker` dan `run_with` di `src/checks/load.rs`.

### Worker pool dalam satu task
`min(concurrency, requests)` future `worker` meminjam state bersama lewat `&` (`Shared<'a>`, struct dengan lifetime) lalu dijalankan `futures_util::future::join_all`. Tanpa `spawn` karena `spawn` butuh `'static` sedangkan `Secret` sengaja tanpa `Clone`. `select! { biased; .. }` memberi stop prioritas atas request yang sedang berjalan.

### Pembulatan dan konversi aman
`u128::div_ceil` membulatkan durasi ke atas (500,0004 ms jadi 501, tidak lolos batas 500), `u64::try_from(..).unwrap_or(u64::MAX)` menjenuhkan alih-alih panic, `Vec::sort_unstable` + rumus nearest-rank `ceil(pct*n/100)` untuk p95. `Option::is_some_and` dan `let ... else` mempersingkat cek dan jalan keluar dini. Lihat `ceil_ms`, `nearest_rank_ms`, `evaluate`.

### Alat bantu test P3
`Box<dyn Fn(u32) -> Step>` sebagai skrip respons; closure pengirim bertipe `Pin<Box<dyn Future + 'a>>`; `std::future::pending()` / `ready(())` sebagai future cancel yang tak pernah/langsung selesai; `std::mem::forget` menjaga sender `watch` tetap hidup; struct update syntax `..facts(&s)`; `std::panic::catch_unwind` membuktikan kontrol positif benar-benar panic; `u64::is_multiple_of` di fixture untuk "setiap request ke-10". Lihat `src/checks/load.rs` (`mod tests`), `tests/load_fixture.rs`, `tests/support/fixture.rs`.

## Catatan P4 (suite lengkap)

### `Vec` enum diiterasi dengan `await` per elemen
`Prepared.selected` kini `Vec<Selected>` (urutan race, load, brute_force). `run_check` melakukan `for selected in ...` di dalam satu blok `async` dan `await` tiap check, jadi check berjalan berurutan tanpa `spawn`. Lihat `src/cli.rs` (`run_check`, loop sekitar baris 278).

### Satu sinyal, banyak pembaca: `watch::Receiver::clone`
Setiap check mendapat `stop_rx.clone()` dari satu channel `watch`. Karena nilainya sudah `Stop` bila cancel/deadline terjadi sebelumnya, check berikutnya langsung melihatnya dan menjadi `not_run` tanpa request. Lihat `src/cli.rs`, `run_check`.

### `Option::take` untuk memindahkan nilai keluar
`check_args` memakai `take()` agar bagian config (race/load/brute_force) dipindahkan keluar dari struct tanpa `clone` dan tanpa membiarkan field setengah kosong; `Option` menjadi `None` setelahnya. Lihat `src/cli.rs`, `check_args`.

### `Vec::swap_remove`
Mengambil elemen berdasarkan posisi dalam O(1) dengan menukarnya dengan elemen terakhir; urutan sisa vektor berubah, jadi cocok hanya bila urutan akhir diatur ulang di tempat lain. Dipakai saat menyusun report di `build_report` (`src/cli.rs`).

### Cek atomic lewat inode: `MetadataExt` dan `#[cfg(unix)]`
`std::os::unix::fs::MetadataExt::ino` memberi nomor inode. Rename menghasilkan inode baru, sedangkan tulis di tempat mempertahankannya, jadi test membuktikan penulisan atomic. `#[cfg(unix)]` membuat kode itu hanya dikompilasi di Unix. Lihat `tests/fixture_proof.rs` (`inode`, test `out_with_old_content_is_replaced_by_valid_json_and_no_temp_file_remains`).
