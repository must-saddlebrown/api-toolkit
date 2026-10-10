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
    let cfg = cfg.clone();
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
            .envs(ENV)
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
