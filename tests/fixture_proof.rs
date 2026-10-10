//! Fixture proof (P4 S4): the real binary runs the full suite against the local fixture
//! (random port, 127.0.0.1 only). Healthy configs must exit 0, broken ones must fail.
//! Run it with `cargo test --test fixture_proof`. Env goes through `Command::env` only.
mod support;
use serde_json::{Value, json};
use std::io::{Read, Write};
use std::net::SocketAddr;
use std::path::Path;
use std::process::Command;
use support::fixture::{Fixture, RunningFixture};

const HEALTHY: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/examples/core-checks.json");
const BROKEN: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/examples/core-checks-broken.json"
);

const ENV: [(&str, &str); 5] = [
    ("LOGIN_USER", support::fixture::LOGIN_USER),
    (
        "LOGIN_VALID_PASSWORD",
        support::fixture::LOGIN_VALID_PASSWORD,
    ),
    (
        "LOGIN_INVALID_PASSWORD",
        support::fixture::LOGIN_INVALID_PASSWORD,
    ),
    ("TEST_ACTION_ID", support::fixture::TEST_ACTION_ID),
    ("TEST_AUTHORIZATION", support::fixture::TEST_AUTHORIZATION),
];

const UNPROTECTED: &str = "/broken/login/unprotected";
const ACCEPTS_INVALID: &str = "/broken/login/accepts-invalid";
const RACY: &str = "/broken/actions/{id}/apply-racy";
const FLAKY: &str = "/broken/critical/flaky";
const SLOW: &str = "/broken/critical/slow";

struct Out {
    code: i32,
    stdout: String,
    stderr: String,
    report: Value,
}

impl Out {
    fn lines(&self) -> Vec<&str> {
        self.stdout.lines().collect()
    }
    fn check(&self, i: usize) -> &Value {
        &self.report["checks"][i]
    }
    /// (check, verdict, reasons) for the three entries.
    fn summary(&self) -> Vec<(String, String, Value)> {
        (0..3)
            .map(|i| {
                let c = self.check(i);
                (
                    c["check"].as_str().unwrap().to_string(),
                    c["verdict"].as_str().unwrap().to_string(),
                    c["reasons"].clone(),
                )
            })
            .collect()
    }
}

fn config(example: &str, base: &str, mutate: impl FnOnce(&mut Value)) -> Value {
    let mut v: Value = serde_json::from_str(&std::fs::read_to_string(example).unwrap()).unwrap();
    v["base_url"] = json!(base);
    mutate(&mut v);
    v
}

fn set_login(v: &mut Value, path: &str) {
    v["brute_force"]["baseline_request"]["path"] = json!(path);
    v["brute_force"]["probe_request"]["path"] = json!(path);
}

/// Writes the config to a temp dir and runs `check` (full suite) with the binary.
async fn run_suite(cfg: &Value) -> Out {
    run_suite_env(cfg, &ENV).await
}

async fn run_suite_env(cfg: &Value, env: &[(&str, &str)]) -> Out {
    let cfg = cfg.clone();
    let env: Vec<(String, String)> = env
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    tokio::task::spawn_blocking(move || {
        let dir = tempfile::tempdir().unwrap();
        let cfg_path = dir.path().join("c.json");
        let out_path = dir.path().join("result.json");
        std::fs::write(&cfg_path, serde_json::to_string_pretty(&cfg).unwrap()).unwrap();
        let o = Command::new(env!("CARGO_BIN_EXE_api-toolkit"))
            .arg("check")
            .arg("--config")
            .arg(&cfg_path)
            .arg("--out")
            .arg(&out_path)
            .env_clear()
            .envs(env)
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&o.stderr).into_owned();
        let report_text = std::fs::read_to_string(&out_path)
            .unwrap_or_else(|e| panic!("no report ({e}); stderr: {stderr}"));
        Out {
            code: o.status.code().expect("exit by signal"),
            stdout: String::from_utf8_lossy(&o.stdout).into_owned(),
            stderr,
            report: serde_json::from_str(&report_text).unwrap(),
        }
    })
    .await
    .unwrap()
}

fn http_blocking(addr: SocketAddr, method: &str, path: &str) -> String {
    let mut s = std::net::TcpStream::connect(addr).unwrap();
    s.set_read_timeout(Some(std::time::Duration::from_secs(10)))
        .unwrap();
    let req = format!(
        "{method} {path} HTTP/1.1\r\nHost: x\r\nConnection: close\r\nContent-Length: 0\r\n\r\n"
    );
    s.write_all(req.as_bytes()).unwrap();
    let mut resp = String::new();
    s.read_to_string(&mut resp).unwrap();
    resp
}

async fn admin(fx: &RunningFixture, method: &'static str, path: &'static str) -> String {
    let addr = fx.addr;
    tokio::task::spawn_blocking(move || http_blocking(addr, method, path))
        .await
        .unwrap()
}

async fn stat(fx: &RunningFixture, key: &str) -> u64 {
    let resp = admin(fx, "GET", "/__admin/stats").await;
    let body = resp.split("\r\n\r\n").nth(1).expect("http body");
    serde_json::from_str::<Value>(body.trim()).unwrap()[key]
        .as_u64()
        .unwrap()
}

fn assert_full_shape(o: &Out) {
    assert_eq!(o.report["report_version"], 1);
    assert_eq!(o.report["scope"], "full");
    let names: Vec<&str> = o.report["checks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["check"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["race", "load", "brute_force"]);
    assert!(!o.report.to_string().contains("excluded_by_only"));
}

/// stdout is exactly three lines: race, load, brute_force with the given verdict words.
fn assert_lines(o: &Out, verdicts: [&str; 3]) {
    let lines = o.lines();
    assert_eq!(lines.len(), 3, "stdout: {}", o.stdout);
    for (line, (name, v)) in lines
        .iter()
        .zip(["race", "load", "brute_force"].iter().zip(verdicts))
    {
        assert!(
            line.starts_with(&format!("{name} {v}")),
            "line `{line}` should start with `{name} {v}`"
        );
        let tokens: &[&str] = match *name {
            "race" => &[" delta=", " skew="],
            "load" => &[" error_rate=", " max_in_flight="],
            _ => &[" first_protected="],
        };
        for t in tokens {
            assert!(line.contains(t), "line `{line}` should contain `{t}`");
        }
    }
    assert!(!o.stderr.contains("partial run"), "stderr: {}", o.stderr);
}

fn assert_entry(o: &Out, i: usize, verdict: &str, reasons: Value, completion: &str) {
    let c = o.check(i);
    assert_eq!(c["verdict"], verdict, "check {i}, report: {}", o.report);
    assert_eq!(c["reasons"], reasons, "check {i}");
    assert_eq!(c["completion"], completion, "check {i}");
}

fn assert_has_evidence(o: &Out, i: usize) {
    assert!(
        o.check(i).get("evidence").is_some(),
        "check {i}: no evidence"
    );
}

fn assert_healthy(o: &Out) {
    assert_eq!(o.code, 0, "stdout: {} stderr: {}", o.stdout, o.stderr);
    assert_full_shape(o);
    assert_lines(o, ["PASS", "PASS", "PASS"]);
    assert_entry(o, 0, "pass", json!(["single_side_effect"]), "complete");
    assert_entry(o, 1, "pass", json!(["thresholds_met"]), "complete");
    assert_entry(o, 2, "pass", json!(["protection_observed"]), "complete");
    for i in 0..3 {
        assert_has_evidence(o, i);
    }
    assert_eq!(o.report["completion"], "complete");
}

// AC1.3, AC1.5, AC1.6, AC3.1, AC9
#[tokio::test(flavor = "multi_thread")]
async fn healthy_suite_is_exit_0_three_pass_in_order() {
    let fx = Fixture::start(0).await.unwrap();
    let o = run_suite(&config(HEALTHY, &fx.base_url(), |_| {})).await;
    assert_healthy(&o);
    let l = o.lines();
    assert!(l[0].ends_with("reasons=single_side_effect"), "{}", l[0]);
    assert!(l[1].ends_with("reasons=thresholds_met"), "{}", l[1]);
    assert!(l[2].ends_with("reasons=protection_observed"), "{}", l[2]);
    fx.shutdown().await;
}

// AC1.4, AC3.2
#[tokio::test(flavor = "multi_thread")]
async fn broken_example_suite_is_exit_1_three_fail() {
    let fx = Fixture::start(0).await.unwrap();
    let o = run_suite(&config(BROKEN, &fx.base_url(), |_| {})).await;
    assert_eq!(o.code, 1, "stdout: {} stderr: {}", o.stdout, o.stderr);
    assert_full_shape(&o);
    assert_lines(&o, ["FAIL", "FAIL", "FAIL"]);
    assert_entry(&o, 0, "fail", json!(["duplicate_side_effect"]), "complete");
    assert_entry(&o, 1, "fail", json!(["error_rate_exceeded"]), "complete");
    assert_entry(
        &o,
        2,
        "fail",
        json!(["protection_policy_not_met"]),
        "complete",
    );
    fx.shutdown().await;
}

// AC2.1, AC3.2: one broken variant per position, two healthy; all three reached the fixture.
#[tokio::test(flavor = "multi_thread")]
async fn one_broken_variant_per_position_fails_only_that_check() {
    for broken in 0..3usize {
        let fx = Fixture::start(0).await.unwrap();
        let cfg = config(HEALTHY, &fx.base_url(), |v| match broken {
            0 => v["race"]["action_request"]["path"] = json!(RACY),
            1 => v["load"]["request"]["path"] = json!(FLAKY),
            _ => set_login(v, UNPROTECTED),
        });
        let o = run_suite(&cfg).await;
        assert_eq!(o.code, 1, "broken={broken}: {} {}", o.stdout, o.stderr);
        assert_full_shape(&o);
        let mut verdicts = ["PASS"; 3];
        verdicts[broken] = "FAIL";
        assert_lines(&o, verdicts);
        let want = [
            ("single_side_effect", "duplicate_side_effect"),
            ("thresholds_met", "error_rate_exceeded"),
            ("protection_observed", "protection_policy_not_met"),
        ];
        for (i, (pass, fail)) in want.iter().enumerate() {
            if i == broken {
                assert_entry(&o, i, "fail", json!([fail]), "complete");
            } else {
                assert_entry(&o, i, "pass", json!([pass]), "complete");
            }
            assert_has_evidence(&o, i);
        }
        assert!(stat(&fx, "login_requests").await > 0, "broken={broken}");
        assert!(stat(&fx, "action_requests").await > 0, "broken={broken}");
        assert!(stat(&fx, "load_requests").await > 0, "broken={broken}");
        fx.shutdown().await;
    }
}

// AC1.4: login accepts any password.
#[tokio::test(flavor = "multi_thread")]
async fn accepts_invalid_login_is_fail_invalid_credentials_accepted() {
    let fx = Fixture::start(0).await.unwrap();
    let o = run_suite(&config(HEALTHY, &fx.base_url(), |v| {
        set_login(v, ACCEPTS_INVALID)
    }))
    .await;
    assert_eq!(o.code, 1, "{} {}", o.stdout, o.stderr);
    assert_full_shape(&o);
    assert_lines(&o, ["PASS", "PASS", "FAIL"]);
    assert_entry(
        &o,
        2,
        "fail",
        json!(["invalid_credentials_accepted"]),
        "complete",
    );
    fx.shutdown().await;
}

// AC1.4: slow route (fixture sleeps 300 ms) against max_p95_ms=100.
#[tokio::test(flavor = "multi_thread")]
async fn slow_load_with_p95_100_is_fail_latency_exceeded() {
    let fx = Fixture::start(0).await.unwrap();
    let o = run_suite(&config(HEALTHY, &fx.base_url(), |v| {
        v["load"]["request"]["path"] = json!(SLOW);
        v["load"]["requests"] = json!(20);
        v["load"]["concurrency"] = json!(5);
        v["load"]["max_p95_ms"] = json!(100);
    }))
    .await;
    assert_eq!(o.code, 1, "{} {}", o.stdout, o.stderr);
    assert_full_shape(&o);
    assert_lines(&o, ["PASS", "FAIL", "PASS"]);
    assert_entry(&o, 1, "fail", json!(["latency_exceeded"]), "complete");
    fx.shutdown().await;
}

// AC2.2, AC3.3: ERROR in race does not hide load PASS or the brute_force FAIL.
#[tokio::test(flavor = "multi_thread")]
async fn race_error_does_not_hide_load_pass_or_brute_force_fail_exit_2() {
    let fx = Fixture::start(0).await.unwrap();
    let o = run_suite(&config(HEALTHY, &fx.base_url(), |v| {
        v["race"]["counter_pointer"] = json!("/missing");
        set_login(v, UNPROTECTED);
    }))
    .await;
    assert_eq!(o.code, 2, "{} {}", o.stdout, o.stderr);
    assert_full_shape(&o);
    assert_lines(&o, ["ERROR", "PASS", "FAIL"]);
    assert_entry(&o, 0, "error", json!(["counter_invalid"]), "complete");
    assert_entry(&o, 1, "pass", json!(["thresholds_met"]), "complete");
    assert_entry(
        &o,
        2,
        "fail",
        json!(["protection_policy_not_met"]),
        "complete",
    );
    assert!(
        o.lines().iter().any(|l| l.starts_with("brute_force FAIL")),
        "stdout: {}",
        o.stdout
    );
    assert_eq!(o.report["completion"], "complete");
    fx.shutdown().await;
}

// AC4.1, AC4.2, AC3.3: repeat without reset is never a false PASS; reset restores run 1.
#[tokio::test(flavor = "multi_thread")]
async fn repeat_without_reset_is_inconclusive_and_reset_restores_exit_0() {
    let fx = Fixture::start(0).await.unwrap();
    let cfg = config(HEALTHY, &fx.base_url(), |_| {});

    let first = run_suite(&cfg).await;
    assert_healthy(&first);

    let second = run_suite(&cfg).await;
    assert_eq!(second.code, 2, "{} {}", second.stdout, second.stderr);
    assert_full_shape(&second);
    assert_lines(&second, ["INCONCLUSIVE", "PASS", "INCONCLUSIVE"]);
    assert_entry(
        &second,
        0,
        "inconclusive",
        json!(["initial_counter_not_zero"]),
        "complete",
    );
    assert_entry(&second, 1, "pass", json!(["thresholds_met"]), "complete");
    assert_entry(
        &second,
        2,
        "inconclusive",
        json!(["account_already_locked"]),
        "complete",
    );

    let resp = admin(&fx, "POST", "/__admin/reset").await;
    assert!(resp.starts_with("HTTP/1.1 200"), "{resp}");

    let third = run_suite(&cfg).await;
    assert_healthy(&third);
    assert_eq!(third.summary(), first.summary());
    fx.shutdown().await;
}

// AC10: the fixture proof must never need a fixture on the default port; it only uses port 0.
#[test]
fn config_examples_are_not_modified_by_these_tests() {
    // The examples keep the documented default base_url; variants are built in memory.
    for p in [HEALTHY, BROKEN] {
        assert!(Path::new(p).exists());
        let v: Value = serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap();
        assert_eq!(v["base_url"], "http://127.0.0.1:8080", "{p}");
    }
}

// ---------- S5: report atomicity, SIGINT, redaction ----------

fn file_names(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}

/// Collects every string value that looks like an RFC 3339 UTC timestamp.
fn timestamps(v: &Value, out: &mut Vec<String>) {
    match v {
        Value::String(s) if s.len() >= 19 && s.as_bytes()[4] == b'-' && s.contains('T') => {
            out.push(s.clone())
        }
        Value::Array(a) => a.iter().for_each(|x| timestamps(x, out)),
        Value::Object(m) => m.values().for_each(|x| timestamps(x, out)),
        _ => {}
    }
}

/// Inode of a file (unix only; 0 elsewhere so the atomic check is skipped).
fn inode(p: &Path) -> u64 {
    #[cfg(unix)]
    {
        std::os::unix::fs::MetadataExt::ino(&std::fs::metadata(p).unwrap())
    }
    #[cfg(not(unix))]
    {
        let _ = p;
        0
    }
}

// AC5.1, AC5.2
#[tokio::test(flavor = "multi_thread")]
async fn out_with_old_content_is_replaced_by_valid_json_and_no_temp_file_remains() {
    let fx = Fixture::start(0).await.unwrap();
    let cfg = config(HEALTHY, &fx.base_url(), |_| {});
    let (code, stdout, stderr, names, text, inodes) = tokio::task::spawn_blocking(move || {
        let dir = tempfile::tempdir().unwrap();
        let cfg_path = dir.path().join("c.json");
        let out_dir = dir.path().join("out");
        std::fs::create_dir(&out_dir).unwrap();
        let out_path = out_dir.join("result.json");
        std::fs::write(&cfg_path, serde_json::to_string(&cfg).unwrap()).unwrap();
        std::fs::write(&out_path, "OLD").unwrap();
        let ino_before = inode(&out_path);
        let o = Command::new(env!("CARGO_BIN_EXE_api-toolkit"))
            .args(["check", "--config"])
            .arg(&cfg_path)
            .arg("--out")
            .arg(&out_path)
            .env_clear()
            .envs(ENV)
            .output()
            .unwrap();
        (
            o.status.code(),
            String::from_utf8_lossy(&o.stdout).into_owned(),
            String::from_utf8_lossy(&o.stderr).into_owned(),
            file_names(&out_dir),
            std::fs::read_to_string(&out_path).unwrap(),
            (ino_before, inode(&out_path)),
        )
    })
    .await
    .unwrap();
    assert_eq!(code, Some(0), "stdout: {stdout} stderr: {stderr}");
    assert_eq!(names, ["result.json"], "temp file left behind");
    #[cfg(unix)]
    assert_ne!(
        inodes.0, inodes.1,
        "--out was written in place, not renamed"
    );
    #[cfg(not(unix))]
    let _ = inodes;
    let report: Value = serde_json::from_str(&text).expect("report is valid JSON");
    assert_eq!(report["report_version"], 1);
    assert_eq!(report["scope"], "full");
    assert_eq!(report["checks"].as_array().unwrap().len(), 3);
    let mut ts = Vec::new();
    timestamps(&report, &mut ts);
    assert!(!ts.is_empty(), "no timestamp found in report");
    for t in &ts {
        assert!(
            t.ends_with('Z') && !t.contains('.'),
            "timestamp has fraction or no Z: {t}"
        );
    }
    fx.shutdown().await;
}

/// Accepts connections and never answers. `seen()` is the number of accepted connections.
struct Hang {
    port: u16,
    count: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    task: tokio::task::JoinHandle<()>,
}

impl Hang {
    async fn start() -> Hang {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = l.local_addr().unwrap().port();
        let count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let c = count.clone();
        let task = tokio::spawn(async move {
            let mut held = Vec::new();
            while let Ok((s, _)) = l.accept().await {
                held.push(s);
                c.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }
        });
        Hang { port, count, task }
    }
    fn base_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }
    fn seen(&self) -> usize {
        self.count.load(std::sync::atomic::Ordering::SeqCst)
    }
}

impl Drop for Hang {
    fn drop(&mut self) {
        self.task.abort();
    }
}

// AC3.4 (binary)
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn sigint_on_full_suite_is_exit_130_all_three_reported_and_no_new_connection() {
    use std::time::Duration;
    let h = Hang::start().await;
    let cfg = config(HEALTHY, &h.base_url(), |_| {});
    let dir = tempfile::tempdir().unwrap();
    let cfg_path = dir.path().join("c.json");
    let out_dir = dir.path().join("out");
    std::fs::create_dir(&out_dir).unwrap();
    let out_path = out_dir.join("result.json");
    std::fs::write(&cfg_path, serde_json::to_string(&cfg).unwrap()).unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_api-toolkit"))
        .args(["check", "--config"])
        .arg(&cfg_path)
        .arg("--out")
        .arg(&out_path)
        .env_clear()
        .envs(ENV)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    // Observed condition, not a guessed sleep: the first request reached the listener.
    let mut waited = 0;
    while h.seen() == 0 {
        if waited >= 10_000 {
            let _ = child.kill();
            let _ = child.wait();
            panic!("the binary never connected");
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
        waited += 10;
    }
    let seen_at_sigint = h.seen();
    assert_eq!(
        seen_at_sigint, 1,
        "only the race request should be in flight"
    );
    let st = Command::new("kill")
        .args(["-INT", &child.id().to_string()])
        .status()
        .unwrap();
    if !st.success() {
        let _ = child.kill();
        let _ = child.wait();
        panic!("kill -INT failed");
    }
    let o = tokio::task::spawn_blocking(move || child.wait_with_output().unwrap())
        .await
        .unwrap();
    let stdout = String::from_utf8_lossy(&o.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&o.stderr).into_owned();
    assert_eq!(
        o.status.code(),
        Some(130),
        "stdout: {stdout} stderr: {stderr}"
    );
    let report: Value = serde_json::from_str(&std::fs::read_to_string(&out_path).unwrap()).unwrap();
    assert_eq!(report["report_version"], 1);
    assert_eq!(report["scope"], "full");
    assert_eq!(report["completion"], "incomplete");
    let checks = report["checks"].as_array().unwrap();
    let names: Vec<&str> = checks
        .iter()
        .map(|c| c["check"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["race", "load", "brute_force"]);
    for c in checks {
        assert_ne!(c["verdict"], "pass", "cancelled run must not pass: {c}");
        assert_eq!(c["completion"], "incomplete", "{c}");
    }
    // The hanging check (race) and the ones that never started: not_run / cancelled.
    for i in [0, 1, 2] {
        assert_eq!(checks[i]["verdict"], "not_run", "{}", checks[i]);
        assert_eq!(checks[i]["reasons"], json!(["cancelled"]), "{}", checks[i]);
    }
    assert_eq!(stdout.lines().count(), 3, "stdout: {stdout}");
    for (line, name) in stdout.lines().zip(["race", "load", "brute_force"]) {
        assert!(line.starts_with(name), "line `{line}`");
        assert!(!line.contains("PASS"), "line `{line}`");
    }
    assert_eq!(
        file_names(&out_dir),
        ["result.json"],
        "temp file left behind"
    );
    // The process is gone. Give a stray connection time to arrive; the count must not grow.
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        h.seen(),
        seen_at_sigint,
        "a connection was opened after cancel"
    );
}

const SENT_ID: &str = "SENTINEL-id-7d41c9";
const SENT_PW: &str = "SENTINEL-pw-3b9e52";
const SENT_ENV: [(&str, &str); 5] = [
    ("LOGIN_USER", support::fixture::LOGIN_USER),
    (
        "LOGIN_VALID_PASSWORD",
        support::fixture::LOGIN_VALID_PASSWORD,
    ),
    ("LOGIN_INVALID_PASSWORD", SENT_PW),
    ("TEST_ACTION_ID", SENT_ID),
    ("TEST_AUTHORIZATION", support::fixture::TEST_AUTHORIZATION),
];

fn assert_no_sentinel(o: &Out) {
    let report_text = o.report.to_string();
    for hay in [&o.stdout, &o.stderr, &report_text] {
        for s in [SENT_ID, SENT_PW] {
            assert!(!hay.contains(s), "sentinel {s} leaked in: {hay}");
        }
    }
    // Positive control: output and report really carry the three checks.
    for name in ["race", "load", "brute_force"] {
        assert!(o.stdout.contains(name), "stdout: {}", o.stdout);
        assert!(report_text.contains(name));
    }
}

// AC5.3 (redaction), full suite
#[tokio::test(flavor = "multi_thread")]
async fn sentinel_env_values_never_appear_in_full_suite_output_or_report() {
    let fx = Fixture::start(0).await.unwrap();
    let healthy = run_suite_env(&config(HEALTHY, &fx.base_url(), |_| {}), &SENT_ENV).await;
    // The sentinel id is accepted by the fixture, so a real verdict proves it was used.
    assert_eq!(
        healthy.code, 0,
        "stdout: {} stderr: {}",
        healthy.stdout, healthy.stderr
    );
    assert_no_sentinel(&healthy);
    admin(&fx, "POST", "/__admin/reset").await;
    let broken = run_suite_env(&config(BROKEN, &fx.base_url(), |_| {}), &SENT_ENV).await;
    assert_eq!(
        broken.code, 1,
        "stdout: {} stderr: {}",
        broken.stdout, broken.stderr
    );
    assert_no_sentinel(&broken);
    fx.shutdown().await;
}

// AC5.3 (redaction), sentinel really on the wire
#[tokio::test(flavor = "multi_thread")]
async fn sentinel_sent_on_the_wire_still_absent_from_output_and_report() {
    use tokio::io::AsyncReadExt;
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://127.0.0.1:{}", l.local_addr().unwrap().port());
    let wire = std::sync::Arc::new(std::sync::Mutex::new(Vec::<u8>::new()));
    let w = wire.clone();
    let task = tokio::spawn(async move {
        let mut held = Vec::new();
        while let Ok((mut s, _)) = l.accept().await {
            let w = w.clone();
            held.push(tokio::spawn(async move {
                let mut buf = [0u8; 4096];
                while let Ok(n) = s.read(&mut buf).await {
                    if n == 0 {
                        break;
                    }
                    w.lock().unwrap().extend_from_slice(&buf[..n]);
                }
            }));
        }
    });
    let cfg = config(HEALTHY, &base, |v| {
        v["limits"]["request_timeout_ms"] = json!(300);
        v["load"]["requests"] = json!(10);
        let b = &mut v["brute_force"];
        b["attempts"] = json!(2);
        b["protect_by_attempt"] = json!(2);
        b["protected_tail_count"] = json!(1);
    });
    // The valid password is the sentinel too, so the baseline request carries it.
    let env = [
        ("LOGIN_USER", "demo"),
        ("LOGIN_VALID_PASSWORD", SENT_PW),
        ("LOGIN_INVALID_PASSWORD", SENT_PW),
        ("TEST_ACTION_ID", SENT_ID),
        ("TEST_AUTHORIZATION", "Bearer fixture-token"),
    ];
    let o = run_suite_env(&cfg, &env).await;
    let wire_text = String::from_utf8_lossy(&wire.lock().unwrap()).into_owned();
    task.abort();
    // Positive control: both sentinels were really sent.
    assert!(wire_text.contains(SENT_ID), "id not on wire: {wire_text}");
    assert!(
        wire_text.contains(SENT_PW),
        "password not on wire: {wire_text}"
    );
    // The server never answers: nothing can PASS.
    assert_ne!(o.code, 0, "stdout: {}", o.stdout);
    assert!(!o.stdout.contains("PASS"), "stdout: {}", o.stdout);
    assert_no_sentinel(&o);
}
