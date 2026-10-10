//! brute_force check (PLAN §5). Pure parts: `classify` (one response -> one outcome) and
//! `evaluate` (all outcomes -> verdict). Async parts: `run_with` (the sequential loop, with a
//! closure as sender so tests need no network), `run` (real transport) and `console_line`.

use std::time::Duration;

use tokio::sync::watch;
use tokio::time::{Instant, sleep};

use crate::checks::Stop;
use crate::config::{BruteForce, MatcherSpec};
use crate::http::request::ResolvedRequest;
use crate::http::{Transport, TransportError, TransportResponse, send};
use crate::report::{
    AttemptEvidence, BruteForceEvidence, CheckName, CheckResult, Completion, Outcome, ReasonCode,
    TransportErrorCode, Verdict,
};

/// Result of classifying one attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Classified {
    pub outcome: Outcome,
    /// More than one matcher matched at runtime (`validate` should prevent this).
    pub overlap: bool,
    /// Set only when the outcome is `TransportError`.
    pub error: Option<TransportErrorCode>,
}

/// Thresholds `evaluate` needs, copied from the config.
#[derive(Debug, Clone, Copy)]
pub struct Thresholds {
    pub attempts: u32,
    pub protect_by_attempt: u32,
    pub protected_tail_count: u32,
}

/// What `evaluate` decides.
#[derive(Debug, PartialEq, Eq)]
pub struct Evaluation {
    pub verdict: Verdict,
    pub reasons: Vec<ReasonCode>,
    pub completion: Completion,
    pub first_protected_attempt: Option<u32>,
    pub protected_tail_observed: u32,
}

/// PLAN §5 step 4: exactly one matcher must match. 0 matches = unexpected,
/// more than one = unexpected plus the overlap flag. A transport error is its own outcome.
pub fn classify(
    result: &Result<TransportResponse, TransportError>,
    success: &MatcherSpec,
    rejection: &MatcherSpec,
    protection: &MatcherSpec,
) -> Classified {
    let response = match result {
        Ok(response) => response,
        Err(e) => {
            return Classified {
                outcome: Outcome::TransportError,
                overlap: false,
                error: Some(e.code),
            };
        }
    };
    let body = response.body.as_ref();
    let is_success = success.matches(response.status, body);
    let is_rejection = rejection.matches(response.status, body);
    let is_protection = protection.matches(response.status, body);

    let matched = [is_success, is_rejection, is_protection]
        .iter()
        .filter(|m| **m)
        .count();
    let outcome = if matched > 1 || matched == 0 {
        Outcome::Unexpected
    } else if is_success {
        Outcome::Success
    } else if is_rejection {
        Outcome::NormalRejection
    } else {
        Outcome::Protection
    };
    Classified {
        outcome,
        overlap: matched > 1,
        error: None,
    }
}

/// First protected probe number (1-based) and how many protected probes sit at the very
/// end (the real count, not capped).
fn protection_stats(probes: &[Classified]) -> (Option<u32>, u32) {
    let first = probes
        .iter()
        .position(|p| p.outcome == Outcome::Protection)
        .map(|i| i as u32 + 1);
    let tail = probes
        .iter()
        .rev()
        .take_while(|p| p.outcome == Outcome::Protection)
        .count() as u32;
    (first, tail)
}

/// PLAN §5 Verdict, in the precedence order of the P1 handoff.
/// `baseline` is `None` when the check stopped before the baseline finished.
/// `probes` holds only the finished probes, in order.
pub fn evaluate(
    baseline: Option<Classified>,
    probes: &[Classified],
    stop: Option<Stop>,
    t: Thresholds,
) -> Evaluation {
    let (first_protected_attempt, protected_tail_observed) = protection_stats(probes);
    let make = |verdict, reasons, completion| Evaluation {
        verdict,
        reasons,
        completion,
        first_protected_attempt,
        protected_tail_observed,
    };

    // 1. Stopped before the baseline finished.
    let Some(baseline) = baseline else {
        let reason = match stop {
            Some(s) => s.reason(),
            None => ReasonCode::NotStarted, // no baseline and no stop should not happen
        };
        return make(Verdict::NotRun, vec![reason], Completion::Incomplete);
    };

    // 2. Baseline did not succeed: no probes were sent.
    if baseline.outcome != Outcome::Success {
        let (verdict, reasons) = match baseline.outcome {
            Outcome::Protection => (
                Verdict::Inconclusive,
                vec![ReasonCode::AccountAlreadyLocked],
            ),
            Outcome::NormalRejection => (Verdict::Inconclusive, vec![ReasonCode::BaselineFailed]),
            Outcome::TransportError => (
                Verdict::Error,
                vec![ReasonCode::BaselineFailed, ReasonCode::TransportError],
            ),
            _ => {
                let mut reasons = vec![ReasonCode::BaselineFailed];
                if baseline.overlap {
                    reasons.push(ReasonCode::OverlappingRules);
                }
                (Verdict::Error, reasons)
            }
        };
        return make(verdict, reasons, Completion::Complete);
    }

    // Facts about the probes, used by the steps below.
    let has_success = probes.iter().any(|p| p.outcome == Outcome::Success);
    let has_unexpected = probes.iter().any(|p| p.outcome == Outcome::Unexpected);
    let has_transport = probes.iter().any(|p| p.outcome == Outcome::TransportError);
    let has_overlap = probes.iter().any(|p| p.overlap);
    let first_is_protected = probes
        .first()
        .is_some_and(|p| p.outcome == Outcome::Protection);

    // Reasons shared by steps 3 and 5, in a fixed order.
    let mut problems = Vec::new();
    if has_unexpected || has_transport {
        problems.push(ReasonCode::ProbeUnclassifiable);
    }
    if has_transport {
        problems.push(ReasonCode::TransportError);
    }
    if has_overlap {
        problems.push(ReasonCode::OverlappingRules);
    }
    if first_is_protected {
        problems.push(ReasonCode::AccountAlreadyLocked);
    }

    // 3. An accepted invalid credential is definite evidence: it beats everything, even a stop.
    if has_success {
        let mut reasons = vec![ReasonCode::InvalidCredentialsAccepted];
        reasons.extend(problems);
        let mut completion = Completion::Complete;
        if let Some(s) = stop {
            reasons.push(s.reason());
            completion = Completion::Incomplete;
        }
        return make(Verdict::Fail, reasons, completion);
    }

    // 4. The probe list is not exactly `attempts` long: no other verdict is trusted.
    //    With a stop that is the normal partial run; without one it should not happen,
    //    so it can never be PASS (same fallback as a missing baseline).
    if probes.len() as u32 != t.attempts {
        let reason = match stop {
            Some(s) => s.reason(),
            None => ReasonCode::NotStarted,
        };
        return make(Verdict::NotRun, vec![reason], Completion::Incomplete);
    }

    // 5. A probe we could not classify.
    if has_unexpected || has_transport {
        return make(Verdict::Error, problems, Completion::Complete);
    }

    // 6. Probe 1 already protected: the account was locked before the probes.
    if first_is_protected {
        return make(
            Verdict::Inconclusive,
            vec![ReasonCode::AccountAlreadyLocked],
            Completion::Complete,
        );
    }

    // 7. Everything classified, no success: check timing and persistence of protection.
    let in_time = first_protected_attempt.is_some_and(|n| n <= t.protect_by_attempt);
    let persistent = protected_tail_observed >= t.protected_tail_count;
    if in_time && persistent {
        make(
            Verdict::Pass,
            vec![ReasonCode::ProtectionObserved],
            Completion::Complete,
        )
    } else {
        make(
            Verdict::Fail,
            vec![ReasonCode::ProtectionPolicyNotMet],
            Completion::Complete,
        )
    }
}

/// Which request the sender closure must send.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Baseline,
    Probe,
}

/// The stop signal now, if any. `Stop` is `Copy`, so the borrow ends right away.
fn current_stop(rx: &watch::Receiver<Option<Stop>>) -> Option<Stop> {
    *rx.borrow()
}

/// Wait until a stop is signalled. If the sender is dropped, no stop can ever come,
/// so wait forever (the other `select!` branch then decides).
async fn wait_stop(rx: &mut watch::Receiver<Option<Stop>>) -> Stop {
    loop {
        if let Some(s) = current_stop(rx) {
            return s;
        }
        if rx.changed().await.is_err() {
            std::future::pending::<()>().await;
        }
    }
}

fn millis(d: Duration) -> u64 {
    d.as_millis() as u64
}

/// PLAN §5 steps 1-3: one baseline, then `attempts` probes, one at a time, `interval_ms`
/// between probes only, never a retry. A stop ends the run: the request in flight is dropped
/// (counted in `attempts_dispatched`, not recorded as an attempt).
/// Precondition: `cfg` passed `validate`.
pub async fn run_with<F, Fut>(
    mut send_one: F,
    cfg: &BruteForce,
    mut stop_rx: watch::Receiver<Option<Stop>>,
) -> CheckResult
where
    F: FnMut(Kind) -> Fut,
    Fut: Future<Output = Result<TransportResponse, TransportError>>,
{
    let start = Instant::now();
    let interval = Duration::from_millis(cfg.interval_ms);
    let mut dispatched: u32 = 0;
    let mut stopped: Option<Stop> = None;
    let mut baseline: Option<(Classified, AttemptEvidence)> = None;
    let mut probes: Vec<(Classified, AttemptEvidence)> = Vec::new();

    // Attempt 0 is the baseline, 1..=attempts are probes.
    for number in 0..=cfg.attempts {
        // Interval only between probes (not before probe 1, not after the last).
        if number >= 2 {
            tokio::select! {
                biased;
                s = wait_stop(&mut stop_rx) => { stopped = Some(s); break; }
                _ = sleep(interval) => {}
            }
        }
        // Never dispatch once a stop is signalled (also covers "stop before start").
        if let Some(s) = current_stop(&stop_rx) {
            stopped = Some(s);
            break;
        }

        let kind = if number == 0 {
            Kind::Baseline
        } else {
            Kind::Probe
        };
        let offset = start.elapsed();
        dispatched += 1;
        let sent_at = Instant::now();
        let result = tokio::select! {
            biased;
            s = wait_stop(&mut stop_rx) => { stopped = Some(s); break; }
            r = send_one(kind) => r,
        };
        let elapsed = sent_at.elapsed();

        let classified = classify(
            &result,
            &cfg.success_match,
            &cfg.rejection_match,
            &cfg.protection_match,
        );
        let evidence = AttemptEvidence {
            attempt: number,
            outcome: classified.outcome,
            status: result.as_ref().ok().map(|r| r.status),
            error: classified.error,
            offset_ms: millis(offset),
            elapsed_ms: millis(elapsed),
        };
        // `result` (and its body) is dropped here; only numbers and enums are kept.

        if number == 0 {
            let baseline_ok = classified.outcome == Outcome::Success;
            baseline = Some((classified, evidence));
            if !baseline_ok {
                break; // PLAN §5 step 1: no probes after a failed baseline.
            }
        } else {
            probes.push((classified, evidence));
        }
    }

    let baseline_classified = baseline.as_ref().map(|(c, _)| *c);
    let probe_classified: Vec<Classified> = probes.iter().map(|(c, _)| *c).collect();
    let evaluation = evaluate(
        baseline_classified,
        &probe_classified,
        stopped,
        Thresholds {
            attempts: cfg.attempts,
            protect_by_attempt: cfg.protect_by_attempt,
            protected_tail_count: cfg.protected_tail_count,
        },
    );

    CheckResult {
        check: CheckName::BruteForce,
        verdict: evaluation.verdict,
        reasons: evaluation.reasons,
        completion: evaluation.completion,
        evidence: Some(BruteForceEvidence {
            attempts_planned: cfg.attempts,
            attempts_dispatched: dispatched,
            interval_ms: cfg.interval_ms,
            protect_by_attempt: cfg.protect_by_attempt,
            protected_tail_count: cfg.protected_tail_count,
            first_protected_attempt: evaluation.first_protected_attempt,
            protected_tail_observed: evaluation.protected_tail_observed,
            baseline: baseline.map(|(_, e)| e),
            attempts: probes.into_iter().map(|(_, e)| e).collect(),
        }),
    }
}

/// The real check: `run_with` sending through the shared transport.
pub async fn run(
    transport: &Transport,
    cfg: &BruteForce,
    baseline: &ResolvedRequest,
    probe: &ResolvedRequest,
    stop: watch::Receiver<Option<Stop>>,
) -> CheckResult {
    let sender = |kind: Kind| {
        let request = match kind {
            Kind::Baseline => baseline,
            Kind::Probe => probe,
        };
        send(transport, request)
    };
    run_with(sender, cfg, stop).await
}

/// One console line (PLAN §10), e.g.
/// `brute_force PASS first_protected=6 tail=3/2 attempts=8/8 reasons=protection_observed`.
/// Built from enums and numbers only.
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
    match &result.evidence {
        Some(e) => {
            let first = match e.first_protected_attempt {
                Some(n) => n.to_string(),
                None => "none".to_string(),
            };
            // Finished probes only: a request dropped by a stop is not counted.
            let probes_done = e.attempts.len();
            format!(
                "{head} first_protected={first} tail={}/{} attempts={probes_done}/{} reasons={reasons}",
                e.protected_tail_observed, e.protected_tail_count, e.attempts_planned
            )
        }
        None => format!("{head} reasons={reasons}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};
    use std::time::Duration;

    // ---- classify ----

    fn spec(v: Value) -> MatcherSpec {
        serde_json::from_value(v).expect("test matcher must parse")
    }

    fn resp(status: u16, body: Option<Value>) -> Result<TransportResponse, TransportError> {
        Ok(TransportResponse {
            status,
            elapsed: Duration::ZERO,
            body,
        })
    }

    /// success = 200 + /access_token, rejection = 401, protection = 429 (example config).
    fn classify_with_defaults(r: &Result<TransportResponse, TransportError>) -> Classified {
        classify(
            r,
            &spec(json!({"status_in": [200], "json_exists": ["/access_token"]})),
            &spec(json!({"status_in": [401]})),
            &spec(json!({"status_in": [429]})),
        )
    }

    #[test]
    fn classify_each_category() {
        let ok = classify_with_defaults(&resp(200, Some(json!({"access_token": "x"}))));
        assert_eq!(ok.outcome, Outcome::Success);
        assert!(!ok.overlap);
        assert_eq!(ok.error, None);
        assert_eq!(
            classify_with_defaults(&resp(401, None)).outcome,
            Outcome::NormalRejection
        );
        assert_eq!(
            classify_with_defaults(&resp(429, None)).outcome,
            Outcome::Protection
        );
        let other = classify_with_defaults(&resp(404, None));
        assert_eq!(other.outcome, Outcome::Unexpected);
        assert!(!other.overlap);
    }

    #[test]
    fn classify_missing_body_with_json_exists_is_unexpected() {
        let r = classify_with_defaults(&resp(200, None));
        assert_eq!(r.outcome, Outcome::Unexpected);
        assert!(!r.overlap);
        // Body present but pointer missing: same result.
        let r = classify_with_defaults(&resp(200, Some(json!({"other": 1}))));
        assert_eq!(r.outcome, Outcome::Unexpected);
    }

    #[test]
    fn classify_overlap_sets_flag() {
        // Success and rejection both match status 200 with no body conditions.
        let r = classify(
            &resp(200, None),
            &spec(json!({"status_in": [200]})),
            &spec(json!({"status_in": [200, 401]})),
            &spec(json!({"status_in": [429]})),
        );
        assert_eq!(r.outcome, Outcome::Unexpected);
        assert!(r.overlap);
    }

    #[test]
    fn classify_transport_error_keeps_code() {
        let r = classify_with_defaults(&Err(TransportError {
            code: TransportErrorCode::Timeout,
        }));
        assert_eq!(r.outcome, Outcome::TransportError);
        assert_eq!(r.error, Some(TransportErrorCode::Timeout));
        assert!(!r.overlap);
    }

    // ---- evaluate ----

    /// Example config: 8 attempts, protect by 6, tail 2.
    const DEFAULT: Thresholds = Thresholds {
        attempts: 8,
        protect_by_attempt: 6,
        protected_tail_count: 2,
    };

    fn one(c: char) -> Classified {
        let (outcome, error) = match c {
            'S' => (Outcome::Success, None),
            'R' => (Outcome::NormalRejection, None),
            'P' => (Outcome::Protection, None),
            'U' => (Outcome::Unexpected, None),
            'T' => (Outcome::TransportError, Some(TransportErrorCode::Timeout)),
            _ => panic!("unknown outcome letter"),
        };
        Classified {
            outcome,
            overlap: false,
            error,
        }
    }

    /// "RRRRRPPP" -> one Classified per letter (S success, R rejection, P protection,
    /// U unexpected, T transport error).
    fn seq(letters: &str) -> Vec<Classified> {
        letters.chars().map(one).collect()
    }

    fn overlap() -> Classified {
        Classified {
            outcome: Outcome::Unexpected,
            overlap: true,
            error: None,
        }
    }

    fn eval(baseline: char, probes: &str, stop: Option<Stop>, t: Thresholds) -> Evaluation {
        evaluate(Some(one(baseline)), &seq(probes), stop, t)
    }

    /// Check verdict, reasons and completion in one call.
    #[track_caller]
    fn assert_eval(e: &Evaluation, verdict: Verdict, reasons: &[ReasonCode], c: Completion) {
        assert_eq!(e.verdict, verdict);
        assert_eq!(e.reasons, reasons);
        assert_eq!(e.completion, c);
    }

    use Completion::{Complete, Incomplete};
    use ReasonCode as R;

    #[test]
    fn m1_healthy_passes() {
        let e = eval('S', "RRRRRPPP", None, DEFAULT);
        assert_eval(&e, Verdict::Pass, &[R::ProtectionObserved], Complete);
        assert_eq!(e.first_protected_attempt, Some(6));
        assert_eq!(e.protected_tail_observed, 3);
    }

    #[test]
    fn m2_unprotected_fails() {
        let e = eval('S', "RRRRRRRR", None, DEFAULT);
        assert_eval(&e, Verdict::Fail, &[R::ProtectionPolicyNotMet], Complete);
        assert_eq!(e.first_protected_attempt, None);
        assert_eq!(e.protected_tail_observed, 0);
    }

    #[test]
    fn m3_invalid_credentials_accepted() {
        let e = eval('S', "SSSSSSSS", None, DEFAULT);
        assert_eval(
            &e,
            Verdict::Fail,
            &[R::InvalidCredentialsAccepted],
            Complete,
        );
    }

    #[test]
    fn m4_baseline_protected_is_already_locked() {
        let e = eval('P', "", None, DEFAULT);
        assert_eval(
            &e,
            Verdict::Inconclusive,
            &[R::AccountAlreadyLocked],
            Complete,
        );
    }

    #[test]
    fn m5_baseline_rejected() {
        let e = eval('R', "", None, DEFAULT);
        assert_eval(&e, Verdict::Inconclusive, &[R::BaselineFailed], Complete);
    }

    #[test]
    fn m6_baseline_unexpected() {
        let e = eval('U', "", None, DEFAULT);
        assert_eval(&e, Verdict::Error, &[R::BaselineFailed], Complete);
    }

    #[test]
    fn m7_baseline_transport_error() {
        let e = eval('T', "", None, DEFAULT);
        assert_eval(
            &e,
            Verdict::Error,
            &[R::BaselineFailed, R::TransportError],
            Complete,
        );
    }

    #[test]
    fn m8_probe_one_protected_is_already_locked() {
        let e = eval('S', "PPPPPPPP", None, DEFAULT);
        assert_eval(
            &e,
            Verdict::Inconclusive,
            &[R::AccountAlreadyLocked],
            Complete,
        );
        assert_eq!(e.first_protected_attempt, Some(1));
    }

    #[test]
    fn m9_unclassifiable_probe_is_error() {
        let e = eval('S', "RRRRRUUU", None, DEFAULT);
        assert_eval(&e, Verdict::Error, &[R::ProbeUnclassifiable], Complete);
    }

    #[test]
    fn m10_probe_transport_error() {
        let e = eval('S', "RRRTRRPP", None, DEFAULT);
        assert_eval(
            &e,
            Verdict::Error,
            &[R::ProbeUnclassifiable, R::TransportError],
            Complete,
        );
    }

    #[test]
    fn m11_success_plus_transport_error_is_fail_with_both_reasons() {
        let e = eval('S', "RRSRTRRR", None, DEFAULT);
        assert_eval(
            &e,
            Verdict::Fail,
            &[
                R::InvalidCredentialsAccepted,
                R::ProbeUnclassifiable,
                R::TransportError,
            ],
            Complete,
        );
    }

    #[test]
    fn m12_late_protection_fails() {
        let t = Thresholds {
            protect_by_attempt: 5,
            ..DEFAULT
        };
        let e = eval('S', "RRRRRPPP", None, t);
        assert_eval(&e, Verdict::Fail, &[R::ProtectionPolicyNotMet], Complete);
        assert_eq!(e.first_protected_attempt, Some(6));
    }

    #[test]
    fn m13_protection_not_persistent_fails() {
        let e = eval('S', "RRRPPPPR", None, DEFAULT);
        assert_eval(&e, Verdict::Fail, &[R::ProtectionPolicyNotMet], Complete);
        assert_eq!(e.first_protected_attempt, Some(4));
        assert_eq!(e.protected_tail_observed, 0);
    }

    #[test]
    fn m14_tail_too_short_fails() {
        let t = Thresholds {
            protect_by_attempt: 8,
            ..DEFAULT
        };
        let e = eval('S', "RRRRRRRP", None, t);
        assert_eval(&e, Verdict::Fail, &[R::ProtectionPolicyNotMet], Complete);
        assert_eq!(e.first_protected_attempt, Some(8));
        assert_eq!(e.protected_tail_observed, 1);
    }

    #[test]
    fn m15_exact_boundary_passes() {
        let t = Thresholds {
            protect_by_attempt: 7,
            ..DEFAULT
        };
        let e = eval('S', "RRRRRRPP", None, t);
        assert_eval(&e, Verdict::Pass, &[R::ProtectionObserved], Complete);
        assert_eq!(e.first_protected_attempt, Some(7));
        assert_eq!(e.protected_tail_observed, 2);
    }

    #[test]
    fn m16_gap_in_the_middle_still_passes() {
        let e = eval('S', "RRPRPPPP", None, DEFAULT);
        assert_eval(&e, Verdict::Pass, &[R::ProtectionObserved], Complete);
        assert_eq!(e.first_protected_attempt, Some(3));
        assert_eq!(e.protected_tail_observed, 4);
    }

    #[test]
    fn m17_probe_one_protected_and_unclassifiable() {
        let e = eval('S', "PRRRURRR", None, DEFAULT);
        assert_eval(
            &e,
            Verdict::Error,
            &[R::ProbeUnclassifiable, R::AccountAlreadyLocked],
            Complete,
        );
    }

    #[test]
    fn m18_overlap_in_probe() {
        let mut probes = seq("RRRRRRRR");
        probes[2] = overlap();
        let e = evaluate(Some(one('S')), &probes, None, DEFAULT);
        assert_eval(
            &e,
            Verdict::Error,
            &[R::ProbeUnclassifiable, R::OverlappingRules],
            Complete,
        );
    }

    #[test]
    fn m18_overlap_in_baseline() {
        let e = evaluate(Some(overlap()), &[], None, DEFAULT);
        assert_eval(
            &e,
            Verdict::Error,
            &[R::BaselineFailed, R::OverlappingRules],
            Complete,
        );
    }

    #[test]
    fn m19_stop_before_start() {
        for (stop, reason) in [
            (Stop::Cancelled, R::Cancelled),
            (Stop::DeadlineExceeded, R::DeadlineExceeded),
        ] {
            let e = evaluate(None, &[], Some(stop), DEFAULT);
            assert_eval(&e, Verdict::NotRun, &[reason], Incomplete);
        }
    }

    #[test]
    fn m20_cancel_during_interval_is_not_run() {
        let e = eval('S', "RRR", Some(Stop::Cancelled), DEFAULT);
        assert_eval(&e, Verdict::NotRun, &[R::Cancelled], Incomplete);
        assert_eq!(e.first_protected_attempt, None);
    }

    #[test]
    fn m21_deadline_mid_probes_is_not_run() {
        // Partial error/protection evidence is not promoted to another verdict.
        let e = eval('S', "RRRT", Some(Stop::DeadlineExceeded), DEFAULT);
        assert_eval(&e, Verdict::NotRun, &[R::DeadlineExceeded], Incomplete);
        let e = eval('S', "PPPP", Some(Stop::DeadlineExceeded), DEFAULT);
        assert_eval(&e, Verdict::NotRun, &[R::DeadlineExceeded], Incomplete);
        assert_eq!(e.first_protected_attempt, Some(1));
    }

    #[test]
    fn m22_success_beats_stop() {
        let e = eval('S', "RS", Some(Stop::Cancelled), DEFAULT);
        assert_eval(
            &e,
            Verdict::Fail,
            &[R::InvalidCredentialsAccepted, R::Cancelled],
            Incomplete,
        );
    }

    #[test]
    fn m22_success_with_other_problems_orders_stop_last() {
        let e = eval('S', "PSTR", Some(Stop::DeadlineExceeded), DEFAULT);
        assert_eval(
            &e,
            Verdict::Fail,
            &[
                R::InvalidCredentialsAccepted,
                R::ProbeUnclassifiable,
                R::TransportError,
                R::AccountAlreadyLocked,
                R::DeadlineExceeded,
            ],
            Incomplete,
        );
    }

    #[test]
    fn m23_stop_during_baseline() {
        let e = evaluate(None, &[], Some(Stop::Cancelled), DEFAULT);
        assert_eval(&e, Verdict::NotRun, &[R::Cancelled], Incomplete);
        assert_eq!(e.protected_tail_observed, 0);
    }

    #[test]
    fn wrong_probe_count_without_stop_is_never_pass() {
        // Too few probes, none at all, and too many: all NotRun [not_started] incomplete.
        for probes in ["RRPP", "", "RRRRRPPPPP"] {
            let e = eval('S', probes, None, DEFAULT);
            assert_eval(&e, Verdict::NotRun, &[R::NotStarted], Incomplete);
        }
    }

    #[test]
    fn missing_baseline_without_stop_is_not_started() {
        let e = evaluate(None, &[], None, DEFAULT);
        assert_eval(&e, Verdict::NotRun, &[R::NotStarted], Incomplete);
    }

    #[test]
    fn stop_after_all_probes_finished_keeps_normal_verdict() {
        // Decision: the oracle is already proven, so a late stop does not turn PASS into NotRun.
        let e = eval('S', "RRRRRPPP", Some(Stop::Cancelled), DEFAULT);
        assert_eval(&e, Verdict::Pass, &[R::ProtectionObserved], Complete);
    }

    #[test]
    fn stop_after_failed_baseline_keeps_baseline_verdict() {
        // The baseline already ended the check; a late stop does not change that.
        let e = eval('R', "", Some(Stop::Cancelled), DEFAULT);
        assert_eval(&e, Verdict::Inconclusive, &[R::BaselineFailed], Complete);
    }

    // ---- run_with (paused clock, scripted sender, no network) ----

    use std::cell::RefCell;
    use std::future::Future;
    use std::pin::Pin;
    use std::rc::Rc;

    const SENTINEL: &str = "SENTINEL-TOKEN-9f3a";

    /// What the fake sender does on one call.
    #[derive(Clone)]
    enum Step {
        /// Answer at once with this status and body.
        Reply(u16, Option<Value>),
        /// Answer after `ms` of (virtual) time.
        Slow(u64, u16),
        /// Fail with a transport error.
        Fail(TransportErrorCode),
        /// Never answer (request still in flight when a stop arrives).
        Hang,
    }

    #[derive(Default)]
    struct Log {
        kinds: Vec<Kind>,
        in_flight: bool,
        reentrant: bool,
    }

    /// Sender double: call n plays `steps[n]`; records kinds and detects overlapping calls.
    fn scripted(
        steps: Vec<Step>,
        log: Rc<RefCell<Log>>,
    ) -> impl FnMut(Kind) -> Pin<Box<dyn Future<Output = Result<TransportResponse, TransportError>>>>
    {
        move |kind| {
            let index = {
                let mut l = log.borrow_mut();
                if l.in_flight {
                    l.reentrant = true;
                }
                l.in_flight = true;
                l.kinds.push(kind);
                l.kinds.len() - 1
            };
            let step = steps
                .get(index)
                .cloned()
                .expect("sender called more often than scripted");
            let log = log.clone();
            Box::pin(async move {
                let result = match step {
                    Step::Reply(status, body) => resp(status, body),
                    Step::Slow(ms, status) => {
                        sleep(Duration::from_millis(ms)).await;
                        resp(status, None)
                    }
                    Step::Fail(code) => Err(TransportError { code }),
                    Step::Hang => std::future::pending().await,
                };
                log.borrow_mut().in_flight = false;
                result
            })
        }
    }

    fn cfg() -> BruteForce {
        serde_json::from_value(json!({
            "baseline_request": {"method": "POST", "path": "/login"},
            "probe_request": {"method": "POST", "path": "/login"},
            "success_match": {"status_in": [200], "json_exists": ["/access_token"]},
            "rejection_match": {"status_in": [401]},
            "protection_match": {"status_in": [429]},
            "attempts": 8,
            "interval_ms": 100,
            "protect_by_attempt": 6,
            "protected_tail_count": 2
        }))
        .expect("test config must parse")
    }

    fn token() -> Option<Value> {
        Some(json!({"access_token": SENTINEL}))
    }

    fn reply(status: u16) -> Step {
        Step::Reply(status, None)
    }

    /// Baseline success, then one step per letter of `probes` (R=401, P=429, S=success).
    fn script(probes: &str) -> Vec<Step> {
        let mut steps = vec![Step::Reply(200, token())];
        for c in probes.chars() {
            steps.push(match c {
                'R' => Step::Reply(401, Some(json!({"error": SENTINEL}))),
                'P' => reply(429),
                'S' => Step::Reply(200, token()),
                'T' => Step::Fail(TransportErrorCode::Timeout),
                _ => panic!("unknown letter"),
            });
        }
        steps
    }

    const HEALTHY: &str = "RRRRRPPP";

    fn no_stop() -> (watch::Sender<Option<Stop>>, watch::Receiver<Option<Stop>>) {
        watch::channel(None)
    }

    /// Run `steps`; optionally signal `stop` after `stop_after_ms` of virtual time.
    async fn run_script(
        steps: Vec<Step>,
        stop: Option<(u64, Stop)>,
    ) -> (CheckResult, Rc<RefCell<Log>>) {
        let log = Rc::new(RefCell::new(Log::default()));
        let (tx, rx) = no_stop();
        if let Some((ms, s)) = stop {
            tokio::spawn(async move {
                sleep(Duration::from_millis(ms)).await;
                let _ = tx.send(Some(s));
            });
        } else {
            std::mem::forget(tx); // keep the sender alive: "no stop ever"
        }
        let result = run_with(scripted(steps, log.clone()), &cfg(), rx).await;
        (result, log)
    }

    fn ev(r: &CheckResult) -> &BruteForceEvidence {
        r.evidence
            .as_ref()
            .expect("brute force always has evidence")
    }

    /// Invariants that must hold for every result: baseline is attempt 0, probes are
    /// numbered 1..n without gaps, and dispatched counts at least every recorded attempt.
    #[track_caller]
    fn assert_invariants(r: &CheckResult, log: &Rc<RefCell<Log>>) {
        let e = ev(r);
        if let Some(b) = &e.baseline {
            assert_eq!(b.attempt, 0);
        }
        for (i, a) in e.attempts.iter().enumerate() {
            assert_eq!(a.attempt, i as u32 + 1);
        }
        let recorded = e.attempts.len() as u32 + u32::from(e.baseline.is_some());
        assert!(e.attempts_dispatched >= recorded);
        assert_eq!(e.attempts_dispatched as usize, log.borrow().kinds.len());
        assert!(!log.borrow().reentrant, "two requests overlapped");
    }

    #[tokio::test(start_paused = true)]
    async fn run_healthy_makes_nine_sequential_calls() {
        let (r, log) = run_script(script(HEALTHY), None).await;
        assert_eq!(r.verdict, Verdict::Pass);
        assert_eq!(r.reasons, [R::ProtectionObserved]);
        assert_eq!(r.completion, Complete);
        let kinds = log.borrow().kinds.clone();
        assert_eq!(kinds.len(), 9);
        assert_eq!(kinds[0], Kind::Baseline);
        assert!(kinds[1..].iter().all(|k| *k == Kind::Probe));
        assert_invariants(&r, &log);
        let e = ev(&r);
        assert_eq!(e.attempts_dispatched, 9);
        assert_eq!(e.attempts.len(), 8);
        assert_eq!(e.first_protected_attempt, Some(6));
        assert_eq!(e.protected_tail_observed, 3);
        assert_eq!(e.attempts[0].status, Some(401));
        assert_eq!(e.attempts[0].outcome, Outcome::NormalRejection);
        assert_eq!(e.attempts[7].outcome, Outcome::Protection);
    }

    #[tokio::test(start_paused = true)]
    async fn run_offsets_follow_interval_between_probes_only() {
        let (r, _) = run_script(script(HEALTHY), None).await;
        let e = ev(&r);
        let baseline = e.baseline.as_ref().expect("baseline recorded");
        assert_eq!(baseline.offset_ms, 0);
        // Probe 1 starts right after the baseline (no interval); probe k at (k-1)*interval.
        for (i, a) in e.attempts.iter().enumerate() {
            assert_eq!(a.offset_ms, i as u64 * 100, "probe {}", i + 1);
        }
        assert_eq!(e.attempts[0].offset_ms, baseline.offset_ms);
    }

    #[tokio::test(start_paused = true)]
    async fn run_slow_baseline_shifts_probe_1_not_the_interval() {
        let mut steps = script(HEALTHY);
        steps[0] = Step::Slow(7, 200);
        // The slow step has no body, so it would not be a success; give it the token path
        // by using a config whose success needs only status 200.
        let log = Rc::new(RefCell::new(Log::default()));
        let mut c = cfg();
        c.success_match = spec(json!({"status_in": [200]}));
        let (_tx, rx) = no_stop();
        let r = run_with(scripted(steps, log.clone()), &c, rx).await;
        let e = ev(&r);
        let b = e.baseline.as_ref().expect("baseline");
        assert_eq!((b.offset_ms, b.elapsed_ms), (0, 7));
        assert_eq!(e.attempts[0].offset_ms, 7);
        assert_eq!(e.attempts[1].offset_ms, 107);
        assert_eq!(e.attempts[0].elapsed_ms, 0);
        assert_invariants(&r, &log);
    }

    #[tokio::test(start_paused = true)]
    async fn run_interval_counts_from_the_end_of_a_slow_probe() {
        // Probe 2 takes 30 ms, so probe 3 starts at 100 + 30 + 100 = 230, not at 200.
        let mut steps = script(HEALTHY);
        steps[2] = Step::Slow(30, 401);
        let (r, log) = run_script(steps, None).await;
        let e = ev(&r);
        assert_eq!(e.attempts[1].offset_ms, 100);
        assert_eq!(e.attempts[1].elapsed_ms, 30);
        assert_eq!(e.attempts[2].offset_ms, 230);
        assert_eq!(e.attempts[3].offset_ms, 330);
        assert_invariants(&r, &log);
    }

    #[tokio::test(start_paused = true)]
    async fn run_timeout_on_probe_4_is_not_retried() {
        let (r, log) = run_script(script("RRRTRPPP"), None).await;
        assert_eq!(log.borrow().kinds.len(), 9);
        assert_eq!(r.verdict, Verdict::Error);
        assert_eq!(r.reasons, [R::ProbeUnclassifiable, R::TransportError]);
        assert_eq!(r.completion, Complete);
        let a = &ev(&r).attempts[3];
        assert_eq!(a.outcome, Outcome::TransportError);
        assert_eq!(a.status, None);
        assert_eq!(a.error, Some(TransportErrorCode::Timeout));
        assert_eq!(ev(&r).attempts.len(), 8);
        assert_invariants(&r, &log);
    }

    #[tokio::test(start_paused = true)]
    async fn run_failed_baseline_sends_exactly_one_call() {
        let cases = [
            (
                reply(429),
                Verdict::Inconclusive,
                vec![R::AccountAlreadyLocked],
            ),
            (reply(401), Verdict::Inconclusive, vec![R::BaselineFailed]),
            (
                Step::Fail(TransportErrorCode::Timeout),
                Verdict::Error,
                vec![R::BaselineFailed, R::TransportError],
            ),
        ];
        for (first, verdict, reasons) in cases {
            let (r, log) = run_script(vec![first], None).await;
            assert_eq!(log.borrow().kinds, [Kind::Baseline]);
            assert_eq!(r.verdict, verdict);
            assert_eq!(r.reasons, reasons);
            assert_eq!(r.completion, Complete);
            assert!(ev(&r).attempts.is_empty());
            assert_eq!(ev(&r).attempts_dispatched, 1);
            assert_invariants(&r, &log);
        }
    }

    #[tokio::test(start_paused = true)]
    async fn m19_stop_before_start_sends_nothing() {
        let log = Rc::new(RefCell::new(Log::default()));
        let (tx, rx) = no_stop();
        tx.send(Some(Stop::Cancelled)).expect("receiver alive");
        let r = run_with(scripted(vec![], log.clone()), &cfg(), rx).await;
        assert_eq!(r.verdict, Verdict::NotRun);
        assert_eq!(r.reasons, [R::Cancelled]);
        assert_eq!(r.completion, Incomplete);
        assert!(log.borrow().kinds.is_empty());
        assert_eq!(ev(&r).attempts_dispatched, 0);
        assert!(ev(&r).baseline.is_none());
        assert_invariants(&r, &log);
    }

    #[tokio::test(start_paused = true)]
    async fn m20_cancel_during_interval_keeps_finished_attempts() {
        // Probes 1..3 start at 0, 100, 200 and finish at once; the wait before probe 4
        // runs 200..300, so 250 cancels inside it.
        let (r, log) = run_script(script(HEALTHY), Some((250, Stop::Cancelled))).await;
        assert_eq!(r.verdict, Verdict::NotRun);
        assert_eq!(r.reasons, [R::Cancelled]);
        assert_eq!(r.completion, Incomplete);
        assert_eq!(log.borrow().kinds.len(), 4); // baseline + 3 probes, nothing new
        assert_eq!(ev(&r).attempts.len(), 3);
        assert_eq!(ev(&r).attempts_dispatched, 4);
        assert_invariants(&r, &log);
    }

    #[tokio::test(start_paused = true)]
    async fn m21_deadline_with_request_in_flight_is_not_recorded() {
        // Probe 5 starts at 400 and never answers; the deadline hits at 450.
        let mut steps = script(HEALTHY);
        steps[5] = Step::Hang;
        let (r, log) = run_script(steps, Some((450, Stop::DeadlineExceeded))).await;
        assert_eq!(r.verdict, Verdict::NotRun);
        assert_eq!(r.reasons, [R::DeadlineExceeded]);
        assert_eq!(r.completion, Incomplete);
        let e = ev(&r);
        assert_eq!(e.attempts.len(), 4);
        assert_eq!(e.attempts_dispatched, 6); // baseline + 5 probes, probe 5 dropped
        assert_eq!(
            console_line(&r),
            "brute_force NOT_RUN first_protected=none tail=0/2 attempts=4/8 reasons=deadline_exceeded"
        );
        assert!(e.attempts_dispatched > e.attempts.len() as u32 + 1);
        assert_invariants(&r, &log);
    }

    #[tokio::test(start_paused = true)]
    async fn m22_cancel_after_success_probe_is_still_fail() {
        // Probe 2 (offset 100) succeeds; cancel at 150 lands in the wait before probe 3.
        let (r, log) = run_script(script("RSRRRPPP"), Some((150, Stop::Cancelled))).await;
        assert_eq!(r.verdict, Verdict::Fail);
        assert_eq!(r.reasons, [R::InvalidCredentialsAccepted, R::Cancelled]);
        assert_eq!(r.completion, Incomplete);
        assert_eq!(ev(&r).attempts.len(), 2);
        assert_invariants(&r, &log);
    }

    #[tokio::test(start_paused = true)]
    async fn m23_cancel_during_baseline_in_flight() {
        let (r, log) = run_script(vec![Step::Hang], Some((10, Stop::Cancelled))).await;
        assert_eq!(r.verdict, Verdict::NotRun);
        assert_eq!(r.reasons, [R::Cancelled]);
        assert_eq!(r.completion, Incomplete);
        assert_eq!(log.borrow().kinds, [Kind::Baseline]);
        assert!(ev(&r).baseline.is_none());
        assert!(ev(&r).attempts.is_empty());
        assert_eq!(ev(&r).attempts_dispatched, 1);
        assert_invariants(&r, &log);
    }

    #[tokio::test(start_paused = true)]
    async fn run_dropped_stop_sender_never_stops_the_check() {
        let log = Rc::new(RefCell::new(Log::default()));
        let (tx, rx) = no_stop();
        drop(tx);
        let r = run_with(scripted(script(HEALTHY), log.clone()), &cfg(), rx).await;
        assert_eq!(r.verdict, Verdict::Pass);
        assert_invariants(&r, &log);
    }

    #[tokio::test(start_paused = true)]
    async fn run_never_reports_a_wrong_probe_count_without_stop() {
        // Without a stop the probe list is either complete (8) or empty (failed baseline).
        for probes in [HEALTHY, "RRRRRRRR", "SSSSSSSS", "TTTTTTTT"] {
            let (r, log) = run_script(script(probes), None).await;
            assert_eq!(ev(&r).attempts.len(), 8, "{probes}");
            assert_eq!(r.completion, Complete);
            assert_invariants(&r, &log);
        }
        // A stop at any moment: a partial list is never PASS and never "complete".
        for t in (0..=900).step_by(25) {
            let (r, log) = run_script(script(HEALTHY), Some((t, Stop::Cancelled))).await;
            assert_invariants(&r, &log);
            let n = ev(&r).attempts.len();
            if n < 8 {
                assert_eq!(r.verdict, Verdict::NotRun, "stop at {t}ms");
                assert_eq!(r.completion, Incomplete, "stop at {t}ms");
            } else {
                assert_eq!(r.verdict, Verdict::Pass, "stop at {t}ms");
            }
        }
    }

    #[tokio::test(start_paused = true)]
    async fn sentinel_is_absent_from_debug_and_console() {
        // Baseline and probe bodies carry the sentinel as a token.
        let (r, _) = run_script(script(HEALTHY), None).await;
        let line = console_line(&r);
        assert!(!format!("{r:?}").contains(SENTINEL));
        assert!(!line.contains(SENTINEL));
        // The success path (accepted credential) as well.
        let (r, _) = run_script(script("SSSSSSSS"), None).await;
        assert!(!format!("{r:?}").contains(SENTINEL));
        assert!(!console_line(&r).contains(SENTINEL));
    }

    #[tokio::test(start_paused = true)]
    async fn console_line_format() {
        let (r, _) = run_script(script(HEALTHY), None).await;
        assert_eq!(
            console_line(&r),
            "brute_force PASS first_protected=6 tail=3/2 attempts=8/8 reasons=protection_observed"
        );
        let (r, _) = run_script(script("RRRRRRRR"), None).await;
        assert_eq!(
            console_line(&r),
            "brute_force FAIL first_protected=none tail=0/2 attempts=8/8 reasons=protection_policy_not_met"
        );
    }

    #[test]
    fn console_line_without_evidence_and_reasons() {
        let r = CheckResult {
            check: CheckName::BruteForce,
            verdict: Verdict::NotRun,
            reasons: vec![],
            completion: Incomplete,
            evidence: None,
        };
        assert_eq!(console_line(&r), "brute_force NOT_RUN reasons=none");
    }
}
