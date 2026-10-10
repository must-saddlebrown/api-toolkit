//! P3 S6: `checks::load::run` against the real local fixture (127.0.0.1, port 0).
//! No `tokio::time::pause` here: real sockets plus a paused clock give fake timeouts.
//! Latency checks assert lower bounds only, so laptop speed is never a verdict factor.
mod support;

use api_toolkit::checks::load::{console_line, run};
use api_toolkit::config::{self, Selection, resolve, validate};
use api_toolkit::http::Transport;
use api_toolkit::report::*;
use serde_json::{Value, json};
use support::fixture::*;
use time::OffsetDateTime;
use tokio::net::TcpListener;
use tokio::sync::watch;

/// Fixed secret sentinels: env values that must never appear in output.
const SENTINEL_HDR: &str = "SENTINEL-hdr-c0ffee";
const SENTINEL_Q: &str = "SENTINEL-qry-9a41d7";

const HEALTHY: &str = "/critical";
const SLOW: &str = "/broken/critical/slow";
const FLAKY: &str = "/broken/critical/flaky";
const HANG: &str = "/broken/critical/hang";

/// Load-only config with auth. Thresholds are loose; each test tightens what it needs.
fn load_config(url: &str, path: &str, requests: u32, concurrency: u32) -> Value {
    json!({
        "config_version": 1,
        "base_url": url,
        "limits": {"request_timeout_ms": 5000, "suite_deadline_ms": 120000},
        "load": {
            "request": {"method": "GET", "path": path,
                "headers": {"Authorization": "${ENV:TEST_AUTHORIZATION}"}},
            "response_match": {"status_in": [200]},
            "requests": requests, "concurrency": concurrency,
            "max_error_rate": 0.01, "max_p95_ms": 5000
        }
    })
}

fn env_lookup(name: &str) -> Option<String> {
    Some(
        match name {
            "TEST_AUTHORIZATION" => TEST_AUTHORIZATION,
            "SENTINEL" => SENTINEL_HDR,
            "SENTINEL_Q" => SENTINEL_Q,
            _ => return None,
        }
        .to_string(),
    )
}

/// parse -> validate(Only(Load)) -> resolve (injected env) -> run. Process env is never touched.
async fn run_cfg(cfg: &Value) -> CheckResult {
    let text = cfg.to_string();
    let parsed = config::parse(&text).unwrap();
    let sel = Selection::Only(CheckName::Load);
    validate(&parsed, sel).unwrap();
    let resolved = resolve(&parsed, sel, &env_lookup).unwrap();
    let req = resolved.load.as_ref().unwrap();
    let dbg = format!("{resolved:?}");
    assert!(!dbg.contains(SENTINEL_HDR) && !dbg.contains(SENTINEL_Q));
    let transport = Transport::new(&parsed.limits).unwrap();
    let (_tx, rx) = watch::channel(None);
    run(&transport, parsed.load.as_ref().unwrap(), req, rx).await
}

async fn load_requests(url: &str) -> u64 {
    let v: Value = reqwest::Client::builder()
        .no_proxy()
        .build()
        .unwrap()
        .get(format!("{url}/__admin/stats"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    v["load_requests"].as_u64().unwrap()
}

fn assert_verdict(r: &CheckResult, v: Verdict, reasons: &[ReasonCode]) {
    assert_eq!(r.verdict, v, "{}", console_line(r));
    assert_eq!(r.reasons, reasons, "{}", console_line(r));
    assert_eq!(r.completion, Completion::Complete);
}

fn evidence(r: &CheckResult) -> &LoadEvidence {
    match r.evidence.as_ref() {
        Some(Evidence::Load(e)) => e,
        _ => panic!("expected load evidence"),
    }
}

fn statuses(e: &LoadEvidence) -> Vec<(u16, u32)> {
    e.status_counts.iter().map(|(s, n)| (*s, *n)).collect()
}

/// L1, AC1, AC9, AC10, AC11: healthy -> PASS; 1 preflight + 100 load on the wire, 10 in flight.
#[tokio::test(flavor = "multi_thread")]
async fn l1_healthy_passes() {
    let fx = Fixture::start(0).await.unwrap();
    let url = fx.base_url();
    let r = run_cfg(&load_config(&url, HEALTHY, 100, 10)).await;
    assert_verdict(&r, Verdict::Pass, &[ReasonCode::ThresholdsMet]);
    let e = evidence(&r);
    assert_eq!((e.requests, e.concurrency, e.max_p95_ms), (100, 10, 5000));
    assert_eq!((e.dispatched, e.attempted, e.succeeded), (100, 100, 100));
    assert_eq!((e.failed, e.timed_out), (0, 0));
    assert_eq!(e.max_in_flight, 10, "{}", console_line(&r));
    assert_eq!(statuses(e), vec![(200, 100)]);
    assert!(e.error_counts.is_empty());
    assert_eq!(e.error_rate, Some(0.0));
    assert_eq!(e.preflight.as_ref().unwrap().status, Some(200));
    assert_eq!(load_requests(&url).await, 101);
    let line = console_line(&r);
    assert!(line.starts_with("load PASS "), "{line}");
    assert!(line.ends_with("reasons=thresholds_met"), "{line}");
    assert!(line.contains("status=200:100"), "{line}");
    fx.shutdown().await;
}

/// L2, AC2: every answer is 200 but >= 300 ms -> FAIL latency_exceeded (lower bound only).
#[tokio::test(flavor = "multi_thread")]
async fn l2_slow_fails_latency() {
    let fx = Fixture::start(0).await.unwrap();
    let url = fx.base_url();
    let mut cfg = load_config(&url, SLOW, 20, 5);
    cfg["load"]["max_p95_ms"] = json!(100);
    let r = run_cfg(&cfg).await;
    assert_verdict(&r, Verdict::Fail, &[ReasonCode::LatencyExceeded]);
    let e = evidence(&r);
    assert!(e.p95_ms.unwrap() >= SLOW_DELAY.as_millis() as u64);
    assert_eq!((e.succeeded, e.failed), (20, 0));
    assert_eq!(load_requests(&url).await, 21);
    fx.shutdown().await;
}

/// L3, AC3: every 10th fixture request is 500. Preflight is request 1, so load requests
/// 2..=101 hit n = 10, 20, ..., 100 -> exactly 10 failures; the preflight 200 is not in the histogram.
#[tokio::test(flavor = "multi_thread")]
async fn l3_flaky_fails_error_rate() {
    let fx = Fixture::start(0).await.unwrap();
    let url = fx.base_url();
    let r = run_cfg(&load_config(&url, FLAKY, 100, 10)).await;
    assert_verdict(&r, Verdict::Fail, &[ReasonCode::ErrorRateExceeded]);
    let e = evidence(&r);
    assert_eq!((e.succeeded, e.failed, e.timed_out), (90, 10, 0));
    assert_eq!(statuses(e), vec![(200, 90), (500, 10)]);
    assert!(e.error_counts.is_empty());
    assert_eq!(e.error_rate, Some(0.1));
    assert_eq!(load_requests(&url).await, 101);
    fx.shutdown().await;
}

/// L4, AC4: requests 2..=21 hit n = 10 and 20, which sleep 3 s -> exactly 2 client timeouts,
/// never retried (21 requests on the wire). A route that never sleeps makes this test red.
#[tokio::test(flavor = "multi_thread")]
async fn l4_hang_times_out_without_retry() {
    let fx = Fixture::start(0).await.unwrap();
    let url = fx.base_url();
    let mut cfg = load_config(&url, HANG, 20, 5);
    cfg["limits"]["request_timeout_ms"] = json!(500);
    let r = run_cfg(&cfg).await;
    assert_verdict(&r, Verdict::Fail, &[ReasonCode::ErrorRateExceeded]);
    let e = evidence(&r);
    assert_eq!(e.timed_out, 2, "{}", console_line(&r));
    assert_eq!(e.error_counts.get(&TransportErrorCode::Timeout), Some(&2));
    assert_eq!((e.succeeded, e.failed, e.attempted), (18, 2, 20));
    assert_eq!(statuses(e), vec![(200, 18)]);
    assert_eq!(load_requests(&url).await, 21);
    fx.shutdown().await;
}

/// L13, AC9/AC10: preflight without Authorization -> 401 -> INCONCLUSIVE, zero load requests.
#[tokio::test(flavor = "multi_thread")]
async fn l13_preflight_401_is_target_not_ready() {
    let fx = Fixture::start(0).await.unwrap();
    let url = fx.base_url();
    let mut cfg = load_config(&url, HEALTHY, 50, 5);
    cfg["load"]["request"]
        .as_object_mut()
        .unwrap()
        .remove("headers");
    let r = run_cfg(&cfg).await;
    assert_verdict(&r, Verdict::Inconclusive, &[ReasonCode::TargetNotReady]);
    let e = evidence(&r);
    assert_eq!(e.preflight.as_ref().unwrap().status, Some(401));
    assert_eq!((e.dispatched, e.attempted), (0, 0));
    assert_eq!(load_requests(&url).await, 1);
    fx.shutdown().await;
}

/// L13, AC9/AC10: preflight 200 but `status_in=[204]` -> mismatch -> INCONCLUSIVE, zero load requests.
#[tokio::test(flavor = "multi_thread")]
async fn l13_preflight_matcher_mismatch_is_target_not_ready() {
    let fx = Fixture::start(0).await.unwrap();
    let url = fx.base_url();
    let mut cfg = load_config(&url, HEALTHY, 50, 5);
    cfg["load"]["response_match"] = json!({"status_in": [204]});
    let r = run_cfg(&cfg).await;
    assert_verdict(&r, Verdict::Inconclusive, &[ReasonCode::TargetNotReady]);
    let e = evidence(&r);
    assert_eq!(e.preflight.as_ref().unwrap().status, Some(200));
    assert_eq!((e.dispatched, e.attempted), (0, 0));
    assert_eq!(load_requests(&url).await, 1);
    fx.shutdown().await;
}

/// L14, AC9/AC10: closed port -> preflight cannot connect -> ERROR, no load.
#[tokio::test(flavor = "multi_thread")]
async fn l14_closed_port_is_connect_failed() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let r = run_cfg(&load_config(
        &format!("http://127.0.0.1:{port}"),
        HEALTHY,
        50,
        5,
    ))
    .await;
    assert_verdict(
        &r,
        Verdict::Error,
        &[ReasonCode::PreflightFailed, ReasonCode::TransportError],
    );
    let e = evidence(&r);
    assert_eq!(
        e.preflight.as_ref().unwrap().error,
        Some(TransportErrorCode::ConnectFailed)
    );
    assert_eq!((e.dispatched, e.attempted), (0, 0));
}

/// L15, AC9/AC10: preflight on the slow route with a 100 ms timeout -> ERROR (timeout), no load.
#[tokio::test(flavor = "multi_thread")]
async fn l15_preflight_timeout_is_error() {
    let fx = Fixture::start(0).await.unwrap();
    let url = fx.base_url();
    let mut cfg = load_config(&url, SLOW, 50, 5);
    cfg["limits"]["request_timeout_ms"] = json!(100);
    let r = run_cfg(&cfg).await;
    assert_verdict(
        &r,
        Verdict::Error,
        &[ReasonCode::PreflightFailed, ReasonCode::TransportError],
    );
    let e = evidence(&r);
    assert_eq!(
        e.preflight.as_ref().unwrap().error,
        Some(TransportErrorCode::Timeout)
    );
    assert_eq!((e.dispatched, e.attempted), (0, 0));
    assert_eq!(load_requests(&url).await, 1);
    fx.shutdown().await;
}

/// Write `r` through `write_report_atomic`; return (file text, parsed JSON).
fn write_and_read(r: &CheckResult) -> (String, Value) {
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
    let v = serde_json::from_str(&file).unwrap();
    (file, v)
}

/// The three output channels that are searched for leaks.
fn channels(r: &CheckResult) -> Vec<(&'static str, String)> {
    let (file, _) = write_and_read(r);
    assert!(file.contains("\"load\""), "report must hold the check");
    vec![
        ("report file", file),
        ("Debug", format!("{r:?}")),
        ("console_line", console_line(r)),
    ]
}

/// Search the report file, `Debug` and `console_line` for every forbidden string.
fn assert_no_leak(r: &CheckResult, forbidden: &[&str]) {
    for (name, text) in channels(r) {
        for f in forbidden {
            assert!(!text.contains(f), "{f} leaked into {name}");
        }
    }
}

fn sentinel_config(url: &str, path: &str) -> Value {
    let mut cfg = load_config(url, path, 100, 10);
    cfg["load"]["request"]["headers"]["X-Sentinel"] = json!("${ENV:SENTINEL}");
    cfg["load"]["request"]["query"] = json!({"q": "${ENV:SENTINEL_Q}"});
    cfg
}

const FORBIDDEN: [&str; 3] = [SENTINEL_HDR, SENTINEL_Q, TEST_AUTHORIZATION];

/// Positive control: `assert_no_leak` really panics for a needle in each channel. Every channel
/// has its own needle that exists in that channel only, so dropping one channel from the
/// search turns exactly one `catch_unwind` below into a failure.
#[tokio::test(flavor = "multi_thread")]
async fn l26_control_search_can_find_needle() {
    let fx = Fixture::start(0).await.unwrap();
    let r = run_cfg(&sentinel_config(&fx.base_url(), HEALTHY)).await;
    let ch = channels(&r);
    assert_eq!(ch.len(), 3);
    let needles = ["\"run_id\"", "LoadEvidence", "samples="];
    for ((name, _), needle) in ch.iter().zip(needles) {
        for (other, text) in &ch {
            assert_eq!(
                text.contains(needle),
                other == name,
                "needle {needle} must be unique to {name}, checked in {other}"
            );
        }
        assert!(
            std::panic::catch_unwind(|| assert_no_leak(&r, &[needle])).is_err(),
            "assert_no_leak missed {needle} in {name}"
        );
    }
    fx.shutdown().await;
}

/// The sentinels are really in the resolved load request (header + query). Wire delivery is
/// covered by the client unit tests; S8 adds a raw listener for end-to-end proof.
#[test]
fn l26_sentinels_are_in_the_resolved_request() {
    let cfg = sentinel_config("http://127.0.0.1:1", HEALTHY);
    let parsed = config::parse(&cfg.to_string()).unwrap();
    let sel = Selection::Only(CheckName::Load);
    let resolved = resolve(&parsed, sel, &env_lookup).unwrap();
    let req = resolved.load.as_ref().unwrap();
    assert!(req.url.expose().contains(&format!("q={SENTINEL_Q}")));
    let hdr = req
        .headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("x-sentinel"));
    assert_eq!(hdr.unwrap().1.expose(), SENTINEL_HDR);
}

/// L26, AC19: PASS path. Header + query sentinels are sent (fixture accepts them) but never printed.
#[tokio::test(flavor = "multi_thread")]
async fn l26_sentinel_does_not_leak_pass() {
    let fx = Fixture::start(0).await.unwrap();
    let r = run_cfg(&sentinel_config(&fx.base_url(), HEALTHY)).await;
    assert_verdict(&r, Verdict::Pass, &[ReasonCode::ThresholdsMet]);
    assert_no_leak(&r, &FORBIDDEN);
    fx.shutdown().await;
}

/// L26, AC19: FAIL path (flaky route).
#[tokio::test(flavor = "multi_thread")]
async fn l26_sentinel_does_not_leak_fail() {
    let fx = Fixture::start(0).await.unwrap();
    let r = run_cfg(&sentinel_config(&fx.base_url(), FLAKY)).await;
    assert_verdict(&r, Verdict::Fail, &[ReasonCode::ErrorRateExceeded]);
    assert_no_leak(&r, &FORBIDDEN);
    fx.shutdown().await;
}

/// AC17: real report JSON for load has exactly the documented keys, and every leaf is a number
/// or null (no body, header, or URL text can be there).
#[tokio::test(flavor = "multi_thread")]
async fn ac17_report_evidence_shape() {
    let fx = Fixture::start(0).await.unwrap();
    let r = run_cfg(&load_config(&fx.base_url(), FLAKY, 20, 4)).await;
    let (_, v) = write_and_read(&r);
    let ev = &v["checks"][0]["evidence"];
    let mut keys: Vec<&str> = ev.as_object().unwrap().keys().map(String::as_str).collect();
    keys.sort_unstable();
    let mut want = vec![
        "requests",
        "concurrency",
        "max_error_rate",
        "max_p95_ms",
        "preflight",
        "dispatched",
        "attempted",
        "succeeded",
        "failed",
        "timed_out",
        "error_rate",
        "p50_ms",
        "p95_ms",
        "max_ms",
        "max_in_flight",
        "load_elapsed_ms",
        "throughput_rps",
        "status_counts",
        "error_counts",
    ];
    want.sort_unstable();
    assert_eq!(keys, want);
    let mut pk: Vec<&str> = ev["preflight"]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    pk.sort_unstable();
    assert_eq!(pk, ["elapsed_ms", "error", "offset_ms", "status"]);
    // status_counts keys are the status as a string; flaky: 20 requests -> n = 10, 20 -> 2 x 500.
    assert_eq!(ev["status_counts"], json!({"200": 18, "500": 2}));
    assert_eq!(ev["error_counts"], json!({}));
    fn no_text(v: &Value) {
        match v {
            Value::String(s) => panic!("unexpected text in evidence: {s}"),
            Value::Array(a) => a.iter().for_each(no_text),
            Value::Object(o) => o.values().for_each(no_text),
            _ => {}
        }
    }
    no_text(ev);
    fx.shutdown().await;
}
