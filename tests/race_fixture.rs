//! P2 S6: `checks::race::run` against the real local fixture (127.0.0.1, port 0).
//! No `start_paused` here: real sockets plus a paused clock give fake timeouts.
mod support;

use api_toolkit::checks::race::{console_line, run};
use api_toolkit::config::{self, Selection, resolve, validate};
use api_toolkit::http::Transport;
use api_toolkit::report::*;
use serde_json::{Value, json};
use support::fixture::*;
use time::OffsetDateTime;
use tokio::net::TcpListener;
use tokio::sync::watch;

/// Fixed secret sentinels: env values that must never appear in output.
const SENTINEL_ID: &str = "SENTINEL-id-5d1e83";
const SENTINEL_HDR: &str = "SENTINEL-hdr-c0ffee";

const APPLY: &str = "/actions/{id}/apply";
const RACY: &str = "/broken/actions/{id}/apply-racy";
const IDEMPOTENT: &str = "/idempotent/actions/{id}/apply";

/// Race-only config. `request_timeout_ms` 5000 > the racy gate timeout (2 s); skew budget
/// 1000 ms so client speed is never a verdict factor.
fn race_config(url: &str, apply_path: &str) -> Value {
    json!({
        "config_version": 1,
        "base_url": url,
        "limits": {"request_timeout_ms": 5000, "suite_deadline_ms": 120000},
        "race": {
            "action_request": {"method": "POST", "path": apply_path,
                "path_params": {"id": "${ENV:TEST_ACTION_ID}"},
                "headers": {"Authorization": "${ENV:TEST_AUTHORIZATION}"}},
            "verify_request": {"method": "GET", "path": "/actions/{id}",
                "path_params": {"id": "${ENV:TEST_ACTION_ID}"},
                "headers": {"Authorization": "${ENV:TEST_AUTHORIZATION}"}},
            "verify_match": {"status_in": [200]},
            "counter_pointer": "/execution_count", "initial_counter": 0, "expected_delta": 1,
            "concurrency": 5, "expected_action_statuses": [200, 409],
            "max_dispatch_skew_ms": 1000, "settle_ms": 50
        }
    })
}

/// parse -> validate(Only(Race)) -> resolve (injected env) -> run. Process env is never touched.
async fn run_cfg(cfg: &Value, action_id: &str) -> CheckResult {
    let lookup = |name: &str| -> Option<String> {
        Some(
            match name {
                "TEST_ACTION_ID" => action_id,
                "TEST_AUTHORIZATION" => TEST_AUTHORIZATION,
                "SENTINEL" => SENTINEL_HDR,
                _ => return None,
            }
            .to_string(),
        )
    };
    let text = cfg.to_string();
    let parsed = config::parse(&text).unwrap();
    let sel = Selection::Only(CheckName::Race);
    validate(&parsed, sel).unwrap();
    let resolved = resolve(&parsed, sel, &lookup).unwrap();
    let reqs = resolved.race.as_ref().unwrap();
    let dbg = format!("{resolved:?}");
    assert!(!dbg.contains(SENTINEL_ID) && !dbg.contains(SENTINEL_HDR));
    let transport = Transport::new(&parsed.limits).unwrap();
    let (_tx, rx) = watch::channel(None);
    run(
        &transport,
        parsed.race.as_ref().unwrap(),
        &reqs.action,
        &reqs.verify,
        rx,
    )
    .await
}

fn client() -> reqwest::Client {
    reqwest::Client::builder().no_proxy().build().unwrap()
}

/// (action_requests, verify_requests) as counted by the fixture.
async fn stats(url: &str) -> (u64, u64) {
    let v: Value = client()
        .get(format!("{url}/__admin/stats"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    (
        v["action_requests"].as_u64().unwrap(),
        v["verify_requests"].as_u64().unwrap(),
    )
}

fn assert_verdict(r: &CheckResult, v: Verdict, reasons: &[ReasonCode]) {
    assert_eq!(r.verdict, v, "{}", console_line(r));
    assert_eq!(r.reasons, reasons, "{}", console_line(r));
    assert_eq!(r.completion, Completion::Complete);
}

fn evidence(r: &CheckResult) -> &RaceEvidence {
    match r.evidence.as_ref() {
        Some(Evidence::Race(e)) => e,
        _ => panic!("expected race evidence"),
    }
}

fn count_status(e: &RaceEvidence, status: u16) -> usize {
    e.actions
        .iter()
        .filter(|a| a.status == Some(status))
        .count()
}

/// R1, AC1, AC9, AC10: healthy -> PASS; 1 verifier + 5 actions + 1 verifier on the wire.
#[tokio::test(flavor = "multi_thread")]
async fn r1_healthy_passes_single_side_effect() {
    let fx = Fixture::start(0).await.unwrap();
    let url = fx.base_url();
    let r = run_cfg(&race_config(&url, APPLY), "r1").await;
    assert_verdict(&r, Verdict::Pass, &[ReasonCode::SingleSideEffect]);
    let e = evidence(&r);
    assert_eq!(
        (e.counter_before, e.counter_after, e.delta),
        (Some(0), Some(1), Some(1))
    );
    assert_eq!((e.actions.len(), e.actions_dispatched), (5, 5));
    assert_eq!(count_status(e, 200), 1);
    assert_eq!(count_status(e, 409), 4);
    assert!(e.actions.iter().all(|a| a.allowed));
    assert_eq!(stats(&url).await, (5, 2));
    // The console skew number must be the report evidence value, not a separate computation.
    let line = console_line(&r);
    let shown = line
        .split("skew=")
        .nth(1)
        .and_then(|t| t.split('/').next())
        .unwrap_or_else(|| panic!("no skew in: {line}"));
    let want = e
        .dispatch_skew_ms
        .expect("skew known after dispatch")
        .to_string();
    assert_eq!(shown, want, "{line}");
    fx.shutdown().await;
}

/// R2, AC2, AC8: racy variant, 10 distinct ids on one fixture, every run FAIL (not flaky).
#[tokio::test(flavor = "multi_thread")]
async fn r2_racy_fails_ten_times() {
    let fx = Fixture::start(0).await.unwrap();
    let url = fx.base_url();
    let cfg = race_config(&url, RACY);
    for i in 0..10 {
        let r = run_cfg(&cfg, &format!("racy-{i}")).await;
        assert_verdict(&r, Verdict::Fail, &[ReasonCode::DuplicateSideEffect]);
        let e = evidence(&r);
        assert!(e.delta.unwrap() >= 2, "run {i}: {}", console_line(&r));
        assert!(count_status(e, 200) >= 2, "run {i}");
        assert_eq!(e.counter_before, Some(0));
    }
    assert_eq!(stats(&url).await, (50, 20));
    fx.shutdown().await;
}

/// R3, AC3: idempotent variant answers 200 to all five, one side effect -> PASS.
#[tokio::test(flavor = "multi_thread")]
async fn r3_idempotent_passes_with_five_200() {
    let fx = Fixture::start(0).await.unwrap();
    let url = fx.base_url();
    let r = run_cfg(&race_config(&url, IDEMPOTENT), "r3").await;
    assert_verdict(&r, Verdict::Pass, &[ReasonCode::SingleSideEffect]);
    let e = evidence(&r);
    assert_eq!(e.delta, Some(1));
    assert_eq!(count_status(e, 200), 5);
    assert_eq!(stats(&url).await, (5, 2));
    fx.shutdown().await;
}

/// R4, AC4: second run with the same id (no reset) -> INCONCLUSIVE, zero actions, one verifier.
#[tokio::test(flavor = "multi_thread")]
async fn r4_used_resource_is_inconclusive_without_actions() {
    let fx = Fixture::start(0).await.unwrap();
    let url = fx.base_url();
    let cfg = race_config(&url, APPLY);
    let first = run_cfg(&cfg, "r4").await;
    assert_eq!(first.verdict, Verdict::Pass);
    assert_eq!(stats(&url).await, (5, 2));
    let r = run_cfg(&cfg, "r4").await;
    assert_verdict(
        &r,
        Verdict::Inconclusive,
        &[ReasonCode::InitialCounterNotZero],
    );
    let e = evidence(&r);
    assert_eq!(e.counter_before, Some(1));
    assert!(e.actions.is_empty() && e.verify_after.is_none());
    assert_eq!(stats(&url).await, (5, 3), "actions +0, verifier +1");
    fx.shutdown().await;
}

/// R5, AC5: counter pointer missing from the verifier body -> ERROR, zero actions.
#[tokio::test(flavor = "multi_thread")]
async fn r5_missing_counter_is_error() {
    let fx = Fixture::start(0).await.unwrap();
    let url = fx.base_url();
    let mut cfg = race_config(&url, APPLY);
    cfg["race"]["counter_pointer"] = json!("/nope");
    let r = run_cfg(&cfg, "r5").await;
    assert_verdict(&r, Verdict::Error, &[ReasonCode::CounterInvalid]);
    assert!(evidence(&r).actions.is_empty());
    assert_eq!(stats(&url).await, (0, 1));
    fx.shutdown().await;
}

/// R6, AC6: pointer `/id` is a string, not a non-negative integer -> ERROR, zero actions.
#[tokio::test(flavor = "multi_thread")]
async fn r6_non_integer_counter_is_error() {
    let fx = Fixture::start(0).await.unwrap();
    let url = fx.base_url();
    let mut cfg = race_config(&url, APPLY);
    cfg["race"]["counter_pointer"] = json!("/id");
    let r = run_cfg(&cfg, "r6").await;
    assert_verdict(&r, Verdict::Error, &[ReasonCode::CounterInvalid]);
    assert_eq!(evidence(&r).counter_before, None);
    assert_eq!(stats(&url).await, (0, 1));
    fx.shutdown().await;
}

/// R11, AC12: action hits id `a`, verifier reads id `b` -> delta 0 -> FAIL action_not_executed.
#[tokio::test(flavor = "multi_thread")]
async fn r11_action_id_differs_from_verifier_id() {
    let fx = Fixture::start(0).await.unwrap();
    let url = fx.base_url();
    let mut cfg = race_config(&url, APPLY);
    cfg["race"]["verify_request"]["path_params"] = json!({"id": "b"});
    let r = run_cfg(&cfg, "a").await;
    assert_verdict(&r, Verdict::Fail, &[ReasonCode::ActionNotExecuted]);
    let e = evidence(&r);
    assert_eq!(
        (e.counter_before, e.counter_after, e.delta),
        (Some(0), Some(0), Some(0))
    );
    assert_eq!(stats(&url).await, (5, 2));
    fx.shutdown().await;
}

/// R13, AC13: only 200 allowed on the healthy route -> the four 409 are ambiguous -> INCONCLUSIVE.
#[tokio::test(flavor = "multi_thread")]
async fn r13_unallowed_status_is_ambiguous() {
    let fx = Fixture::start(0).await.unwrap();
    let url = fx.base_url();
    let mut cfg = race_config(&url, APPLY);
    cfg["race"]["expected_action_statuses"] = json!([200]);
    let r = run_cfg(&cfg, "r13").await;
    assert_verdict(
        &r,
        Verdict::Inconclusive,
        &[ReasonCode::ActionOutcomeAmbiguous],
    );
    let e = evidence(&r);
    assert_eq!(e.delta, Some(1));
    assert_eq!(e.actions.iter().filter(|a| !a.allowed).count(), 4);
    assert_eq!(stats(&url).await, (5, 2));
    fx.shutdown().await;
}

/// R18, AC15: closed port -> verifier before fails to connect -> ERROR, no action sent.
#[tokio::test(flavor = "multi_thread")]
async fn r18_closed_port_is_connect_failed() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let r = run_cfg(
        &race_config(&format!("http://127.0.0.1:{port}"), APPLY),
        "r18",
    )
    .await;
    assert_verdict(
        &r,
        Verdict::Error,
        &[ReasonCode::VerifierFailed, ReasonCode::TransportError],
    );
    let e = evidence(&r);
    assert_eq!(
        e.verify_before.as_ref().unwrap().error,
        Some(TransportErrorCode::ConnectFailed)
    );
    assert!(e.actions.is_empty() && e.actions_dispatched == 0);
}

/// R19, AC15: verifier without Authorization gets 401 -> mismatch -> ERROR, no action sent.
#[tokio::test(flavor = "multi_thread")]
async fn r19_unauthorized_verifier_is_error() {
    let fx = Fixture::start(0).await.unwrap();
    let url = fx.base_url();
    let mut cfg = race_config(&url, APPLY);
    cfg["race"]["verify_request"]
        .as_object_mut()
        .unwrap()
        .remove("headers");
    let r = run_cfg(&cfg, "r19").await;
    assert_verdict(&r, Verdict::Error, &[ReasonCode::VerifierFailed]);
    let e = evidence(&r);
    assert_eq!(e.verify_before.as_ref().unwrap().status, Some(401));
    assert!(e.actions.is_empty());
    assert_eq!(stats(&url).await, (0, 1));
    fx.shutdown().await;
}

/// Write `r` through `write_report_atomic`, read it back, and search every output for `forbidden`.
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
        Scope::Partial,
        vec![r.clone()],
    )
    .unwrap();
    write_report_atomic(&path, &report).unwrap();
    let file = std::fs::read_to_string(&path).unwrap();
    assert!(file.contains("\"race\""), "report must hold the check");
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

fn sentinel_config(url: &str, apply_path: &str) -> Value {
    let mut cfg = race_config(url, apply_path);
    for k in ["action_request", "verify_request"] {
        cfg["race"][k]["headers"]["X-Sentinel"] = json!("${ENV:SENTINEL}");
    }
    cfg
}

/// R30, AC20: sentinel id (path param, reflected by the verifier body) and header; PASS path.
#[tokio::test(flavor = "multi_thread")]
async fn r30_sentinel_does_not_leak_pass() {
    let fx = Fixture::start(0).await.unwrap();
    let r = run_cfg(&sentinel_config(&fx.base_url(), APPLY), SENTINEL_ID).await;
    assert_verdict(&r, Verdict::Pass, &[ReasonCode::SingleSideEffect]);
    assert_no_leak(&r, &[SENTINEL_ID, SENTINEL_HDR, TEST_AUTHORIZATION]);
    fx.shutdown().await;
}

/// R30, AC20: same sentinels on the racy route; FAIL path.
#[tokio::test(flavor = "multi_thread")]
async fn r30_sentinel_does_not_leak_fail() {
    let fx = Fixture::start(0).await.unwrap();
    let r = run_cfg(&sentinel_config(&fx.base_url(), RACY), SENTINEL_ID).await;
    assert_verdict(&r, Verdict::Fail, &[ReasonCode::DuplicateSideEffect]);
    assert_no_leak(&r, &[SENTINEL_ID, SENTINEL_HDR, TEST_AUTHORIZATION]);
    fx.shutdown().await;
}
