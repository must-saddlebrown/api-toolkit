//! race check (PLAN §6). This file holds the pure parts: `read_verifier` (one verifier
//! response -> counter or problem), `action_allowed` and `evaluate` (all facts -> verdict).
//! Dispatch spread (`dispatch_spread_ms`) lives in `checks::mod` and is shared with
//! `brute_force` burst. The async parts are `run_with` (before -> N actions together ->
//! settle -> after, with a closure as sender so tests need no network), `run` (real
//! transport) and `console_line`.

use std::time::Duration;

use futures_util::future::join_all;
use tokio::sync::watch;
use tokio::time::{Instant, sleep};

use crate::checks::{Stop, current_stop, dispatch_spread_ms, wait_stop};
use crate::config::{MatcherSpec, Race};
use crate::http::request::ResolvedRequest;
use crate::http::{Transport, TransportError, TransportResponse, send};
use crate::report::{
    ActionEvidence, CheckName, CheckResult, Completion, Evidence, RaceEvidence, ReasonCode,
    TransportErrorCode, Verdict, VerifyEvidence,
};

/// What one verifier response tells us.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerifyRead {
    Transport(TransportErrorCode),
    /// The response did not satisfy `verify_match`.
    Mismatch,
    /// Body is JSON but `counter_pointer` points at nothing (absent; `null` is not absent).
    CounterMissing,
    /// Body is not JSON / empty, or the value is not an integer >= 0.
    CounterInvalid,
    Counter(u64),
}

/// PLAN §6: transport error, then `verify_match`, then the counter at `pointer`.
/// `as_u64` accepts only non-negative integers: strings, floats, bools, null and
/// negatives are all `CounterInvalid`. The body is dropped after reading.
pub fn read_verifier(
    result: &Result<TransportResponse, TransportError>,
    verify_match: &MatcherSpec,
    pointer: &str,
) -> VerifyRead {
    let response = match result {
        Ok(response) => response,
        Err(e) => return VerifyRead::Transport(e.code),
    };
    let body = response.body.as_ref();
    if !verify_match.matches(response.status, body) {
        return VerifyRead::Mismatch;
    }
    let Some(body) = body else {
        return VerifyRead::CounterInvalid;
    };
    // `pointer` gives None for absent but Some(Null) for an explicit null.
    match body.pointer(pointer) {
        None => VerifyRead::CounterMissing,
        Some(v) => match v.as_u64() {
            Some(n) => VerifyRead::Counter(n),
            None => VerifyRead::CounterInvalid,
        },
    }
}

/// True when the action got a response whose status is in `expected`.
/// A transport error is never allowed.
pub fn action_allowed(
    result: &Result<TransportResponse, TransportError>,
    expected: &[u16],
) -> bool {
    matches!(result, Ok(r) if expected.contains(&r.status))
}

/// The part of one finished action that the verdict needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ActionOutcome {
    pub allowed: bool,
    pub transport_error: bool,
}

/// Thresholds `evaluate` needs, copied from the config.
#[derive(Debug, Clone, Copy)]
pub struct Thresholds {
    pub concurrency: u32,
    pub max_dispatch_skew_ms: u64,
}

/// What `evaluate` decides.
#[derive(Debug, PartialEq, Eq)]
pub struct Evaluation {
    pub verdict: Verdict,
    pub reasons: Vec<ReasonCode>,
    pub completion: Completion,
    pub counter_before: Option<u64>,
    pub counter_after: Option<u64>,
    pub delta: Option<u64>,
}

/// Verifier AFTER the actions that gave no counter (always ERROR). `None` means it gave one.
fn verifier_problem(read: VerifyRead) -> Option<Vec<ReasonCode>> {
    match read {
        VerifyRead::Transport(_) => {
            Some(vec![ReasonCode::VerifierFailed, ReasonCode::TransportError])
        }
        VerifyRead::Mismatch => Some(vec![ReasonCode::VerifierFailed]),
        VerifyRead::CounterMissing | VerifyRead::CounterInvalid => {
            Some(vec![ReasonCode::CounterInvalid])
        }
        VerifyRead::Counter(_) => None,
    }
}

/// Verifier BEFORE the actions (PLAN §6 V1-V5). `None` means a usable counter.
/// Mismatch and a missing counter mean the oracle is not ready yet: INCONCLUSIVE.
fn before_problem(read: VerifyRead) -> Option<(Verdict, Vec<ReasonCode>)> {
    match read {
        VerifyRead::Mismatch => Some((Verdict::Inconclusive, vec![ReasonCode::BaselineUnreadable])),
        VerifyRead::CounterMissing => {
            Some((Verdict::Inconclusive, vec![ReasonCode::CounterNotAvailable]))
        }
        VerifyRead::Transport(_) | VerifyRead::CounterInvalid => {
            verifier_problem(read).map(|r| (Verdict::Error, r))
        }
        VerifyRead::Counter(_) => None,
    }
}

/// PLAN §6 Verdict, in the precedence order of the P2 handoff.
/// `before` / `after` are `None` when the check stopped before that read finished.
/// `actions` holds only the finished actions. `skew` comes from `skew_ms`.
pub fn evaluate(
    before: Option<VerifyRead>,
    after: Option<VerifyRead>,
    actions: &[ActionOutcome],
    skew: Option<u64>,
    stop: Option<Stop>,
    t: Thresholds,
) -> Evaluation {
    let counter_before = match before {
        Some(VerifyRead::Counter(n)) => Some(n),
        _ => None,
    };
    let counter_after = match after {
        Some(VerifyRead::Counter(n)) => Some(n),
        _ => None,
    };
    let mut delta = None;
    let make = |verdict, reasons, completion, delta| Evaluation {
        verdict,
        reasons,
        completion,
        counter_before,
        counter_after,
        delta,
    };
    // No read and no stop should not happen, but it must never become a PASS.
    let stop_reason = match stop {
        Some(s) => s.reason(),
        None => ReasonCode::NotStarted,
    };

    // 1. Stopped before the before-read finished.
    let Some(before) = before else {
        return make(
            Verdict::NotRun,
            vec![stop_reason],
            Completion::Incomplete,
            delta,
        );
    };

    // 2. Before is not a usable counter: no action was sent.
    if let Some((verdict, reasons)) = before_problem(before) {
        return make(verdict, reasons, Completion::Complete, delta);
    }
    if counter_before != Some(0) {
        return make(
            Verdict::Inconclusive,
            vec![ReasonCode::InitialCounterNotZero],
            Completion::Complete,
            delta,
        );
    }

    // 3. Stopped before the after-read finished. Action statuses are no proof of duplication.
    let Some(after) = after else {
        return make(
            Verdict::NotRun,
            vec![stop_reason],
            Completion::Incomplete,
            delta,
        );
    };

    // 4. Extras, in a fixed order.
    let has_ambiguous = actions.iter().any(|a| !a.allowed);
    let has_transport = actions.iter().any(|a| a.transport_error);
    let mut extras = Vec::new();
    if has_ambiguous {
        extras.push(ReasonCode::ActionOutcomeAmbiguous);
    }
    if has_transport {
        extras.push(ReasonCode::TransportError);
    }
    // Finished actions without a skew value cannot happen in `run_with`; never PASS on it.
    let dispatch_incomplete =
        actions.len() as u32 != t.concurrency || (skew.is_none() && !actions.is_empty());
    if dispatch_incomplete {
        extras.push(ReasonCode::DispatchIncomplete);
    }
    let skew_exceeded = skew.is_some_and(|s| s > t.max_dispatch_skew_ms);
    if skew_exceeded {
        extras.push(ReasonCode::DispatchSkewExceeded);
    }

    // 5. After is not a usable counter.
    if let Some(mut reasons) = verifier_problem(after) {
        for extra in extras {
            if !reasons.contains(&extra) {
                reasons.push(extra);
            }
        }
        return make(Verdict::Error, reasons, Completion::Complete, delta);
    }

    // 6. Before is 0, so the delta is the after counter.
    let d = counter_after.unwrap_or(0);
    delta = Some(d);
    if d > 1 {
        let mut reasons = vec![ReasonCode::DuplicateSideEffect];
        reasons.extend(extras);
        return make(Verdict::Fail, reasons, Completion::Complete, delta);
    }

    // 7. Wrong number of finished actions, 8. ambiguous action: never PASS, never FAIL.
    if dispatch_incomplete || has_ambiguous {
        return make(Verdict::Inconclusive, extras, Completion::Complete, delta);
    }

    // 9. Everything was allowed and nothing happened.
    if d == 0 {
        let mut reasons = vec![ReasonCode::ActionNotExecuted];
        reasons.extend(extras);
        return make(Verdict::Fail, reasons, Completion::Complete, delta);
    }

    // 10. Dispatch was not tight enough to call it a race.
    if skew_exceeded {
        return make(Verdict::Inconclusive, extras, Completion::Complete, delta);
    }

    // 11. d == 1
    make(
        Verdict::Pass,
        vec![ReasonCode::SingleSideEffect],
        Completion::Complete,
        delta,
    )
}

/// Which request the sender closure must send.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    VerifyBefore,
    Action,
    VerifyAfter,
}

fn millis(d: Duration) -> u64 {
    d.as_millis() as u64
}

/// Send one verifier request and keep only the counter reading and numbers.
/// `Err(stop)` means a stop dropped the request in flight.
async fn verify_once<F, Fut>(
    send_one: &F,
    kind: Kind,
    cfg: &Race,
    stop_rx: &mut watch::Receiver<Option<Stop>>,
    start: Instant,
) -> Result<(VerifyRead, VerifyEvidence), Stop>
where
    F: Fn(Kind) -> Fut,
    Fut: Future<Output = Result<TransportResponse, TransportError>>,
{
    let offset = start.elapsed();
    let sent_at = Instant::now();
    let result = tokio::select! {
        biased;
        s = wait_stop(stop_rx) => return Err(s),
        r = send_one(kind) => r,
    };
    let read = read_verifier(&result, &cfg.verify_match, &cfg.counter_pointer);
    let evidence = VerifyEvidence {
        status: result.as_ref().ok().map(|r| r.status),
        error: result.as_ref().err().map(|e| e.code),
        offset_ms: millis(offset),
        elapsed_ms: millis(sent_at.elapsed()),
    };
    // `result` (and its body) is dropped here; only numbers and enums are kept.
    Ok((read, evidence))
}

/// PLAN §6 steps 1-4: read the counter, send `concurrency` actions together, wait `settle_ms`,
/// read the counter once more. No retry. A stop drops whatever is in flight (actions are still
/// counted in `actions_dispatched`, not recorded in `actions`).
/// Precondition: `cfg` passed `validate`.
pub async fn run_with<F, Fut>(
    send_one: F,
    cfg: &Race,
    mut stop_rx: watch::Receiver<Option<Stop>>,
) -> CheckResult
where
    F: Fn(Kind) -> Fut,
    Fut: Future<Output = Result<TransportResponse, TransportError>>,
{
    let start = Instant::now();
    let mut stopped: Option<Stop> = None;
    let mut before: Option<(VerifyRead, VerifyEvidence)> = None;
    let mut after: Option<(VerifyRead, VerifyEvidence)> = None;
    let mut action_evidence: Vec<ActionEvidence> = Vec::new();
    let mut outcomes: Vec<ActionOutcome> = Vec::new();
    let mut dispatch_offsets: Vec<Duration> = Vec::new();

    'run: {
        // Never dispatch once a stop is signalled (also covers "stop before start").
        if let Some(s) = current_stop(&stop_rx) {
            stopped = Some(s);
            break 'run;
        }
        match verify_once(&send_one, Kind::VerifyBefore, cfg, &mut stop_rx, start).await {
            Ok(r) => before = Some(r),
            Err(s) => {
                stopped = Some(s);
                break 'run;
            }
        }
        // Only a counter of exactly 0 allows actions (PLAN §6 step 1).
        if !matches!(before, Some((VerifyRead::Counter(0), _))) {
            break 'run;
        }
        if let Some(s) = current_stop(&stop_rx) {
            stopped = Some(s);
            break 'run;
        }

        // Barrier: futures are lazy, so nothing is sent until `join_all` polls them, and it
        // polls all of them in one go. Each one notes its dispatch time on the first poll.
        // ponytail: one task, so skew is microseconds within a single poll; if that proves too
        // coarse, spawn per action and release them with a `tokio::sync::Barrier`.
        let send_ref = &send_one;
        let expected = &cfg.expected_action_statuses;
        let runs = join_all((1..=cfg.concurrency).map(|number| {
            let mut rx = stop_rx.clone();
            async move {
                let offset = start.elapsed();
                let sent_at = Instant::now();
                let result = tokio::select! {
                    biased;
                    _ = wait_stop(&mut rx) => return (offset, None),
                    r = send_ref(Kind::Action) => r,
                };
                let allowed = action_allowed(&result, expected);
                let evidence = ActionEvidence {
                    action: number,
                    status: result.as_ref().ok().map(|r| r.status),
                    error: result.as_ref().err().map(|e| e.code),
                    allowed,
                    offset_ms: millis(offset),
                    elapsed_ms: millis(sent_at.elapsed()),
                };
                let outcome = ActionOutcome {
                    allowed,
                    transport_error: result.is_err(),
                };
                (offset, Some((evidence, outcome)))
            }
        }))
        .await;
        let mut any_dropped = false;
        for (offset, done) in runs {
            dispatch_offsets.push(offset);
            match done {
                Some((evidence, outcome)) => {
                    action_evidence.push(evidence);
                    outcomes.push(outcome);
                }
                None => any_dropped = true,
            }
        }
        if any_dropped {
            stopped = current_stop(&stop_rx);
            break 'run;
        }

        // Settle, then exactly one verifier after.
        tokio::select! {
            biased;
            s = wait_stop(&mut stop_rx) => { stopped = Some(s); break 'run; }
            _ = sleep(Duration::from_millis(cfg.settle_ms)) => {}
        }
        if let Some(s) = current_stop(&stop_rx) {
            stopped = Some(s);
            break 'run;
        }
        match verify_once(&send_one, Kind::VerifyAfter, cfg, &mut stop_rx, start).await {
            Ok(r) => after = Some(r),
            Err(s) => stopped = Some(s),
        }
    }

    let dispatch_skew_ms = dispatch_spread_ms(&dispatch_offsets);
    let evaluation = evaluate(
        before.as_ref().map(|(r, _)| *r),
        after.as_ref().map(|(r, _)| *r),
        &outcomes,
        dispatch_skew_ms,
        stopped,
        Thresholds {
            concurrency: cfg.concurrency,
            max_dispatch_skew_ms: cfg.max_dispatch_skew_ms,
        },
    );

    CheckResult {
        check: CheckName::Race,
        verdict: evaluation.verdict,
        reasons: evaluation.reasons,
        completion: evaluation.completion,
        evidence: Some(Evidence::Race(RaceEvidence {
            concurrency: cfg.concurrency,
            expected_action_statuses: cfg.expected_action_statuses.clone(),
            max_dispatch_skew_ms: cfg.max_dispatch_skew_ms,
            settle_ms: cfg.settle_ms,
            actions_dispatched: dispatch_offsets.len() as u32,
            dispatch_skew_ms,
            counter_before: evaluation.counter_before,
            counter_after: evaluation.counter_after,
            delta: evaluation.delta,
            verify_before: before.map(|(_, e)| e),
            verify_after: after.map(|(_, e)| e),
            actions: action_evidence,
        })),
    }
}

/// The real check: `run_with` sending through the shared transport.
pub async fn run(
    transport: &Transport,
    cfg: &Race,
    action: &ResolvedRequest,
    verify: &ResolvedRequest,
    stop: watch::Receiver<Option<Stop>>,
) -> CheckResult {
    let sender = |kind: Kind| {
        let request = match kind {
            Kind::Action => action,
            Kind::VerifyBefore | Kind::VerifyAfter => verify,
        };
        send(transport, request)
    };
    run_with(sender, cfg, stop).await
}

/// One console line (PLAN §10), e.g.
/// `race PASS delta=1 before=0 after=1 actions=5/5 skew=0/100ms reasons=single_side_effect`.
/// Built from enums and numbers only; `none` for a null value.
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
    let or_none = |v: Option<u64>| v.map_or("none".to_string(), |n| n.to_string());
    match &result.evidence {
        Some(Evidence::Race(e)) => format!(
            "{head} delta={} before={} after={} actions={}/{} skew={}/{}ms reasons={reasons}",
            or_none(e.delta),
            or_none(e.counter_before),
            or_none(e.counter_after),
            e.actions.len(),
            e.concurrency,
            or_none(e.dispatch_skew_ms),
            e.max_dispatch_skew_ms,
        ),
        _ => format!("{head} reasons={reasons}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    fn ok(status: u16, body: Option<Value>) -> Result<TransportResponse, TransportError> {
        Ok(TransportResponse {
            status,
            elapsed: Duration::ZERO,
            body,
        })
    }

    fn err(code: TransportErrorCode) -> Result<TransportResponse, TransportError> {
        Err(TransportError::from(code))
    }

    fn verify_200() -> MatcherSpec {
        MatcherSpec {
            status_in: vec![200],
            json_equals: None,
            json_exists: None,
        }
    }

    fn read(body: Option<Value>) -> VerifyRead {
        read_verifier(&ok(200, body), &verify_200(), "/execution_count")
    }

    #[test]
    fn read_verifier_classifies_each_response() {
        use VerifyRead::*;
        assert_eq!(
            read_verifier(
                &err(TransportErrorCode::Timeout),
                &verify_200(),
                "/execution_count"
            ),
            Transport(TransportErrorCode::Timeout)
        );
        let good = Some(json!({"execution_count": 1}));
        assert_eq!(
            read_verifier(&ok(401, good), &verify_200(), "/execution_count"),
            Mismatch
        );
        assert_eq!(read(None), CounterInvalid);
        assert_eq!(read(Some(json!({"other": 1}))), CounterMissing);
        // explicit null is present, so not CounterMissing
        assert_eq!(read(Some(json!({"execution_count": null}))), CounterInvalid);
        // a whole-body JSON null: pointer "/x" is absent
        assert_eq!(read(Some(json!(null))), CounterMissing);
        let cases: [(Value, VerifyRead); 9] = [
            (json!("1"), CounterInvalid),
            (json!(1.5), CounterInvalid),
            (json!(true), CounterInvalid),
            (json!(null), CounterInvalid),
            (json!(-1), CounterInvalid),
            (json!(0), Counter(0)),
            (json!(7), Counter(7)),
            (json!(u64::MAX), Counter(u64::MAX)),
            (json!(1.0), CounterInvalid),
        ];
        for (value, expected) in cases {
            let shown = value.to_string();
            assert_eq!(
                read(Some(json!({ "execution_count": value }))),
                expected,
                "value {shown}"
            );
        }
    }

    #[test]
    fn action_allowed_checks_status_and_transport() {
        let expected = [200, 409];
        assert!(action_allowed(&ok(200, None), &expected));
        assert!(action_allowed(&ok(409, None), &expected));
        assert!(!action_allowed(&ok(202, None), &expected));
        assert!(!action_allowed(&ok(500, None), &expected));
        assert!(!action_allowed(
            &err(TransportErrorCode::Timeout),
            &expected
        ));
    }

    // ---- evaluate ----

    const T: Thresholds = Thresholds {
        concurrency: 5,
        max_dispatch_skew_ms: 100,
    };
    const GOOD: ActionOutcome = ActionOutcome {
        allowed: true,
        transport_error: false,
    };
    const AMBIGUOUS: ActionOutcome = ActionOutcome {
        allowed: false,
        transport_error: false,
    };
    const TIMEOUT: ActionOutcome = ActionOutcome {
        allowed: false,
        transport_error: true,
    };

    fn five(first: ActionOutcome) -> Vec<ActionOutcome> {
        vec![first, GOOD, GOOD, GOOD, GOOD]
    }

    fn counter(n: u64) -> Option<VerifyRead> {
        Some(VerifyRead::Counter(n))
    }

    /// Complete run: before 0, `after`, the given actions and skew, no stop.
    fn run(after: Option<VerifyRead>, actions: &[ActionOutcome], skew: u64) -> Evaluation {
        evaluate(counter(0), after, actions, Some(skew), None, T)
    }

    fn check(
        e: Evaluation,
        verdict: Verdict,
        reasons: &[ReasonCode],
        completion: Completion,
        delta: Option<u64>,
    ) {
        assert_eq!(e.verdict, verdict);
        assert_eq!(e.reasons, reasons);
        assert_eq!(e.completion, completion);
        assert_eq!(e.delta, delta);
    }

    use Completion::{Complete, Incomplete};
    use ReasonCode as R;
    use Verdict::{Error, Fail, Inconclusive, NotRun, Pass};

    #[test]
    fn r1_healthy_passes() {
        let e = run(counter(1), &five(GOOD), 0);
        assert_eq!((e.counter_before, e.counter_after), (Some(0), Some(1)));
        check(e, Pass, &[R::SingleSideEffect], Complete, Some(1));
    }

    #[test]
    fn r2_duplicate_fails() {
        let e = run(counter(2), &five(GOOD), 0);
        check(e, Fail, &[R::DuplicateSideEffect], Complete, Some(2));
    }

    #[test]
    fn r3_idempotent_all_allowed_passes() {
        let e = run(counter(1), &[GOOD; 5], 3);
        check(e, Pass, &[R::SingleSideEffect], Complete, Some(1));
    }

    #[test]
    fn r4_counter_not_zero_is_inconclusive() {
        let e = evaluate(counter(1), None, &[], None, None, T);
        check(e, Inconclusive, &[R::InitialCounterNotZero], Complete, None);
    }

    #[test]
    fn r5_r6_before_counter_invalid_is_error() {
        let e = evaluate(Some(VerifyRead::CounterInvalid), None, &[], None, None, T);
        check(e, Error, &[R::CounterInvalid], Complete, None);
    }

    #[test]
    fn r7_after_counter_invalid_is_error_with_extras() {
        let after = Some(VerifyRead::CounterInvalid);
        let e = run(after, &five(GOOD), 0);
        check(e, Error, &[R::CounterInvalid], Complete, None);
        let e = run(after, &five(AMBIGUOUS), 150);
        check(
            e,
            Error,
            &[
                R::CounterInvalid,
                R::ActionOutcomeAmbiguous,
                R::DispatchSkewExceeded,
            ],
            Complete,
            None,
        );
    }

    #[test]
    fn r8_skew_over_budget_is_inconclusive() {
        let e = run(counter(1), &five(GOOD), 150);
        check(
            e,
            Inconclusive,
            &[R::DispatchSkewExceeded],
            Complete,
            Some(1),
        );
    }

    #[test]
    fn r9_skew_at_budget_passes() {
        let e = run(counter(1), &five(GOOD), 100);
        check(e, Pass, &[R::SingleSideEffect], Complete, Some(1));
    }

    #[test]
    fn r10_skew_and_duplicate_fails() {
        let e = run(counter(3), &five(GOOD), 150);
        check(
            e,
            Fail,
            &[R::DuplicateSideEffect, R::DispatchSkewExceeded],
            Complete,
            Some(3),
        );
    }

    #[test]
    fn r11_delta_zero_fails() {
        let e = run(counter(0), &five(GOOD), 0);
        check(e, Fail, &[R::ActionNotExecuted], Complete, Some(0));
    }

    #[test]
    fn r12_delta_zero_and_skew_fails() {
        let e = run(counter(0), &five(GOOD), 150);
        check(
            e,
            Fail,
            &[R::ActionNotExecuted, R::DispatchSkewExceeded],
            Complete,
            Some(0),
        );
    }

    #[test]
    fn r13_disallowed_status_is_inconclusive() {
        let e = run(counter(1), &five(AMBIGUOUS), 0);
        check(
            e,
            Inconclusive,
            &[R::ActionOutcomeAmbiguous],
            Complete,
            Some(1),
        );
    }

    #[test]
    fn r14_pending_202_with_delta_zero_is_not_a_fail() {
        let e = run(counter(0), &five(AMBIGUOUS), 0);
        check(
            e,
            Inconclusive,
            &[R::ActionOutcomeAmbiguous],
            Complete,
            Some(0),
        );
    }

    #[test]
    fn r15_action_transport_error_is_inconclusive() {
        let e = run(counter(1), &five(TIMEOUT), 0);
        check(
            e,
            Inconclusive,
            &[R::ActionOutcomeAmbiguous, R::TransportError],
            Complete,
            Some(1),
        );
    }

    #[test]
    fn r16_ambiguous_and_duplicate_fails() {
        let e = run(counter(2), &five(AMBIGUOUS), 0);
        check(
            e,
            Fail,
            &[R::DuplicateSideEffect, R::ActionOutcomeAmbiguous],
            Complete,
            Some(2),
        );
    }

    #[test]
    fn r16b_duplicate_with_transport_error_action_fails_with_three_reasons() {
        let e = run(counter(2), &five(TIMEOUT), 0);
        check(
            e,
            Fail,
            &[
                R::DuplicateSideEffect,
                R::ActionOutcomeAmbiguous,
                R::TransportError,
            ],
            Complete,
            Some(2),
        );
    }

    #[test]
    fn r17_ambiguous_and_skew_is_inconclusive() {
        let e = run(counter(1), &five(AMBIGUOUS), 150);
        check(
            e,
            Inconclusive,
            &[R::ActionOutcomeAmbiguous, R::DispatchSkewExceeded],
            Complete,
            Some(1),
        );
    }

    #[test]
    fn r18_before_transport_error_is_error() {
        let before = Some(VerifyRead::Transport(TransportErrorCode::ConnectFailed));
        let e = evaluate(before, None, &[], None, None, T);
        check(
            e,
            Error,
            &[R::VerifierFailed, R::TransportError],
            Complete,
            None,
        );
    }

    #[test]
    fn r19_before_mismatch_is_inconclusive_baseline_unreadable() {
        let e = evaluate(Some(VerifyRead::Mismatch), None, &[], None, None, T);
        check(e, Inconclusive, &[R::BaselineUnreadable], Complete, None);
    }

    #[test]
    fn q2_v1_before_transport_error_is_error_verifier_failed_transport_error() {
        for code in [
            TransportErrorCode::Timeout,
            TransportErrorCode::ConnectFailed,
        ] {
            let e = evaluate(Some(VerifyRead::Transport(code)), None, &[], None, None, T);
            check(
                e,
                Error,
                &[R::VerifierFailed, R::TransportError],
                Complete,
                None,
            );
        }
    }

    #[test]
    fn q2_v2_mismatch_is_inconclusive_baseline_unreadable() {
        // read_verifier level: 401/404/500 do not satisfy verify_match.
        for status in [401, 404, 500] {
            let r = read_verifier(
                &ok(status, Some(json!({"execution_count": 0}))),
                &verify_200(),
                "/execution_count",
            );
            assert_eq!(r, VerifyRead::Mismatch, "status {status}");
        }
        let e = evaluate(Some(VerifyRead::Mismatch), None, &[], None, None, T);
        assert_eq!(e.counter_before, None);
        check(e, Inconclusive, &[R::BaselineUnreadable], Complete, None);
    }

    #[test]
    fn q2_v3_non_json_or_empty_body_is_error_counter_invalid() {
        // status matches, body is absent (non-JSON or empty)
        assert_eq!(read(None), VerifyRead::CounterInvalid);
        let e = evaluate(Some(VerifyRead::CounterInvalid), None, &[], None, None, T);
        check(e, Error, &[R::CounterInvalid], Complete, None);
    }

    #[test]
    fn q2_v3b_mismatch_wins_over_non_json_body() {
        // verify_match has json_exists and the body is not JSON: V2 before V3.
        let m = MatcherSpec {
            status_in: vec![200],
            json_equals: None,
            json_exists: Some(vec!["/execution_count".to_string()]),
        };
        let r = read_verifier(&ok(200, None), &m, "/execution_count");
        assert_eq!(r, VerifyRead::Mismatch);
        let e = evaluate(Some(r), None, &[], None, None, T);
        check(e, Inconclusive, &[R::BaselineUnreadable], Complete, None);
    }

    #[test]
    fn q2_v4_missing_counter_is_inconclusive_counter_not_available() {
        let r = read(Some(json!({"id": "a"})));
        assert_eq!(r, VerifyRead::CounterMissing);
        let e = evaluate(Some(r), None, &[], None, None, T);
        check(e, Inconclusive, &[R::CounterNotAvailable], Complete, None);
    }

    #[test]
    fn q2_v5_present_but_not_integer_is_error_counter_invalid() {
        for v in [json!("0"), json!(1.5), json!(true), json!(null), json!(-1)] {
            let shown = v.to_string();
            let r = read(Some(json!({ "execution_count": v })));
            assert_eq!(r, VerifyRead::CounterInvalid, "value {shown}");
            let e = evaluate(Some(r), None, &[], None, None, T);
            check(e, Error, &[R::CounterInvalid], Complete, None);
        }
    }

    #[test]
    fn q2_null_counter_is_not_counter_not_available() {
        let r = read(Some(json!({"execution_count": null})));
        assert_ne!(r, VerifyRead::CounterMissing);
        assert_eq!(r, VerifyRead::CounterInvalid);
    }

    #[test]
    fn q2_after_unreadable_stays_error() {
        let cases = [
            (VerifyRead::Mismatch, vec![R::VerifierFailed]),
            (VerifyRead::CounterMissing, vec![R::CounterInvalid]),
            (VerifyRead::CounterInvalid, vec![R::CounterInvalid]),
            (
                VerifyRead::Transport(TransportErrorCode::Timeout),
                vec![R::VerifierFailed, R::TransportError],
            ),
        ];
        for (after, reasons) in cases {
            let e = run(Some(after), &five(GOOD), 0);
            check(e, Error, &reasons, Complete, None);
        }
        // extras are still appended
        let e = run(Some(VerifyRead::CounterMissing), &five(AMBIGUOUS), 150);
        check(
            e,
            Error,
            &[
                R::CounterInvalid,
                R::ActionOutcomeAmbiguous,
                R::DispatchSkewExceeded,
            ],
            Complete,
            None,
        );
    }

    #[test]
    fn q2_never_pass_exhaustive() {
        let reads = [
            None,
            Some(VerifyRead::Transport(TransportErrorCode::Timeout)),
            Some(VerifyRead::Mismatch),
            Some(VerifyRead::CounterMissing),
            Some(VerifyRead::CounterInvalid),
            Some(VerifyRead::Counter(0)),
            Some(VerifyRead::Counter(1)),
            Some(VerifyRead::Counter(2)),
        ];
        let stops = [None, Some(Stop::Cancelled), Some(Stop::DeadlineExceeded)];
        let action_sets: Vec<Vec<ActionOutcome>> = vec![
            vec![],
            vec![GOOD; 4],
            vec![GOOD; 5],
            vec![GOOD; 6],
            five(AMBIGUOUS),
            five(TIMEOUT),
        ];
        let skews = [None, Some(0), Some(100), Some(101)];
        let mut passes = 0;
        for before in reads {
            for after in reads {
                for stop in stops {
                    for actions in &action_sets {
                        for skew in skews {
                            let e = evaluate(before, after, actions, skew, stop, T);
                            let expect_pass = before == Some(VerifyRead::Counter(0))
                                && after == Some(VerifyRead::Counter(1))
                                && actions.iter().all(|a| a.allowed)
                                && actions.len() as u32 == T.concurrency
                                && skew.is_some_and(|s| s <= T.max_dispatch_skew_ms)
                                // a stop after the after-read finished keeps the verdict (r29);
                                // before that, `after` is None and PASS is already excluded.
                                && (stop.is_none() || after.is_some());
                            assert_eq!(
                                e.verdict == Pass,
                                expect_pass,
                                "{before:?} {after:?} {stop:?} {actions:?} {skew:?}"
                            );
                            passes += expect_pass as u32;
                        }
                    }
                }
            }
        }
        assert!(passes > 0);
    }

    #[test]
    fn r20_after_transport_error_is_error_without_duplicate_reason() {
        let after = Some(VerifyRead::Transport(TransportErrorCode::Timeout));
        let e = run(after, &five(TIMEOUT), 150);
        check(
            e,
            Error,
            &[
                R::VerifierFailed,
                R::TransportError,
                R::ActionOutcomeAmbiguous,
                R::DispatchSkewExceeded,
            ],
            Complete,
            None,
        );
    }

    #[test]
    fn r21_after_mismatch_is_error_with_extras() {
        let e = run(Some(VerifyRead::Mismatch), &five(AMBIGUOUS), 0);
        check(
            e,
            Error,
            &[R::VerifierFailed, R::ActionOutcomeAmbiguous],
            Complete,
            None,
        );
    }

    #[test]
    fn r22_wrong_action_count_without_stop_is_inconclusive() {
        let e = run(counter(1), &[GOOD; 4], 0);
        check(e, Inconclusive, &[R::DispatchIncomplete], Complete, Some(1));
        // With an ambiguous action the order is fixed: ambiguous first.
        let e = run(counter(1), &[AMBIGUOUS; 4], 0);
        check(
            e,
            Inconclusive,
            &[R::ActionOutcomeAmbiguous, R::DispatchIncomplete],
            Complete,
            Some(1),
        );
        // Too many is just as wrong; and delta 0 with a short list is not a FAIL.
        let e = run(counter(0), &[GOOD; 4], 0);
        check(e, Inconclusive, &[R::DispatchIncomplete], Complete, Some(0));
        let e = run(counter(1), &[GOOD; 6], 0);
        check(e, Inconclusive, &[R::DispatchIncomplete], Complete, Some(1));
    }

    #[test]
    fn r24_stop_before_start_is_not_run() {
        let e = evaluate(None, None, &[], None, Some(Stop::Cancelled), T);
        check(e, NotRun, &[R::Cancelled], Incomplete, None);
        let e = evaluate(None, None, &[], None, Some(Stop::DeadlineExceeded), T);
        check(e, NotRun, &[R::DeadlineExceeded], Incomplete, None);
        // Defensive: no read and no stop is NOT_RUN, never PASS.
        let e = evaluate(None, None, &[], None, None, T);
        check(e, NotRun, &[R::NotStarted], Incomplete, None);
    }

    #[test]
    fn stop_after_before_is_not_run_whatever_the_actions_say() {
        let e = evaluate(
            counter(0),
            None,
            &five(GOOD),
            Some(0),
            Some(Stop::DeadlineExceeded),
            T,
        );
        check(e, NotRun, &[R::DeadlineExceeded], Incomplete, None);
        let e = evaluate(counter(0), None, &[], None, Some(Stop::Cancelled), T);
        check(e, NotRun, &[R::Cancelled], Incomplete, None);
        // A stop that came after the after-read does not change the verdict.
        let e = evaluate(
            counter(0),
            counter(1),
            &five(GOOD),
            Some(0),
            Some(Stop::Cancelled),
            T,
        );
        check(e, Pass, &[R::SingleSideEffect], Complete, Some(1));
    }

    // ---- run_with (paused clock, scripted sender, no network) ----

    use std::cell::{Cell, RefCell};
    use std::pin::Pin;

    const SENTINEL: &str = "SENTINEL-ACTION-ID-7c1e";

    /// What the fake sender does on one call.
    #[derive(Clone)]
    enum Step {
        /// Answer at once.
        Reply(u16, Option<Value>),
        /// Answer with this status after `ms` of (virtual) time.
        Slow(u64, u16),
        Fail(TransportErrorCode),
        /// Never answer (request still in flight when a stop arrives).
        Hang,
    }

    /// Verifier body: counter plus a reflected id that carries the sentinel.
    fn count(n: u64) -> Step {
        Step::Reply(200, Some(json!({"execution_count": n, "id": SENTINEL})))
    }

    struct Script {
        before: Step,
        /// Action call n plays `actions[n]`.
        actions: Vec<Step>,
        after: Step,
        /// Signal this stop right before the after-verifier answers.
        stop_on_after: Option<(watch::Sender<Option<Stop>>, Stop)>,
        calls: RefCell<Vec<Kind>>,
        action_calls: Cell<usize>,
        in_flight: Cell<u32>,
        max_in_flight: Cell<u32>,
    }

    impl Script {
        fn new(before: Step, actions: Vec<Step>, after: Step) -> Script {
            Script {
                before,
                actions,
                after,
                stop_on_after: None,
                calls: RefCell::new(Vec::new()),
                action_calls: Cell::new(0),
                in_flight: Cell::new(0),
                max_in_flight: Cell::new(0),
            }
        }

        /// Healthy run: counter 0, then 1; five actions of 10..50 ms (200, then 409s).
        fn healthy() -> Script {
            Script::new(count(0), timed(), count(1))
        }

        fn kinds(&self) -> Vec<Kind> {
            self.calls.borrow().clone()
        }
    }

    /// 200 after 10 ms, then 409 after 20, 30, 40, 50 ms.
    fn timed() -> Vec<Step> {
        vec![
            Step::Slow(10, 200),
            Step::Slow(20, 409),
            Step::Slow(30, 409),
            Step::Slow(40, 409),
            Step::Slow(50, 409),
        ]
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
                Kind::VerifyBefore => s.before.clone(),
                Kind::VerifyAfter => s.after.clone(),
                Kind::Action => {
                    let n = s.action_calls.get();
                    s.action_calls.set(n + 1);
                    s.actions
                        .get(n)
                        .cloned()
                        .expect("sender called more often than scripted")
                }
            };
            Box::pin(async move {
                s.in_flight.set(s.in_flight.get() + 1);
                s.max_in_flight
                    .set(s.max_in_flight.get().max(s.in_flight.get()));
                let result = match step {
                    Step::Reply(status, body) => ok(status, body),
                    Step::Slow(ms, status) => {
                        sleep(Duration::from_millis(ms)).await;
                        ok(status, None)
                    }
                    Step::Fail(code) => err(code),
                    Step::Hang => std::future::pending().await,
                };
                if let (Kind::VerifyAfter, Some((tx, stop))) = (kind, &s.stop_on_after) {
                    let _ = tx.send(Some(*stop));
                }
                s.in_flight.set(s.in_flight.get() - 1);
                result
            })
        }
    }

    fn cfg() -> Race {
        serde_json::from_value(json!({
            "action_request": {"method": "POST", "path": "/actions/1/apply"},
            "verify_request": {"method": "GET", "path": "/actions/1"},
            "verify_match": {"status_in": [200]},
            "counter_pointer": "/execution_count",
            "initial_counter": 0,
            "expected_delta": 1,
            "concurrency": 5,
            "expected_action_statuses": [200, 409],
            "max_dispatch_skew_ms": 100,
            "settle_ms": 250
        }))
        .expect("test config must parse")
    }

    fn no_stop() -> (watch::Sender<Option<Stop>>, watch::Receiver<Option<Stop>>) {
        watch::channel(None)
    }

    /// Run the script; optionally signal `stop` after `ms` of virtual time.
    async fn go(script: &Script, stop: Option<(u64, Stop)>) -> CheckResult {
        let (tx, rx) = no_stop();
        if let Some((ms, s)) = stop {
            tokio::spawn(async move {
                sleep(Duration::from_millis(ms)).await;
                let _ = tx.send(Some(s));
            });
        } else {
            std::mem::forget(tx); // keep the sender alive: "no stop ever"
        }
        run_with(sender(script), &cfg(), rx).await
    }

    fn rev(r: &CheckResult) -> &RaceEvidence {
        match r.evidence.as_ref() {
            Some(Evidence::Race(e)) => e,
            _ => panic!("race always has race evidence"),
        }
    }

    /// Invariants for every result: unique action numbers 1..=N in order, dispatched counts
    /// at least every recorded action, and skew is known whenever something was dispatched.
    #[track_caller]
    fn assert_invariants(r: &CheckResult, s: &Script) {
        let e = rev(r);
        assert!(e.actions_dispatched as usize >= e.actions.len());
        let numbers: Vec<u32> = e.actions.iter().map(|a| a.action).collect();
        let mut sorted = numbers.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), numbers.len(), "action numbers must be unique");
        assert!(numbers.iter().all(|n| (1..=e.concurrency).contains(n)));
        assert_eq!(
            e.actions_dispatched as usize,
            s.kinds().iter().filter(|k| **k == Kind::Action).count()
        );
        if e.actions_dispatched > 0 {
            assert!(e.dispatch_skew_ms.is_some(), "skew must be known");
        } else {
            assert_eq!(e.dispatch_skew_ms, None);
        }
        assert!(
            s.kinds()
                .iter()
                .filter(|k| **k == Kind::VerifyAfter)
                .count()
                <= 1,
            "at most one verifier after"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn r1_healthy_runs_seven_calls_in_order() {
        let s = Script::healthy();
        let r = go(&s, None).await;
        assert_eq!(r.verdict, Pass);
        assert_eq!(r.reasons, [R::SingleSideEffect]);
        assert_eq!(r.completion, Complete);
        let mut expected = vec![Kind::VerifyBefore];
        expected.extend([Kind::Action; 5]);
        expected.push(Kind::VerifyAfter);
        assert_eq!(s.kinds(), expected);
        let e = rev(&r);
        assert_eq!(e.actions_dispatched, 5);
        assert_eq!(e.actions.len(), 5);
        assert_eq!(
            (e.counter_before, e.counter_after, e.delta),
            (Some(0), Some(1), Some(1))
        );
        assert_eq!(e.dispatch_skew_ms, Some(0));
        assert_eq!(e.actions[0].status, Some(200));
        assert_eq!(e.actions[1].status, Some(409));
        assert!(e.actions.iter().all(|a| a.allowed));
        let numbers: Vec<u32> = e.actions.iter().map(|a| a.action).collect();
        assert_eq!(numbers, [1, 2, 3, 4, 5]);
        assert_eq!(e.concurrency, 5);
        assert_eq!(e.expected_action_statuses, [200, 409]);
        assert_eq!((e.max_dispatch_skew_ms, e.settle_ms), (100, 250));
        assert_eq!(
            console_line(&r),
            "race PASS delta=1 before=0 after=1 actions=5/5 skew=0/100ms reasons=single_side_effect"
        );
        assert_invariants(&r, &s);
    }

    #[tokio::test(start_paused = true)]
    async fn all_actions_are_in_flight_together_with_the_same_dispatch_offset() {
        let s = Script::healthy();
        let r = go(&s, None).await;
        assert_eq!(s.max_in_flight.get(), 5);
        let e = rev(&r);
        assert!(
            e.actions
                .iter()
                .all(|a| a.offset_ms == e.actions[0].offset_ms)
        );
        let elapsed: Vec<u64> = e.actions.iter().map(|a| a.elapsed_ms).collect();
        assert_eq!(elapsed, [10, 20, 30, 40, 50]);
    }

    #[tokio::test(start_paused = true)]
    async fn after_is_sent_once_settle_ms_after_the_last_action() {
        let s = Script::healthy();
        let r = go(&s, None).await;
        let e = rev(&r);
        let last_end = e
            .actions
            .iter()
            .map(|a| a.offset_ms + a.elapsed_ms)
            .max()
            .expect("five actions");
        assert_eq!(last_end, 50);
        let after = e.verify_after.as_ref().expect("after read");
        assert_eq!(after.offset_ms, last_end + 250);
        // No wait before the actions: they start right after the (instant) before read.
        assert_eq!(e.actions[0].offset_ms, 0);
    }

    #[tokio::test(start_paused = true)]
    async fn r15_action_timeout_is_not_retried() {
        let mut actions = timed();
        actions[2] = Step::Fail(TransportErrorCode::Timeout);
        let s = Script::new(count(0), actions, count(1));
        let r = go(&s, None).await;
        assert_eq!(s.kinds().len(), 7);
        assert_eq!(r.verdict, Inconclusive);
        assert_eq!(r.reasons, [R::ActionOutcomeAmbiguous, R::TransportError]);
        assert_eq!(r.completion, Complete);
        let a = &rev(&r).actions[2];
        assert_eq!(a.status, None);
        assert_eq!(a.error, Some(TransportErrorCode::Timeout));
        assert!(!a.allowed);
        assert_invariants(&r, &s);
    }

    #[tokio::test(start_paused = true)]
    async fn failing_before_sends_exactly_one_call() {
        let cases = [
            (count(1), Inconclusive, vec![R::InitialCounterNotZero]),
            (
                Step::Reply(200, Some(json!({"other": 1}))),
                Inconclusive,
                vec![R::CounterNotAvailable],
            ),
            (Step::Reply(200, None), Error, vec![R::CounterInvalid]),
            (
                Step::Reply(200, Some(json!({"execution_count": null}))),
                Error,
                vec![R::CounterInvalid],
            ),
            (
                Step::Reply(200, Some(json!({"execution_count": "1"}))),
                Error,
                vec![R::CounterInvalid],
            ),
            (
                Step::Fail(TransportErrorCode::Timeout),
                Error,
                vec![R::VerifierFailed, R::TransportError],
            ),
            (
                Step::Reply(401, None),
                Inconclusive,
                vec![R::BaselineUnreadable],
            ),
        ];
        for (before, verdict, reasons) in cases {
            let s = Script::new(before, vec![], count(1));
            let r = go(&s, None).await;
            assert_eq!(s.kinds(), [Kind::VerifyBefore]);
            assert_eq!(r.verdict, verdict);
            assert_eq!(r.reasons, reasons);
            assert_eq!(r.completion, Complete);
            let e = rev(&r);
            assert!(e.actions.is_empty());
            assert_eq!(e.actions_dispatched, 0);
            assert!(e.verify_before.is_some());
            assert!(e.verify_after.is_none());
            assert_invariants(&r, &s);
        }
    }

    #[tokio::test(start_paused = true)]
    async fn r4_used_resource_keeps_before_counter_and_sends_no_action() {
        let s = Script::new(count(1), vec![], count(1));
        let r = go(&s, None).await;
        let e = rev(&r);
        assert_eq!(e.counter_before, Some(1));
        assert_eq!(e.delta, None);
        assert_eq!(
            console_line(&r),
            "race INCONCLUSIVE delta=none before=1 after=none actions=0/5 skew=none/100ms reasons=initial_counter_not_zero"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn r18_before_status_and_error_reach_the_evidence() {
        let s = Script::new(
            Step::Fail(TransportErrorCode::ConnectFailed),
            vec![],
            count(1),
        );
        let r = go(&s, None).await;
        let b = rev(&r).verify_before.as_ref().expect("before evidence");
        assert_eq!(
            (b.status, b.error),
            (None, Some(TransportErrorCode::ConnectFailed))
        );
    }

    #[tokio::test(start_paused = true)]
    async fn r20_after_transport_error_is_error() {
        let s = Script::new(count(0), timed(), Step::Fail(TransportErrorCode::Timeout));
        let r = go(&s, None).await;
        assert_eq!(s.kinds().len(), 7);
        assert_eq!(r.verdict, Error);
        assert_eq!(r.reasons, [R::VerifierFailed, R::TransportError]);
        assert_eq!(r.completion, Complete);
        let e = rev(&r);
        assert_eq!(e.counter_after, None);
        assert_eq!(e.delta, None);
        assert_eq!(
            e.verify_after.as_ref().and_then(|v| v.error),
            Some(TransportErrorCode::Timeout)
        );
        assert_invariants(&r, &s);
    }

    #[tokio::test(start_paused = true)]
    async fn r24_stop_before_start_sends_nothing() {
        for (stop, reason) in [
            (Stop::Cancelled, R::Cancelled),
            (Stop::DeadlineExceeded, R::DeadlineExceeded),
        ] {
            let s = Script::healthy();
            let (tx, rx) = no_stop();
            tx.send(Some(stop)).expect("receiver alive");
            let r = run_with(sender(&s), &cfg(), rx).await;
            assert_eq!(r.verdict, NotRun);
            assert_eq!(r.reasons, [reason]);
            assert_eq!(r.completion, Incomplete);
            assert!(s.kinds().is_empty());
            let e = rev(&r);
            assert!(e.verify_before.is_none());
            assert_eq!(e.actions_dispatched, 0);
            assert_invariants(&r, &s);
        }
        let s = Script::healthy();
        let (tx, rx) = no_stop();
        tx.send(Some(Stop::Cancelled)).expect("receiver alive");
        let r = run_with(sender(&s), &cfg(), rx).await;
        assert_eq!(
            console_line(&r),
            "race NOT_RUN delta=none before=none after=none actions=0/5 skew=none/100ms reasons=cancelled"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn r25_cancel_while_before_is_in_flight() {
        let s = Script::new(Step::Hang, vec![], count(1));
        let r = go(&s, Some((10, Stop::Cancelled))).await;
        assert_eq!(s.kinds(), [Kind::VerifyBefore]);
        assert_eq!(r.verdict, NotRun);
        assert_eq!(r.reasons, [R::Cancelled]);
        assert_eq!(r.completion, Incomplete);
        assert!(rev(&r).verify_before.is_none());
        assert_invariants(&r, &s);
    }

    #[tokio::test(start_paused = true)]
    async fn r26_cancel_while_actions_are_in_flight() {
        // Two actions finish (10, 20 ms), three never answer; cancel at 50 ms.
        let actions = vec![
            Step::Slow(10, 200),
            Step::Slow(20, 409),
            Step::Hang,
            Step::Hang,
            Step::Hang,
        ];
        let s = Script::new(count(0), actions, count(1));
        let r = go(&s, Some((50, Stop::Cancelled))).await;
        assert_eq!(s.kinds().len(), 6); // before + 5 actions, no after
        assert_eq!(r.verdict, NotRun);
        assert_eq!(r.reasons, [R::Cancelled]);
        assert_eq!(r.completion, Incomplete);
        let e = rev(&r);
        assert_eq!(e.actions.len(), 2);
        assert_eq!(e.actions_dispatched, 5);
        assert_eq!(e.counter_after, None);
        assert!(e.verify_after.is_none());
        assert_eq!(e.dispatch_skew_ms, Some(0));
        assert_eq!(
            console_line(&r),
            "race NOT_RUN delta=none before=0 after=none actions=2/5 skew=0/100ms reasons=cancelled"
        );
        assert_invariants(&r, &s);
    }

    #[tokio::test(start_paused = true)]
    async fn r27_deadline_during_settle() {
        // Actions end at 50 ms, settle runs 50..300; the deadline hits at 100 ms.
        let s = Script::healthy();
        let r = go(&s, Some((100, Stop::DeadlineExceeded))).await;
        assert_eq!(s.kinds().len(), 6);
        assert_eq!(r.verdict, NotRun);
        assert_eq!(r.reasons, [R::DeadlineExceeded]);
        assert_eq!(r.completion, Incomplete);
        let e = rev(&r);
        assert_eq!(e.actions.len(), 5);
        assert!(e.verify_after.is_none());
        assert_eq!(e.counter_after, None);
        assert_invariants(&r, &s);
    }

    #[tokio::test(start_paused = true)]
    async fn r28_deadline_while_after_is_in_flight() {
        // The after read is sent at 300 ms and never answers; the deadline hits at 400 ms.
        let s = Script::new(count(0), timed(), Step::Hang);
        let r = go(&s, Some((400, Stop::DeadlineExceeded))).await;
        assert_eq!(s.kinds().len(), 7);
        assert_eq!(r.verdict, NotRun);
        assert_eq!(r.reasons, [R::DeadlineExceeded]);
        assert_eq!(r.completion, Incomplete);
        let e = rev(&r);
        assert_eq!(e.actions.len(), 5);
        assert!(e.verify_after.is_none());
        assert_eq!(e.counter_after, None);
        assert_invariants(&r, &s);
    }

    #[tokio::test(start_paused = true)]
    async fn r29_stop_after_the_after_read_keeps_the_verdict() {
        let (tx, rx) = no_stop();
        let mut s = Script::healthy();
        s.stop_on_after = Some((tx, Stop::Cancelled));
        let r = run_with(sender(&s), &cfg(), rx).await;
        assert_eq!(s.kinds().len(), 7);
        assert_eq!(r.verdict, Pass);
        assert_eq!(r.reasons, [R::SingleSideEffect]);
        assert_eq!(r.completion, Complete);
        assert_invariants(&r, &s);
    }

    #[tokio::test(start_paused = true)]
    async fn skew_is_known_whenever_something_was_dispatched() {
        // Healthy, timeout, cancel in flight, duplicate: skew is always Some after a dispatch.
        let mut with_timeout = timed();
        with_timeout[0] = Step::Fail(TransportErrorCode::Timeout);
        let runs = [
            (Script::healthy(), None),
            (Script::new(count(0), with_timeout, count(1)), None),
            (
                Script::new(count(0), vec![Step::Hang; 5], count(1)),
                Some((5, Stop::Cancelled)),
            ),
            (Script::new(count(0), timed(), count(2)), None),
        ];
        for (s, stop) in runs {
            let r = go(&s, stop).await;
            assert_eq!(rev(&r).actions_dispatched, 5);
            assert_eq!(rev(&r).dispatch_skew_ms, Some(0));
            assert_invariants(&r, &s);
        }
    }

    #[test]
    fn evaluate_never_passes_finished_actions_without_a_skew_value() {
        let e = evaluate(counter(0), counter(1), &five(GOOD), None, None, T);
        check(e, Inconclusive, &[R::DispatchIncomplete], Complete, Some(1));
    }

    #[tokio::test(start_paused = true)]
    async fn duplicate_side_effect_fails_through_run_with() {
        let s = Script::new(count(0), timed(), count(2));
        let r = go(&s, None).await;
        assert_eq!(r.verdict, Fail);
        assert_eq!(r.reasons, [R::DuplicateSideEffect]);
        assert_eq!(rev(&r).delta, Some(2));
    }

    #[tokio::test(start_paused = true)]
    async fn r30_sentinel_is_absent_from_debug_and_console() {
        // Verifier bodies reflect the sentinel; PASS and FAIL paths.
        for after in [1, 2] {
            let s = Script::new(count(0), timed(), count(after));
            let r = go(&s, None).await;
            assert!(!format!("{r:?}").contains(SENTINEL));
            assert!(!console_line(&r).contains(SENTINEL));
        }
    }

    #[test]
    fn console_line_without_evidence() {
        let r = CheckResult {
            check: CheckName::Race,
            verdict: NotRun,
            reasons: vec![],
            completion: Incomplete,
            evidence: None,
        };
        assert_eq!(console_line(&r), "race NOT_RUN reasons=none");
    }
}
