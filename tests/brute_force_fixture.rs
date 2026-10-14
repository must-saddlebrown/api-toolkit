//! P1 S5: `checks::brute_force::run` against the real local fixture (127.0.0.1, port 0).
//! No `start_paused` here: real sockets plus a paused clock give fake timeouts.
mod support;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use api_toolkit::checks::brute_force::{console_line, run};
use api_toolkit::config::{self, Selection, resolve, validate};
use api_toolkit::http::Transport;
use api_toolkit::report::*;
use serde_json::{Value, json};
use support::fixture::*;
use time::OffsetDateTime;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::watch;

/// Fixed secret sentinels: env values that must never appear in output.
const SENTINEL: &str = "SENTINEL-s3cr3t-9f2c41";
const SENTINEL_PW: &str = "SENTINEL-pw-77aa10";

fn base_config(url: &str) -> Value {
    json!({
        "config_version": 1,
        "base_url": url,
        "limits": {"request_timeout_ms": 2000, "suite_deadline_ms": 120000},
        "brute_force": {
            "baseline_request": {"method": "POST", "path": "/login",
                "json": {"username": "${ENV:LOGIN_USER}", "password": "${ENV:LOGIN_VALID_PASSWORD}"}},
            "probe_request": {"method": "POST", "path": "/login",
                "json": {"username": "${ENV:LOGIN_USER}", "password": "${ENV:LOGIN_INVALID_PASSWORD}"}},
            "success_match": {"status_in": [200], "json_exists": ["/access_token"]},
            "rejection_match": {"status_in": [401]},
            "protection_match": {"status_in": [429]},
            "attempts": 8, "interval_ms": 10, "protect_by_attempt": 6, "protected_tail_count": 2
        },
        "race": {
            "action_request": {"method": "POST", "path": "/actions/{id}/apply",
                "path_params": {"id": "${ENV:TEST_ACTION_ID}"},
                "headers": {"Authorization": "${ENV:TEST_AUTHORIZATION}"}},
            "verify_request": {"method": "GET", "path": "/actions/{id}",
                "path_params": {"id": "${ENV:TEST_ACTION_ID}"},
                "headers": {"Authorization": "${ENV:TEST_AUTHORIZATION}"}},
            "verify_match": {"status_in": [200]},
            "counter_pointer": "/execution_count", "initial_counter": 0, "expected_delta": 1,
            "concurrency": 5, "expected_action_statuses": [200, 409],
            "max_dispatch_skew_ms": 100, "settle_ms": 250
        },
        "load": {
            "request": {"method": "GET", "path": "/critical",
                "headers": {"Authorization": "${ENV:TEST_AUTHORIZATION}"}},
            "response_match": {"status_in": [200]},
            "requests": 100, "concurrency": 10, "max_error_rate": 0.01, "max_p95_ms": 500
        }
    })
}

fn lookup(name: &str) -> Option<String> {
    Some(
        match name {
            "LOGIN_USER" => LOGIN_USER,
            "LOGIN_VALID_PASSWORD" => LOGIN_VALID_PASSWORD,
            "LOGIN_INVALID_PASSWORD" => LOGIN_INVALID_PASSWORD,
            "TEST_ACTION_ID" => TEST_ACTION_ID,
            "TEST_AUTHORIZATION" => TEST_AUTHORIZATION,
            "SENTINEL" => SENTINEL,
            "SENTINEL_PW" => SENTINEL_PW,
            _ => return None,
        }
        .to_string(),
    )
}

/// parse -> validate -> resolve (injected env) -> run. Process env is never touched.
async fn run_cfg(cfg: &Value) -> CheckResult {
    let text = cfg.to_string();
    let parsed = config::parse(&text).unwrap();
    validate(&parsed, Selection::All).unwrap();
    let resolved = resolve(&parsed, Selection::All, &lookup).unwrap();
    let bf_reqs = resolved.brute_force.as_ref().unwrap();
    let dbg = format!("{resolved:?}");
    assert!(!dbg.contains(SENTINEL));
    assert!(!dbg.contains(SENTINEL_PW));
    let transport = Transport::new(&parsed.limits).unwrap();
    let (_tx, rx) = watch::channel(None);
    run(
        &transport,
        parsed.brute_force.as_ref().unwrap(),
        &bf_reqs.baseline,
        &bf_reqs.probe,
        rx,
    )
    .await
}

fn client() -> reqwest::Client {
    reqwest::Client::builder().no_proxy().build().unwrap()
}

async fn login_requests(c: &reqwest::Client, url: &str) -> u64 {
    let v: Value = c
        .get(format!("{url}/__admin/stats"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    v["login_requests"].as_u64().unwrap()
}

/// Send `n` wrong logins straight to the fixture (prime the lockout state).
async fn prime(c: &reqwest::Client, url: &str, n: u32) {
    for _ in 0..n {
        c.post(format!("{url}/login"))
            .json(&json!({"username": LOGIN_USER, "password": LOGIN_INVALID_PASSWORD}))
            .send()
            .await
            .unwrap();
    }
}

fn assert_verdict(r: &CheckResult, v: Verdict, reasons: &[ReasonCode]) {
    assert_eq!(r.verdict, v, "{}", console_line(r));
    assert_eq!(r.reasons, reasons, "{}", console_line(r));
    assert_eq!(r.completion, Completion::Complete);
}

fn evidence(r: &CheckResult) -> &BruteForceEvidence {
    match r.evidence.as_ref() {
        Some(Evidence::BruteForce(e)) => e,
        _ => panic!("expected brute force evidence"),
    }
}

fn burst_evidence(r: &CheckResult) -> &BurstEvidence {
    match r.evidence.as_ref() {
        Some(Evidence::BruteForceBurst(e)) => e,
        _ => panic!("expected burst evidence: {}", console_line(r)),
    }
}

/// `base_config` turned into a burst config: drops the three sequential-only fields, adds
/// `mode`/`window_ms`, and points both requests at `path`.
fn burst_config(url: &str, path: &str, attempts: u32, window_ms: u64) -> Value {
    let mut cfg = base_config(url);
    let bf = cfg["brute_force"].as_object_mut().unwrap();
    bf.remove("interval_ms");
    bf.remove("protect_by_attempt");
    bf.remove("protected_tail_count");
    bf.insert("mode".into(), json!("burst"));
    bf.insert("window_ms".into(), json!(window_ms));
    bf.insert("attempts".into(), json!(attempts));
    for k in ["baseline_request", "probe_request"] {
        cfg["brute_force"][k]["path"] = json!(path);
    }
    cfg
}

/// M1, AC1, AC7, AC9, AC12: healthy -> PASS, exactly 1 + 8 requests on the wire.
#[tokio::test(flavor = "multi_thread")]
async fn m1_healthy_passes_with_nine_requests() {
    let fx = Fixture::start(0).await.unwrap();
    let url = fx.base_url();
    let r = run_cfg(&base_config(&url)).await;
    assert_verdict(&r, Verdict::Pass, &[ReasonCode::ProtectionObserved]);
    let e = evidence(&r);
    assert_eq!(e.first_protected_attempt, Some(6));
    assert_eq!(e.protected_tail_observed, 3);
    assert_eq!((e.attempts_planned, e.attempts_dispatched), (8, 9));
    assert_eq!(e.baseline.as_ref().unwrap().outcome, Outcome::Success);
    let outcomes: Vec<_> = e.attempts.iter().map(|a| (a.attempt, a.status)).collect();
    let want: Vec<_> = (1..=8u32)
        .map(|k| (k, Some(if k < 6 { 401 } else { 429 })))
        .collect();
    assert_eq!(outcomes, want);
    assert_eq!(login_requests(&client(), &url).await, 9);
    fx.shutdown().await;
}

/// M2, AC2: no protection -> FAIL.
#[tokio::test(flavor = "multi_thread")]
async fn m2_unprotected_fails() {
    let fx = Fixture::start(0).await.unwrap();
    let url = fx.base_url();
    let mut cfg = base_config(&url);
    for k in ["baseline_request", "probe_request"] {
        cfg["brute_force"][k]["path"] = json!("/broken/login/unprotected");
    }
    let r = run_cfg(&cfg).await;
    assert_verdict(&r, Verdict::Fail, &[ReasonCode::ProtectionPolicyNotMet]);
    assert_eq!(evidence(&r).first_protected_attempt, None);
    assert_eq!(login_requests(&client(), &url).await, 9);
    fx.shutdown().await;
}

/// M3, AC3: wrong credentials accepted -> FAIL.
#[tokio::test(flavor = "multi_thread")]
async fn m3_accepts_invalid_fails() {
    let fx = Fixture::start(0).await.unwrap();
    let url = fx.base_url();
    let mut cfg = base_config(&url);
    for k in ["baseline_request", "probe_request"] {
        cfg["brute_force"][k]["path"] = json!("/broken/login/accepts-invalid");
    }
    let r = run_cfg(&cfg).await;
    assert_verdict(&r, Verdict::Fail, &[ReasonCode::InvalidCredentialsAccepted]);
    assert_eq!(evidence(&r).attempts.len(), 8);
    assert!(
        evidence(&r)
            .attempts
            .iter()
            .all(|a| a.outcome == Outcome::Success)
    );
    assert_eq!(login_requests(&client(), &url).await, 9);
    fx.shutdown().await;
}

/// M4, AC4: prelocked (6 wrong logins first) -> INCONCLUSIVE, only the baseline is sent.
#[tokio::test(flavor = "multi_thread")]
async fn m4_prelocked_is_inconclusive_with_one_request() {
    let fx = Fixture::start(0).await.unwrap();
    let (c, url) = (client(), fx.base_url());
    prime(&c, &url, 6).await;
    let r = run_cfg(&base_config(&url)).await;
    assert_verdict(
        &r,
        Verdict::Inconclusive,
        &[ReasonCode::AccountAlreadyLocked],
    );
    assert!(evidence(&r).attempts.is_empty());
    assert_eq!(login_requests(&c, &url).await, 6 + 1);
    fx.shutdown().await;
}

/// M4 variant (second run without reset) and M1 -> M4: a passing run leaves the account locked.
#[tokio::test(flavor = "multi_thread")]
async fn m4_second_run_without_reset_is_inconclusive() {
    let fx = Fixture::start(0).await.unwrap();
    let (c, url) = (client(), fx.base_url());
    let cfg = base_config(&url);
    assert_eq!(run_cfg(&cfg).await.verdict, Verdict::Pass);
    let second = run_cfg(&cfg).await;
    assert_verdict(
        &second,
        Verdict::Inconclusive,
        &[ReasonCode::AccountAlreadyLocked],
    );
    assert_eq!(login_requests(&c, &url).await, 9 + 1);
    // Reset makes the check repeatable again.
    c.post(format!("{url}/__admin/reset")).send().await.unwrap();
    assert_eq!(run_cfg(&cfg).await.verdict, Verdict::Pass);
    fx.shutdown().await;
}

/// M5, AC15: baseline credentials rejected (401) -> INCONCLUSIVE baseline_failed, no probes.
#[tokio::test(flavor = "multi_thread")]
async fn m5_baseline_rejected_is_inconclusive() {
    let fx = Fixture::start(0).await.unwrap();
    let (c, url) = (client(), fx.base_url());
    let mut cfg = base_config(&url);
    cfg["brute_force"]["baseline_request"]["json"]["password"] =
        json!("${ENV:LOGIN_INVALID_PASSWORD}");
    let r = run_cfg(&cfg).await;
    assert_verdict(&r, Verdict::Inconclusive, &[ReasonCode::BaselineFailed]);
    assert_eq!(
        evidence(&r).baseline.as_ref().unwrap().outcome,
        Outcome::NormalRejection
    );
    assert_eq!(login_requests(&c, &url).await, 1);
    fx.shutdown().await;
}

/// M6, AC6: baseline answered 404 (matches nothing) -> ERROR baseline_failed, no probes.
#[tokio::test(flavor = "multi_thread")]
async fn m6_baseline_unclassifiable_is_error() {
    let fx = Fixture::start(0).await.unwrap();
    let url = fx.base_url();
    let mut cfg = base_config(&url);
    cfg["brute_force"]["baseline_request"]["path"] = json!("/nope");
    let r = run_cfg(&cfg).await;
    assert_verdict(&r, Verdict::Error, &[ReasonCode::BaselineFailed]);
    let b = evidence(&r).baseline.as_ref().unwrap();
    assert_eq!((b.outcome, b.status), (Outcome::Unexpected, Some(404)));
    assert!(evidence(&r).attempts.is_empty());
    fx.shutdown().await;
}

/// M7, AC5, AC15: nothing listens on the port -> ERROR baseline_failed + transport_error.
#[tokio::test(flavor = "multi_thread")]
async fn m7_connection_refused_is_error() {
    let port = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let r = run_cfg(&base_config(&format!("http://127.0.0.1:{port}"))).await;
    assert_verdict(
        &r,
        Verdict::Error,
        &[ReasonCode::BaselineFailed, ReasonCode::TransportError],
    );
    let b = evidence(&r).baseline.as_ref().unwrap();
    assert_eq!((b.outcome, b.status), (Outcome::TransportError, None));
    assert_eq!(b.error, Some(TransportErrorCode::ConnectFailed));
    assert!(evidence(&r).attempts.is_empty());
}

/// M8, AC14: 5 wrong logins first -> baseline 200, probe 1 already 429 -> INCONCLUSIVE.
#[tokio::test(flavor = "multi_thread")]
async fn m8_probe_1_protected_is_inconclusive() {
    let fx = Fixture::start(0).await.unwrap();
    let (c, url) = (client(), fx.base_url());
    prime(&c, &url, 5).await;
    let r = run_cfg(&base_config(&url)).await;
    assert_verdict(
        &r,
        Verdict::Inconclusive,
        &[ReasonCode::AccountAlreadyLocked],
    );
    assert_eq!(evidence(&r).attempts[0].outcome, Outcome::Protection);
    assert_eq!(login_requests(&c, &url).await, 5 + 9);
    fx.shutdown().await;
}

/// M9, AC6: protection_match [403] makes the real 429 unclassifiable -> ERROR.
#[tokio::test(flavor = "multi_thread")]
async fn m9_probe_unclassifiable_is_error() {
    let fx = Fixture::start(0).await.unwrap();
    let url = fx.base_url();
    let mut cfg = base_config(&url);
    cfg["brute_force"]["protection_match"]["status_in"] = json!([403]);
    let r = run_cfg(&cfg).await;
    assert_verdict(&r, Verdict::Error, &[ReasonCode::ProbeUnclassifiable]);
    assert_eq!(evidence(&r).attempts[5].outcome, Outcome::Unexpected);
    fx.shutdown().await;
}

/// M12, AC13: protection starts at 6 but protect_by_attempt is 5 -> FAIL (late).
#[tokio::test(flavor = "multi_thread")]
async fn m12_late_protection_fails() {
    let fx = Fixture::start(0).await.unwrap();
    let url = fx.base_url();
    let mut cfg = base_config(&url);
    cfg["brute_force"]["protect_by_attempt"] = json!(5);
    let r = run_cfg(&cfg).await;
    assert_verdict(&r, Verdict::Fail, &[ReasonCode::ProtectionPolicyNotMet]);
    assert_eq!(evidence(&r).first_protected_attempt, Some(6));
    fx.shutdown().await;
}

/// AC13 boundary against the real fixture: protect_by 6 with a tail longer than the
/// protected run (tail 4, only attempts 6..8 protected) must FAIL, tail 3 passes.
#[tokio::test(flavor = "multi_thread")]
async fn tail_longer_than_protection_fails() {
    let cases: [(u32, Verdict, &[ReasonCode]); 2] = [
        (3, Verdict::Pass, &[ReasonCode::ProtectionObserved]),
        (4, Verdict::Fail, &[ReasonCode::ProtectionPolicyNotMet]),
    ];
    for (tail, want, reasons) in cases {
        let fx = Fixture::start(0).await.unwrap();
        let mut cfg = base_config(&fx.base_url());
        cfg["brute_force"]["protected_tail_count"] = json!(tail);
        let r = run_cfg(&cfg).await;
        assert_eq!(r.verdict, want, "tail={tail}: {}", console_line(&r));
        assert_eq!(r.reasons, reasons, "tail={tail}: {}", console_line(&r));
        assert_eq!(r.completion, Completion::Complete, "tail={tail}");
        fx.shutdown().await;
    }
}

/// AC8 (lower bound only; real timers): each probe starts at least interval_ms after the
/// previous probe started.
#[tokio::test(flavor = "multi_thread")]
async fn interval_lower_bound_between_probes() {
    let fx = Fixture::start(0).await.unwrap();
    let mut cfg = base_config(&fx.base_url());
    cfg["brute_force"]["interval_ms"] = json!(50);
    let r = run_cfg(&cfg).await;
    assert_eq!(r.verdict, Verdict::Pass);
    let e = evidence(&r);
    assert_eq!(e.interval_ms, 50);
    for w in e.attempts.windows(2) {
        assert!(
            w[1].offset_ms >= w[0].offset_ms + w[0].elapsed_ms + 50,
            "attempt {} -> {}: {} ms (previous elapsed {} ms)",
            w[0].attempt,
            w[1].attempt,
            w[1].offset_ms - w[0].offset_ms,
            w[0].elapsed_ms
        );
    }
    assert!(e.attempts[7].offset_ms >= 7 * 50);
    fx.shutdown().await;
}

/// Minimal scripted HTTP server on 127.0.0.1 for transport faults the fixture cannot
/// produce. `script(n)` gives the reply for the n-th connection (0-based): `Some((status,
/// json))` or `None` = accept and never answer. Returns the port and the accepted count.
async fn scripted_server(
    script: fn(usize) -> Option<(u16, &'static str)>,
) -> (u16, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let count = Arc::new(AtomicUsize::new(0));
    let seen = count.clone();
    tokio::spawn(async move {
        loop {
            let Ok((mut s, _)) = listener.accept().await else {
                return;
            };
            let n = seen.fetch_add(1, Ordering::SeqCst);
            tokio::spawn(async move {
                let mut buf = Vec::new();
                let mut chunk = [0u8; 1024];
                // Read headers, then the body announced by content-length.
                loop {
                    let Ok(k) = s.read(&mut chunk).await else {
                        return;
                    };
                    if k == 0 {
                        return;
                    }
                    buf.extend_from_slice(&chunk[..k]);
                    let text = String::from_utf8_lossy(&buf).to_lowercase();
                    if let Some(end) = text.find("\r\n\r\n") {
                        let len = text[..end]
                            .lines()
                            .find_map(|l| l.strip_prefix("content-length:"))
                            .and_then(|v| v.trim().parse::<usize>().ok())
                            .unwrap_or(0);
                        if buf.len() >= end + 4 + len {
                            break;
                        }
                    }
                }
                match script(n) {
                    Some((status, body)) => {
                        let head = format!(
                            "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                            body.len()
                        );
                        let _ = s.write_all(head.as_bytes()).await;
                        let _ = s.shutdown().await;
                    }
                    None => tokio::time::sleep(std::time::Duration::from_secs(30)).await,
                }
            });
        }
    });
    (port, count)
}

/// M7 (timeout), AC5, AC9: baseline never answers -> ERROR, one connection only (no retry).
#[tokio::test(flavor = "multi_thread")]
async fn m7_baseline_timeout_is_error_without_retry() {
    let (port, count) = scripted_server(|_| None).await;
    let mut cfg = base_config(&format!("http://127.0.0.1:{port}"));
    cfg["limits"]["request_timeout_ms"] = json!(200);
    let r = run_cfg(&cfg).await;
    assert_verdict(
        &r,
        Verdict::Error,
        &[ReasonCode::BaselineFailed, ReasonCode::TransportError],
    );
    assert_eq!(
        evidence(&r).baseline.as_ref().unwrap().error,
        Some(TransportErrorCode::Timeout)
    );
    assert_eq!(count.load(Ordering::SeqCst), 1);
}

/// M10, AC5, AC9: probe 3 times out; all 9 attempts still sent once each, never PASS.
#[tokio::test(flavor = "multi_thread")]
async fn m10_probe_timeout_is_error_without_retry() {
    let (port, count) = scripted_server(|n| match n {
        0 => Some((200, r#"{"access_token":"t"}"#)),
        3 => None,
        _ => Some((401, r#"{"error":"invalid_credentials"}"#)),
    })
    .await;
    let mut cfg = base_config(&format!("http://127.0.0.1:{port}"));
    cfg["limits"]["request_timeout_ms"] = json!(200);
    let r = run_cfg(&cfg).await;
    assert_verdict(
        &r,
        Verdict::Error,
        &[ReasonCode::ProbeUnclassifiable, ReasonCode::TransportError],
    );
    let e = evidence(&r);
    assert_eq!(e.attempts.len(), 8);
    assert_eq!(e.attempts[2].outcome, Outcome::TransportError);
    assert_eq!(e.attempts[2].error, Some(TransportErrorCode::Timeout));
    assert_eq!(count.load(Ordering::SeqCst), 9);
}

fn assert_no_leak(r: &CheckResult, forbidden: &[&str]) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("report.json");
    let t = OffsetDateTime::UNIX_EPOCH;
    let report = Report::new(
        "run-1".into(),
        "hash".into(),
        t,
        t,
        r.completion,
        Scope::Full,
        vec![r.clone()],
    )
    .unwrap();
    write_report_atomic(&path, &report).unwrap();
    let file = std::fs::read_to_string(&path).unwrap();
    assert!(
        file.contains("\"brute_force\""),
        "report must hold the check"
    );
    for (name, text) in [
        ("report file", file),
        ("Debug", format!("{r:?}")),
        ("console_line", console_line(r)),
    ] {
        for f in forbidden {
            assert!(!text.contains(f), "{f} leaked into {name}");
        }
    }
}

/// AC10, AC11: sentinel in headers (sent on the wire) and the fixture's access token
/// (present in the baseline response body) do not reach report file, Debug or console.
#[tokio::test(flavor = "multi_thread")]
async fn secrets_do_not_leak_healthy_run() {
    let fx = Fixture::start(0).await.unwrap();
    let mut cfg = base_config(&fx.base_url());
    for k in ["baseline_request", "probe_request"] {
        cfg["brute_force"][k]["headers"] = json!({"X-Sentinel": "${ENV:SENTINEL}"});
    }
    let r = run_cfg(&cfg).await;
    assert_eq!(r.verdict, Verdict::Pass);
    assert_no_leak(
        &r,
        &[
            SENTINEL,
            ACCESS_TOKEN,
            LOGIN_VALID_PASSWORD,
            LOGIN_INVALID_PASSWORD,
            TEST_AUTHORIZATION,
        ],
    );
    fx.shutdown().await;
}

/// AC10, AC11: sentinel as the actual password; every probe gets a token back (FAIL path).
#[tokio::test(flavor = "multi_thread")]
async fn secrets_do_not_leak_failing_run() {
    let fx = Fixture::start(0).await.unwrap();
    let mut cfg = base_config(&fx.base_url());
    for k in ["baseline_request", "probe_request"] {
        cfg["brute_force"][k]["path"] = json!("/broken/login/accepts-invalid");
        cfg["brute_force"][k]["json"]["password"] = json!("${ENV:SENTINEL_PW}");
    }
    let r = run_cfg(&cfg).await;
    assert_eq!(r.verdict, Verdict::Fail);
    assert_no_leak(&r, &[SENTINEL_PW, ACCESS_TOKEN, LOGIN_VALID_PASSWORD]);
    fx.shutdown().await;
}

// ---------------------------------------------------------------------------
// Q3-S5: mode burst against the real fixture. `/windowed/login` and
// `/broken/login/always-protected` are burst-only routes (docs/fixture.md).
// ---------------------------------------------------------------------------

/// P1-AC19/AC20: mode absent (sequential default); `interval_ms >= 150` never collects
/// 5 failures inside the fixture's 300 ms window (docs/fixture.md), so this reproduces
/// Finding B deterministically: FAIL, never flaky.
#[tokio::test(flavor = "multi_thread")]
async fn b20_windowed_sequential_is_fail_protection_policy_not_met() {
    let fx = Fixture::start(0).await.unwrap();
    let (c, url) = (client(), fx.base_url());
    let mut cfg = base_config(&url);
    for k in ["baseline_request", "probe_request"] {
        cfg["brute_force"][k]["path"] = json!("/windowed/login");
    }
    cfg["brute_force"]["interval_ms"] = json!(150);
    let r = run_cfg(&cfg).await;
    assert_verdict(&r, Verdict::Fail, &[ReasonCode::ProtectionPolicyNotMet]);
    assert_eq!(login_requests(&c, &url).await, 9);
    fx.shutdown().await;
}

/// P1-AC23, AC31: the same route and `interval_ms`'s probe count via burst (all dispatched
/// together) must cross the 300 ms/5 limiter -> PASS, some protected, some rejected.
#[tokio::test(flavor = "multi_thread")]
async fn b21_windowed_burst_is_pass_protection_observed() {
    let fx = Fixture::start(0).await.unwrap();
    let (c, url) = (client(), fx.base_url());
    let cfg = burst_config(&url, "/windowed/login", 8, 1000);
    let r = run_cfg(&cfg).await;
    assert_verdict(&r, Verdict::Pass, &[ReasonCode::ProtectionObserved]);
    let e = burst_evidence(&r);
    assert!(
        e.protected_count >= 1,
        "protected_count={}",
        e.protected_count
    );
    assert!(e.rejected_count >= 1, "rejected_count={}", e.rejected_count);
    assert_eq!(login_requests(&c, &url).await, 9);
    fx.shutdown().await;
}

/// P1-AC24: `P == 0` -> FAIL, same as sequential.
#[tokio::test(flavor = "multi_thread")]
async fn b22_unprotected_burst_is_fail() {
    let fx = Fixture::start(0).await.unwrap();
    let url = fx.base_url();
    let cfg = burst_config(&url, "/broken/login/unprotected", 8, 1000);
    let r = run_cfg(&cfg).await;
    assert_verdict(&r, Verdict::Fail, &[ReasonCode::ProtectionPolicyNotMet]);
    fx.shutdown().await;
}

/// P1-AC24: a probe success beats everything else, also in burst.
#[tokio::test(flavor = "multi_thread")]
async fn b23_accepts_invalid_burst_is_fail_invalid_credentials_accepted() {
    let fx = Fixture::start(0).await.unwrap();
    let url = fx.base_url();
    let cfg = burst_config(&url, "/broken/login/accepts-invalid", 8, 1000);
    let r = run_cfg(&cfg).await;
    assert_verdict(&r, Verdict::Fail, &[ReasonCode::InvalidCredentialsAccepted]);
    fx.shutdown().await;
}

/// P1-AC25: `P == N` (always-protected route) -> INCONCLUSIVE, never PASS.
#[tokio::test(flavor = "multi_thread")]
async fn b24_always_protected_burst_is_inconclusive_account_already_locked() {
    let fx = Fixture::start(0).await.unwrap();
    let url = fx.base_url();
    let cfg = burst_config(&url, "/broken/login/always-protected", 8, 1000);
    let r = run_cfg(&cfg).await;
    assert_verdict(
        &r,
        Verdict::Inconclusive,
        &[ReasonCode::AccountAlreadyLocked],
    );
    let e = burst_evidence(&r);
    assert_eq!(e.protected_count, e.attempts_planned);
    fx.shutdown().await;
}

/// P1-AC25: baseline not success -> sequential outcome, zero probes. Second run on the
/// healthy (count-based) `/login` without a reset: the account is already locked by the
/// first run's burst, so the baseline itself is protected and no probe is sent at all.
#[tokio::test(flavor = "multi_thread")]
async fn b25_prelocked_baseline_burst_is_inconclusive_with_zero_probes() {
    let fx = Fixture::start(0).await.unwrap();
    let (c, url) = (client(), fx.base_url());
    let cfg = burst_config(&url, "/login", 8, 1000);
    run_cfg(&cfg).await; // locks the account (shared count-based limiter on /login)
    let after_first = login_requests(&c, &url).await;
    let second = run_cfg(&cfg).await;
    assert_verdict(
        &second,
        Verdict::Inconclusive,
        &[ReasonCode::AccountAlreadyLocked],
    );
    assert!(burst_evidence(&second).attempts.is_empty());
    assert_eq!(
        login_requests(&c, &url).await,
        after_first + 1,
        "baseline only"
    );
    fx.shutdown().await;
}

/// P1-AC26: a `rejection_match` that does not match the real response makes every probe
/// `unexpected` -> ERROR `probe_unclassifiable`, beating the window entirely.
#[tokio::test(flavor = "multi_thread")]
async fn b26_burst_bad_rejection_matcher_is_error_probe_unclassifiable() {
    let fx = Fixture::start(0).await.unwrap();
    let url = fx.base_url();
    let mut cfg = burst_config(&url, "/broken/login/unprotected", 8, 1000);
    cfg["brute_force"]["rejection_match"]["status_in"] = json!([400]);
    let r = run_cfg(&cfg).await;
    assert_verdict(&r, Verdict::Error, &[ReasonCode::ProbeUnclassifiable]);
    fx.shutdown().await;
}

/// P1-AC26/AC28: nothing listens on the port -> ERROR on the baseline, same reasons as
/// sequential (M7), zero probes sent.
#[tokio::test(flavor = "multi_thread")]
async fn b27_closed_port_burst_is_error_baseline_failed_transport_error() {
    let port = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let cfg = burst_config(
        &format!("http://127.0.0.1:{port}"),
        "/windowed/login",
        8,
        1000,
    );
    let r = run_cfg(&cfg).await;
    assert_verdict(
        &r,
        Verdict::Error,
        &[ReasonCode::BaselineFailed, ReasonCode::TransportError],
    );
    let e = burst_evidence(&r);
    let b = e.baseline.as_ref().unwrap();
    assert_eq!(b.outcome, Outcome::TransportError);
    assert!(e.attempts.is_empty());
}

/// P1-AC28: exactly 1 baseline + N probes on the wire, no retry.
#[tokio::test(flavor = "multi_thread")]
async fn b28_burst_sends_exactly_one_baseline_and_n_probes_no_retry() {
    let fx = Fixture::start(0).await.unwrap();
    let (c, url) = (client(), fx.base_url());
    let cfg = burst_config(&url, "/windowed/login", 8, 1000);
    let r = run_cfg(&cfg).await;
    assert_eq!(login_requests(&c, &url).await, 9);
    assert_eq!(burst_evidence(&r).attempts_dispatched, 9);
    fx.shutdown().await;
}

/// P1-AC29: the burst evidence path leaks nothing either (same sentinel pattern as the
/// sequential tests above).
#[tokio::test(flavor = "multi_thread")]
async fn b29_burst_sentinel_not_in_report_or_debug_or_console() {
    let fx = Fixture::start(0).await.unwrap();
    let mut cfg = burst_config(&fx.base_url(), "/windowed/login", 8, 1000);
    for k in ["baseline_request", "probe_request"] {
        cfg["brute_force"][k]["headers"] = json!({"X-Sentinel": "${ENV:SENTINEL}"});
    }
    let r = run_cfg(&cfg).await;
    assert_eq!(r.verdict, Verdict::Pass);
    assert_no_leak(
        &r,
        &[
            SENTINEL,
            ACCESS_TOKEN,
            LOGIN_VALID_PASSWORD,
            LOGIN_INVALID_PASSWORD,
            TEST_AUTHORIZATION,
        ],
    );
    fx.shutdown().await;
}

/// P1-AC22: `mode: "sequential"` explicit and mode omitted must behave and serialize
/// identically (separate fixtures so neither run's lockout state leaks into the other).
#[tokio::test(flavor = "multi_thread")]
async fn b30_explicit_sequential_equals_omitted_mode() {
    let fx1 = Fixture::start(0).await.unwrap();
    let omitted = run_cfg(&base_config(&fx1.base_url())).await;
    fx1.shutdown().await;

    let fx2 = Fixture::start(0).await.unwrap();
    let mut cfg = base_config(&fx2.base_url());
    cfg["brute_force"]["mode"] = json!("sequential");
    let explicit = run_cfg(&cfg).await;
    fx2.shutdown().await;

    assert_eq!(omitted.verdict, explicit.verdict);
    assert_eq!(omitted.reasons, explicit.reasons);
    assert_eq!(omitted.completion, explicit.completion);
    // `offset_ms`/`elapsed_ms` are real wall-clock timing and legitimately differ between
    // two separate runs; strip them before comparing, everything else (shape, outcomes,
    // statuses, counters) must be byte-identical.
    let strip_timing = |e: &Option<Evidence>| -> Value {
        let mut v = serde_json::to_value(e).unwrap();
        if let Some(attempts) = v.get_mut("attempts").and_then(Value::as_array_mut) {
            for a in attempts {
                a.as_object_mut().unwrap().remove("offset_ms");
                a.as_object_mut().unwrap().remove("elapsed_ms");
            }
        }
        if let Some(obj) = v.get_mut("baseline").and_then(Value::as_object_mut) {
            obj.remove("offset_ms");
            obj.remove("elapsed_ms");
        }
        v
    };
    assert_eq!(
        strip_timing(&omitted.evidence),
        strip_timing(&explicit.evidence),
        "evidence must match except real timing fields"
    );
}

/// P1-AC19/AC31 regression: the two pre-Q3 example configs are still sequential and their
/// locked request/fixed-wait budgets are unchanged (values confirmed against the files, not
/// assumed: see `examples/core-checks.json` and `examples/brute-force-only.json`).
#[test]
fn b31_core_example_configs_still_sequential_and_validate_same() {
    // core-checks.json has all three sections (whole-suite budget, like plain `validate`);
    // brute-force-only.json has only `brute_force` (needs `--only brute_force`, like its
    // own doc example).
    for (path, selection, want_requests, want_wait_ms) in [
        (
            concat!(env!("CARGO_MANIFEST_DIR"), "/examples/core-checks.json"),
            Selection::All,
            117u64,
            950u64,
        ),
        (
            concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/examples/brute-force-only.json"
            ),
            Selection::Only(CheckName::BruteForce),
            9u64,
            700u64,
        ),
    ] {
        let text = std::fs::read_to_string(path).unwrap();
        let cfg = config::parse(&text).unwrap();
        assert_eq!(
            cfg.brute_force.as_ref().unwrap().mode,
            config::BruteForceMode::Sequential,
            "{path}"
        );
        let budget = validate(&cfg, selection).unwrap();
        assert_eq!(budget.requests, want_requests, "{path}");
        assert_eq!(budget.fixed_wait_ms, want_wait_ms, "{path}");
    }
}
