//! brute_force check (PLAN §5). Pure parts: `classify` (one response -> one outcome) and
//! `evaluate` (all outcomes -> verdict). Async parts: `run_with` (the sequential loop, with a
//! closure as sender so tests need no network), `run` (real transport) and `console_line`.

use std::net::SocketAddr;
use std::time::Duration;

use futures_util::future::join_all;
use tokio::sync::watch;
use tokio::time::{Instant, sleep};

use crate::checks::{Stop, current_stop, dispatch_spread_ms, wait_stop};
use crate::config::{Baseline, BruteForce, BruteForceMode, MatcherSpec};
use crate::http::request::ResolvedRequest;
use crate::http::{Transport, TransportError, TransportResponse, send};
use crate::report::{
    AttemptEvidence, BruteForceEvidence, BurstEvidence, CheckName, CheckResult, Completion,
    Evidence, Outcome, ReasonCode, TransportErrorCode, Verdict,
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

/// PLAN §5 step 2 / "Mode burst" step 2: what a baseline that did not succeed means, for
/// sequential and burst alike. Only called once `baseline.outcome != Outcome::Success` is
/// already known; both callers return zero probes in that case.
fn baseline_result(baseline: Classified) -> (Verdict, Vec<ReasonCode>) {
    match baseline.outcome {
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
    }
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
        let (verdict, reasons) = baseline_result(baseline);
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

/// Thresholds `evaluate_burst` needs, copied from the config (DECISIONS 2026-10-14 Q3).
#[derive(Debug, Clone, Copy)]
pub struct BurstThresholds {
    pub attempts: u32,
    pub window_ms: u64,
}

/// What `evaluate_burst` decides.
#[derive(Debug, PartialEq, Eq)]
pub struct BurstEvaluation {
    pub verdict: Verdict,
    pub reasons: Vec<ReasonCode>,
    pub completion: Completion,
    pub protected_count: u32,
    pub rejected_count: u32,
}

/// PLAN §5 "Mode burst" verdict, in its precedence order. Unlike `evaluate`, probe order
/// does not matter (all probes dispatch together): P = `protected_count`, N = `t.attempts`.
/// `baseline` is `None` when the check stopped before the baseline finished. `probes` holds
/// only the finished probes. `spread_ms` comes from `dispatch_spread_ms` over the probes'
/// dispatch offsets.
pub fn evaluate_burst(
    baseline: Option<Classified>,
    probes: &[Classified],
    spread_ms: Option<u64>,
    stop: Option<Stop>,
    t: BurstThresholds,
) -> BurstEvaluation {
    let protected_count = probes
        .iter()
        .filter(|p| p.outcome == Outcome::Protection)
        .count() as u32;
    let rejected_count = probes
        .iter()
        .filter(|p| p.outcome == Outcome::NormalRejection)
        .count() as u32;
    let make = |verdict, reasons, completion| BurstEvaluation {
        verdict,
        reasons,
        completion,
        protected_count,
        rejected_count,
    };
    let stop_reason = match stop {
        Some(s) => s.reason(),
        None => ReasonCode::NotStarted, // no baseline/probes and no stop should not happen
    };

    // 1. Stopped before the baseline finished.
    let Some(baseline) = baseline else {
        return make(Verdict::NotRun, vec![stop_reason], Completion::Incomplete);
    };

    // 2. Baseline did not succeed: like sequential, no probes were sent.
    if baseline.outcome != Outcome::Success {
        let (verdict, reasons) = baseline_result(baseline);
        return make(verdict, reasons, Completion::Complete);
    }

    // Facts about the probes, used by the steps below.
    let has_success = probes.iter().any(|p| p.outcome == Outcome::Success);
    let has_unexpected = probes.iter().any(|p| p.outcome == Outcome::Unexpected);
    let has_transport = probes.iter().any(|p| p.outcome == Outcome::TransportError);
    let has_overlap = probes.iter().any(|p| p.overlap);
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

    // 3. An accepted invalid credential is definite evidence: it beats everything, even a
    //    stop and an incomplete probe count.
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
    if probes.len() as u32 != t.attempts {
        return make(Verdict::NotRun, vec![stop_reason], Completion::Incomplete);
    }

    // 5. A probe we could not classify: beats the window check below.
    if has_unexpected || has_transport {
        return make(Verdict::Error, problems, Completion::Complete);
    }

    // 6. Dispatch spread unknown (defensive safeguard; `run_burst_with` always records one
    //    once probes finish).
    let Some(spread) = spread_ms else {
        return make(
            Verdict::Inconclusive,
            vec![ReasonCode::DispatchIncomplete],
            Completion::Complete,
        );
    };

    // 7. Dispatch was not tight enough to call it a burst, even if protection was seen.
    if spread > t.window_ms {
        return make(
            Verdict::Inconclusive,
            vec![ReasonCode::DispatchSkewExceeded],
            Completion::Complete,
        );
    }

    // 8. Every probe protected: indistinguishable from an already-locked account or an
    //    already-full limiter window; never PASS.
    if protected_count == t.attempts {
        return make(
            Verdict::Inconclusive,
            vec![ReasonCode::AccountAlreadyLocked],
            Completion::Complete,
        );
    }

    // 9. No probe protected: the policy did not react at this attempt count.
    if protected_count == 0 {
        return make(
            Verdict::Fail,
            vec![ReasonCode::ProtectionPolicyNotMet],
            Completion::Complete,
        );
    }

    // 10. 1 <= P <= N-1: the policy reacted to some, not all, probes.
    make(
        Verdict::Pass,
        vec![ReasonCode::ProtectionObserved],
        Completion::Complete,
    )
}

/// PLAN §5 Verdict for `baseline = "none"` (DECISIONS 2026-10-14 Q4), sequential mode:
/// no baseline attempt is ever sent, so there is no step 1/2 of `evaluate` and no
/// `baseline_result`. `probes` holds only the finished probes, in order; `stop` and `t`
/// have the same meaning as in `evaluate`. `classifiable` below means a probe outcome of
/// `NormalRejection` or `Protection` (an on-protocol response), as opposed to `has_bad`
/// (`Unexpected`/`TransportError`): see the verdict table in `.claude/work/Q4-plan.md`.
pub fn evaluate_none(probes: &[Classified], stop: Option<Stop>, t: Thresholds) -> Evaluation {
    let (first_protected_attempt, protected_tail_observed) = protection_stats(probes);
    let make = |verdict, reasons, completion| Evaluation {
        verdict,
        reasons,
        completion,
        first_protected_attempt,
        protected_tail_observed,
    };

    let has_success = probes.iter().any(|p| p.outcome == Outcome::Success);
    let has_unexpected = probes.iter().any(|p| p.outcome == Outcome::Unexpected);
    let has_transport = probes.iter().any(|p| p.outcome == Outcome::TransportError);
    let has_bad = has_unexpected || has_transport;
    let has_overlap = probes.iter().any(|p| p.overlap);
    let has_classifiable = probes
        .iter()
        .any(|p| matches!(p.outcome, Outcome::NormalRejection | Outcome::Protection));
    let first_is_protected = probes
        .first()
        .is_some_and(|p| p.outcome == Outcome::Protection);

    // 1. An accepted invalid credential is definite evidence: it beats everything,
    //    including a stop and an incomplete probe count.
    if has_success {
        let mut reasons = vec![ReasonCode::InvalidCredentialsAccepted];
        if has_bad {
            reasons.push(ReasonCode::ProbeUnclassifiable);
        }
        if has_transport {
            reasons.push(ReasonCode::TransportError);
        }
        if has_overlap {
            reasons.push(ReasonCode::OverlappingRules);
        }
        if first_is_protected {
            reasons.push(ReasonCode::AccountAlreadyLocked);
        }
        let mut completion = Completion::Complete;
        if let Some(s) = stop {
            reasons.push(s.reason());
            completion = Completion::Incomplete;
        }
        return make(Verdict::Fail, reasons, completion);
    }

    // 2. The probe list is not exactly `attempts` long: no other verdict is trusted.
    if probes.len() as u32 != t.attempts {
        let reason = match stop {
            Some(s) => s.reason(),
            None => ReasonCode::NotStarted,
        };
        return make(Verdict::NotRun, vec![reason], Completion::Incomplete);
    }

    // 3. Overlap, or an unclassifiable/transport probe alongside a classifiable one: the
    //    endpoint answered inconsistently, which is worse than simply unconfirmed.
    if has_overlap || (has_bad && has_classifiable) {
        let mut reasons = Vec::new();
        if has_bad {
            reasons.push(ReasonCode::ProbeUnclassifiable);
        }
        if has_transport {
            reasons.push(ReasonCode::TransportError);
        }
        if has_overlap {
            reasons.push(ReasonCode::OverlappingRules);
        }
        return make(Verdict::Error, reasons, Completion::Complete);
    }

    // 4. Every probe was unclassifiable/transport-error and none were on-protocol: the
    //    endpoint itself was never confirmed to speak the expected protocol.
    if has_bad {
        return make(
            Verdict::Inconclusive,
            vec![ReasonCode::EndpointNotConfirmed],
            Completion::Complete,
        );
    }

    // 5. Probe 1 already protected: without a baseline there is no "before probing" signal
    //    other than this, same meaning as `evaluate`'s already-locked case.
    if first_is_protected {
        return make(
            Verdict::Inconclusive,
            vec![ReasonCode::AccountAlreadyLocked],
            Completion::Complete,
        );
    }

    // 6/7. Everything classified, no success: check timing and persistence of protection.
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
            vec![ReasonCode::ProtectionNotTriggered],
            Completion::Complete,
        )
    }
}

/// PLAN §5 "Mode burst" verdict for `baseline = "none"` (DECISIONS 2026-10-14 Q4): same
/// shape as `evaluate_burst`, minus the baseline step, with `protection_not_triggered`
/// taking the place of `protection_policy_not_met` and an `endpoint_not_confirmed` step
/// inserted before the window/`P` checks. P = `protected_count`, N = `t.attempts`.
pub fn evaluate_burst_none(
    probes: &[Classified],
    spread_ms: Option<u64>,
    stop: Option<Stop>,
    t: BurstThresholds,
) -> BurstEvaluation {
    let protected_count = probes
        .iter()
        .filter(|p| p.outcome == Outcome::Protection)
        .count() as u32;
    let rejected_count = probes
        .iter()
        .filter(|p| p.outcome == Outcome::NormalRejection)
        .count() as u32;
    let make = |verdict, reasons, completion| BurstEvaluation {
        verdict,
        reasons,
        completion,
        protected_count,
        rejected_count,
    };
    let stop_reason = match stop {
        Some(s) => s.reason(),
        None => ReasonCode::NotStarted,
    };

    let has_success = probes.iter().any(|p| p.outcome == Outcome::Success);
    let has_unexpected = probes.iter().any(|p| p.outcome == Outcome::Unexpected);
    let has_transport = probes.iter().any(|p| p.outcome == Outcome::TransportError);
    let has_bad = has_unexpected || has_transport;
    let has_overlap = probes.iter().any(|p| p.overlap);
    let has_classifiable = probes
        .iter()
        .any(|p| matches!(p.outcome, Outcome::NormalRejection | Outcome::Protection));

    // 1. An accepted invalid credential beats everything, even a stop and an incomplete
    //    probe count.
    if has_success {
        let mut reasons = vec![ReasonCode::InvalidCredentialsAccepted];
        if has_bad {
            reasons.push(ReasonCode::ProbeUnclassifiable);
        }
        if has_transport {
            reasons.push(ReasonCode::TransportError);
        }
        if has_overlap {
            reasons.push(ReasonCode::OverlappingRules);
        }
        let mut completion = Completion::Complete;
        if let Some(s) = stop {
            reasons.push(s.reason());
            completion = Completion::Incomplete;
        }
        return make(Verdict::Fail, reasons, completion);
    }

    // 2. The probe list is not exactly `attempts` long: no other verdict is trusted.
    if probes.len() as u32 != t.attempts {
        return make(Verdict::NotRun, vec![stop_reason], Completion::Incomplete);
    }

    // 3. Overlap, or an unclassifiable/transport probe alongside a classifiable one.
    if has_overlap || (has_bad && has_classifiable) {
        let mut reasons = Vec::new();
        if has_bad {
            reasons.push(ReasonCode::ProbeUnclassifiable);
        }
        if has_transport {
            reasons.push(ReasonCode::TransportError);
        }
        if has_overlap {
            reasons.push(ReasonCode::OverlappingRules);
        }
        return make(Verdict::Error, reasons, Completion::Complete);
    }

    // 4. Every probe was unclassifiable/transport-error: endpoint never confirmed. Spread
    //    and window are meaningless without a single on-protocol response.
    if has_bad {
        return make(
            Verdict::Inconclusive,
            vec![ReasonCode::EndpointNotConfirmed],
            Completion::Complete,
        );
    }

    // 5. Dispatch spread unknown (defensive safeguard, same as `evaluate_burst`).
    let Some(spread) = spread_ms else {
        return make(
            Verdict::Inconclusive,
            vec![ReasonCode::DispatchIncomplete],
            Completion::Complete,
        );
    };

    // 6. Dispatch was not tight enough to call it a burst.
    if spread > t.window_ms {
        return make(
            Verdict::Inconclusive,
            vec![ReasonCode::DispatchSkewExceeded],
            Completion::Complete,
        );
    }

    // 7. Every probe protected: indistinguishable from an already-full limiter window.
    if protected_count == t.attempts {
        return make(
            Verdict::Inconclusive,
            vec![ReasonCode::AccountAlreadyLocked],
            Completion::Complete,
        );
    }

    // 8. No probe protected: the policy did not react at this attempt count/window.
    if protected_count == 0 {
        return make(
            Verdict::Fail,
            vec![ReasonCode::ProtectionNotTriggered],
            Completion::Complete,
        );
    }

    // 9. 1 <= P <= N-1: the policy reacted to some, not all, probes.
    make(
        Verdict::Pass,
        vec![ReasonCode::ProtectionObserved],
        Completion::Complete,
    )
}

/// Which request the sender closure must send.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Baseline,
    Probe,
}

fn millis(d: Duration) -> u64 {
    d.as_millis() as u64
}

/// `peer_ip`/`peer_family` evidence (DECISIONS 2026-10-14 Q4): IP and `"v4"`/`"v6"`, from
/// one request's `remote_addr`. Only IP + port ever flow through this, never a header,
/// body or URL.
fn peer_of(addr: SocketAddr) -> (String, &'static str) {
    (
        addr.ip().to_string(),
        if addr.is_ipv4() { "v4" } else { "v6" },
    )
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
    // ponytail: `cfg.interval_ms` is `Option` since Q3 (burst has no interval). `run` only
    // calls `run_with` for `BruteForceMode::Sequential`, and `validate` guarantees `Some`
    // in that branch, so `unwrap_or_default` is never reached with a real `None` here.
    let interval = Duration::from_millis(cfg.interval_ms.unwrap_or_default());
    let mut dispatched: u32 = 0;
    let mut stopped: Option<Stop> = None;
    let mut baseline: Option<(Classified, AttemptEvidence)> = None;
    let mut probes: Vec<(Classified, AttemptEvidence)> = Vec::new();
    let mut peer_addr: Option<SocketAddr> = None;
    // `baseline = "none"` (Q4): skip attempt 0 entirely, start at probe 1.
    let start_number = match cfg.baseline {
        Baseline::RequireSuccess => 0,
        Baseline::None => 1,
    };

    // Attempt 0 is the baseline (when sent), 1..=attempts are probes.
    for number in start_number..=cfg.attempts {
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
        if peer_addr.is_none() {
            peer_addr = result.as_ref().ok().and_then(|r| r.remote_addr);
        }

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
    let thresholds = Thresholds {
        attempts: cfg.attempts,
        // ponytail: `Option` since Q3 (burst), never `None` on this sequential-only path.
        protect_by_attempt: cfg.protect_by_attempt.unwrap_or_default(),
        protected_tail_count: cfg.protected_tail_count.unwrap_or_default(),
    };
    let evaluation = match cfg.baseline {
        Baseline::RequireSuccess => {
            evaluate(baseline_classified, &probe_classified, stopped, thresholds)
        }
        Baseline::None => evaluate_none(&probe_classified, stopped, thresholds),
    };
    let (peer_ip, peer_family) = match peer_addr.map(peer_of) {
        Some((ip, family)) => (Some(ip), Some(family)),
        None => (None, None),
    };

    CheckResult {
        check: CheckName::BruteForce,
        verdict: evaluation.verdict,
        reasons: evaluation.reasons,
        completion: evaluation.completion,
        evidence: Some(Evidence::BruteForce(BruteForceEvidence {
            attempts_planned: cfg.attempts,
            attempts_dispatched: dispatched,
            // ponytail: `Option` since Q3 (burst), never `None` on this sequential-only path.
            interval_ms: cfg.interval_ms.unwrap_or_default(),
            protect_by_attempt: cfg.protect_by_attempt.unwrap_or_default(),
            protected_tail_count: cfg.protected_tail_count.unwrap_or_default(),
            first_protected_attempt: evaluation.first_protected_attempt,
            protected_tail_observed: evaluation.protected_tail_observed,
            baseline_mode: match cfg.baseline {
                Baseline::RequireSuccess => "require_success",
                Baseline::None => "none",
            },
            peer_ip,
            peer_family,
            baseline: baseline.map(|(_, e)| e),
            attempts: probes.into_iter().map(|(_, e)| e).collect(),
        })),
    }
}

/// PLAN §5 "Mode burst" steps 1-3: one baseline, then `attempts` probes dispatched together
/// in one `join_all` (pattern from `race.rs`'s action barrier); each probe notes its dispatch
/// offset on first poll. No retry, no interval. A stop drops whatever probe is still in
/// flight (counted in `attempts_dispatched`, not recorded in `attempts`).
/// Precondition: `cfg` passed `validate` (burst fields are `Some`/`None` as `validate` requires).
pub async fn run_burst_with<F, Fut>(
    send_one: F,
    cfg: &BruteForce,
    mut stop_rx: watch::Receiver<Option<Stop>>,
) -> CheckResult
where
    F: Fn(Kind) -> Fut,
    Fut: Future<Output = Result<TransportResponse, TransportError>>,
{
    let start = Instant::now();
    let mut baseline_dispatched = 0u32;
    let mut stopped: Option<Stop> = None;
    let mut baseline: Option<(Classified, AttemptEvidence)> = None;
    let mut probe_classified: Vec<Classified> = Vec::new();
    let mut probe_evidence: Vec<AttemptEvidence> = Vec::new();
    let mut dispatch_offsets: Vec<Duration> = Vec::new();
    let mut peer_addr: Option<SocketAddr> = None;

    'run: {
        // Never dispatch once a stop is signalled (also covers "stop before start").
        if let Some(s) = current_stop(&stop_rx) {
            stopped = Some(s);
            break 'run;
        }
        // `baseline = "none"` (Q4): skip the baseline attempt entirely, go straight to
        // the probe barrier below.
        if cfg.baseline == Baseline::RequireSuccess {
            let offset = start.elapsed();
            baseline_dispatched = 1;
            let sent_at = Instant::now();
            let result = tokio::select! {
                biased;
                s = wait_stop(&mut stop_rx) => { stopped = Some(s); break 'run; }
                r = send_one(Kind::Baseline) => r,
            };
            peer_addr = result.as_ref().ok().and_then(|r| r.remote_addr);
            let classified = classify(
                &result,
                &cfg.success_match,
                &cfg.rejection_match,
                &cfg.protection_match,
            );
            let evidence = AttemptEvidence {
                attempt: 0,
                outcome: classified.outcome,
                status: result.as_ref().ok().map(|r| r.status),
                error: classified.error,
                offset_ms: millis(offset),
                elapsed_ms: millis(sent_at.elapsed()),
            };
            let baseline_ok = classified.outcome == Outcome::Success;
            baseline = Some((classified, evidence));
            if !baseline_ok {
                break 'run; // like sequential: no probes after a failed baseline
            }
            if let Some(s) = current_stop(&stop_rx) {
                stopped = Some(s);
                break 'run;
            }
        }

        // Barrier: all N probe futures are lazy, so nothing is sent until `join_all` polls
        // them, and it polls all of them in one go. Each one notes its dispatch time on the
        // first poll.
        let send_ref = &send_one;
        let runs = join_all((1..=cfg.attempts).map(|number| {
            let mut rx = stop_rx.clone();
            async move {
                let offset = start.elapsed();
                let sent_at = Instant::now();
                let result = tokio::select! {
                    biased;
                    _ = wait_stop(&mut rx) => return (offset, None),
                    r = send_ref(Kind::Probe) => r,
                };
                let peer = result.as_ref().ok().and_then(|r| r.remote_addr);
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
                    elapsed_ms: millis(sent_at.elapsed()),
                };
                (offset, Some((classified, evidence, peer)))
            }
        }))
        .await;
        let mut any_dropped = false;
        for (offset, done) in runs {
            dispatch_offsets.push(offset);
            match done {
                Some((classified, evidence, peer)) => {
                    probe_classified.push(classified);
                    probe_evidence.push(evidence);
                    if peer_addr.is_none() {
                        peer_addr = peer;
                    }
                }
                None => any_dropped = true,
            }
        }
        if any_dropped {
            stopped = current_stop(&stop_rx);
        }
    }

    let spread_ms = dispatch_spread_ms(&dispatch_offsets);
    let baseline_classified = baseline.as_ref().map(|(c, _)| *c);
    let burst_thresholds = BurstThresholds {
        attempts: cfg.attempts,
        // ponytail: `Option` since Q3, never `None` on this burst-only path.
        window_ms: cfg.window_ms.unwrap_or_default(),
    };
    let evaluation = match cfg.baseline {
        Baseline::RequireSuccess => evaluate_burst(
            baseline_classified,
            &probe_classified,
            spread_ms,
            stopped,
            burst_thresholds,
        ),
        Baseline::None => {
            evaluate_burst_none(&probe_classified, spread_ms, stopped, burst_thresholds)
        }
    };
    let (peer_ip, peer_family) = match peer_addr.map(peer_of) {
        Some((ip, family)) => (Some(ip), Some(family)),
        None => (None, None),
    };

    CheckResult {
        check: CheckName::BruteForce,
        verdict: evaluation.verdict,
        reasons: evaluation.reasons,
        completion: evaluation.completion,
        evidence: Some(Evidence::BruteForceBurst(BurstEvidence {
            mode: "burst",
            attempts_planned: cfg.attempts,
            attempts_dispatched: baseline_dispatched + dispatch_offsets.len() as u32,
            window_ms: cfg.window_ms.unwrap_or_default(),
            dispatch_window_ms: spread_ms,
            protected_count: evaluation.protected_count,
            rejected_count: evaluation.rejected_count,
            baseline_mode: match cfg.baseline {
                Baseline::RequireSuccess => "require_success",
                Baseline::None => "none",
            },
            peer_ip,
            peer_family,
            baseline: baseline.map(|(_, e)| e),
            attempts: probe_evidence,
        })),
    }
}

/// The real check: `run_with`/`run_burst_with` sending through the shared transport,
/// chosen by `cfg.mode`.
pub async fn run(
    transport: &Transport,
    cfg: &BruteForce,
    baseline: Option<&ResolvedRequest>,
    probe: &ResolvedRequest,
    stop: watch::Receiver<Option<Stop>>,
) -> CheckResult {
    let sender = |kind: Kind| {
        let request = match kind {
            // `run_with`/`run_burst_with` only ever request `Kind::Baseline` when
            // `cfg.baseline == RequireSuccess`, and `validate` guarantees `Some` there.
            Kind::Baseline => {
                baseline.expect("baseline present when cfg.baseline is require_success")
            }
            Kind::Probe => probe,
        };
        send(transport, request)
    };
    match cfg.mode {
        BruteForceMode::Sequential => run_with(sender, cfg, stop).await,
        BruteForceMode::Burst => run_burst_with(sender, cfg, stop).await,
    }
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
        Some(Evidence::BruteForce(e)) => {
            let first = match e.first_protected_attempt {
                Some(n) => n.to_string(),
                None => "none".to_string(),
            };
            // Finished probes only: a request dropped by a stop is not counted.
            let probes_done = e.attempts.len();
            // `baseline=none` token only on the none path; require_success stays byte-identical.
            let baseline_tok = if e.baseline_mode == "none" {
                " baseline=none"
            } else {
                ""
            };
            format!(
                "{head}{baseline_tok} first_protected={first} tail={}/{} attempts={probes_done}/{} reasons={reasons}",
                e.protected_tail_observed, e.protected_tail_count, e.attempts_planned
            )
        }
        Some(Evidence::BruteForceBurst(e)) => {
            let window = e
                .dispatch_window_ms
                .map_or("none".to_string(), |w| w.to_string());
            // Finished probes only: a probe dropped by a stop is not counted.
            let probes_done = e.attempts.len();
            let baseline_tok = if e.baseline_mode == "none" {
                " baseline=none"
            } else {
                ""
            };
            format!(
                "{head} mode=burst{baseline_tok} protected={}/{} rejected={} window={window}/{}ms attempts={probes_done}/{} reasons={reasons}",
                e.protected_count,
                e.attempts_planned,
                e.rejected_count,
                e.window_ms,
                e.attempts_planned
            )
        }
        // Race and load evidence have their own console lines.
        Some(Evidence::Race(_)) | Some(Evidence::Load(_)) | None => {
            format!("{head} reasons={reasons}")
        }
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
            remote_addr: None,
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

    // ---- evaluate_burst ----

    /// Example burst config: 8 attempts, window 100ms.
    const BURST: BurstThresholds = BurstThresholds {
        attempts: 8,
        window_ms: 100,
    };

    fn eval_burst(
        baseline: char,
        probes: &str,
        spread_ms: Option<u64>,
        stop: Option<Stop>,
        t: BurstThresholds,
    ) -> BurstEvaluation {
        evaluate_burst(Some(one(baseline)), &seq(probes), spread_ms, stop, t)
    }

    #[track_caller]
    fn assert_burst(e: &BurstEvaluation, verdict: Verdict, reasons: &[ReasonCode], c: Completion) {
        assert_eq!(e.verdict, verdict);
        assert_eq!(e.reasons, reasons);
        assert_eq!(e.completion, c);
    }

    #[test]
    fn b1_partial_protection_passes() {
        let e = eval_burst('S', "RRRRRPPP", Some(50), None, BURST);
        assert_burst(&e, Verdict::Pass, &[R::ProtectionObserved], Complete);
        assert_eq!(e.protected_count, 3);
        assert_eq!(e.rejected_count, 5);
    }

    #[test]
    fn b2_boundary_p1_passes() {
        let e = eval_burst('S', "RRRRRRRP", Some(50), None, BURST);
        assert_burst(&e, Verdict::Pass, &[R::ProtectionObserved], Complete);
        assert_eq!(e.protected_count, 1);
    }

    #[test]
    fn b3_boundary_p_n_minus_1_passes() {
        let e = eval_burst('S', "RPPPPPPP", Some(50), None, BURST);
        assert_burst(&e, Verdict::Pass, &[R::ProtectionObserved], Complete);
        assert_eq!(e.protected_count, 7);
    }

    #[test]
    fn b4_no_protection_fails() {
        let e = eval_burst('S', "RRRRRRRR", Some(50), None, BURST);
        assert_burst(&e, Verdict::Fail, &[R::ProtectionPolicyNotMet], Complete);
        assert_eq!(e.protected_count, 0);
    }

    #[test]
    fn b5_all_protected_is_inconclusive_account_already_locked() {
        let e = eval_burst('S', "PPPPPPPP", Some(50), None, BURST);
        assert_burst(
            &e,
            Verdict::Inconclusive,
            &[R::AccountAlreadyLocked],
            Complete,
        );
        assert_eq!(e.protected_count, 8);
    }

    #[test]
    fn b6_success_probe_fails_even_with_stop() {
        // Only 7 of 8 probes finished, but a success is still definite evidence.
        let e = eval_burst('S', "RRSRRRR", Some(50), Some(Stop::Cancelled), BURST);
        assert_burst(
            &e,
            Verdict::Fail,
            &[R::InvalidCredentialsAccepted, R::Cancelled],
            Incomplete,
        );
    }

    #[test]
    fn b7_unclassifiable_probe_is_error_and_beats_window() {
        // Spread (200) exceeds the window (100), but the unclassifiable probe wins.
        let e = eval_burst('S', "RRRRRRPU", Some(200), None, BURST);
        assert_burst(&e, Verdict::Error, &[R::ProbeUnclassifiable], Complete);
    }

    #[test]
    fn b8_transport_error_probe_is_error_with_transport_error() {
        let e = eval_burst('S', "RRRRRRPT", Some(50), None, BURST);
        assert_burst(
            &e,
            Verdict::Error,
            &[R::ProbeUnclassifiable, R::TransportError],
            Complete,
        );
    }

    #[test]
    fn b9_overlap_is_error_with_overlapping_rules() {
        let mut probes = seq("RRRRRRRP");
        probes[0] = overlap();
        let e = evaluate_burst(Some(one('S')), &probes, Some(50), None, BURST);
        assert_burst(
            &e,
            Verdict::Error,
            &[R::ProbeUnclassifiable, R::OverlappingRules],
            Complete,
        );
    }

    #[test]
    fn b10_window_exceeded_is_inconclusive_even_with_protection() {
        // Exact boundary: spread == window still passes.
        let e = eval_burst('S', "RRRRRPPP", Some(100), None, BURST);
        assert_burst(&e, Verdict::Pass, &[R::ProtectionObserved], Complete);
        // One ms over: inconclusive, even though probes show protection.
        let e = eval_burst('S', "RRRRRPPP", Some(101), None, BURST);
        assert_burst(
            &e,
            Verdict::Inconclusive,
            &[R::DispatchSkewExceeded],
            Complete,
        );
    }

    #[test]
    fn b11_dispatch_incomplete_when_spread_missing() {
        let e = eval_burst('S', "RRRRRPPP", None, None, BURST);
        assert_burst(
            &e,
            Verdict::Inconclusive,
            &[R::DispatchIncomplete],
            Complete,
        );
    }

    #[test]
    fn b12_wrong_probe_count_without_stop_is_not_run_not_started() {
        for probes in ["RRPP", "", "RRRRRPPPPP"] {
            let e = eval_burst('S', probes, Some(50), None, BURST);
            assert_burst(&e, Verdict::NotRun, &[R::NotStarted], Incomplete);
        }
    }

    #[test]
    fn b13_stop_during_burst_is_not_run() {
        let e = eval_burst('S', "RRPP", Some(50), Some(Stop::DeadlineExceeded), BURST);
        assert_burst(&e, Verdict::NotRun, &[R::DeadlineExceeded], Incomplete);
    }

    #[test]
    fn b14_stop_before_baseline() {
        for (stop, reason) in [
            (Stop::Cancelled, R::Cancelled),
            (Stop::DeadlineExceeded, R::DeadlineExceeded),
        ] {
            let e = evaluate_burst(None, &[], None, Some(stop), BURST);
            assert_burst(&e, Verdict::NotRun, &[reason], Incomplete);
        }
    }

    #[test]
    fn b15_stop_after_all_probes_keeps_verdict() {
        // All 8 probes finished and show a clean PASS; a late stop does not change that.
        let e = eval_burst('S', "RRRRRPPP", Some(50), Some(Stop::Cancelled), BURST);
        assert_burst(&e, Verdict::Pass, &[R::ProtectionObserved], Complete);
    }

    #[test]
    fn b16_baseline_variants_match_sequential() {
        let cases = [
            ('P', Verdict::Inconclusive, vec![R::AccountAlreadyLocked]),
            ('R', Verdict::Inconclusive, vec![R::BaselineFailed]),
            ('U', Verdict::Error, vec![R::BaselineFailed]),
            (
                'T',
                Verdict::Error,
                vec![R::BaselineFailed, R::TransportError],
            ),
        ];
        for (baseline, verdict, reasons) in cases {
            let e = eval_burst(baseline, "", Some(50), None, BURST);
            assert_burst(&e, verdict, &reasons, Complete);
            assert_eq!(e.protected_count, 0);
            assert_eq!(e.rejected_count, 0);
        }
    }

    #[test]
    fn b17_never_pass_exhaustive() {
        // Oracle independent of the implementation: PASS only for 1 <= P <= N-1, baseline
        // success, spread within budget, exactly N probes classified, no success/unexpected
        // probe, and no stop that cuts the run short.
        let letters = ['S', 'R', 'P', 'U', 'T'];
        let stops = [None, Some(Stop::Cancelled), Some(Stop::DeadlineExceeded)];
        let spreads = [None, Some(50), Some(100), Some(101)];
        let probe_lists: Vec<Vec<Classified>> = {
            let mut lists = vec![seq("RRRRRRRR"), seq("PPPPPPPP")];
            for p in 1..8 {
                lists.push(seq(&("P".repeat(p) + &"R".repeat(8 - p))));
            }
            lists.push(seq("RRRRRRRS"));
            lists.push(seq("RRRRRRRU"));
            lists.push(seq("RRRRRRRT"));
            lists.push(seq("RRRPPP")); // wrong count (6)
            lists
        };
        let mut passes = 0;
        for baseline in letters {
            for probes in &probe_lists {
                for spread in spreads {
                    for stop in stops {
                        let e = evaluate_burst(Some(one(baseline)), probes, spread, stop, BURST);
                        let protected = probes
                            .iter()
                            .filter(|p| p.outcome == Outcome::Protection)
                            .count() as u32;
                        let expect_pass = baseline == 'S'
                            && probes.len() as u32 == BURST.attempts
                            && probes.iter().all(|p| {
                                p.outcome != Outcome::Success
                                    && p.outcome != Outcome::Unexpected
                                    && p.outcome != Outcome::TransportError
                            })
                            && spread.is_some_and(|s| s <= BURST.window_ms)
                            && protected > 0
                            && protected < BURST.attempts;
                        // A stop never blocks PASS once all N probes finished (b15); it
                        // only matters through the branches already excluded above.
                        let _ = stop;
                        assert_eq!(
                            e.verdict == Verdict::Pass,
                            expect_pass,
                            "{baseline:?} {probes:?} {spread:?} {stop:?}"
                        );
                        passes += expect_pass as u32;
                    }
                }
            }
        }
        assert!(passes > 0);
    }

    // ---- evaluate_none / evaluate_burst_none (DECISIONS 2026-10-14 Q4) ----

    fn eval_none(probes: &str, stop: Option<Stop>, t: Thresholds) -> Evaluation {
        evaluate_none(&seq(probes), stop, t)
    }

    #[test]
    fn none_seq_pass_healthy() {
        let e = eval_none("RRRRRPPP", None, DEFAULT);
        assert_eval(&e, Verdict::Pass, &[R::ProtectionObserved], Complete);
        assert_eq!(e.first_protected_attempt, Some(6));
        assert_eq!(e.protected_tail_observed, 3);
    }

    #[test]
    fn none_seq_pass_boundary_p1_and_gap() {
        let t = Thresholds {
            protect_by_attempt: 7,
            ..DEFAULT
        };
        let e = eval_none("RRRRRRPP", None, t);
        assert_eval(&e, Verdict::Pass, &[R::ProtectionObserved], Complete);
        let e = eval_none("RRPRPPPP", None, DEFAULT);
        assert_eval(&e, Verdict::Pass, &[R::ProtectionObserved], Complete);
    }

    #[test]
    fn none_seq_fail_protection_not_triggered() {
        let e = eval_none("RRRRRRRR", None, DEFAULT);
        assert_eval(&e, Verdict::Fail, &[R::ProtectionNotTriggered], Complete);
        assert_eq!(e.first_protected_attempt, None);
    }

    #[test]
    fn none_seq_fail_late_or_short_protection() {
        let t = Thresholds {
            protect_by_attempt: 5,
            ..DEFAULT
        };
        let e = eval_none("RRRRRPPP", None, t);
        assert_eval(&e, Verdict::Fail, &[R::ProtectionNotTriggered], Complete);
        let e = eval_none("RRRPPPPR", None, DEFAULT);
        assert_eval(&e, Verdict::Fail, &[R::ProtectionNotTriggered], Complete);
    }

    #[test]
    fn none_seq_success_fails_even_with_problems_and_stop() {
        let e = eval_none("SSSSSSSS", None, DEFAULT);
        assert_eval(
            &e,
            Verdict::Fail,
            &[R::InvalidCredentialsAccepted],
            Complete,
        );
        // Probe 1 protected, a transport error elsewhere, stop: success still wins,
        // reasons in the documented order, stop last.
        let e = eval_none("PSTR", Some(Stop::DeadlineExceeded), DEFAULT);
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
    fn none_seq_probe_one_protected_is_already_locked() {
        let e = eval_none("PPPPPPPP", None, DEFAULT);
        assert_eval(
            &e,
            Verdict::Inconclusive,
            &[R::AccountAlreadyLocked],
            Complete,
        );
        assert_eq!(e.first_protected_attempt, Some(1));
    }

    #[test]
    fn none_seq_all_unclassifiable_or_transport_is_endpoint_not_confirmed() {
        for probes in ["UUUUUUUU", "TTTTTTTT", "UUUUTTTT"] {
            let e = eval_none(probes, None, DEFAULT);
            assert_eval(
                &e,
                Verdict::Inconclusive,
                &[R::EndpointNotConfirmed],
                Complete,
            );
        }
    }

    #[test]
    fn none_seq_bad_mixed_with_classifiable_is_error() {
        let e = eval_none("RRRRRRRU", None, DEFAULT);
        assert_eval(&e, Verdict::Error, &[R::ProbeUnclassifiable], Complete);
        let e = eval_none("RRRTRRPP", None, DEFAULT);
        assert_eval(
            &e,
            Verdict::Error,
            &[R::ProbeUnclassifiable, R::TransportError],
            Complete,
        );
    }

    #[test]
    fn none_seq_overlap_is_error() {
        let mut probes = seq("RRRRRRRR");
        probes[2] = overlap();
        let e = evaluate_none(&probes, None, DEFAULT);
        assert_eval(
            &e,
            Verdict::Error,
            &[R::ProbeUnclassifiable, R::OverlappingRules],
            Complete,
        );
    }

    #[test]
    fn none_seq_wrong_probe_count_without_stop_is_never_pass() {
        for probes in ["RRPP", "", "RRRRRPPPPP"] {
            let e = eval_none(probes, None, DEFAULT);
            assert_eval(&e, Verdict::NotRun, &[R::NotStarted], Incomplete);
        }
    }

    #[test]
    fn none_seq_stop_during_probes_is_not_run() {
        let e = eval_none("RRR", Some(Stop::Cancelled), DEFAULT);
        assert_eval(&e, Verdict::NotRun, &[R::Cancelled], Incomplete);
    }

    #[test]
    fn none_seq_stop_after_all_probes_keeps_verdict() {
        let e = eval_none("RRRRRPPP", Some(Stop::Cancelled), DEFAULT);
        assert_eval(&e, Verdict::Pass, &[R::ProtectionObserved], Complete);
    }

    /// Oracle independent of the implementation (pattern: `b17`/`wrong_probe_count...`):
    /// PASS only for the exact table row 6 (in-time, persistent protection; no success,
    /// no unclassifiable/transport probe, exactly `attempts` probes classified).
    #[test]
    fn none_seq_never_pass_exhaustive() {
        let stops = [None, Some(Stop::Cancelled), Some(Stop::DeadlineExceeded)];
        let probe_lists: Vec<Vec<Classified>> = {
            let mut lists = vec![seq("RRRRRRRR"), seq("PPPPPPPP")];
            for p in 1..8 {
                lists.push(seq(&("P".repeat(p) + &"R".repeat(8 - p))));
                lists.push(seq(&("R".repeat(8 - p) + &"P".repeat(p))));
            }
            lists.push(seq("SRRRRRRR"));
            lists.push(seq("RRRRRRRU"));
            lists.push(seq("RRRRRRRT"));
            lists.push(seq("RRRPPP")); // wrong count
            lists
        };
        let mut passes = 0;
        for probes in &probe_lists {
            for stop in stops {
                let e = evaluate_none(probes, stop, DEFAULT);
                let has_success = probes.iter().any(|p| p.outcome == Outcome::Success);
                let has_bad = probes
                    .iter()
                    .any(|p| matches!(p.outcome, Outcome::Unexpected | Outcome::TransportError));
                let first_is_protected = probes
                    .first()
                    .is_some_and(|p| p.outcome == Outcome::Protection);
                let (first_protected, tail) = protection_stats(probes);
                let in_time = first_protected.is_some_and(|n| n <= DEFAULT.protect_by_attempt);
                let persistent = tail >= DEFAULT.protected_tail_count;
                let expect_pass = !has_success
                    && probes.len() as u32 == DEFAULT.attempts
                    && !has_bad
                    && !first_is_protected
                    && in_time
                    && persistent;
                assert_eq!(
                    e.verdict == Verdict::Pass,
                    expect_pass,
                    "{probes:?} {stop:?}"
                );
                passes += expect_pass as u32;
            }
        }
        assert!(passes > 0);
    }

    fn eval_burst_none(
        probes: &str,
        spread_ms: Option<u64>,
        stop: Option<Stop>,
        t: BurstThresholds,
    ) -> BurstEvaluation {
        evaluate_burst_none(&seq(probes), spread_ms, stop, t)
    }

    #[test]
    fn none_burst_pass_partial_and_boundaries() {
        let e = eval_burst_none("RRRRRPPP", Some(50), None, BURST);
        assert_burst(&e, Verdict::Pass, &[R::ProtectionObserved], Complete);
        assert_eq!(e.protected_count, 3);
        let e = eval_burst_none("RRRRRRRP", Some(50), None, BURST);
        assert_burst(&e, Verdict::Pass, &[R::ProtectionObserved], Complete);
        let e = eval_burst_none("RPPPPPPP", Some(50), None, BURST);
        assert_burst(&e, Verdict::Pass, &[R::ProtectionObserved], Complete);
    }

    #[test]
    fn none_burst_fail_protection_not_triggered() {
        let e = eval_burst_none("RRRRRRRR", Some(50), None, BURST);
        assert_burst(&e, Verdict::Fail, &[R::ProtectionNotTriggered], Complete);
        assert_eq!(e.protected_count, 0);
    }

    #[test]
    fn none_burst_all_protected_is_account_already_locked() {
        let e = eval_burst_none("PPPPPPPP", Some(50), None, BURST);
        assert_burst(
            &e,
            Verdict::Inconclusive,
            &[R::AccountAlreadyLocked],
            Complete,
        );
    }

    #[test]
    fn none_burst_success_fails_even_with_stop() {
        let e = eval_burst_none("RRSRRRR", Some(50), Some(Stop::Cancelled), BURST);
        assert_burst(
            &e,
            Verdict::Fail,
            &[R::InvalidCredentialsAccepted, R::Cancelled],
            Incomplete,
        );
    }

    #[test]
    fn none_burst_all_unclassifiable_is_endpoint_not_confirmed() {
        for probes in ["UUUUUUUU", "TTTTTTTT"] {
            let e = eval_burst_none(probes, Some(50), None, BURST);
            assert_burst(
                &e,
                Verdict::Inconclusive,
                &[R::EndpointNotConfirmed],
                Complete,
            );
        }
        // Endpoint unconfirmed beats a bad window/spread.
        let e = eval_burst_none("UUUUUUUU", Some(500), None, BURST);
        assert_burst(
            &e,
            Verdict::Inconclusive,
            &[R::EndpointNotConfirmed],
            Complete,
        );
    }

    #[test]
    fn none_burst_bad_mixed_with_classifiable_is_error() {
        let e = eval_burst_none("RRRRRRPU", Some(200), None, BURST);
        assert_burst(&e, Verdict::Error, &[R::ProbeUnclassifiable], Complete);
        let e = eval_burst_none("RRRRRRPT", Some(50), None, BURST);
        assert_burst(
            &e,
            Verdict::Error,
            &[R::ProbeUnclassifiable, R::TransportError],
            Complete,
        );
    }

    #[test]
    fn none_burst_overlap_is_error() {
        let mut probes = seq("RRRRRRRP");
        probes[0] = overlap();
        let e = evaluate_burst_none(&probes, Some(50), None, BURST);
        assert_burst(
            &e,
            Verdict::Error,
            &[R::ProbeUnclassifiable, R::OverlappingRules],
            Complete,
        );
    }

    #[test]
    fn none_burst_dispatch_incomplete_and_skew_exceeded() {
        let e = eval_burst_none("RRRRRPPP", None, None, BURST);
        assert_burst(
            &e,
            Verdict::Inconclusive,
            &[R::DispatchIncomplete],
            Complete,
        );
        let e = eval_burst_none("RRRRRPPP", Some(101), None, BURST);
        assert_burst(
            &e,
            Verdict::Inconclusive,
            &[R::DispatchSkewExceeded],
            Complete,
        );
    }

    #[test]
    fn none_burst_wrong_probe_count_is_not_run() {
        for probes in ["RRPP", "", "RRRRRPPPPP"] {
            let e = eval_burst_none(probes, Some(50), None, BURST);
            assert_burst(&e, Verdict::NotRun, &[R::NotStarted], Incomplete);
        }
    }

    /// Oracle independent of the implementation, same shape as `b17` minus the baseline.
    #[test]
    fn none_burst_never_pass_exhaustive() {
        let spreads = [None, Some(50), Some(100), Some(101)];
        let probe_lists: Vec<Vec<Classified>> = {
            let mut lists = vec![seq("RRRRRRRR"), seq("PPPPPPPP")];
            for p in 1..8 {
                lists.push(seq(&("P".repeat(p) + &"R".repeat(8 - p))));
            }
            lists.push(seq("RRRRRRRS"));
            lists.push(seq("RRRRRRRU"));
            lists.push(seq("RRRRRRRT"));
            lists.push(seq("RRRPPP")); // wrong count
            lists
        };
        let mut passes = 0;
        for probes in &probe_lists {
            for spread in spreads {
                let e = evaluate_burst_none(probes, spread, None, BURST);
                let has_success = probes.iter().any(|p| p.outcome == Outcome::Success);
                let has_bad = probes
                    .iter()
                    .any(|p| matches!(p.outcome, Outcome::Unexpected | Outcome::TransportError));
                let protected = probes
                    .iter()
                    .filter(|p| p.outcome == Outcome::Protection)
                    .count() as u32;
                let expect_pass = !has_success
                    && probes.len() as u32 == BURST.attempts
                    && !has_bad
                    && spread.is_some_and(|s| s <= BURST.window_ms)
                    && protected > 0
                    && protected < BURST.attempts;
                assert_eq!(
                    e.verdict == Verdict::Pass,
                    expect_pass,
                    "{probes:?} {spread:?}"
                );
                passes += expect_pass as u32;
            }
        }
        assert!(passes > 0);
    }

    // ---- run_with (paused clock, scripted sender, no network) ----

    use std::cell::{Cell, RefCell};
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
        match r.evidence.as_ref() {
            Some(Evidence::BruteForce(e)) => e,
            _ => panic!("brute force always has brute force evidence"),
        }
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

    // ---- run_with baseline = none (DECISIONS 2026-10-14 Q4) ----

    fn cfg_none() -> BruteForce {
        serde_json::from_value(json!({
            "baseline": "none",
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

    /// Like `script` minus the leading baseline step: `baseline = "none"` never sends one.
    fn script_none(probes: &str) -> Vec<Step> {
        probes
            .chars()
            .map(|c| match c {
                'R' => Step::Reply(401, Some(json!({"error": SENTINEL}))),
                'P' => reply(429),
                'S' => Step::Reply(200, token()),
                'T' => Step::Fail(TransportErrorCode::Timeout),
                _ => panic!("unknown letter"),
            })
            .collect()
    }

    async fn run_script_none(
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
            std::mem::forget(tx);
        }
        let result = run_with(scripted(steps, log.clone()), &cfg_none(), rx).await;
        (result, log)
    }

    #[tokio::test(start_paused = true)]
    async fn run_none_seq_sends_only_probes_no_baseline() {
        let (r, log) = run_script_none(script_none(HEALTHY), None).await;
        assert_eq!(r.verdict, Verdict::Pass);
        assert_eq!(r.reasons, [R::ProtectionObserved]);
        let kinds = log.borrow().kinds.clone();
        assert_eq!(kinds.len(), 8);
        assert!(kinds.iter().all(|k| *k == Kind::Probe));
        let e = ev(&r);
        assert!(e.baseline.is_none());
        assert_eq!(e.baseline_mode, "none");
        // No `+1` baseline request: dispatched == attempts.
        assert_eq!(e.attempts_dispatched, 8);
        assert_eq!(e.attempts.len(), 8);
        assert_eq!(e.attempts[0].attempt, 1);
    }

    #[tokio::test(start_paused = true)]
    async fn run_none_seq_offsets_follow_interval_from_probe_1() {
        let (r, _) = run_script_none(script_none(HEALTHY), None).await;
        let e = ev(&r);
        // Probe 1 starts immediately; probe k at (k-1)*interval, same spacing as require_success.
        for (i, a) in e.attempts.iter().enumerate() {
            assert_eq!(a.offset_ms, i as u64 * 100, "probe {}", i + 1);
        }
    }

    #[tokio::test(start_paused = true)]
    async fn run_none_seq_success_probe_fails() {
        let (r, log) = run_script_none(script_none("RRSRRRRR"), None).await;
        assert_eq!(r.verdict, Verdict::Fail);
        assert_eq!(r.reasons, [R::InvalidCredentialsAccepted]);
        assert_eq!(log.borrow().kinds.len(), 8);
    }

    #[tokio::test(start_paused = true)]
    async fn run_none_seq_console_line_shows_baseline_none_token() {
        let (r, _) = run_script_none(script_none(HEALTHY), None).await;
        let line = console_line(&r);
        assert!(line.contains(" baseline=none "), "{line}");
        let (r2, _) = run_script(script(HEALTHY), None).await;
        let line2 = console_line(&r2);
        assert!(!line2.contains("baseline=none"), "{line2}");
    }

    #[tokio::test(start_paused = true)]
    async fn run_none_seq_captures_peer_ip_and_family() {
        let addr: std::net::SocketAddr = "203.0.113.5:443".parse().unwrap();
        let calls = Rc::new(Cell::new(0u32));
        let calls2 = calls.clone();
        let send_one = move |_: Kind| {
            let n = calls2.get();
            calls2.set(n + 1);
            async move {
                Ok(TransportResponse {
                    status: 401,
                    elapsed: Duration::ZERO,
                    body: None,
                    // Only the third response reports a peer; the evidence keeps the
                    // first one seen, same as every other request from the same host.
                    remote_addr: if n == 2 { Some(addr) } else { None },
                })
            }
        };
        let r = run_with(send_one, &cfg_none(), no_stop().1).await;
        let e = ev(&r);
        assert_eq!(e.peer_ip.as_deref(), Some("203.0.113.5"));
        assert_eq!(e.peer_family, Some("v4"));
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

    // ---- run_burst_with (paused clock except for one real-time test, scripted sender) ----

    fn burst_cfg(attempts: u32, window_ms: u64) -> BruteForce {
        serde_json::from_value(json!({
            "baseline_request": {"method": "POST", "path": "/login"},
            "probe_request": {"method": "POST", "path": "/login"},
            "success_match": {"status_in": [200], "json_exists": ["/access_token"]},
            "rejection_match": {"status_in": [401]},
            "protection_match": {"status_in": [429]},
            "attempts": attempts,
            "mode": "burst",
            "window_ms": window_ms
        }))
        .expect("test burst config must parse")
    }

    fn burst_evidence(r: &CheckResult) -> &BurstEvidence {
        match r.evidence.as_ref() {
            Some(Evidence::BruteForceBurst(e)) => e,
            _ => panic!("burst brute force always has burst evidence"),
        }
    }

    #[tokio::test(start_paused = true)]
    async fn run_burst_dispatches_all_probes_before_any_finishes() {
        const N: u32 = 8;
        // Every probe waits (via `yield_now`, no sleep) until all N have arrived before
        // answering, so a passing test proves dispatch happened first, not luck.
        let arrived = Rc::new(Cell::new(0u32));
        let log: Rc<RefCell<Vec<Kind>>> = Rc::new(RefCell::new(Vec::new()));
        let arrived2 = arrived.clone();
        let log2 = log.clone();
        let sender = move |kind: Kind| {
            log2.borrow_mut().push(kind);
            let arrived = arrived2.clone();
            Box::pin(async move {
                match kind {
                    Kind::Baseline => resp(200, token()),
                    Kind::Probe => {
                        arrived.set(arrived.get() + 1);
                        while arrived.get() < N {
                            tokio::task::yield_now().await;
                        }
                        resp(401, Some(json!({"error": SENTINEL})))
                    }
                }
            })
                as Pin<Box<dyn Future<Output = Result<TransportResponse, TransportError>>>>
        };
        let cfg = burst_cfg(N, 1000);
        let (_tx, rx) = no_stop();
        let r = run_burst_with(sender, &cfg, rx).await;

        let kinds = log.borrow().clone();
        assert_eq!(kinds.len(), 1 + N as usize);
        assert_eq!(kinds[0], Kind::Baseline);
        assert!(kinds[1..].iter().all(|k| *k == Kind::Probe));
        assert_eq!(arrived.get(), N);

        let e = burst_evidence(&r);
        assert_eq!(e.attempts.len(), N as usize);
        assert_eq!(e.attempts_dispatched, 1 + N);
        assert_eq!(e.attempts_planned, N);
        // No sleep anywhere: the (paused) virtual clock never advances.
        assert!(e.baseline.as_ref().unwrap().offset_ms == 0);
        assert!(e.attempts.iter().all(|a| a.offset_ms == 0));
        assert_eq!(e.dispatch_window_ms, Some(0));
        let numbers: Vec<u32> = e.attempts.iter().map(|a| a.attempt).collect();
        assert_eq!(numbers, (1..=N).collect::<Vec<u32>>());
        assert_eq!(r.verdict, Verdict::Fail); // all 8 rejected: protected_count == 0
        assert!(!format!("{r:?}").contains(SENTINEL));
        assert!(!console_line(&r).contains(SENTINEL));
    }

    #[tokio::test(start_paused = true)]
    async fn run_burst_stop_before_baseline_sends_nothing() {
        let calls = Rc::new(Cell::new(0u32));
        let calls2 = calls.clone();
        let sender = move |_: Kind| {
            calls2.set(calls2.get() + 1);
            Box::pin(async { resp(200, token()) })
                as Pin<Box<dyn Future<Output = Result<TransportResponse, TransportError>>>>
        };
        let (tx, rx) = no_stop();
        tx.send(Some(Stop::Cancelled)).expect("receiver alive");
        let cfg = burst_cfg(8, 1000);
        let r = run_burst_with(sender, &cfg, rx).await;
        assert_eq!(r.verdict, Verdict::NotRun);
        assert_eq!(r.reasons, [R::Cancelled]);
        assert_eq!(r.completion, Incomplete);
        assert_eq!(calls.get(), 0);
        let e = burst_evidence(&r);
        assert_eq!(e.attempts_dispatched, 0);
        assert!(e.baseline.is_none());
        assert!(e.attempts.is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn run_burst_stop_during_probes_drops_in_flight_requests_but_counts_them() {
        // All 8 probes dispatch together (one `join_all` poll); a stop that lands before any
        // of them answers drops every one of them, but `attempts_dispatched` still counts
        // them (they were polled, not skipped).
        let log: Rc<RefCell<Vec<Kind>>> = Rc::new(RefCell::new(Vec::new()));
        let log2 = log.clone();
        let sender = move |kind: Kind| {
            log2.borrow_mut().push(kind);
            Box::pin(async move {
                match kind {
                    Kind::Baseline => resp(200, token()),
                    Kind::Probe => std::future::pending().await,
                }
            })
                as Pin<Box<dyn Future<Output = Result<TransportResponse, TransportError>>>>
        };
        let cfg = burst_cfg(8, 1000);
        let (tx, rx) = no_stop();
        tokio::spawn(async move {
            sleep(Duration::from_millis(5)).await;
            let _ = tx.send(Some(Stop::DeadlineExceeded));
        });
        let r = run_burst_with(sender, &cfg, rx).await;
        assert_eq!(r.verdict, Verdict::NotRun);
        assert_eq!(r.reasons, [R::DeadlineExceeded]);
        assert_eq!(r.completion, Incomplete);
        assert_eq!(log.borrow().len(), 9); // baseline + 8 probes, all polled
        let e = burst_evidence(&r);
        assert_eq!(e.attempts_dispatched, 9);
        assert!(e.attempts.is_empty()); // none finished before the stop
        assert!(e.baseline.is_some());
    }

    #[tokio::test]
    async fn run_burst_real_delay_exceeds_window_is_inconclusive_dispatch_skew_exceeded() {
        // Real wall-clock time (no paused clock): the first probe's sender blocks the whole
        // (single-threaded) test thread synchronously for 30ms before `join_all` can poll the
        // next probe, so later probes measure a real, not simulated, dispatch offset. A 10ms
        // window must be exceeded by that real delay.
        let first = Rc::new(Cell::new(true));
        let sender = move |kind: Kind| {
            if kind == Kind::Probe && first.replace(false) {
                std::thread::sleep(Duration::from_millis(30));
            }
            Box::pin(async move {
                match kind {
                    Kind::Baseline => resp(200, token()),
                    Kind::Probe => resp(401, None),
                }
            })
                as Pin<Box<dyn Future<Output = Result<TransportResponse, TransportError>>>>
        };
        let cfg = burst_cfg(8, 10);
        let (_tx, rx) = no_stop();
        let r = run_burst_with(sender, &cfg, rx).await;
        assert_eq!(r.verdict, Verdict::Inconclusive);
        assert_eq!(r.reasons, [R::DispatchSkewExceeded]);
        let e = burst_evidence(&r);
        assert!(e.dispatch_window_ms.is_some_and(|w| w > 10));
    }

    #[tokio::test(start_paused = true)]
    async fn console_line_burst_format() {
        let arrived = Rc::new(Cell::new(0u32));
        let sender = move |kind: Kind| {
            let arrived = arrived.clone();
            Box::pin(async move {
                match kind {
                    Kind::Baseline => resp(200, token()),
                    Kind::Probe => {
                        let n = arrived.get();
                        arrived.set(n + 1);
                        if n < 5 {
                            resp(401, None)
                        } else {
                            resp(429, None)
                        }
                    }
                }
            })
                as Pin<Box<dyn Future<Output = Result<TransportResponse, TransportError>>>>
        };
        let cfg = burst_cfg(8, 100);
        let (_tx, rx) = no_stop();
        let r = run_burst_with(sender, &cfg, rx).await;
        assert_eq!(
            console_line(&r),
            "brute_force PASS mode=burst protected=3/8 rejected=5 window=0/100ms attempts=8/8 reasons=protection_observed"
        );
    }

    // ---- run_burst_with baseline = none (DECISIONS 2026-10-14 Q4) ----

    fn burst_cfg_none(attempts: u32, window_ms: u64) -> BruteForce {
        serde_json::from_value(json!({
            "baseline": "none",
            "probe_request": {"method": "POST", "path": "/login"},
            "success_match": {"status_in": [200], "json_exists": ["/access_token"]},
            "rejection_match": {"status_in": [401]},
            "protection_match": {"status_in": [429]},
            "attempts": attempts,
            "mode": "burst",
            "window_ms": window_ms
        }))
        .expect("test burst config must parse")
    }

    #[tokio::test(start_paused = true)]
    async fn run_burst_none_sends_only_probes_no_baseline() {
        let log: Rc<RefCell<Vec<Kind>>> = Rc::new(RefCell::new(Vec::new()));
        let log2 = log.clone();
        let sender = move |kind: Kind| {
            log2.borrow_mut().push(kind);
            Box::pin(async move {
                match kind {
                    Kind::Baseline => panic!("baseline = none must never send a baseline"),
                    Kind::Probe => resp(401, None),
                }
            })
                as Pin<Box<dyn Future<Output = Result<TransportResponse, TransportError>>>>
        };
        let cfg = burst_cfg_none(8, 100);
        let (_tx, rx) = no_stop();
        let r = run_burst_with(sender, &cfg, rx).await;

        let kinds = log.borrow().clone();
        assert_eq!(kinds.len(), 8);
        assert!(kinds.iter().all(|k| *k == Kind::Probe));
        let e = burst_evidence(&r);
        assert!(e.baseline.is_none());
        assert_eq!(e.baseline_mode, "none");
        // No `+1` baseline request: dispatched == attempts.
        assert_eq!(e.attempts_dispatched, 8);
        assert_eq!(e.attempts.len(), 8);
        assert_eq!(r.verdict, Verdict::Fail); // all 8 rejected: protected_count == 0
        assert_eq!(r.reasons, [R::ProtectionNotTriggered]);
    }

    #[tokio::test(start_paused = true)]
    async fn console_line_burst_none_format() {
        let arrived = Rc::new(Cell::new(0u32));
        let sender = move |kind: Kind| {
            let arrived = arrived.clone();
            Box::pin(async move {
                match kind {
                    Kind::Baseline => panic!("baseline = none must never send a baseline"),
                    Kind::Probe => {
                        let n = arrived.get();
                        arrived.set(n + 1);
                        if n < 5 {
                            resp(401, None)
                        } else {
                            resp(429, None)
                        }
                    }
                }
            })
                as Pin<Box<dyn Future<Output = Result<TransportResponse, TransportError>>>>
        };
        let cfg = burst_cfg_none(8, 100);
        let (_tx, rx) = no_stop();
        let r = run_burst_with(sender, &cfg, rx).await;
        assert_eq!(
            console_line(&r),
            "brute_force PASS mode=burst baseline=none protected=3/8 rejected=5 window=0/100ms attempts=8/8 reasons=protection_observed"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn run_burst_none_captures_peer_ip_and_family() {
        let addr: std::net::SocketAddr = "203.0.113.5:443".parse().unwrap();
        let calls = Rc::new(Cell::new(0u32));
        let sender = move |_: Kind| {
            let n = calls.get();
            calls.set(n + 1);
            Box::pin(async move {
                Ok(TransportResponse {
                    status: 401,
                    elapsed: Duration::ZERO,
                    body: None,
                    remote_addr: if n == 2 { Some(addr) } else { None },
                })
            })
                as Pin<Box<dyn Future<Output = Result<TransportResponse, TransportError>>>>
        };
        let cfg = burst_cfg_none(8, 100);
        let (_tx, rx) = no_stop();
        let r = run_burst_with(sender, &cfg, rx).await;
        let e = burst_evidence(&r);
        assert_eq!(e.peer_ip.as_deref(), Some("203.0.113.5"));
        assert_eq!(e.peer_family, Some("v4"));
    }
}
