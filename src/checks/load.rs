//! load check (PLAN §7). This file holds the pure parts: `classify` (one response -> outcome),
//! `ceil_ms`, `nearest_rank_ms`, `throughput_rps` and `evaluate` (all facts -> verdict).
//! The async runner (`run_with`, `run`, `console_line`) is added in P3.S4.
// ponytail: nothing calls these until S4 wires the runner; remove the allow then.
#![allow(dead_code)]

use std::time::Duration;

use crate::checks::Stop;
use crate::config::MatcherSpec;
use crate::http::{TransportError, TransportResponse};
use crate::report::{Completion, ReasonCode, TransportErrorCode, Verdict};

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
}
