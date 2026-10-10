//! load check (PLAN §7). Pure parts: `classify` (one response -> outcome), `ceil_ms`,
//! `nearest_rank_ms`, `throughput_rps` and `evaluate` (all facts -> verdict). Async parts:
//! `run_with` (stop check -> preflight -> worker pool, with a closure as sender so tests need no
//! network), `run` (real transport) and `console_line`.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use futures_util::future::join_all;
use tokio::sync::watch;
use tokio::time::Instant;

use crate::checks::{Stop, current_stop, wait_stop};
use crate::config::{Load, MatcherSpec};
use crate::http::request::ResolvedRequest;
use crate::http::{Transport, TransportError, TransportResponse, send};
use crate::report::{
    CheckName, CheckResult, Completion, Evidence, LoadEvidence, ReasonCode, TransportErrorCode,
    Verdict, VerifyEvidence,
};

/// What one load response (or the preflight) turned out to be. The body is dropped after this.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SampleOutcome {
    /// HTTP response that satisfied `response_match`.
    Success(u16),
    /// HTTP response that did not satisfy `response_match` (status, JSON value, or no JSON body).
    Mismatch(u16),
    /// No HTTP response. A timeout is `Transport(Timeout)`.
    Transport(TransportErrorCode),
}

/// One recorded load request: its outcome and how long it took (monotonic clock).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sample {
    pub outcome: SampleOutcome,
    pub duration: Duration,
}

/// PLAN §7: a request is a success only when the matcher accepts it; everything else is failed.
pub fn classify(
    result: &Result<TransportResponse, TransportError>,
    matcher: &MatcherSpec,
) -> SampleOutcome {
    match result {
        Ok(r) if matcher.matches(r.status, r.body.as_ref()) => SampleOutcome::Success(r.status),
        Ok(r) => SampleOutcome::Mismatch(r.status),
        Err(e) => SampleOutcome::Transport(e.code),
    }
}

/// Round a duration UP to whole milliseconds, so 500.0004 ms is 501 and cannot slip under a
/// 500 ms limit. Saturates at `u64::MAX`.
pub fn ceil_ms(d: Duration) -> u64 {
    u64::try_from(d.as_nanos().div_ceil(1_000_000)).unwrap_or(u64::MAX)
}

/// Nearest-rank percentile (PLAN §7): sort, take rank `ceil(pct * n / 100)` (1-based), then
/// `ceil_ms`. `None` for no samples. `pct` is clamped to 1..=100 (callers use 50/95/100).
pub fn nearest_rank_ms(durations: &[Duration], pct: u64) -> Option<u64> {
    if durations.is_empty() {
        return None;
    }
    let mut sorted = durations.to_vec();
    sorted.sort_unstable();
    let rank = (pct * sorted.len() as u64).div_ceil(100).max(1);
    let index = (rank as usize - 1).min(sorted.len() - 1);
    Some(ceil_ms(sorted[index]))
}

/// Requests per second over the load phase. `None` when no time passed (never divide by 0).
pub fn throughput_rps(attempted: u32, elapsed_ms: u64) -> Option<f64> {
    if elapsed_ms == 0 {
        return None;
    }
    Some(f64::from(attempted) * 1000.0 / elapsed_ms as f64)
}

/// Everything `evaluate` needs. `preflight` is `None` when it was never answered (stop).
pub struct Facts<'a> {
    pub requests: u32,
    pub concurrency: u32,
    pub max_error_rate: f64,
    pub max_p95_ms: u64,
    pub preflight: Option<SampleOutcome>,
    /// Request numbers handed to workers, including ones dropped by a stop.
    pub dispatched: u32,
    pub max_in_flight: u32,
    /// Recorded load requests only (preflight excluded).
    pub samples: &'a [Sample],
    pub stop: Option<Stop>,
}

/// Verdict plus the aggregates the evidence needs.
#[derive(Debug, Clone, PartialEq)]
pub struct Evaluation {
    pub verdict: Verdict,
    pub reasons: Vec<ReasonCode>,
    pub completion: Completion,
    pub succeeded: u32,
    pub failed: u32,
    pub timed_out: u32,
    /// Only set when `attempted > 0`; always finite then.
    pub error_rate: Option<f64>,
    pub p50_ms: Option<u64>,
    pub p95_ms: Option<u64>,
    pub max_ms: Option<u64>,
}

/// PLAN §7 verdict. First rule that applies wins; PASS needs all N samples and both thresholds met.
pub fn evaluate(f: &Facts) -> Evaluation {
    let attempted = f.samples.len() as u64;
    let succeeded = f
        .samples
        .iter()
        .filter(|s| matches!(s.outcome, SampleOutcome::Success(_)))
        .count() as u32;
    let timed_out = f
        .samples
        .iter()
        .filter(|s| s.outcome == SampleOutcome::Transport(TransportErrorCode::Timeout))
        .count() as u32;
    let failed = f.samples.len() as u32 - succeeded;
    let durations: Vec<Duration> = f.samples.iter().map(|s| s.duration).collect();
    // Divide only when attempted > 0, so the rate is never NaN or infinite.
    let error_rate = (attempted > 0).then(|| f64::from(failed) / attempted as f64);
    let p50_ms = nearest_rank_ms(&durations, 50);
    let p95_ms = nearest_rank_ms(&durations, 95);
    let max_ms = nearest_rank_ms(&durations, 100);

    let make = |verdict, reasons, completion| Evaluation {
        verdict,
        reasons,
        completion,
        succeeded,
        failed,
        timed_out,
        error_rate,
        p50_ms,
        p95_ms,
        max_ms,
    };

    // 1. Broken runner counters or unusable thresholds. Never PASS, never FAIL on bad data.
    // `requests == 0` and a non-finite `max_error_rate` are rejected by config validation;
    // here they would let an empty or NaN comparison pass, so they are runner errors.
    let workers = f.concurrency.min(f.requests);
    if attempted > u64::from(f.dispatched)
        || f.dispatched > f.requests
        || f.max_in_flight > workers
        || f.requests == 0
        || !f.max_error_rate.is_finite()
    {
        return make(
            Verdict::Error,
            vec![ReasonCode::RunnerError],
            Completion::Complete,
        );
    }

    // 2. No preflight answer. With a stop that is NOT_RUN; without one it must not look ready.
    let Some(preflight) = f.preflight else {
        let reason = match f.stop {
            Some(s) => s.reason(),
            None => ReasonCode::NotStarted,
        };
        return make(Verdict::NotRun, vec![reason], Completion::Incomplete);
    };
    match preflight {
        // 3. Preflight transport error (timeout included).
        SampleOutcome::Transport(_) => {
            return make(
                Verdict::Error,
                vec![ReasonCode::PreflightFailed, ReasonCode::TransportError],
                Completion::Complete,
            );
        }
        // 4. Preflight answered but not what `response_match` accepts.
        SampleOutcome::Mismatch(_) => {
            return make(
                Verdict::Inconclusive,
                vec![ReasonCode::TargetNotReady],
                Completion::Complete,
            );
        }
        SampleOutcome::Success(_) => {}
    }

    // 5. Fewer than N samples: stop -> NOT_RUN (PLAN §3, §4); no stop -> INCONCLUSIVE.
    if attempted < u64::from(f.requests) {
        return match f.stop {
            Some(s) => make(
                Verdict::NotRun,
                vec![s.reason(), ReasonCode::WorkloadIncomplete],
                Completion::Incomplete,
            ),
            None => make(
                Verdict::Inconclusive,
                vec![ReasonCode::WorkloadIncomplete],
                Completion::Incomplete,
            ),
        };
    }

    // 6. Full workload: compare with the thresholds. A stop after the last sample is ignored.
    // Both values exist here (attempted == requests >= 1); `is_some_and` stays false if one is
    // missing, but then the checks above would already have returned.
    let mut reasons = Vec::new();
    if error_rate.is_some_and(|r| r > f.max_error_rate) {
        reasons.push(ReasonCode::ErrorRateExceeded);
    }
    if p95_ms.is_some_and(|p| p > f.max_p95_ms) {
        reasons.push(ReasonCode::LatencyExceeded);
    }
    if reasons.is_empty() {
        make(
            Verdict::Pass,
            vec![ReasonCode::ThresholdsMet],
            Completion::Complete,
        )
    } else {
        make(Verdict::Fail, reasons, Completion::Complete)
    }
}

/// Which request the sender closure must send. `Load(n)` is request number n, 1..=requests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Preflight,
    Load(u32),
}

/// State shared by all workers of one run. Atomics let several futures update it through `&`.
struct Shared<'a> {
    requests: u32,
    matcher: &'a MatcherSpec,
    /// Last request number handed out.
    next: AtomicU32,
    in_flight: AtomicU32,
    max_in_flight: AtomicU32,
}

/// One closed-loop worker: take the next request number, send, record, repeat. Stops when the
/// numbers run out or a stop is signalled. Returns (numbers taken, recorded samples). A request
/// dropped by a stop counts as taken but is not recorded.
async fn worker<F, Fut>(
    send_one: &F,
    shared: &Shared<'_>,
    mut stop_rx: watch::Receiver<Option<Stop>>,
) -> (u32, Vec<Sample>)
where
    F: Fn(Kind) -> Fut,
    Fut: Future<Output = Result<TransportResponse, TransportError>>,
{
    let mut taken = 0;
    let mut samples = Vec::new();
    loop {
        if current_stop(&stop_rx).is_some() {
            break;
        }
        let number = shared.next.fetch_add(1, Ordering::Relaxed) + 1;
        if number > shared.requests {
            break;
        }
        taken += 1;
        let now = shared.in_flight.fetch_add(1, Ordering::Relaxed) + 1;
        shared.max_in_flight.fetch_max(now, Ordering::Relaxed);
        let started = Instant::now();
        let result = tokio::select! {
            biased;
            _ = wait_stop(&mut stop_rx) => None,
            r = send_one(Kind::Load(number)) => Some(r),
        };
        shared.in_flight.fetch_sub(1, Ordering::Relaxed);
        let Some(result) = result else { break };
        samples.push(Sample {
            outcome: classify(&result, shared.matcher),
            duration: started.elapsed(),
        });
        // `result` (and its body) is dropped here.
    }
    (taken, samples)
}

/// PLAN §7: preflight once, then `min(concurrency, requests)` workers share `requests` numbers.
/// No retry, no ramp. A stop drops what is in flight. Precondition: `cfg` passed `validate`.
pub async fn run_with<F, Fut>(
    send_one: F,
    cfg: &Load,
    mut stop_rx: watch::Receiver<Option<Stop>>,
) -> CheckResult
where
    F: Fn(Kind) -> Fut,
    Fut: Future<Output = Result<TransportResponse, TransportError>>,
{
    let start = Instant::now();
    let shared = Shared {
        requests: cfg.requests,
        matcher: &cfg.response_match,
        next: AtomicU32::new(0),
        in_flight: AtomicU32::new(0),
        max_in_flight: AtomicU32::new(0),
    };
    let mut preflight: Option<(SampleOutcome, VerifyEvidence)> = None;
    let mut samples: Vec<Sample> = Vec::new();
    let mut dispatched = 0;
    let mut load_elapsed = Duration::ZERO;

    'run: {
        // Never dispatch once a stop is signalled (also covers "stop before start").
        if current_stop(&stop_rx).is_some() {
            break 'run;
        }
        let offset = start.elapsed();
        let sent_at = Instant::now();
        let result = tokio::select! {
            biased;
            _ = wait_stop(&mut stop_rx) => break 'run,
            r = send_one(Kind::Preflight) => r,
        };
        let outcome = classify(&result, &cfg.response_match);
        let evidence = VerifyEvidence {
            status: result.as_ref().ok().map(|r| r.status),
            error: result.as_ref().err().map(|e| e.code),
            offset_ms: offset.as_millis() as u64,
            elapsed_ms: sent_at.elapsed().as_millis() as u64,
        };
        preflight = Some((outcome, evidence));
        if !matches!(outcome, SampleOutcome::Success(_)) {
            break 'run;
        }

        // Futures are lazy: `join_all` polls every worker once, so all of them start together.
        let load_start = Instant::now();
        let workers = cfg.concurrency.min(cfg.requests);
        let done =
            join_all((0..workers).map(|_| worker(&send_one, &shared, stop_rx.clone()))).await;
        load_elapsed = load_start.elapsed();
        for (taken, recorded) in done {
            dispatched += taken;
            samples.extend(recorded);
        }
    }

    // A stop that arrives after all N samples are recorded does not change the verdict
    // (`evaluate` ignores it then).
    let stopped = current_stop(&stop_rx);
    let max_in_flight = shared.max_in_flight.load(Ordering::Relaxed);
    let evaluation = evaluate(&Facts {
        requests: cfg.requests,
        concurrency: cfg.concurrency,
        max_error_rate: cfg.max_error_rate,
        max_p95_ms: cfg.max_p95_ms,
        preflight: preflight.as_ref().map(|(o, _)| *o),
        dispatched,
        max_in_flight,
        samples: &samples,
        stop: stopped,
    });

    let mut status_counts: BTreeMap<u16, u32> = BTreeMap::new();
    let mut error_counts: BTreeMap<TransportErrorCode, u32> = BTreeMap::new();
    for sample in &samples {
        match sample.outcome {
            SampleOutcome::Success(s) | SampleOutcome::Mismatch(s) => {
                *status_counts.entry(s).or_insert(0) += 1;
            }
            SampleOutcome::Transport(code) => *error_counts.entry(code).or_insert(0) += 1,
        }
    }
    let load_elapsed_ms = load_elapsed.as_millis() as u64;
    let attempted = samples.len() as u32;

    CheckResult {
        check: CheckName::Load,
        verdict: evaluation.verdict,
        reasons: evaluation.reasons,
        completion: evaluation.completion,
        evidence: Some(Evidence::Load(LoadEvidence {
            requests: cfg.requests,
            concurrency: cfg.concurrency,
            max_error_rate: cfg.max_error_rate,
            max_p95_ms: cfg.max_p95_ms,
            preflight: preflight.map(|(_, e)| e),
            dispatched,
            attempted,
            succeeded: evaluation.succeeded,
            failed: evaluation.failed,
            timed_out: evaluation.timed_out,
            error_rate: evaluation.error_rate,
            p50_ms: evaluation.p50_ms,
            p95_ms: evaluation.p95_ms,
            max_ms: evaluation.max_ms,
            max_in_flight,
            load_elapsed_ms,
            throughput_rps: throughput_rps(attempted, load_elapsed_ms),
            status_counts,
            error_counts,
        })),
    }
}

/// The real check: `run_with` sending the same request (preflight and load) through the transport.
pub async fn run(
    transport: &Transport,
    cfg: &Load,
    request: &ResolvedRequest,
    stop: watch::Receiver<Option<Stop>>,
) -> CheckResult {
    run_with(|_kind: Kind| send(transport, request), cfg, stop).await
}

/// One console line (PLAN §10), e.g.
/// `load PASS error_rate=0/0.01 p95=3/500ms attempted=100/100 samples=100 max_in_flight=10/10
/// timeouts=0 status=200:100 errors=none reasons=thresholds_met` (one line). Numbers and enums
/// only; `none` for a null or empty value.
pub fn console_line(result: &CheckResult) -> String {
    let reasons = if result.reasons.is_empty() {
        "none".to_string()
    } else {
        let names: Vec<&str> = result.reasons.iter().map(|r| r.as_str()).collect();
        names.join(",")
    };
    let head = format!(
        "{} {}",
        result.check.as_str(),
        result.verdict.as_str().to_uppercase()
    );
    let Some(Evidence::Load(e)) = &result.evidence else {
        return format!("{head} reasons={reasons}");
    };
    let or_none = |v: Option<String>| v.unwrap_or_else(|| "none".to_string());
    let join = |parts: Vec<String>| {
        if parts.is_empty() {
            "none".to_string()
        } else {
            parts.join(",")
        }
    };
    let status = join(
        e.status_counts
            .iter()
            .map(|(s, n)| format!("{s}:{n}"))
            .collect(),
    );
    let errors = join(
        e.error_counts
            .iter()
            .map(|(c, n)| format!("{}:{n}", c.as_str()))
            .collect(),
    );
    format!(
        "{head} error_rate={}/{} p95={}/{}ms attempted={}/{} samples={} max_in_flight={}/{} timeouts={} status={status} errors={errors} reasons={reasons}",
        or_none(e.error_rate.map(|r| r.to_string())),
        e.max_error_rate,
        or_none(e.p95_ms.map(|p| p.to_string())),
        e.max_p95_ms,
        e.attempted,
        e.requests,
        e.attempted,
        e.max_in_flight,
        e.concurrency,
        e.timed_out,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const OK: SampleOutcome = SampleOutcome::Success(200);
    const TMO: SampleOutcome = SampleOutcome::Transport(TransportErrorCode::Timeout);

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    fn spec(status: &[u16]) -> MatcherSpec {
        MatcherSpec {
            status_in: status.to_vec(),
            json_equals: None,
            json_exists: None,
        }
    }

    fn resp(
        status: u16,
        body: Option<serde_json::Value>,
    ) -> Result<TransportResponse, TransportError> {
        Ok(TransportResponse {
            status,
            elapsed: ms(1),
            body,
        })
    }

    /// `ok` successes then `bad` failures (given outcome), all with the same duration.
    fn samples(ok: usize, bad: usize, bad_outcome: SampleOutcome, d: Duration) -> Vec<Sample> {
        let mut v = vec![
            Sample {
                outcome: OK,
                duration: d
            };
            ok
        ];
        v.extend(vec![
            Sample {
                outcome: bad_outcome,
                duration: d
            };
            bad
        ]);
        v
    }

    /// Defaults from the handoff: 100 requests, concurrency 10, 0.01, 500 ms, healthy preflight.
    fn facts(samples: &[Sample]) -> Facts<'_> {
        Facts {
            requests: 100,
            concurrency: 10,
            max_error_rate: 0.01,
            max_p95_ms: 500,
            preflight: Some(OK),
            dispatched: samples.len() as u32,
            max_in_flight: 10,
            samples,
            stop: None,
        }
    }

    fn check(e: &Evaluation, verdict: Verdict, reasons: &[ReasonCode], completion: Completion) {
        assert_eq!(e.verdict, verdict);
        assert_eq!(e.reasons, reasons);
        assert_eq!(e.completion, completion);
    }

    // ---- classify ----

    #[test]
    fn classify_200_matches() {
        assert_eq!(classify(&resp(200, None), &spec(&[200])), OK);
    }

    #[test]
    fn classify_500_is_mismatch() {
        assert_eq!(
            classify(&resp(500, None), &spec(&[200])),
            SampleOutcome::Mismatch(500)
        );
    }

    #[test]
    fn classify_missing_body_with_json_exists_is_mismatch() {
        let m = MatcherSpec {
            status_in: vec![200],
            json_equals: None,
            json_exists: Some(vec!["/ok".to_string()]),
        };
        assert_eq!(classify(&resp(200, None), &m), SampleOutcome::Mismatch(200));
        assert_eq!(classify(&resp(200, Some(json!({"ok": true}))), &m), OK);
    }

    #[test]
    fn classify_timeout_and_connect_failed_are_transport() {
        let t: Result<TransportResponse, TransportError> = Err(TransportErrorCode::Timeout.into());
        let c: Result<TransportResponse, TransportError> =
            Err(TransportErrorCode::ConnectFailed.into());
        assert_eq!(classify(&t, &spec(&[200])), TMO);
        assert_eq!(
            classify(&c, &spec(&[200])),
            SampleOutcome::Transport(TransportErrorCode::ConnectFailed)
        );
    }

    // ---- ceil_ms / nearest_rank_ms / throughput ----

    #[test]
    fn ceil_ms_rounds_up() {
        assert_eq!(ceil_ms(Duration::ZERO), 0);
        assert_eq!(ceil_ms(Duration::from_nanos(1)), 1);
        assert_eq!(ceil_ms(ms(500)), 500);
        assert_eq!(ceil_ms(Duration::from_nanos(500_000_400)), 501);
    }

    fn one_to(n: u64) -> Vec<Duration> {
        (1..=n).map(ms).collect()
    }

    #[test]
    fn nearest_rank_known_arrays() {
        assert_eq!(nearest_rank_ms(&one_to(1), 95), Some(1));
        assert_eq!(nearest_rank_ms(&one_to(10), 95), Some(10));
        assert_eq!(nearest_rank_ms(&one_to(20), 95), Some(19));
        assert_eq!(nearest_rank_ms(&one_to(21), 95), Some(20));
        assert_eq!(nearest_rank_ms(&one_to(100), 95), Some(95));
        assert_eq!(nearest_rank_ms(&one_to(100), 50), Some(50));
        assert_eq!(nearest_rank_ms(&one_to(100), 100), Some(100));
    }

    #[test]
    fn nearest_rank_unsorted_duplicates_and_empty() {
        let unsorted = [ms(30), ms(10), ms(20), ms(50), ms(40)];
        assert_eq!(nearest_rank_ms(&unsorted, 50), Some(30));
        assert_eq!(nearest_rank_ms(&unsorted, 95), Some(50));
        let dup = [ms(7); 5];
        assert_eq!(nearest_rank_ms(&dup, 95), Some(7));
        assert_eq!(nearest_rank_ms(&[], 95), None);
    }

    #[test]
    fn throughput_none_when_elapsed_zero() {
        assert_eq!(throughput_rps(100, 0), None);
        assert_eq!(throughput_rps(100, 1000), Some(100.0));
    }

    // ---- evaluate: matrix rows ----

    #[test]
    fn l1_healthy_passes() {
        let s = samples(100, 0, OK, ms(3));
        let e = evaluate(&facts(&s));
        check(
            &e,
            Verdict::Pass,
            &[ReasonCode::ThresholdsMet],
            Completion::Complete,
        );
        assert_eq!((e.succeeded, e.failed, e.timed_out), (100, 0, 0));
        assert_eq!(e.error_rate, Some(0.0));
        assert_eq!(e.p95_ms, Some(3));
    }

    #[test]
    fn l2_slow_fails_latency() {
        let s = samples(20, 0, OK, ms(300));
        let f = Facts {
            requests: 20,
            max_p95_ms: 100,
            ..facts(&s)
        };
        let e = evaluate(&f);
        check(
            &e,
            Verdict::Fail,
            &[ReasonCode::LatencyExceeded],
            Completion::Complete,
        );
        assert!(e.p95_ms >= Some(300));
    }

    #[test]
    fn l3_flaky_fails_error_rate() {
        let s = samples(90, 10, SampleOutcome::Mismatch(500), ms(3));
        let e = evaluate(&facts(&s));
        check(
            &e,
            Verdict::Fail,
            &[ReasonCode::ErrorRateExceeded],
            Completion::Complete,
        );
        assert_eq!(e.failed, 10);
    }

    #[test]
    fn l4_timeouts_count_as_failed_and_sampled() {
        // 18 fast successes + 2 slow timeouts: p95 (rank 19) is 500 only if timeouts are sampled.
        let mut s = samples(18, 0, OK, ms(3));
        s.extend(samples(0, 2, TMO, ms(500)));
        let f = Facts {
            requests: 20,
            max_p95_ms: 5000,
            ..facts(&s)
        };
        let e = evaluate(&f);
        check(
            &e,
            Verdict::Fail,
            &[ReasonCode::ErrorRateExceeded],
            Completion::Complete,
        );
        assert_eq!((e.failed, e.timed_out), (2, 2));
        assert_eq!(e.p95_ms, Some(500));
        assert_eq!(e.max_ms, Some(500));
    }

    #[test]
    fn l5_error_rate_exactly_at_limit_passes() {
        let s = samples(99, 1, SampleOutcome::Mismatch(500), ms(3));
        let e = evaluate(&facts(&s));
        check(
            &e,
            Verdict::Pass,
            &[ReasonCode::ThresholdsMet],
            Completion::Complete,
        );
        assert_eq!(e.error_rate, Some(0.01));
    }

    #[test]
    fn l6_error_rate_above_limit_fails() {
        let s = samples(98, 2, SampleOutcome::Mismatch(500), ms(3));
        let e = evaluate(&facts(&s));
        check(
            &e,
            Verdict::Fail,
            &[ReasonCode::ErrorRateExceeded],
            Completion::Complete,
        );
    }

    #[test]
    fn l7_zero_max_error_rate() {
        let good = samples(100, 0, OK, ms(3));
        let f = Facts {
            max_error_rate: 0.0,
            ..facts(&good)
        };
        check(
            &evaluate(&f),
            Verdict::Pass,
            &[ReasonCode::ThresholdsMet],
            Completion::Complete,
        );

        let bad = samples(99, 1, SampleOutcome::Mismatch(500), ms(3));
        let f = Facts {
            max_error_rate: 0.0,
            ..facts(&bad)
        };
        check(
            &evaluate(&f),
            Verdict::Fail,
            &[ReasonCode::ErrorRateExceeded],
            Completion::Complete,
        );
    }

    #[test]
    fn l8_p95_exactly_at_limit_passes() {
        let s = samples(100, 0, OK, ms(500));
        let e = evaluate(&facts(&s));
        check(
            &e,
            Verdict::Pass,
            &[ReasonCode::ThresholdsMet],
            Completion::Complete,
        );
        assert_eq!(e.p95_ms, Some(500));
    }

    #[test]
    fn l9_p95_one_ms_over_or_rounded_up_fails() {
        let s = samples(100, 0, OK, ms(501));
        check(
            &evaluate(&facts(&s)),
            Verdict::Fail,
            &[ReasonCode::LatencyExceeded],
            Completion::Complete,
        );

        // 500.0004 ms rounds up to 501 and must not pass a 500 ms limit.
        let s = samples(100, 0, OK, Duration::from_nanos(500_000_400));
        let e = evaluate(&facts(&s));
        check(
            &e,
            Verdict::Fail,
            &[ReasonCode::LatencyExceeded],
            Completion::Complete,
        );
        assert_eq!(e.p95_ms, Some(501));
    }

    #[test]
    fn l10_both_thresholds_exceeded_in_fixed_order() {
        let s = samples(98, 2, SampleOutcome::Mismatch(500), ms(501));
        check(
            &evaluate(&facts(&s)),
            Verdict::Fail,
            &[ReasonCode::ErrorRateExceeded, ReasonCode::LatencyExceeded],
            Completion::Complete,
        );
    }

    #[test]
    fn l11_matcher_mismatch_is_failed() {
        let s = samples(98, 2, SampleOutcome::Mismatch(200), ms(3));
        let e = evaluate(&facts(&s));
        check(
            &e,
            Verdict::Fail,
            &[ReasonCode::ErrorRateExceeded],
            Completion::Complete,
        );
        // Mismatch is an HTTP response, not a transport error: not a timeout.
        assert_eq!((e.failed, e.timed_out), (2, 0));
    }

    #[test]
    fn l12_transport_errors_are_failed() {
        for code in [
            TransportErrorCode::ConnectFailed,
            TransportErrorCode::BodyTooLarge,
            TransportErrorCode::Other,
        ] {
            let s = samples(98, 2, SampleOutcome::Transport(code), ms(3));
            let e = evaluate(&facts(&s));
            check(
                &e,
                Verdict::Fail,
                &[ReasonCode::ErrorRateExceeded],
                Completion::Complete,
            );
            assert_eq!((e.failed, e.timed_out), (2, 0));
        }
    }

    #[test]
    fn l13_preflight_mismatch_is_target_not_ready() {
        let f = Facts {
            preflight: Some(SampleOutcome::Mismatch(401)),
            dispatched: 0,
            max_in_flight: 0,
            ..facts(&[])
        };
        check(
            &evaluate(&f),
            Verdict::Inconclusive,
            &[ReasonCode::TargetNotReady],
            Completion::Complete,
        );
    }

    #[test]
    fn l14_preflight_transport_error_is_error() {
        let f = Facts {
            preflight: Some(SampleOutcome::Transport(TransportErrorCode::ConnectFailed)),
            dispatched: 0,
            max_in_flight: 0,
            ..facts(&[])
        };
        check(
            &evaluate(&f),
            Verdict::Error,
            &[ReasonCode::PreflightFailed, ReasonCode::TransportError],
            Completion::Complete,
        );
    }

    #[test]
    fn l15_preflight_timeout_is_error() {
        let f = Facts {
            preflight: Some(TMO),
            dispatched: 0,
            max_in_flight: 0,
            ..facts(&[])
        };
        check(
            &evaluate(&f),
            Verdict::Error,
            &[ReasonCode::PreflightFailed, ReasonCode::TransportError],
            Completion::Complete,
        );
    }

    #[test]
    fn l16_stop_before_start_is_not_run() {
        for (stop, reason) in [
            (Stop::Cancelled, ReasonCode::Cancelled),
            (Stop::DeadlineExceeded, ReasonCode::DeadlineExceeded),
        ] {
            let f = Facts {
                preflight: None,
                dispatched: 0,
                max_in_flight: 0,
                stop: Some(stop),
                ..facts(&[])
            };
            check(
                &evaluate(&f),
                Verdict::NotRun,
                &[reason],
                Completion::Incomplete,
            );
        }
        // No preflight and no stop must never look like a result.
        let f = Facts {
            preflight: None,
            dispatched: 0,
            max_in_flight: 0,
            ..facts(&[])
        };
        check(
            &evaluate(&f),
            Verdict::NotRun,
            &[ReasonCode::NotStarted],
            Completion::Incomplete,
        );
    }

    #[test]
    fn l19_early_deadline_with_all_successes_is_not_run() {
        let s = samples(50, 0, OK, ms(3));
        let f = Facts {
            dispatched: 60,
            stop: Some(Stop::DeadlineExceeded),
            ..facts(&s)
        };
        check(
            &evaluate(&f),
            Verdict::NotRun,
            &[ReasonCode::DeadlineExceeded, ReasonCode::WorkloadIncomplete],
            Completion::Incomplete,
        );
        let f = Facts {
            stop: Some(Stop::Cancelled),
            ..f
        };
        check(
            &evaluate(&f),
            Verdict::NotRun,
            &[ReasonCode::Cancelled, ReasonCode::WorkloadIncomplete],
            Completion::Incomplete,
        );
    }

    #[test]
    fn l20_stop_after_all_samples_keeps_normal_verdict() {
        let s = samples(100, 0, OK, ms(3));
        let f = Facts {
            stop: Some(Stop::Cancelled),
            ..facts(&s)
        };
        check(
            &evaluate(&f),
            Verdict::Pass,
            &[ReasonCode::ThresholdsMet],
            Completion::Complete,
        );
    }

    #[test]
    fn l21_fewer_samples_without_stop_is_inconclusive() {
        let s = samples(99, 0, OK, ms(3));
        let e = evaluate(&facts(&s));
        check(
            &e,
            Verdict::Inconclusive,
            &[ReasonCode::WorkloadIncomplete],
            Completion::Incomplete,
        );
    }

    #[test]
    fn l22_broken_runner_invariants_are_error() {
        let s = samples(100, 0, OK, ms(3));
        let broken = [
            // attempted > dispatched
            Facts {
                dispatched: 99,
                ..facts(&s)
            },
            // dispatched > requests
            Facts {
                dispatched: 101,
                ..facts(&s)
            },
            // max_in_flight > min(concurrency, requests)
            Facts {
                max_in_flight: 11,
                ..facts(&s)
            },
            Facts {
                requests: 5,
                concurrency: 10,
                dispatched: 5,
                max_in_flight: 6,
                ..facts(&s[..5])
            },
        ];
        for f in &broken {
            check(
                &evaluate(f),
                Verdict::Error,
                &[ReasonCode::RunnerError],
                Completion::Complete,
            );
        }
    }

    // ---- review S2: NaN / empty must never PASS ----

    #[test]
    fn empty_samples_never_pass_and_have_no_rates() {
        let f = Facts {
            dispatched: 0,
            max_in_flight: 0,
            ..facts(&[])
        };
        let e = evaluate(&f);
        check(
            &e,
            Verdict::Inconclusive,
            &[ReasonCode::WorkloadIncomplete],
            Completion::Incomplete,
        );
        assert_eq!(
            (e.error_rate, e.p50_ms, e.p95_ms, e.max_ms),
            (None, None, None, None)
        );
    }

    #[test]
    fn zero_requests_or_nan_limit_is_runner_error_not_pass() {
        let f = Facts {
            requests: 0,
            dispatched: 0,
            max_in_flight: 0,
            ..facts(&[])
        };
        check(
            &evaluate(&f),
            Verdict::Error,
            &[ReasonCode::RunnerError],
            Completion::Complete,
        );
        let s = samples(100, 0, OK, ms(3));
        let f = Facts {
            max_error_rate: f64::NAN,
            ..facts(&s)
        };
        check(
            &evaluate(&f),
            Verdict::Error,
            &[ReasonCode::RunnerError],
            Completion::Complete,
        );
    }

    #[test]
    fn error_rate_is_finite_whenever_present() {
        for (ok, bad) in [(1, 0), (0, 1), (5, 5), (100, 0)] {
            let s = samples(ok, bad, SampleOutcome::Mismatch(500), ms(3));
            let f = Facts {
                requests: (ok + bad) as u32,
                ..facts(&s)
            };
            let rate = evaluate(&f).error_rate;
            assert!(rate.is_some_and(f64::is_finite));
        }
    }
    // ---- run_with (paused clock, scripted sender, no network) ----

    use crate::report::Evidence;
    use std::cell::{Cell, RefCell};
    use std::pin::Pin;
    use tokio::time::sleep;

    const SENTINEL: &str = "SENTINEL-LOAD-BODY-5d2f";

    /// What the fake sender does on one call.
    #[derive(Clone, Copy)]
    enum Step {
        /// Answer with this status after `ms` of (virtual) time.
        Slow(u64, u16),
        Fail(TransportErrorCode),
        /// Never answer.
        Hang,
    }

    struct Script {
        pre: Step,
        /// What load request number n does.
        load: Box<dyn Fn(u32) -> Step>,
        /// Signal this stop when the load request with this number answers.
        stop_on_load: Option<(u32, watch::Sender<Option<Stop>>, Stop)>,
        calls: RefCell<Vec<Kind>>,
        in_flight: Cell<u32>,
        max_in_flight: Cell<u32>,
    }

    impl Script {
        fn new(pre: Step, load: impl Fn(u32) -> Step + 'static) -> Script {
            Script {
                pre,
                load: Box::new(load),
                stop_on_load: None,
                calls: RefCell::new(Vec::new()),
                in_flight: Cell::new(0),
                max_in_flight: Cell::new(0),
            }
        }

        /// Everything answers 200 after `ms`; the preflight is instant.
        fn healthy(ms: u64) -> Script {
            Script::new(Step::Slow(0, 200), move |_| Step::Slow(ms, 200))
        }

        fn kinds(&self) -> Vec<Kind> {
            self.calls.borrow().clone()
        }

        fn load_numbers(&self) -> Vec<u32> {
            self.kinds()
                .iter()
                .filter_map(|k| match k {
                    Kind::Load(n) => Some(*n),
                    Kind::Preflight => None,
                })
                .collect()
        }
    }

    // The named lifetime is needed: `'_` is not allowed inside the `Fn(..) -> ..` sugar.
    #[allow(clippy::needless_lifetimes)]
    fn sender<'a>(
        s: &'a Script,
    ) -> impl Fn(Kind) -> Pin<Box<dyn Future<Output = Result<TransportResponse, TransportError>> + 'a>>
    {
        move |kind| {
            s.calls.borrow_mut().push(kind);
            let step = match kind {
                Kind::Preflight => s.pre,
                Kind::Load(n) => (s.load)(n),
            };
            Box::pin(async move {
                // Count load requests only (the preflight runs alone). A future dropped by a
                // stop never reaches the decrement below, so `in_flight` stays up then.
                let is_load = matches!(kind, Kind::Load(_));
                if is_load {
                    s.in_flight.set(s.in_flight.get() + 1);
                    s.max_in_flight
                        .set(s.max_in_flight.get().max(s.in_flight.get()));
                }
                let result = match step {
                    Step::Slow(ms, status) => {
                        sleep(ms_dur(ms)).await;
                        // The body reflects the sentinel; it must never reach the result.
                        resp(status, Some(json!({ "id": SENTINEL })))
                    }
                    Step::Fail(code) => Err(TransportError::from(code)),
                    Step::Hang => std::future::pending().await,
                };
                if let (Kind::Load(n), Some((at, tx, stop))) = (kind, &s.stop_on_load)
                    && n == *at
                {
                    let _ = tx.send(Some(*stop));
                }
                if is_load {
                    s.in_flight.set(s.in_flight.get() - 1);
                }
                result
            })
        }
    }

    fn ms_dur(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    fn cfg(requests: u32, concurrency: u32) -> Load {
        serde_json::from_value(json!({
            "request": {"method": "GET", "path": "/critical"},
            "response_match": {"status_in": [200]},
            "requests": requests,
            "concurrency": concurrency,
            "max_error_rate": 0.01,
            "max_p95_ms": 500
        }))
        .expect("test config must parse")
    }

    fn no_stop() -> (watch::Sender<Option<Stop>>, watch::Receiver<Option<Stop>>) {
        watch::channel(None)
    }

    /// Run the script; optionally signal `stop` after `ms` of virtual time.
    async fn go(script: &Script, cfg: &Load, stop: Option<(u64, Stop)>) -> CheckResult {
        let (tx, rx) = no_stop();
        if let Some((ms, s)) = stop {
            tokio::spawn(async move {
                sleep(ms_dur(ms)).await;
                let _ = tx.send(Some(s));
            });
        } else {
            std::mem::forget(tx); // keep the sender alive: "no stop ever"
        }
        run_with(sender(script), cfg, rx).await
    }

    fn lev(r: &CheckResult) -> &LoadEvidence {
        match r.evidence.as_ref() {
            Some(Evidence::Load(e)) => e,
            _ => panic!("load always has load evidence"),
        }
    }

    /// attempted <= dispatched <= requests, calls match, in-flight is bounded and agrees.
    #[track_caller]
    fn assert_invariants(r: &CheckResult, s: &Script, c: &Load) {
        let e = lev(r);
        assert!(e.attempted <= e.dispatched);
        assert!(e.dispatched <= e.requests);
        assert_eq!(e.dispatched as usize, s.load_numbers().len());
        assert!(e.max_in_flight <= c.concurrency.min(c.requests));
        assert_eq!(e.max_in_flight, s.max_in_flight.get());
        let mut numbers = s.load_numbers();
        numbers.sort_unstable();
        numbers.dedup();
        assert_eq!(
            numbers.len(),
            s.load_numbers().len(),
            "no number sent twice"
        );
        assert_eq!(
            e.attempted,
            e.status_counts.values().sum::<u32>() + e.error_counts.values().sum::<u32>()
        );
        assert_eq!(e.succeeded + e.failed, e.attempted);
    }

    #[tokio::test(start_paused = true)]
    async fn l1_healthy_sends_preflight_then_each_number_once() {
        let s = Script::healthy(10);
        let c = cfg(100, 10);
        let r = go(&s, &c, None).await;
        assert_eq!(r.verdict, Verdict::Pass);
        assert_eq!(r.reasons, [ReasonCode::ThresholdsMet]);
        assert_eq!(r.completion, Completion::Complete);
        assert_eq!(s.kinds().len(), 101);
        assert_eq!(s.kinds()[0], Kind::Preflight);
        let mut numbers = s.load_numbers();
        numbers.sort_unstable();
        assert_eq!(numbers, (1..=100).collect::<Vec<u32>>());
        let e = lev(&r);
        assert_eq!(e.max_in_flight, 10);
        assert_eq!(e.load_elapsed_ms, 100);
        assert_eq!(e.throughput_rps, Some(1000.0));
        assert_eq!(
            (e.dispatched, e.attempted, e.succeeded, e.failed),
            (100, 100, 100, 0)
        );
        assert_eq!(e.status_counts, BTreeMap::from([(200, 100)]));
        assert!(e.error_counts.is_empty());
        let pre = e.preflight.as_ref().expect("preflight evidence");
        assert_eq!((pre.status, pre.error), (Some(200), None));
        assert_eq!(
            console_line(&r),
            "load PASS error_rate=0/0.01 p95=10/500ms attempted=100/100 samples=100 max_in_flight=10/10 timeouts=0 status=200:100 errors=none reasons=thresholds_met"
        );
        assert_invariants(&r, &s, &c);
    }

    #[tokio::test(start_paused = true)]
    async fn p95_comes_from_the_scripted_durations() {
        // Request n takes n ms: sorted durations are 1..=100, so p50 50, p95 95, max 100.
        let s = Script::new(Step::Slow(0, 200), |n| Step::Slow(u64::from(n), 200));
        let c = cfg(100, 10);
        let r = go(&s, &c, None).await;
        let e = lev(&r);
        assert_eq!(
            (e.p50_ms, e.p95_ms, e.max_ms),
            (Some(50), Some(95), Some(100))
        );
        assert_eq!(r.verdict, Verdict::Pass);
        assert_invariants(&r, &s, &c);
    }

    #[tokio::test(start_paused = true)]
    async fn l2_slow_responses_fail_latency() {
        let s = Script::new(Step::Slow(0, 200), |_| Step::Slow(300, 200));
        let mut c = cfg(20, 10);
        c.max_p95_ms = 100;
        let r = go(&s, &c, None).await;
        assert_eq!(r.verdict, Verdict::Fail);
        assert_eq!(r.reasons, [ReasonCode::LatencyExceeded]);
        assert_eq!(lev(&r).p95_ms, Some(300));
        assert_invariants(&r, &s, &c);
    }

    #[tokio::test(start_paused = true)]
    async fn l3_every_tenth_500_fails_error_rate() {
        let s = Script::new(Step::Slow(0, 200), |n| {
            Step::Slow(5, if n % 10 == 0 { 500 } else { 200 })
        });
        let c = cfg(100, 10);
        let r = go(&s, &c, None).await;
        assert_eq!(r.verdict, Verdict::Fail);
        assert_eq!(r.reasons, [ReasonCode::ErrorRateExceeded]);
        let e = lev(&r);
        assert_eq!(e.failed, 10);
        assert_eq!(e.status_counts, BTreeMap::from([(200, 90), (500, 10)]));
        assert_eq!(
            console_line(&r),
            "load FAIL error_rate=0.1/0.01 p95=5/500ms attempted=100/100 samples=100 max_in_flight=10/10 timeouts=0 status=200:90,500:10 errors=none reasons=error_rate_exceeded"
        );
        assert_invariants(&r, &s, &c);
    }

    #[tokio::test(start_paused = true)]
    async fn l4_timeouts_are_not_retried_and_are_sampled() {
        // Load 7 and 13 time out after 500 ms; all others answer in 3 ms.
        let s = Script::new(Step::Slow(0, 200), |n| {
            if n == 7 || n == 13 {
                Step::Fail(TransportErrorCode::Timeout)
            } else {
                Step::Slow(3, 200)
            }
        });
        let c = cfg(20, 10);
        let r = go(&s, &c, None).await;
        assert_eq!(s.kinds().len(), 21, "a timeout is never repeated");
        assert_eq!(r.verdict, Verdict::Fail);
        assert_eq!(r.reasons, [ReasonCode::ErrorRateExceeded]);
        let e = lev(&r);
        assert_eq!((e.attempted, e.failed, e.timed_out), (20, 2, 2));
        assert_eq!(
            e.error_counts,
            BTreeMap::from([(TransportErrorCode::Timeout, 2)])
        );
        assert_eq!(e.status_counts, BTreeMap::from([(200, 18)]));
        assert!(console_line(&r).contains("timeouts=2 status=200:18 errors=timeout:2 "));
        assert_invariants(&r, &s, &c);
    }

    #[tokio::test(start_paused = true)]
    async fn l12_connect_failed_during_load_is_failed_and_not_retried() {
        let s = Script::new(Step::Slow(0, 200), |n| {
            if n <= 2 {
                Step::Fail(TransportErrorCode::ConnectFailed)
            } else {
                Step::Slow(3, 200)
            }
        });
        let c = cfg(20, 5);
        let r = go(&s, &c, None).await;
        assert_eq!(s.kinds().len(), 21);
        assert_eq!(r.verdict, Verdict::Fail);
        assert_eq!(r.reasons, [ReasonCode::ErrorRateExceeded]);
        let e = lev(&r);
        assert_eq!((e.failed, e.timed_out), (2, 0));
        assert_eq!(
            e.error_counts,
            BTreeMap::from([(TransportErrorCode::ConnectFailed, 2)])
        );
        assert_invariants(&r, &s, &c);
    }

    #[tokio::test(start_paused = true)]
    async fn l23_concurrency_above_requests_uses_requests_workers() {
        let s = Script::healthy(10);
        let c = cfg(5, 10);
        let r = go(&s, &c, None).await;
        assert_eq!(r.verdict, Verdict::Pass);
        assert_eq!(s.kinds().len(), 6);
        assert_eq!(lev(&r).max_in_flight, 5);
        assert_eq!(s.max_in_flight.get(), 5);
        assert_invariants(&r, &s, &c);
    }

    #[tokio::test(start_paused = true)]
    async fn l24_concurrency_one_is_sequential() {
        let s = Script::healthy(10);
        let c = cfg(10, 1);
        let r = go(&s, &c, None).await;
        assert_eq!(r.verdict, Verdict::Pass);
        assert_eq!(lev(&r).max_in_flight, 1);
        assert_eq!(lev(&r).load_elapsed_ms, 100);
        // One worker takes the numbers in order.
        assert_eq!(s.load_numbers(), (1..=10).collect::<Vec<u32>>());
        assert_invariants(&r, &s, &c);
    }

    #[tokio::test(start_paused = true)]
    async fn l13_l15_failing_preflight_sends_exactly_one_call() {
        let cases = [
            (
                Step::Slow(0, 401),
                Verdict::Inconclusive,
                vec![ReasonCode::TargetNotReady],
                (Some(401), None),
            ),
            (
                Step::Fail(TransportErrorCode::Timeout),
                Verdict::Error,
                vec![ReasonCode::PreflightFailed, ReasonCode::TransportError],
                (None, Some(TransportErrorCode::Timeout)),
            ),
            (
                Step::Fail(TransportErrorCode::ConnectFailed),
                Verdict::Error,
                vec![ReasonCode::PreflightFailed, ReasonCode::TransportError],
                (None, Some(TransportErrorCode::ConnectFailed)),
            ),
        ];
        for (pre, verdict, reasons, (status, error)) in cases {
            let s = Script::new(pre, |_| Step::Slow(1, 200));
            let c = cfg(100, 10);
            let r = go(&s, &c, None).await;
            assert_eq!(s.kinds(), [Kind::Preflight]);
            assert_eq!(r.verdict, verdict);
            assert_eq!(r.reasons, reasons);
            assert_eq!(r.completion, Completion::Complete);
            let e = lev(&r);
            assert_eq!((e.dispatched, e.attempted), (0, 0));
            let p = e.preflight.as_ref().expect("preflight evidence");
            assert_eq!((p.status, p.error), (status, error));
            assert_eq!(e.p95_ms, None);
            assert_invariants(&r, &s, &c);
        }
    }

    #[tokio::test(start_paused = true)]
    async fn l16_stop_before_start_sends_nothing() {
        for (stop, reason) in [
            (Stop::Cancelled, ReasonCode::Cancelled),
            (Stop::DeadlineExceeded, ReasonCode::DeadlineExceeded),
        ] {
            let s = Script::healthy(10);
            let c = cfg(100, 10);
            let (tx, rx) = no_stop();
            tx.send(Some(stop)).expect("receiver alive");
            let r = run_with(sender(&s), &c, rx).await;
            assert_eq!(r.verdict, Verdict::NotRun);
            assert_eq!(r.reasons, [reason]);
            assert_eq!(r.completion, Completion::Incomplete);
            assert!(s.kinds().is_empty());
            let e = lev(&r);
            assert!(e.preflight.is_none());
            assert_eq!((e.dispatched, e.attempted), (0, 0));
            assert_invariants(&r, &s, &c);
        }
    }

    #[tokio::test(start_paused = true)]
    async fn l17_cancel_while_preflight_is_in_flight() {
        let s = Script::new(Step::Hang, |_| Step::Slow(1, 200));
        let c = cfg(100, 10);
        let r = go(&s, &c, Some((10, Stop::Cancelled))).await;
        assert_eq!(s.kinds(), [Kind::Preflight]);
        assert_eq!(r.verdict, Verdict::NotRun);
        assert_eq!(r.reasons, [ReasonCode::Cancelled]);
        assert_eq!(r.completion, Completion::Incomplete);
        assert!(lev(&r).preflight.is_none());
        assert_invariants(&r, &s, &c);
    }

    #[tokio::test(start_paused = true)]
    async fn l18_cancel_during_load_stops_new_dispatch() {
        // 10 workers, 10 ms per request: batches finish at 10, 20, 30 ms (30 recorded);
        // cancel at 35 ms drops load 31..=40 in flight.
        let s = Script::healthy(10);
        let c = cfg(100, 10);
        let r = go(&s, &c, Some((35, Stop::Cancelled))).await;
        assert_eq!(r.verdict, Verdict::NotRun);
        assert_eq!(
            r.reasons,
            [ReasonCode::Cancelled, ReasonCode::WorkloadIncomplete]
        );
        assert_eq!(r.completion, Completion::Incomplete);
        let e = lev(&r);
        assert_eq!((e.attempted, e.dispatched), (30, 40));
        assert_eq!(s.kinds().len(), 41, "no Load(n) after the stop");
        assert_eq!(*s.load_numbers().iter().max().expect("sent"), 40);
        assert!(e.preflight.is_some());
        assert_invariants(&r, &s, &c);
    }

    #[tokio::test(start_paused = true)]
    async fn l18b_stop_after_preflight_before_workers_start_sends_no_load() {
        let (tx, rx) = no_stop(); // `tx` lives until the end of the test
        let calls = RefCell::new(Vec::new());
        let send = |kind: Kind| {
            calls.borrow_mut().push(kind);
            let tx = &tx;
            async move {
                if kind == Kind::Preflight {
                    tx.send(Some(Stop::Cancelled)).expect("receiver alive");
                }
                resp(200, None)
            }
        };
        let c = cfg(100, 10);
        let r = run_with(send, &c, rx).await;
        assert_eq!(*calls.borrow(), [Kind::Preflight]);
        assert_eq!(r.verdict, Verdict::NotRun);
        assert_eq!(
            r.reasons,
            [ReasonCode::Cancelled, ReasonCode::WorkloadIncomplete]
        );
        assert_eq!(r.completion, Completion::Incomplete);
        let e = lev(&r);
        assert_eq!((e.dispatched, e.attempted), (0, 0));
        assert!(e.preflight.is_some());
    }

    #[tokio::test(start_paused = true)]
    async fn load_elapsed_excludes_the_preflight() {
        let s = Script::new(Step::Slow(7, 200), |_| Step::Slow(10, 200));
        let r = go(&s, &cfg(100, 10), None).await;
        let e = lev(&r);
        assert_eq!(e.load_elapsed_ms, 100);
        assert_eq!(e.preflight.as_ref().expect("preflight").elapsed_ms, 7);
    }

    #[tokio::test(start_paused = true)]
    async fn l19_deadline_during_load_with_all_successes_is_not_run() {
        let s = Script::healthy(10);
        let c = cfg(100, 10);
        let r = go(&s, &c, Some((55, Stop::DeadlineExceeded))).await;
        assert_eq!(r.verdict, Verdict::NotRun);
        assert_eq!(
            r.reasons,
            [ReasonCode::DeadlineExceeded, ReasonCode::WorkloadIncomplete]
        );
        assert_eq!(r.completion, Completion::Incomplete);
        let e = lev(&r);
        assert_eq!((e.attempted, e.failed), (50, 0));
        assert_invariants(&r, &s, &c);
    }

    #[tokio::test(start_paused = true)]
    async fn l20_stop_after_the_last_sample_keeps_the_verdict() {
        let (tx, rx) = no_stop();
        let mut s = Script::healthy(10);
        s.stop_on_load = Some((5, tx, Stop::Cancelled));
        let c = cfg(5, 1);
        let r = run_with(sender(&s), &c, rx).await;
        assert_eq!(r.verdict, Verdict::Pass);
        assert_eq!(r.reasons, [ReasonCode::ThresholdsMet]);
        assert_eq!(r.completion, Completion::Complete);
        assert_eq!(lev(&r).attempted, 5);
        assert_invariants(&r, &s, &c);
    }

    #[tokio::test(start_paused = true)]
    async fn l26_sentinel_in_response_bodies_is_absent_from_debug_and_console() {
        // PASS path and FAIL path (every other request is a 500).
        for bad in [false, true] {
            let s = Script::new(Step::Slow(0, 200), move |n| {
                Step::Slow(2, if bad && n % 2 == 0 { 500 } else { 200 })
            });
            let c = cfg(20, 4);
            let r = go(&s, &c, None).await;
            assert_eq!(r.verdict, if bad { Verdict::Fail } else { Verdict::Pass });
            assert!(!format!("{r:?}").contains(SENTINEL));
            assert!(!console_line(&r).contains(SENTINEL));
        }
    }

    #[test]
    fn console_line_without_evidence_and_with_none_values() {
        let r = CheckResult {
            check: CheckName::Load,
            verdict: Verdict::NotRun,
            reasons: vec![],
            completion: Completion::Incomplete,
            evidence: None,
        };
        assert_eq!(console_line(&r), "load NOT_RUN reasons=none");
    }

    #[tokio::test(start_paused = true)]
    async fn console_line_shows_none_for_missing_values() {
        let s = Script::new(Step::Slow(0, 401), |_| Step::Slow(1, 200));
        let r = go(&s, &cfg(100, 10), None).await;
        assert_eq!(
            console_line(&r),
            "load INCONCLUSIVE error_rate=none/0.01 p95=none/500ms attempted=0/100 samples=0 max_in_flight=0/10 timeouts=0 status=none errors=none reasons=target_not_ready"
        );
    }
}
