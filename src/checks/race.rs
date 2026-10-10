//! race check (PLAN §6). This file holds the pure parts: `read_verifier` (one verifier
//! response -> counter or problem), `action_allowed`, `skew_ms` and `evaluate` (all facts ->
//! verdict). The async loop (`run_with`, `run`, `console_line`) comes in a later slice.

use std::time::Duration;

use crate::checks::Stop;
use crate::config::MatcherSpec;
use crate::http::{TransportError, TransportResponse};
use crate::report::{Completion, ReasonCode, TransportErrorCode, Verdict};

/// What one verifier response tells us.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerifyRead {
    Transport(TransportErrorCode),
    /// The response did not satisfy `verify_match`.
    Mismatch,
    /// Pointer missing, or the value is not an integer >= 0.
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
    match body
        .and_then(|b| b.pointer(pointer))
        .and_then(|v| v.as_u64())
    {
        Some(n) => VerifyRead::Counter(n),
        None => VerifyRead::CounterInvalid,
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

/// Spread between the first and the last dispatch (offsets from one common start),
/// in whole ms rounded UP, so 100.4 ms is never shown or judged as 100. `None` if empty.
pub fn skew_ms(dispatch_offsets: &[Duration]) -> Option<u64> {
    let min = dispatch_offsets.iter().min()?;
    let max = dispatch_offsets.iter().max()?;
    Some((*max - *min).as_micros().div_ceil(1000) as u64)
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

/// Reasons for a verifier that gave no counter. `None` means it did give one.
fn verifier_problem(read: VerifyRead) -> Option<Vec<ReasonCode>> {
    match read {
        VerifyRead::Transport(_) => {
            Some(vec![ReasonCode::VerifierFailed, ReasonCode::TransportError])
        }
        VerifyRead::Mismatch => Some(vec![ReasonCode::VerifierFailed]),
        VerifyRead::CounterInvalid => Some(vec![ReasonCode::CounterInvalid]),
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
    if let Some(reasons) = verifier_problem(before) {
        return make(Verdict::Error, reasons, Completion::Complete, delta);
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
    if actions.len() as u32 != t.concurrency {
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
    if actions.len() as u32 != t.concurrency || has_ambiguous {
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
        assert_eq!(read(Some(json!({"other": 1}))), CounterInvalid);
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

    #[test]
    fn skew_ms_rounds_up() {
        let ms = Duration::from_millis;
        assert_eq!(skew_ms(&[]), None);
        assert_eq!(skew_ms(&[ms(40)]), Some(0));
        assert_eq!(skew_ms(&[ms(0), ms(150)]), Some(150));
        assert_eq!(skew_ms(&[ms(150), ms(0), ms(20)]), Some(150));
        assert_eq!(skew_ms(&[ms(0), Duration::from_micros(100_400)]), Some(101));
        assert_eq!(skew_ms(&[ms(0), Duration::from_micros(100_000)]), Some(100));
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
    fn r19_before_mismatch_is_error() {
        let e = evaluate(Some(VerifyRead::Mismatch), None, &[], None, None, T);
        check(e, Error, &[R::VerifierFailed], Complete, None);
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
}
