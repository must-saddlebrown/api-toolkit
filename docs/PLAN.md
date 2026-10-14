# API Toolkit — Small Core Proof

Tanggal: 9 Oktober 2026  
Target: v0.1 CLI dengan tiga non-functional checks  
Status: baseline implementasi yang menggantikan rencana flow builder/test engine sebelumnya.

## 1. Masalah dan tujuan

Dalam pengembangan API, developer sering memprioritaskan fitur. Pengujian stabilitas, reliability, dan keamanan belum menjadi kegiatan rutin; masalah baru terlihat setelah insiden, ketika perbaikannya dapat lebih mahal.

Tujuan awal adalah membuktikan satu core function: satu perintah menjalankan tiga pemeriksaan terkonfigurasi terhadap API dan menghasilkan bukti serta verdict yang dapat diulang.

1. Login abuse/brute-force protection: percobaan login gagal berulang.
2. Race condition: beberapa request bersamaan terhadap aksi yang hanya boleh terjadi sekali.
3. Parallel load: endpoint penting diperiksa pada concurrency dan volume terbatas.

Satu command tidak berarti semua pemeriksaan memakai satu endpoint. Umumnya ada endpoint login, endpoint aksi, endpoint pembacaan state aksi, dan endpoint load. Semuanya dikonfigurasi eksplisit untuk target API yang sama.

Pembuktian pertama memakai fixture lokal dengan versi healthy dan intentionally broken. Setelah engine terbukti membedakan keduanya, pengguna mengonfigurasi API nyata miliknya sebelum production.

## 2. Prioritas dan batas scope

| Masuk v0.1 | Ditunda |
| --- | --- |
| Rust CLI lokal, satu binary | TUI/web UI |
| Satu konfigurasi JSON | OpenAPI import/parser dan endpoint catalog |
| Tiga check dan oracle yang eksplisit | Flow builder, scripts, generic assertion DSL |
| Shared HTTP client, bounded concurrency, timeout/cancel | Distributed load, scheduler, plugin registry |
| Console summary, JSON report, exit code | JUnit, dashboard, history database |
| Fixture healthy/broken dan repeatable setup | CVSS dan automatic vulnerability classification |
| Secret references dari environment variables | AI runtime, provider integration, AI flow generation |

Bahasa Rust, local-first, dan target Linux tetap berlaku. Struktur awal satu package dengan modul kecil; tidak perlu banyak crate. Kandidat dependency: Clap, Tokio, Reqwest, Serde/serde_json. Harness memilih versi compatible, menjalankan spike singkat, dan mengunci dependency.

v0.1 memakai JSON request body atau tanpa body; auth target supplied sebagai bearer/API-key header. Basic auth dapat dipasok sebagai header yang sudah disiapkan. Tidak ada OAuth acquisition, cookie jar, multipart, atau token chaining otomatis. Token diperoleh di luar toolkit dan diberikan lewat secret reference.

Proyek dikembangkan dengan AI coding, tetapi fungsi produk awal berjalan tanpa AI. AI-native fit pada tahap ini adalah membantu membangun, mereview, dan memverifikasi implementasi; integrasi model ke aplikasi menjadi roadmap.

## 3. Command dan alur pengguna

Nama binary sementara: `api-toolkit`.

```bash
api-toolkit validate --config ./core-checks.json
api-toolkit check --config ./core-checks.json --out ./result.json
```

`validate` memeriksa konfigurasi dan keberadaan env references tanpa mengirim request. `check` menjalankan ketiga check (mulai P4; sebelumnya hanya mode `--only`) dan menulis report. Tidak ada wizard, interactive prompt, export bundle, atau command editor pada v0.1.

`check` tanpa `--only` menjalankan suite lengkap (P4): race → load → brute_force, berurutan, satu deadline suite. `--only <check>` tetap ada sebagai run parsial yang hanya menjalankan check itu: `api-toolkit check --config ./brute-force-only.json --out ./result.json --only brute_force`. `validate --only <check>` memvalidasi bagian check itu saja. Nama check yang tidak dikenal ditolak dengan exit 2 dan nol request. Run `--only` adalah run parsial: report ditandai `scope: partial`, check lain dicatat NOT_RUN `excluded_by_only`, dan exit code tidak pernah 0.

Urutan default: race → load → brute-force. Login abuse diletakkan terakhir agar account lockout tidak menghalangi skenario sebelumnya. Auth untuk race/load menggunakan identitas pengujian terpisah dari akun login abuse. Jika shared limiter/state masih mencemari precondition, hasil tidak boleh dipromosikan menjadi PASS.

Check yang FAIL tidak otomatis menghentikan check independen lain. Runtime error satu check tetap dilaporkan dan check lain dicoba bila precondition lokalnya valid. Invalid konfigurasi seluruh suite menghasilkan nol request. Cancel/deadline global menghentikan dispatch baru dan mencatat check yang belum selesai sebagai NOT_RUN.

Sebelum mengulang: reset akun login dan siapkan resource write-once baru atau reset fixture. Repeatable berarti konfigurasi/workload/aturan verdict sama dan state awal memenuhi precondition; tidak berarti request stateful boleh diulang tanpa persiapan.

## 4. Oracle dan status hasil

| Status | Makna |
| --- | --- |
| PASS | Bukti yang diperlukan tersedia dan memenuhi kriteria skenario dalam run ini |
| FAIL | Bukti menunjukkan kriteria skenario tidak terpenuhi |
| INCONCLUSIVE | Precondition/oracle/timing/observasi tidak cukup untuk kesimpulan |
| ERROR | Konfigurasi/runtime/verifier/report failure membuat evaluasi tidak dapat diselesaikan |
| NOT_RUN | Dibatalkan atau deadline habis sebelum check selesai |

PASS/FAIL adalah hasil normal untuk konfigurasi dan target yang siap diuji. Status tambahan mencegah timeout, state yang salah, dan informasi yang hilang menjadi PASS palsu. PASS tidak menyatakan seluruh API aman, bebas race, atau memenuhi SLA di semua beban.

Batas ERROR dan INCONCLUSIVE (keputusan 2026-10-14): INCONCLUSIVE berarti prasyarat oracle belum terpenuhi sebelum ada efek apa pun di target (verifier belum bisa dibaca, counter belum ada, state awal salah, akun sudah terkunci) atau bukti tidak cukup. ERROR berarti pengamatan itu sendiri rusak (transport gagal, timeout, body bukan JSON, tipe nilai melanggar kontrak) atau runner/konfigurasi salah. Tabel per check: race di §6; brute_force di §5; load di §7.

Matcher awal sengaja kecil: `status_in` dan opsional `json_equals`/`json_exists`. Field matcher digabung AND, bukan expression language. JSON selector memakai JSON Pointer. Matcher wajib memiliki status criterion; overlapping success/rejection/protection rules ditolak atau, jika overlap baru diketahui runtime, menghasilkan ERROR.

## 5. Check 1 — Login brute-force protection

### Tujuan dan input

Mengamati apakah API menahan percobaan login gagal berulang sesuai kebijakan yang dikonfigurasi. v0.1 tidak mencoba dictionary password, credential stuffing, distributed attack, atau CAPTCHA bypass.

Input: satu akun tes, baseline request dengan credential benar, probe request dengan credential yang diketahui salah, success/rejection/protection matchers, jumlah attempt, interval, dan batas attempt ketika proteksi seharusnya mulai terlihat.

Credential baseline dan probe harus merujuk akun yang sama. Ini precondition pengguna; toolkit tidak menebak nama field username/password. Akun harus siap digunakan dan tidak terkunci sebelum run.

### Algoritma

1. Kirim baseline credential benar sekali. Harus cocok success matcher; nilai token tidak dipersist.
2. Kirim probe invalid pertama. Harus ditolak normal, bukan sudah protected; attempt ini dihitung sebagai attempt 1.
3. Lanjutkan probe yang sama secara sequential sampai jumlah attempt yang ditetapkan, dengan interval tetap dan tanpa retry.
4. Catat outcome setiap attempt: success, normal rejection, protection, unexpected, atau transport error.
5. Tentukan attempt pertama yang protected dan apakah protection tetap terlihat pada minimal `protected_tail_count` attempt terakhir yang berurutan (config, 1..=attempts-1; contoh config memakai 2).

### Verdict

PASS: baseline sukses, probe pertama rejected, tidak ada invalid-credential success, proteksi mulai paling lambat `protect_by_attempt`, dan `protected_tail_count` attempt terakhir protected. Semua request yang diperlukan harus menghasilkan outcome yang dapat diklasifikasi.

FAIL: invalid credential diterima; atau seluruh probe dapat diklasifikasi tetapi proteksi tidak memenuhi deadline/persistensi policy. Reason membedakan `invalid_credentials_accepted` dan `protection_policy_not_met`.

INCONCLUSIVE: baseline menunjukkan akun sudah locked/rate-limited atau tidak memenuhi state awal. ERROR: baseline gagal karena respons tak sesuai/transport, probe tidak dapat diklasifikasi, atau oracle malformed. Bukti acceptance invalid credential tetap dilaporkan sebagai FAIL walaupun ada error lain; report menyimpan error tambahan.

### Mode burst (keputusan 2026-10-14, fase Q3)

`brute_force.mode` = `"sequential"` (default; algoritma di atas tidak berubah) atau `"burst"`. Catatan: FAIL `protection_policy_not_met` pada sequential berarti "kebijakan terkonfigurasi tidak terlihat pada pacing ini", bukan "API tanpa proteksi" — limiter berbasis waktu bisa tidak terjangkau karena `interval_ms` hanya menjeda setelah probe selesai, sehingga jarak mulai antar probe jauh lebih besar dari `interval_ms` saat latency tinggi (lihat `docs/findings.md` F-002).

Burst: baseline dikirim sekali (aturan baseline sama dengan sequential), lalu N = `attempts` probe dikirim serentak (satu batch `join_all`, tanpa retry, tanpa jeda tetap). `window_ms` (field baru, wajib dan > 0 di mode burst, ditolak di sequential) adalah anggaran maksimum selisih waktu dispatch client antara probe pertama dan terakhir, dibulatkan ke atas ke ms; ini anggaran dispatch client, bukan durasi limiter server. `attempts` di mode burst dibatasi 2..=32. Field sequential `interval_ms`, `protect_by_attempt`, `protected_tail_count` DITOLAK di mode burst (tidak ada nilai yang diabaikan diam-diam); config lama tanpa `mode` tetap sequential dan perilakunya tidak berubah.

Verdict burst (P = jumlah probe yang terklasifikasi protected, N = jumlah probe; dihitung bukan diurutkan), presedensi dari atas ke bawah:
1. Stop (cancel/deadline) sebelum baseline selesai → NOT_RUN.
2. Baseline tidak sukses → hasil sama seperti sequential (INCONCLUSIVE/ERROR sesuai state baseline), nol probe dikirim.
3. Ada probe dengan invalid credential diterima sebagai success → FAIL `invalid_credentials_accepted`.
4. Jumlah probe yang tercatat != N (dibatalkan/deadline di tengah burst) → NOT_RUN.
5. Ada probe dengan outcome tak terklasifikasi (transport error, status tak dikenal) → ERROR `probe_unclassifiable`.
6. Spread dispatch kosong (tidak terukur) → INCONCLUSIVE `dispatch_incomplete`.
7. Spread dispatch > `window_ms` → INCONCLUSIVE `dispatch_skew_exceeded` (berlaku juga bila ada probe protected; window yang gagal tercapai tidak membuktikan apa pun).
8. `P == N` (semua probe protected) → INCONCLUSIVE `account_already_locked`.
9. `P == 0` (tidak ada probe protected) → FAIL `protection_policy_not_met`.
10. `1 <= P <= N-1` → PASS `protection_observed`.

FAIL burst dengan `P == 0` bukan bukti "API tanpa proteksi" — limiter dengan ambang di atas N tidak akan terpicu pada burst ini. Burst dapat mengunci akun uji sekaligus (sampai `attempts` login gagal dalam satu batch); pengguna harus menyiapkan akun yang boleh terkunci, sama seperti mode sequential.

Contoh policy sequential: 8 probe, interval 100 ms, proteksi harus mulai pada attempt 6 atau sebelumnya, last 2 protected. Ini parameter demonstrasi, bukan standar keamanan universal.

Normal 401 saja tidak membuktikan anti-brute-force. HTTP 429 atau error body tertentu hanya dianggap proteksi sesuai matcher eksplisit. Metode ini memeriksa respons proteksi pada pola terbatas, belum membuktikan lockout enforcement dengan seluruh mekanisme atau kekuatan password. Basis pengujian login gagal/lockout: [OWASP Weak Lock Out Mechanism](https://wstg.owasp.org/latest/4-Web_Application_Security_Testing/04-Authentication/03-Weak_Lock_Out_Mechanism/).

## 6. Check 2 — Race condition pada write-once action

### Tujuan dan oracle

Menguji bahwa N request untuk resource/action yang sama menghasilkan tepat satu side effect. Contoh fixture: redeem token sekali, dengan counter `execution_count` pada endpoint verifier.

HTTP status tidak cukup. Beberapa API mengembalikan 200 untuk semua idempotent retries padahal hanya satu side effect. Sebaliknya, satu success response belum menjamin state tidak terduplikasi. Oracle v0.1 adalah counter integer per resource yang benar-benar merepresentasikan committed side effect, bukan hanya status boolean `processed=true`.

Input: action request, N concurrent requests, verifier request, JSON Pointer counter, expected initial value 0, expected delta 1, expected action response statuses, dispatch-skew budget, dan verification settling delay.

### Algoritma

1. Baca verifier before; response dan counter harus valid, counter harus 0.
2. Siapkan N task dengan action/resource/body/idempotency key yang sama; tahan pada barrier, lalu release bersama.
3. Kumpulkan outcome tanpa retry. Record waktu client dispatch, concurrency, dan skew; ini tidak menjamin kedatangan simultan di server.
4. Tunggu settling delay yang dikonfigurasi, lalu baca verifier after sekali.
5. Hitung `delta=after-before`, dan evaluasi precondition, bukti side effect, workload completion, serta kualitas dispatch.

### Verdict

PASS: baseline 0, semua N action attempts selesai dengan response statuses yang diizinkan, skew dalam budget, verifier after valid, dan delta tepat 1.

FAIL: valid evidence menunjukkan delta >1 (`duplicate_side_effect`); atau fully completed workload dengan delta 0 (`action_not_executed`). Delta 0 adalah kegagalan tujuan aksi, bukan klaim race vulnerability.

INCONCLUSIVE: initial counter bukan 0, verifier before tidak dapat dibaca (`baseline_unreadable`), counter tidak tersedia (`counter_not_available`), jumlah dispatch tidak lengkap, skew terlalu besar, state belum dapat dinilai konsisten, atau action outcome ambigu. ERROR: transport error, body bukan JSON, atau counter ada tetapi bukan integer >= 0; serta semua kegagalan verifier after.

Batas verifier before (urutan evaluasi: transport, `verify_match`, body, pointer, tipe nilai; nol action terkirim pada V1–V5):

| # | Verifier before | Verdict | Reasons |
| --- | --- | --- | --- |
| V1 | Transport error (timeout, connect, body terlalu besar, dst.) | ERROR | `verifier_failed`, `transport_error` |
| V2 | Respons ada tetapi tidak cocok `verify_match` (401, 404, 5xx, atau `json_*` gagal) | INCONCLUSIVE | `baseline_unreadable` |
| V3 | Cocok `verify_match`, body bukan JSON atau kosong | ERROR | `counter_invalid` |
| V4 | Cocok, body JSON, `counter_pointer` tidak ada (absent) | INCONCLUSIVE | `counter_not_available` |
| V5 | Cocok, pointer ada, nilai bukan integer >= 0 (string, float, bool, `null`, negatif) | ERROR | `counter_invalid` |
| V6 | Counter integer bukan 0 | INCONCLUSIVE | `initial_counter_not_zero` |
| V7 | Counter 0 | lanjut ke action | |

V2 mendahului V3 (401 berbadan HTML = INCONCLUSIVE). Pointer yang menunjuk `null` adalah V5, bukan V4. Verifier after tidak berubah (action sudah terkirim): transport atau mismatch → ERROR `verifier_failed`; pointer absent atau bukan integer → ERROR `counter_invalid`. Tidak ada jalur baru menuju PASS.

Delta >1 tetap definite FAIL meskipun skew/action error tambahan terjadi, karena duplikasi telah terlihat; evidence tambahan tetap dicatat.

Verifier harus membaca state resource/action yang sama, bebas perubahan pihak lain selama tes. Counter umum yang mencampur aktivitas pengguna lain tidak memenuhi oracle. Async/eventual consistency membutuhkan settling delay yang cukup dan batasan yang dicatat; tidak ada polling/DB connector pada v0.1.

Scope oracle awal adalah aksi sinkron: setelah action responses selesai, semua side effect dapat dibaca pada verifier authoritative atau dalam bound konsistensi yang diketahui. Respons 202/pending dan side effect async tanpa bukti completion menghasilkan INCONCLUSIVE, bukan delta1 PASS sementara pekerjaan lain masih pending. `settle_ms` hanya membantu read visibility yang memiliki bound jelas, bukan bukti selesai untuk job async. Initial counter 0 dan expected delta 1 menjadi kontrak write-once v0.1.

Default demonstrasi: N=5, client dispatch skew ≤100 ms, settle 250 ms. Ulangi dengan resource baru; satu burst tidak menjamin semua race ditemukan. Rujukan pola concurrent write dan observasi side effect: [OWASP Payment Functionality — Race Conditions](https://wstg.owasp.org/latest/4-Web_Application_Security_Testing/10-Business_Logic/10-Payment_Functionality/).

## 7. Check 3 — Parallel load pada endpoint penting

### Tujuan dan workload

Menjalankan sejumlah request dengan concurrency tetap, lalu menilai success/error rate dan latency terhadap threshold. Ini bounded concurrency smoke check, belum stress test, capacity test, atau SLO certification.

Input: request, total request count, concurrency, response matcher, max error rate, dan max p95 duration. Contoh awal: 100 request, concurrency 10, error rate ≤1%, p95 ≤500 ms. Threshold harus disesuaikan dengan target; nilai contoh bukan rekomendasi SLA.

### Algoritma dan metrik

1. Kirim preflight request sekali; harus sesuai response matcher. Tidak masuk denominator load.
2. Jalankan closed-loop worker pool sampai N request selesai; setiap worker mengirim request berikut setelah sebelumnya selesai. Tidak ada retry, rate ramp, atau infinite loop.
3. Record setiap outcome dan elapsed duration dari sebelum send sampai body selesai/error, memakai monotonic clock. Client duration mencakup connection setup dan body read.
4. Hitung success/failed/timeout counts, error rate, p50/p95/max, dan observed completed throughput.

`error_rate = failed_outcomes / total_attempted`. Failed mencakup matcher mismatch, status unexpected, transport failure, dan timeout. p95 dihitung nearest-rank pada durasi semua attempted request yang outcome-nya selesai, termasuk timeout/error; N scheduled harus seluruhnya dicoba sebelum PASS.

PASS: semua N attempts selesai, error rate ≤threshold, dan p95 ≤threshold. FAIL: workload lengkap tetapi salah satu threshold terlampaui. Network failures yang terjadi saat load masuk failed outcomes, bukan dikeluarkan dari statistik.

NOT_RUN: run dihentikan (cancel) atau deadline habis sebelum N attempts lengkap (reason stop + `workload_incomplete`). INCONCLUSIVE: runner tidak mampu menyelesaikan workload tanpa stop (`workload_incomplete`). ERROR: preflight transport/oracle failure atau internal runner error. Preflight mismatch menghasilkan INCONCLUSIVE `target_not_ready`; load tidak dilanjutkan.

Console/report menampilkan attempted vs planned, concurrency aktual maksimum, jumlah samples, histogram status, timeout, dan thresholds. Closed-loop workload serta client bottleneck membatasi generalisasi hasil. Konsep metric thresholds untuk verdict: [Grafana k6 Thresholds](https://grafana.com/docs/k6/latest/using-k6/thresholds/). Toolkit ini tidak menggunakan k6 sebagai runtime dependency.

## 8. Shared runner dan batas eksekusi

HTTP requests explicit: method, relative path, optional path params/query/headers, optional JSON body. Base URL konkret; tidak ada endpoint discovery. Path params di-encode sebagai nilai; base path dipertahankan. Auth header dapat diisi dari env references.

Konfigurasi memakai satu target origin. Semua path action/verifier/load/login relatif terhadap origin/base path itu. Redirect tidak diikuti; TLS verification aktif; tidak ada automatic retry. Secret environment references tidak menjadi argumen CLI.

Default: connect timeout 3 detik, total request timeout 5 detik, body limit 1 MiB, suite deadline 120 detik. Semua dapat dikonfigurasi. Hitung jumlah request dan fixed waits check yang dijalankan (ketiganya, atau check terpilih pada `--only`) sebelum run; konfigurasi yang fixed waits-nya saja melampaui deadline ditolak. Estimasi worst-case berbasis timeout dilaporkan sebagai budget, bukan jaminan API merespons tepat waktu.

v0.1 menerima probe count ≤100, race concurrency ≤32, load count ≤1000, load concurrency ≤32, brute_force burst `attempts` 2..=32, dan seluruh angka harus positif dalam batas tersebut. Ini batas implementasi pertama, bukan kapasitas API. Extension limits dilakukan sengaja melalui revisi versi.

Task pool bounded; race barrier berbeda dari load worker pool. Cancel menghentikan dispatch baru dan active waits, lalu menyimpan hasil parsial. Server side effect yang sudah terjadi tetap mungkin selesai. Tidak ada generic cleanup flow: fixture reset/resource preparation dilakukan di luar runner.

## 9. Konfigurasi minimum

Contoh untuk fixture lokal; semua paths/field names adalah contoh, bukan tebakan API pengguna. JSON below adalah bentuk kontrak v1 yang dikonkretkan menjadi schema Rust pada P0.

```json
{
  "config_version": 1,
  "base_url": "http://127.0.0.1:8080",
  "limits": {
    "request_timeout_ms": 5000,
    "suite_deadline_ms": 120000,
    "response_body_limit_bytes": 1048576
  },
  "brute_force": {
    "baseline_request": {
      "method": "POST", "path": "/login",
      "json": {"username": "${ENV:LOGIN_USER}", "password": "${ENV:LOGIN_VALID_PASSWORD}"}
    },
    "probe_request": {
      "method": "POST", "path": "/login",
      "json": {"username": "${ENV:LOGIN_USER}", "password": "${ENV:LOGIN_INVALID_PASSWORD}"}
    },
    "success_match": {"status_in": [200], "json_exists": ["/access_token"]},
    "rejection_match": {"status_in": [401]},
    "protection_match": {"status_in": [429]},
    "attempts": 8,
    "interval_ms": 100,
    "protect_by_attempt": 6,
    "protected_tail_count": 2
  },
  "race": {
    "action_request": {
      "method": "POST", "path": "/actions/{id}/apply",
      "path_params": {"id": "${ENV:TEST_ACTION_ID}"},
      "headers": {"Authorization": "${ENV:TEST_AUTHORIZATION}"}
    },
    "verify_request": {
      "method": "GET", "path": "/actions/{id}",
      "path_params": {"id": "${ENV:TEST_ACTION_ID}"},
      "headers": {"Authorization": "${ENV:TEST_AUTHORIZATION}"}
    },
    "verify_match": {"status_in": [200]},
    "counter_pointer": "/execution_count",
    "initial_counter": 0,
    "expected_delta": 1,
    "concurrency": 5,
    "expected_action_statuses": [200, 409],
    "max_dispatch_skew_ms": 100,
    "settle_ms": 250
  },
  "load": {
    "request": {
      "method": "GET", "path": "/critical",
      "headers": {"Authorization": "${ENV:TEST_AUTHORIZATION}"}
    },
    "response_match": {"status_in": [200]},
    "requests": 100,
    "concurrency": 10,
    "max_error_rate": 0.01,
    "max_p95_ms": 500
  }
}
```

Mode default `brute_force.mode` adalah `sequential` (contoh di atas tidak memuat field itu secara eksplisit tetapi berlaku sebagai sequential). Mode burst (keputusan 2026-10-14, fase Q3) memakai bentuk berikut, tanpa `interval_ms`/`protect_by_attempt`/`protected_tail_count`:

```json
  "brute_force": {
    "mode": "burst",
    "baseline_request": { "...": "sama seperti contoh sequential" },
    "probe_request": { "...": "sama seperti contoh sequential" },
    "success_match": { "status_in": [200], "json_exists": ["/access_token"] },
    "rejection_match": { "status_in": [401] },
    "protection_match": { "status_in": [429] },
    "attempts": 8,
    "window_ms": 1000
  }
```

`TEST_AUTHORIZATION` berisi header lengkap, misalnya `Bearer <token>`. Substitusi hanya whole-string `${ENV:NAME}`, tanpa string interpolation/expression. Env values menjadi strings; angka/boolean tetap literal JSON. Missing env, unknown fields, invalid selector/threshold/version memblokir run sebelum dispatch.

Suite lengkap (`check` tanpa `--only`, `validate` tanpa `--only`) mewajibkan ketiga bagian check. Mode `--only <check>` hanya mewajibkan `config_version`, `base_url`, `limits` (opsional), dan bagian check terpilih. Bagian lain yang ada tetap harus berbentuk benar (field tak dikenal dan tipe salah ditolak) tetapi tidak divalidasi semantik, tidak di-resolve (env-nya tidak wajib), dan tidak dihitung dalam budget. Tidak ada skip flag yang membuat command tampil sukses: run parsial tidak pernah exit 0 dan report-nya ditandai `scope: partial`. Template fixture dapat diganti method/path/body/matchers untuk target pengguna; resource state dan policy target tetap eksplisit.

## 10. Reporting dan exit code

Console menampilkan satu baris per check: verdict, evidence utama, threshold, dan reason. Contoh healthy fixture: race PASS delta=1; load PASS error_rate=0/p95=120ms; brute_force PASS first_protected=6 tail=3/2 attempts=8/8 (tail = jumlah protected berurutan yang teramati / `protected_tail_count`; attempts = probe yang selesai dan tercatat / direncanakan). Angka latency merupakan ilustrasi.

Contoh console mode burst (keputusan 2026-10-14, fase Q3): `brute_force PASS mode=burst protected=3/8 rejected=5 window=1/100ms attempts=8/8 reasons=protection_observed` (window = spread dispatch client teramati / anggaran `window_ms`).

JSON report memuat report_version=1, run ID, tool version, config hash nonsecret, timestamps UTC, configured workloads/thresholds, counts/timings/status distribution, before/after counter, per-check verdict/reasons, dan completion status. Request body/header values, raw response bodies, token/password, serta resolved secret path/query tidak dipersist.

Report memuat `scope`: `full` untuk suite lengkap, `partial` untuk run `--only`. Pada run parsial, check yang tidak dipilih tercatat `not_run` dengan reason `excluded_by_only` dan completion `incomplete`; urutan entri mengikuti urutan default §3. Console run parsial mencetak satu baris untuk check terpilih.

Per-attempt evidence cukup: attempt number, HTTP status/outcome category, timing, matched rule, sanitized error code. Jangan memuat raw matcher expected values yang sensitif atau error dari library yang mengandung URL credential. Atomic report write; incomplete/cancel bukan complete PASS.

| Exit code | Meaning |
| --- | --- |
| 0 | Ketiga check selesai dan PASS |
| 1 | Minimal satu FAIL; tidak ada ERROR/INCONCLUSIVE/NOT_RUN |
| 2 | Config/runtime/report error, INCONCLUSIVE, atau incomplete suite |
| 130 | Cancel pengguna |

Run parsial (`--only`): FAIL selesai → 1; PASS dan hasil lain → 2; tidak pernah 0 (PASS parsial dibaca dari console dan report). Entri `excluded_by_only` diabaikan dalam presedensi; cancel tetap 130 dan incomplete tetap mendahului fail.

Precedence suite: cancel → error/inconclusive/incomplete → fail → pass. Definite FAIL pada satu check tetap terlihat walaupun suite exit 2 akibat check lain. Report-save failure tidak pernah memberi exit 0.

## 11. Fase implementasi kecil

### P0 — Kontrak CLI, runner minimum, dan fixture

Pekerjaan: satu Rust package; config/schema; validate command; shared HTTP transport; matcher kecil; report model; local fixture yang menyiapkan akun/action/read endpoint. Tidak membangun generic test engine/flow DSL.

Acceptance criteria: config contoh parse; env missing/unknown field ditolak tanpa request; fixture start/reset terdokumentasi; satu request transport dapat diuji; JSON report dan reason codes stabil. Fixture reset adalah alat pengembangan, bukan fitur produk.

### P1 — Login protection check

Pekerjaan: baseline/probe, sequential attempts, outcome classification, deadline/tail policy.

Acceptance criteria: fixture proteksi pada attempt 6 PASS; fixture tanpa proteksi FAIL; invalid credential accepted FAIL; prelocked account INCONCLUSIVE; network/oracle error tidak PASS; attempts/interval tepat dan tanpa retry. Secret sentinel tidak muncul di console/report.

### P2 — Write-once race check

Pekerjaan: barrier dispatch, counter before/after, result/skew evidence.

Acceptance criteria: healthy atomic fixture delta1 PASS; intentionally broken fixture delta>1 FAIL; idempotent fixture yang memberi 200 ke semua request tetapi delta1 tetap PASS; used resource INCONCLUSIVE; missing/noninteger counter ERROR; excessive skew INCONCLUSIVE. Duplicate fixture dibuat deterministik melalui synchronization test harness, bukan mengandalkan bug timing acak untuk lulus test.

### P3 — Parallel load check

Pekerjaan: worker pool, full outcome accounting, latency/error metrics, thresholds.

Acceptance criteria: healthy fixture PASS; controlled slow/error fixture FAIL; timeout dihitung failed; p95 nearest-rank benar pada array durasi known; active requests tidak melewati configured concurrency; early deadline tidak PASS. Latency verdict tests memakai data/clock terkontrol, bukan performance laptop yang berubah-ubah.

### P4 — Single-command proof dan satu API nyata

Pekerjaan: perluas command `check --only` yang sudah ada (DECISIONS 2026-10-10) menjadi suite lengkap sebagai default; jangan membuat ulang parser, report, atau jalur cancel/deadline. Orchestrator menjalankan ketiganya, partial report/cancel, console summary, exit code, README satu command, sample config. Selesaikan fixture proof sebelum integrasi target nyata.

Acceptance criteria:

1. Satu `check` menjalankan ketiga check; setiap healthy/broken fixture menghasilkan expected verdict.
2. FAIL/error satu check tidak menyembunyikan hasil check independen lainnya.
3. Complete healthy suite exit0; fail suite exit1; inconclusive/error suite exit2; cancel exit130.
4. Reset fixture + command yang sama menghasilkan verdict sama; stateful repeat tanpa reset tidak menghasilkan false PASS.
5. Report file readable, atomic, sanitized; save failure bukan exit0.
6. README menjelaskan setup state/account, secret env, thresholds, command, dan arti hasil.
7. Setelah pengguna memberikan target/config, satu run API nyata disertai report dan baseline yang sah. Tanpa target, tandai fixture proof selesai dan validasi target nyata belum dilakukan.

P0–P4 adalah tahap pengembangan, bukan roadmap fitur tambahan. Harness dapat mengerjakan berurutan tanpa meminta keputusan framework setiap fase. Checkpoint pengguna utama adalah command end-to-end setelah P4 fixture proof.

## 12. Definition of Done dan keberhasilan

Core proof selesai ketika ketiga check dapat dijalankan satu command, memiliki positive/negative fixture evidence, memakai oracle/threshold eksplisit, menghasilkan console/JSON/exit code, dan dapat diulang dari state awal yang terdokumentasi.

Ada dua level evidence: (1) correctness tool terhadap fixture healthy/broken; (2) usefulness terhadap API nyata pengguna. PASS target bukan satu-satunya sukses produk: menemukan kegagalan yang dapat direproduksi juga menunjukkan alat berguna.

Rust decisions dijelaskan singkat per tahap agar pengguna belajar dari implementation: async workers/barrier, ownership config, enum verdict, dan reason codes. Implementasi belum dianggap selesai hanya karena compile.

## 13. Arsitektur minimum dan roadmap

Modul awal: `config`, `http`, `matchers`, `checks/{brute_force,race,load}`, `report`, `cli`. Shared code hanya transport, limits, matcher, timing, dan reporting. Tidak perlu interface plugin atau database history.

| Urutan berikutnya | Fitur | Fondasi yang digunakan |
| --- | --- | --- |
| 1 | Perbaikan oracle; polling verifier; auth/media sesuai API nyata | Config, HTTP, matcher, per-check evidence |
| 2 | OpenAPI import untuk membantu konfigurasi request | Request model; parser tidak menentukan policy/invariant bisnis |
| 3 | Flow testing dan builder | Request/response model, auth, results; dibuat saat use case core terbukti |
| 4 | TUI operasional ringan | Config editor/run/result viewer di atas CLI core |
| 5 | Security findings dan CVSS | Evidence + rule confidence/review; scoring per kerentanan |
| 6 | AI suggestions dan mock server jika dibutuhkan | Stable schema/config; AI di luar execution correctness |

Ini mengganti urutan lama yang mendahulukan flow builder. OpenAPI 3.1.x tetap kandidat adapter pertama nanti, tetapi parser bukan prasyarat core proof. CVSS tidak dihitung dari status HTTP atau latency secara otomatis. API-specific business invariants tetap harus datang dari pengguna/kontrak aplikasi.

## 14. Instruksi harness dan input target nyata

Gunakan dokumen ini sebagai rencana aktif. Rencana flow builder lama adalah historis; jangan menjalankan fase F0–F6 lama atau membawa scope-nya sebagai requirement v0.1.

Mulai dari repository kosong atau inspect code existing jika pengguna sudah menjalankan rencana lama. Reuse code hanya jika membantu core proof; jangan membangun ulang seluruh arsitektur produk. Jangan menghapus implementasi existing tanpa melihat kebutuhan dan perubahan yang diperlukan.

Teknis rutin (dependency compatible, schema defaults, unit/integration fixture, module layout) diputuskan harness sesuai scope. Jangan menambah UI, parser, flow engine, scoring, AI calls, atau publikasi sebagai syarat selesai. Catat implementation decisions, validation evidence, limitations, dan command untuk mencoba.

Untuk API nyata, pengguna cukup menyediakan konfigurasi: base URL; login baseline/probe dan protection policy; aksi/resource baru beserta verifier counter; endpoint load dan thresholds; serta secret env. Informasi ini dapat disiapkan saat fixture core sudah berjalan. Harness tidak boleh mengarang endpoint, credential, business counter, atau menyatakan validasi target selesai tanpa evidence.

Rujukan mendasari pola pengujian, bukan bukti implementasi toolkit atau jaminan keamanan API: OWASP WSTG login lockout, OWASP business-logic race conditions, dan Grafana k6 thresholds sebagaimana ditautkan pada bagian check.
