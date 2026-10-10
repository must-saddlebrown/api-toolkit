//! brute_force check (PLAN §5). This slice holds the pure parts only: `classify`
//! (one response -> one outcome) and `evaluate` (all outcomes -> verdict). No I/O here.

use crate::checks::Stop;
use crate::config::MatcherSpec;
use crate::http::{TransportError, TransportResponse};
use crate::report::{Completion, Outcome, ReasonCode, TransportErrorCode, Verdict};

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
}
