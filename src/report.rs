//! Report model, enum contract (report v1), exit code and atomic writer (PLAN §4, §10).
//! The report has no field for bodies, headers or secrets, so none can leak into it.

use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde::Serialize;
use sha2::{Digest, Sha256};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

/// Current value of `report_version`. Bump it when a variant or field is changed or removed.
pub const REPORT_VERSION: u32 = 1;

/// Sanitized reason a request produced no usable response. It never carries a URL,
/// header value or library message (PLAN §10). Serialized as stable snake_case.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TransportErrorCode {
    Timeout,
    ConnectFailed,
    TlsFailed,
    BodyTooLarge,
    BodyReadFailed,
    RequestInvalid,
    Other,
}

impl TransportErrorCode {
    pub const ALL: [TransportErrorCode; 7] = [
        TransportErrorCode::Timeout,
        TransportErrorCode::ConnectFailed,
        TransportErrorCode::TlsFailed,
        TransportErrorCode::BodyTooLarge,
        TransportErrorCode::BodyReadFailed,
        TransportErrorCode::RequestInvalid,
        TransportErrorCode::Other,
    ];

    /// The same string serde produces; used for `Display` so messages stay stable.
    pub fn as_str(self) -> &'static str {
        match self {
            TransportErrorCode::Timeout => "timeout",
            TransportErrorCode::ConnectFailed => "connect_failed",
            TransportErrorCode::TlsFailed => "tls_failed",
            TransportErrorCode::BodyTooLarge => "body_too_large",
            TransportErrorCode::BodyReadFailed => "body_read_failed",
            TransportErrorCode::RequestInvalid => "request_invalid",
            TransportErrorCode::Other => "other",
        }
    }
}

impl fmt::Display for TransportErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Overall result of one check (PLAN §4). Console may upper-case it; JSON never does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    Pass,
    Fail,
    Inconclusive,
    Error,
    NotRun,
}

impl Verdict {
    pub const ALL: [Verdict; 5] = [
        Verdict::Pass,
        Verdict::Fail,
        Verdict::Inconclusive,
        Verdict::Error,
        Verdict::NotRun,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Verdict::Pass => "pass",
            Verdict::Fail => "fail",
            Verdict::Inconclusive => "inconclusive",
            Verdict::Error => "error",
            Verdict::NotRun => "not_run",
        }
    }
}

/// Which of the three v0.1 checks a result belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckName {
    BruteForce,
    Race,
    Load,
}

impl CheckName {
    pub const ALL: [CheckName; 3] = [CheckName::BruteForce, CheckName::Race, CheckName::Load];

    pub fn as_str(self) -> &'static str {
        match self {
            CheckName::BruteForce => "brute_force",
            CheckName::Race => "race",
            CheckName::Load => "load",
        }
    }
}

/// Whether the suite or a check ran to its end.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Completion {
    Complete,
    Incomplete,
}

impl Completion {
    pub const ALL: [Completion; 2] = [Completion::Complete, Completion::Incomplete];

    pub fn as_str(self) -> &'static str {
        match self {
            Completion::Complete => "complete",
            Completion::Incomplete => "incomplete",
        }
    }
}

/// Why a check got its verdict. Adding a variant is additive; renaming or removing one
/// is a report contract change (bump `REPORT_VERSION`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReasonCode {
    // General
    ConfigInvalid,
    DeadlineExceeded,
    Cancelled,
    NotStarted,
    ReportWriteFailed,
    TransportError,
    OverlappingRules,
    // brute_force
    ProtectionObserved,
    InvalidCredentialsAccepted,
    ProtectionPolicyNotMet,
    AccountAlreadyLocked,
    BaselineFailed,
    ProbeUnclassifiable,
    // race
    SingleSideEffect,
    DuplicateSideEffect,
    ActionNotExecuted,
    InitialCounterNotZero,
    DispatchIncomplete,
    DispatchSkewExceeded,
    ActionOutcomeAmbiguous,
    VerifierFailed,
    CounterInvalid,
    // load
    ThresholdsMet,
    ErrorRateExceeded,
    LatencyExceeded,
    TargetNotReady,
    WorkloadIncomplete,
    PreflightFailed,
    RunnerError,
}

impl ReasonCode {
    pub const ALL: [ReasonCode; 29] = [
        ReasonCode::ConfigInvalid,
        ReasonCode::DeadlineExceeded,
        ReasonCode::Cancelled,
        ReasonCode::NotStarted,
        ReasonCode::ReportWriteFailed,
        ReasonCode::TransportError,
        ReasonCode::OverlappingRules,
        ReasonCode::ProtectionObserved,
        ReasonCode::InvalidCredentialsAccepted,
        ReasonCode::ProtectionPolicyNotMet,
        ReasonCode::AccountAlreadyLocked,
        ReasonCode::BaselineFailed,
        ReasonCode::ProbeUnclassifiable,
        ReasonCode::SingleSideEffect,
        ReasonCode::DuplicateSideEffect,
        ReasonCode::ActionNotExecuted,
        ReasonCode::InitialCounterNotZero,
        ReasonCode::DispatchIncomplete,
        ReasonCode::DispatchSkewExceeded,
        ReasonCode::ActionOutcomeAmbiguous,
        ReasonCode::VerifierFailed,
        ReasonCode::CounterInvalid,
        ReasonCode::ThresholdsMet,
        ReasonCode::ErrorRateExceeded,
        ReasonCode::LatencyExceeded,
        ReasonCode::TargetNotReady,
        ReasonCode::WorkloadIncomplete,
        ReasonCode::PreflightFailed,
        ReasonCode::RunnerError,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            ReasonCode::ConfigInvalid => "config_invalid",
            ReasonCode::DeadlineExceeded => "deadline_exceeded",
            ReasonCode::Cancelled => "cancelled",
            ReasonCode::NotStarted => "not_started",
            ReasonCode::ReportWriteFailed => "report_write_failed",
            ReasonCode::TransportError => "transport_error",
            ReasonCode::OverlappingRules => "overlapping_rules",
            ReasonCode::ProtectionObserved => "protection_observed",
            ReasonCode::InvalidCredentialsAccepted => "invalid_credentials_accepted",
            ReasonCode::ProtectionPolicyNotMet => "protection_policy_not_met",
            ReasonCode::AccountAlreadyLocked => "account_already_locked",
            ReasonCode::BaselineFailed => "baseline_failed",
            ReasonCode::ProbeUnclassifiable => "probe_unclassifiable",
            ReasonCode::SingleSideEffect => "single_side_effect",
            ReasonCode::DuplicateSideEffect => "duplicate_side_effect",
            ReasonCode::ActionNotExecuted => "action_not_executed",
            ReasonCode::InitialCounterNotZero => "initial_counter_not_zero",
            ReasonCode::DispatchIncomplete => "dispatch_incomplete",
            ReasonCode::DispatchSkewExceeded => "dispatch_skew_exceeded",
            ReasonCode::ActionOutcomeAmbiguous => "action_outcome_ambiguous",
            ReasonCode::VerifierFailed => "verifier_failed",
            ReasonCode::CounterInvalid => "counter_invalid",
            ReasonCode::ThresholdsMet => "thresholds_met",
            ReasonCode::ErrorRateExceeded => "error_rate_exceeded",
            ReasonCode::LatencyExceeded => "latency_exceeded",
            ReasonCode::TargetNotReady => "target_not_ready",
            ReasonCode::WorkloadIncomplete => "workload_incomplete",
            ReasonCode::PreflightFailed => "preflight_failed",
            ReasonCode::RunnerError => "runner_error",
        }
    }
}

/// Classification of one attempt (PLAN §5 step 4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Success,
    NormalRejection,
    Protection,
    Unexpected,
    TransportError,
}

impl Outcome {
    pub const ALL: [Outcome; 5] = [
        Outcome::Success,
        Outcome::NormalRejection,
        Outcome::Protection,
        Outcome::Unexpected,
        Outcome::TransportError,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Outcome::Success => "success",
            Outcome::NormalRejection => "normal_rejection",
            Outcome::Protection => "protection",
            Outcome::Unexpected => "unexpected",
            Outcome::TransportError => "transport_error",
        }
    }
}

/// One brute-force attempt. Numbers, enums and nulls only: no body, header or token.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AttemptEvidence {
    /// 0 = baseline, 1.. = probe.
    pub attempt: u32,
    pub outcome: Outcome,
    pub status: Option<u16>,
    pub error: Option<TransportErrorCode>,
    pub offset_ms: u64,
    pub elapsed_ms: u64,
}

/// Evidence of the brute-force check (PLAN §10), including the configured thresholds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BruteForceEvidence {
    pub attempts_planned: u32,
    pub attempts_dispatched: u32,
    pub interval_ms: u64,
    pub protect_by_attempt: u32,
    pub protected_tail_count: u32,
    pub first_protected_attempt: Option<u32>,
    pub protected_tail_observed: u32,
    pub baseline: Option<AttemptEvidence>,
    pub attempts: Vec<AttemptEvidence>,
}

/// Result of one check inside the report.
// ponytail: P2 turns `evidence` into an `#[serde(untagged)]` enum (race/load); JSON stays the same.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CheckResult {
    pub check: CheckName,
    pub verdict: Verdict,
    pub reasons: Vec<ReasonCode>,
    pub completion: Completion,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evidence: Option<BruteForceEvidence>,
}

/// Report v1. Field order here is the field order in the JSON file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Report {
    pub report_version: u32,
    pub run_id: String,
    pub tool_version: String,
    pub config_hash: String,
    pub started_at: String,
    pub finished_at: String,
    pub completion: Completion,
    pub checks: Vec<CheckResult>,
}

impl Report {
    /// Fills `report_version` and `tool_version`; the caller supplies the rest.
    pub fn new(
        run_id: String,
        config_hash: String,
        started_at: OffsetDateTime,
        finished_at: OffsetDateTime,
        completion: Completion,
        checks: Vec<CheckResult>,
    ) -> Result<Report, ReportError> {
        Ok(Report {
            report_version: REPORT_VERSION,
            run_id,
            tool_version: env!("CARGO_PKG_VERSION").to_string(),
            config_hash,
            started_at: rfc3339(started_at)?,
            finished_at: rfc3339(finished_at)?,
            completion,
            checks,
        })
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ReportError {
    #[error("cannot format timestamp")]
    Time,
    #[error("cannot serialize report")]
    Serialize,
    #[error("report path has no file name: {0}")]
    BadPath(PathBuf),
    #[error("cannot write report {path}: {kind}")]
    Io {
        path: PathBuf,
        kind: std::io::ErrorKind,
    },
}

/// RFC3339 UTC string. The clock is injected: callers pass the time in.
pub fn rfc3339(t: OffsetDateTime) -> Result<String, ReportError> {
    t.to_offset(time::UtcOffset::UTC)
        .format(&Rfc3339)
        .map_err(|_| ReportError::Time)
}

/// SHA-256 of the raw config bytes as lower-case hex. The config only holds `${ENV:..}`
/// references, never secret values, and the raw bytes are never stored.
pub fn config_hash(raw: &[u8]) -> String {
    Sha256::digest(raw)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// `{unix_ms}-{first 8 hex of config_hash}`.
// ponytail: two runs in the same millisecond with the same config collide; add uuid if
// parallel runs are ever needed.
pub fn run_id(unix_ms: u64, config_hash: &str) -> String {
    let short: String = config_hash.chars().take(8).collect();
    format!("{unix_ms}-{short}")
}

/// Reasons a `pass` check may carry; any other reason on a `pass` check is inconsistent.
fn is_pass_reason(r: ReasonCode) -> bool {
    matches!(
        r,
        ReasonCode::ProtectionObserved | ReasonCode::SingleSideEffect | ReasonCode::ThresholdsMet
    )
}

/// Suite exit code, PLAN §10 precedence: cancel (130) -> error/inconclusive/not_run/
/// incomplete/malformed (2) -> fail (1) -> pass (0).
/// Cancel is represented by `ReasonCode::Cancelled` on any check (no separate flag).
/// Cancel is checked FIRST, before the structure check, so a cancelled run is 130 even
/// if the check list is malformed. Exit 0 requires exactly one check per `CheckName`,
/// each `pass` with only pass reasons and `complete`, and a complete suite (no false PASS).
pub fn exit_code(report: &Report) -> i32 {
    let cancelled = report
        .checks
        .iter()
        .any(|c| c.reasons.contains(&ReasonCode::Cancelled));
    if cancelled {
        return 130;
    }
    let exactly_one_each = report.checks.len() == CheckName::ALL.len()
        && CheckName::ALL
            .iter()
            .all(|name| report.checks.iter().filter(|c| c.check == *name).count() == 1);
    let not_conclusive = report.completion == Completion::Incomplete
        || report.checks.iter().any(|c| {
            c.completion == Completion::Incomplete
                || matches!(
                    c.verdict,
                    Verdict::Error | Verdict::Inconclusive | Verdict::NotRun
                )
                || (c.verdict == Verdict::Pass && !c.reasons.iter().all(|r| is_pass_reason(*r)))
        });
    if !exactly_one_each || not_conclusive {
        return 2;
    }
    if report.checks.iter().any(|c| c.verdict == Verdict::Fail) {
        return 1;
    }
    0
}

static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);
const TMP_ATTEMPTS: usize = 8;

/// Writes the report as pretty JSON (plus a trailing newline) into a fresh temp file
/// `.<name>.tmp-<pid>-<counter>` next to `path` (created with `create_new`, so an existing
/// file or symlink is never followed), syncs it, then renames it over `path`. On failure
/// the old file is untouched and only the temp file this call created is removed. Errors
/// never contain report content.
pub fn write_report_atomic(path: &Path, report: &Report) -> Result<(), ReportError> {
    write_report_atomic_with(path, report, || TMP_COUNTER.fetch_add(1, Ordering::Relaxed))
}

fn write_report_atomic_with(
    path: &Path,
    report: &Report,
    mut next_id: impl FnMut() -> u64,
) -> Result<(), ReportError> {
    let file_name = path
        .file_name()
        .ok_or_else(|| ReportError::BadPath(path.to_path_buf()))?;
    let mut json = serde_json::to_string_pretty(report).map_err(|_| ReportError::Serialize)?;
    json.push('\n');

    let io_err = |e: std::io::Error| ReportError::Io {
        path: path.to_path_buf(),
        kind: e.kind(),
    };

    let mut created = None;
    for _ in 0..TMP_ATTEMPTS {
        let mut tmp_name = std::ffi::OsString::from(".");
        tmp_name.push(file_name);
        tmp_name.push(format!(".tmp-{}-{}", std::process::id(), next_id()));
        let tmp_path = path.with_file_name(tmp_name);
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp_path)
        {
            Ok(file) => {
                created = Some((file, tmp_path));
                break;
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(io_err(e)),
        }
    }
    let Some((mut file, tmp_path)) = created else {
        return Err(io_err(std::io::ErrorKind::AlreadyExists.into()));
    };

    let result = (|| {
        file.write_all(json.as_bytes())?;
        file.sync_all()?;
        drop(file);
        fs::rename(&tmp_path, path)
    })();
    if let Err(e) = result {
        let _ = fs::remove_file(&tmp_path);
        return Err(io_err(e));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // Exhaustive matches: adding a variant breaks compilation here until the golden
    // string is added. The length asserts catch a variant missing from `ALL`.
    fn verdict_literal(v: Verdict) -> &'static str {
        match v {
            Verdict::Pass => "pass",
            Verdict::Fail => "fail",
            Verdict::Inconclusive => "inconclusive",
            Verdict::Error => "error",
            Verdict::NotRun => "not_run",
        }
    }
    fn check_literal(v: CheckName) -> &'static str {
        match v {
            CheckName::BruteForce => "brute_force",
            CheckName::Race => "race",
            CheckName::Load => "load",
        }
    }
    fn completion_literal(v: Completion) -> &'static str {
        match v {
            Completion::Complete => "complete",
            Completion::Incomplete => "incomplete",
        }
    }
    fn outcome_literal(v: Outcome) -> &'static str {
        match v {
            Outcome::Success => "success",
            Outcome::NormalRejection => "normal_rejection",
            Outcome::Protection => "protection",
            Outcome::Unexpected => "unexpected",
            Outcome::TransportError => "transport_error",
        }
    }
    fn transport_literal(v: TransportErrorCode) -> &'static str {
        match v {
            TransportErrorCode::Timeout => "timeout",
            TransportErrorCode::ConnectFailed => "connect_failed",
            TransportErrorCode::TlsFailed => "tls_failed",
            TransportErrorCode::BodyTooLarge => "body_too_large",
            TransportErrorCode::BodyReadFailed => "body_read_failed",
            TransportErrorCode::RequestInvalid => "request_invalid",
            TransportErrorCode::Other => "other",
        }
    }
    fn reason_literal(v: ReasonCode) -> &'static str {
        match v {
            ReasonCode::ConfigInvalid => "config_invalid",
            ReasonCode::DeadlineExceeded => "deadline_exceeded",
            ReasonCode::Cancelled => "cancelled",
            ReasonCode::NotStarted => "not_started",
            ReasonCode::ReportWriteFailed => "report_write_failed",
            ReasonCode::TransportError => "transport_error",
            ReasonCode::OverlappingRules => "overlapping_rules",
            ReasonCode::ProtectionObserved => "protection_observed",
            ReasonCode::InvalidCredentialsAccepted => "invalid_credentials_accepted",
            ReasonCode::ProtectionPolicyNotMet => "protection_policy_not_met",
            ReasonCode::AccountAlreadyLocked => "account_already_locked",
            ReasonCode::BaselineFailed => "baseline_failed",
            ReasonCode::ProbeUnclassifiable => "probe_unclassifiable",
            ReasonCode::SingleSideEffect => "single_side_effect",
            ReasonCode::DuplicateSideEffect => "duplicate_side_effect",
            ReasonCode::ActionNotExecuted => "action_not_executed",
            ReasonCode::InitialCounterNotZero => "initial_counter_not_zero",
            ReasonCode::DispatchIncomplete => "dispatch_incomplete",
            ReasonCode::DispatchSkewExceeded => "dispatch_skew_exceeded",
            ReasonCode::ActionOutcomeAmbiguous => "action_outcome_ambiguous",
            ReasonCode::VerifierFailed => "verifier_failed",
            ReasonCode::CounterInvalid => "counter_invalid",
            ReasonCode::ThresholdsMet => "thresholds_met",
            ReasonCode::ErrorRateExceeded => "error_rate_exceeded",
            ReasonCode::LatencyExceeded => "latency_exceeded",
            ReasonCode::TargetNotReady => "target_not_ready",
            ReasonCode::WorkloadIncomplete => "workload_incomplete",
            ReasonCode::PreflightFailed => "preflight_failed",
            ReasonCode::RunnerError => "runner_error",
        }
    }

    /// serde output == `as_str` == expected literal, for every variant.
    fn json_of<T: Serialize>(v: &T) -> String {
        serde_json::to_string(v).unwrap()
    }

    #[test]
    fn enum_strings_are_stable() {
        assert_eq!(Verdict::ALL.len(), 5);
        for v in Verdict::ALL {
            assert_eq!(v.as_str(), verdict_literal(v));
            assert_eq!(json_of(&v), format!("\"{}\"", verdict_literal(v)));
        }
        assert_eq!(CheckName::ALL.len(), 3);
        for v in CheckName::ALL {
            assert_eq!(v.as_str(), check_literal(v));
            assert_eq!(json_of(&v), format!("\"{}\"", check_literal(v)));
        }
        assert_eq!(Completion::ALL.len(), 2);
        for v in Completion::ALL {
            assert_eq!(v.as_str(), completion_literal(v));
            assert_eq!(json_of(&v), format!("\"{}\"", completion_literal(v)));
        }
        assert_eq!(Outcome::ALL.len(), 5);
        for v in Outcome::ALL {
            assert_eq!(v.as_str(), outcome_literal(v));
            assert_eq!(json_of(&v), format!("\"{}\"", outcome_literal(v)));
        }
        assert_eq!(TransportErrorCode::ALL.len(), 7);
        for v in TransportErrorCode::ALL {
            assert_eq!(v.as_str(), transport_literal(v));
            assert_eq!(v.to_string(), transport_literal(v));
            assert_eq!(json_of(&v), format!("\"{}\"", transport_literal(v)));
        }
        assert_eq!(ReasonCode::ALL.len(), 29);
        for v in ReasonCode::ALL {
            assert_eq!(v.as_str(), reason_literal(v));
            assert_eq!(json_of(&v), format!("\"{}\"", reason_literal(v)));
        }
    }

    fn at(unix: i64) -> OffsetDateTime {
        OffsetDateTime::from_unix_timestamp(unix).unwrap()
    }

    fn sample_report() -> Report {
        Report::new(
            run_id(1_760_000_000_000, &config_hash(b"{}")),
            config_hash(b"{}"),
            at(1_791_633_600),
            at(1_791_633_605),
            Completion::Complete,
            vec![
                CheckResult {
                    check: CheckName::BruteForce,
                    verdict: Verdict::Pass,
                    reasons: vec![ReasonCode::ProtectionObserved],
                    completion: Completion::Complete,
                    evidence: None,
                },
                CheckResult {
                    check: CheckName::Race,
                    verdict: Verdict::Fail,
                    reasons: vec![ReasonCode::DuplicateSideEffect],
                    completion: Completion::Complete,
                    evidence: None,
                },
                CheckResult {
                    check: CheckName::Load,
                    verdict: Verdict::NotRun,
                    reasons: vec![],
                    completion: Completion::Incomplete,
                    evidence: None,
                },
            ],
        )
        .unwrap()
    }

    fn golden_json() -> String {
        // sha256("{}") = 44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a
        format!(
            r#"{{
  "report_version": 1,
  "run_id": "1760000000000-44136fa3",
  "tool_version": "{}",
  "config_hash": "44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a",
  "started_at": "2026-10-10T12:00:00Z",
  "finished_at": "2026-10-10T12:00:05Z",
  "completion": "complete",
  "checks": [
    {{
      "check": "brute_force",
      "verdict": "pass",
      "reasons": [
        "protection_observed"
      ],
      "completion": "complete"
    }},
    {{
      "check": "race",
      "verdict": "fail",
      "reasons": [
        "duplicate_side_effect"
      ],
      "completion": "complete"
    }},
    {{
      "check": "load",
      "verdict": "not_run",
      "reasons": [],
      "completion": "incomplete"
    }}
  ]
}}
"#,
            env!("CARGO_PKG_VERSION")
        )
    }

    #[test]
    fn report_json_matches_golden() {
        let mut json = serde_json::to_string_pretty(&sample_report()).unwrap();
        json.push('\n');
        assert_eq!(json, golden_json());
    }

    fn evidence_check() -> CheckResult {
        let attempt = |n, outcome, status, error, offset_ms, elapsed_ms| AttemptEvidence {
            attempt: n,
            outcome,
            status,
            error,
            offset_ms,
            elapsed_ms,
        };
        CheckResult {
            check: CheckName::BruteForce,
            verdict: Verdict::Error,
            reasons: vec![ReasonCode::ProbeUnclassifiable, ReasonCode::TransportError],
            completion: Completion::Complete,
            evidence: Some(BruteForceEvidence {
                attempts_planned: 8,
                attempts_dispatched: 3,
                interval_ms: 100,
                protect_by_attempt: 6,
                protected_tail_count: 2,
                first_protected_attempt: None,
                protected_tail_observed: 0,
                baseline: Some(attempt(0, Outcome::Success, Some(200), None, 0, 12)),
                attempts: vec![
                    attempt(1, Outcome::NormalRejection, Some(401), None, 12, 8),
                    attempt(
                        2,
                        Outcome::TransportError,
                        None,
                        Some(TransportErrorCode::Timeout),
                        120,
                        5000,
                    ),
                ],
            }),
        }
    }

    #[test]
    fn evidence_json_matches_golden() {
        let golden = r#"{
  "check": "brute_force",
  "verdict": "error",
  "reasons": [
    "probe_unclassifiable",
    "transport_error"
  ],
  "completion": "complete",
  "evidence": {
    "attempts_planned": 8,
    "attempts_dispatched": 3,
    "interval_ms": 100,
    "protect_by_attempt": 6,
    "protected_tail_count": 2,
    "first_protected_attempt": null,
    "protected_tail_observed": 0,
    "baseline": {
      "attempt": 0,
      "outcome": "success",
      "status": 200,
      "error": null,
      "offset_ms": 0,
      "elapsed_ms": 12
    },
    "attempts": [
      {
        "attempt": 1,
        "outcome": "normal_rejection",
        "status": 401,
        "error": null,
        "offset_ms": 12,
        "elapsed_ms": 8
      },
      {
        "attempt": 2,
        "outcome": "transport_error",
        "status": null,
        "error": "timeout",
        "offset_ms": 120,
        "elapsed_ms": 5000
      }
    ]
  }
}"#;
        assert_eq!(
            serde_json::to_string_pretty(&evidence_check()).unwrap(),
            golden
        );
    }

    #[test]
    fn no_evidence_means_no_field() {
        let json = serde_json::to_string(&sample_report()).unwrap();
        assert!(!json.contains("evidence"));
    }

    #[test]
    fn exit_code_ignores_evidence() {
        let mut with = sample_report();
        let mut without = sample_report();
        with.checks[0] = evidence_check();
        without.checks[0] = CheckResult {
            evidence: None,
            ..evidence_check()
        };
        assert_eq!(exit_code(&with), exit_code(&without));
        assert_eq!(exit_code(&with), 2);
    }

    #[test]
    fn config_hash_and_run_id() {
        assert_eq!(
            config_hash(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(run_id(42, &config_hash(b"abc")), "42-ba7816bf");
    }

    #[test]
    fn config_hash_does_not_contain_raw_config() {
        let sentinel = "SENTINEL-s3cr3t-value";
        let raw = format!(r#"{{"token":"{sentinel}"}}"#);
        let hash = config_hash(raw.as_bytes());
        assert!(!hash.contains(sentinel));
        assert_eq!(hash.len(), 64);
        let mut report = sample_report();
        report.config_hash = hash.clone();
        report.run_id = run_id(1, &hash);
        assert!(!serde_json::to_string(&report).unwrap().contains(sentinel));
    }

    fn report_with(checks: &[(CheckName, Verdict, Completion, &[ReasonCode])]) -> Report {
        let mut r = sample_report();
        r.completion = Completion::Complete;
        r.checks = checks
            .iter()
            .map(|(check, verdict, completion, reasons)| CheckResult {
                check: *check,
                verdict: *verdict,
                reasons: reasons.to_vec(),
                completion: *completion,
                evidence: None,
            })
            .collect();
        r
    }

    #[test]
    fn exit_code_precedence_table() {
        use CheckName::*;
        use Completion::{Complete, Incomplete};
        use Verdict::*;
        let none: &[ReasonCode] = &[];
        let cancel: &[ReasonCode] = &[ReasonCode::Cancelled];
        let all = |b, r, l| {
            report_with(&[
                (BruteForce, b, Complete, none),
                (Race, r, Complete, none),
                (Load, l, Complete, none),
            ])
        };

        assert_eq!(exit_code(&all(Pass, Pass, Pass)), 0);
        assert_eq!(exit_code(&all(Pass, Fail, Pass)), 1);
        assert_eq!(exit_code(&all(Fail, Fail, Fail)), 1);
        assert_eq!(exit_code(&all(Pass, Fail, Error)), 2);
        assert_eq!(exit_code(&all(Pass, Pass, Inconclusive)), 2);
        assert_eq!(exit_code(&all(Fail, Pass, Inconclusive)), 2);
        assert_eq!(exit_code(&all(Pass, Pass, NotRun)), 2);
        // incomplete check, even with a pass verdict
        let incomplete_check = report_with(&[
            (BruteForce, Pass, Complete, none),
            (Race, Pass, Incomplete, none),
            (Load, Pass, Complete, none),
        ]);
        assert_eq!(exit_code(&incomplete_check), 2);
        // incomplete suite
        let mut incomplete_suite = all(Pass, Pass, Pass);
        incomplete_suite.completion = Incomplete;
        assert_eq!(exit_code(&incomplete_suite), 2);
        // cancel beats everything, including fail and error
        let cancelled = report_with(&[
            (BruteForce, Fail, Complete, none),
            (Race, Error, Incomplete, cancel),
            (Load, NotRun, Incomplete, none),
        ]);
        assert_eq!(exit_code(&cancelled), 130);
        // no false PASS: empty and partial check lists
        assert_eq!(exit_code(&report_with(&[])), 2);
        let partial = report_with(&[(BruteForce, Pass, Complete, none)]);
        assert_eq!(exit_code(&partial), 2);
        let duplicated = report_with(&[
            (BruteForce, Pass, Complete, none),
            (BruteForce, Pass, Complete, none),
            (Race, Pass, Complete, none),
        ]);
        assert_eq!(exit_code(&duplicated), 2);
        let dup_pass = report_with(&[
            (BruteForce, Pass, Complete, none),
            (BruteForce, Pass, Complete, none),
            (Race, Pass, Complete, none),
            (Load, Pass, Complete, none),
        ]);
        assert_eq!(exit_code(&dup_pass), 2);
        let dup_fail = report_with(&[
            (BruteForce, Fail, Complete, none),
            (BruteForce, Pass, Complete, none),
            (Race, Pass, Complete, none),
            (Load, Pass, Complete, none),
        ]);
        assert_eq!(exit_code(&dup_fail), 2);
        // cancel stays first even when the list is malformed
        let dup_cancel = report_with(&[
            (BruteForce, Pass, Complete, none),
            (BruteForce, Pass, Complete, none),
            (Race, Error, Incomplete, cancel),
            (Load, Pass, Complete, none),
        ]);
        assert_eq!(exit_code(&dup_cancel), 130);
        assert_eq!(
            exit_code(&report_with(&[(Race, Pass, Complete, cancel)])),
            130
        );
        // order may differ
        let reordered = report_with(&[
            (Load, Pass, Complete, none),
            (BruteForce, Pass, Complete, none),
            (Race, Pass, Complete, none),
        ]);
        assert_eq!(exit_code(&reordered), 0);
    }

    #[test]
    fn exit_code_pass_reasons_table() {
        use CheckName::*;
        use Completion::Complete;
        let with = |reasons: &[ReasonCode]| {
            report_with(&[
                (BruteForce, Verdict::Pass, Complete, reasons),
                (Race, Verdict::Pass, Complete, &[]),
                (Load, Verdict::Pass, Complete, &[]),
            ])
        };
        assert_eq!(exit_code(&with(&[ReasonCode::ProtectionObserved])), 0);
        assert_eq!(exit_code(&with(&[ReasonCode::SingleSideEffect])), 0);
        assert_eq!(exit_code(&with(&[ReasonCode::ThresholdsMet])), 0);
        for bad in [
            ReasonCode::ConfigInvalid,
            ReasonCode::TransportError,
            ReasonCode::DeadlineExceeded,
            ReasonCode::ReportWriteFailed,
        ] {
            assert_eq!(exit_code(&with(&[bad])), 2, "{bad:?}");
            assert_eq!(exit_code(&with(&[ReasonCode::ProtectionObserved, bad])), 2);
        }
        let cancel = with(&[ReasonCode::ReportWriteFailed, ReasonCode::Cancelled]);
        assert_eq!(exit_code(&cancel), 130);
    }

    #[test]
    fn atomic_write_creates_and_overwrites() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("report.json");
        let mut report = sample_report();
        write_report_atomic(&path, &report).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), golden_json());

        report.completion = Completion::Incomplete;
        write_report_atomic(&path, &report).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains("\"completion\": \"incomplete\",\n  \"checks\""));
        // only the report remains in the directory (no temp file)
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    fn tmp_files(dir: &Path) -> usize {
        fs::read_dir(dir)
            .unwrap()
            .filter(|e| {
                e.as_ref()
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .contains(".tmp-")
            })
            .count()
    }

    #[cfg(unix)]
    #[test]
    fn atomic_write_does_not_follow_planted_symlink() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("report.json");
        let victim = dir.path().join("victim.txt");
        fs::write(&victim, "victim").unwrap();
        let planted = dir
            .path()
            .join(format!(".report.json.tmp-{}-1000", std::process::id()));
        std::os::unix::fs::symlink(&victim, &planted).unwrap();

        // id 1000 hits the symlink (AlreadyExists), id 1001 is free
        let mut ids = 1000..;
        write_report_atomic_with(&path, &sample_report(), || ids.next().unwrap()).unwrap();
        assert_eq!(fs::read_to_string(&victim).unwrap(), "victim");
        assert_eq!(fs::read_to_string(&path).unwrap(), golden_json());
        // the planted symlink was not removed or replaced by us
        assert!(fs::symlink_metadata(&planted).unwrap().is_symlink());

        // every candidate taken: bounded retries, clean error, victim untouched
        let path2 = dir.path().join("other.json");
        for id in 0..TMP_ATTEMPTS {
            let p = dir
                .path()
                .join(format!(".other.json.tmp-{}-{id}", std::process::id()));
            std::os::unix::fs::symlink(&victim, p).unwrap();
        }
        let mut ids = 0..;
        let err =
            write_report_atomic_with(&path2, &sample_report(), || ids.next().unwrap()).unwrap_err();
        assert!(matches!(
            err,
            ReportError::Io {
                kind: std::io::ErrorKind::AlreadyExists,
                ..
            }
        ));
        assert!(!path2.exists());
        assert_eq!(fs::read_to_string(&victim).unwrap(), "victim");
    }

    #[test]
    fn atomic_write_concurrent_writers_same_target() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("report.json");
        let reports: Vec<Report> = (0..8)
            .map(|i| {
                let mut r = sample_report();
                r.run_id = format!("writer-{i}");
                r
            })
            .collect();
        std::thread::scope(|s| {
            for r in &reports {
                let path = &path;
                s.spawn(move || {
                    for _ in 0..50 {
                        write_report_atomic(path, r).unwrap();
                    }
                });
            }
        });
        let text = fs::read_to_string(&path).unwrap();
        let value: serde_json::Value = serde_json::from_str(&text).unwrap();
        let expected: Vec<serde_json::Value> = reports
            .iter()
            .map(|r| serde_json::to_value(r).unwrap())
            .collect();
        assert!(expected.contains(&value));
        assert_eq!(tmp_files(dir.path()), 0);
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn atomic_write_missing_directory_fails() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nope").join("report.json");
        let err = write_report_atomic(&path, &sample_report()).unwrap_err();
        assert!(matches!(err, ReportError::Io { .. }));
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[test]
    fn atomic_write_target_is_directory_fails_and_cleans_temp() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("report.json");
        fs::create_dir(&path).unwrap();
        fs::write(path.join("keep.txt"), "old").unwrap();
        let err = write_report_atomic(&path, &sample_report()).unwrap_err();
        assert!(matches!(err, ReportError::Io { .. }));
        assert_eq!(fs::read_to_string(path.join("keep.txt")).unwrap(), "old");
        // temp file removed: only the target directory remains
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn atomic_write_path_without_file_name_fails() {
        let err = write_report_atomic(Path::new("/"), &sample_report()).unwrap_err();
        assert!(matches!(err, ReportError::BadPath(_)));
    }

    #[cfg(unix)]
    #[test]
    fn atomic_write_read_only_directory_keeps_old_file() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("report.json");
        fs::write(&path, "old").unwrap();
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o555)).unwrap();
        // root ignores directory permissions; skip the check in that case
        let probe = fs::File::create(dir.path().join("probe"));
        let result = if probe.is_ok() {
            None
        } else {
            Some(write_report_atomic(&path, &sample_report()))
        };
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o755)).unwrap();
        if let Some(result) = result {
            assert!(matches!(result, Err(ReportError::Io { .. })));
            assert_eq!(fs::read_to_string(&path).unwrap(), "old");
        }
    }

    #[test]
    fn report_error_has_no_report_content() {
        let msg = ReportError::Io {
            path: PathBuf::from("/x/report.json"),
            kind: std::io::ErrorKind::NotFound,
        }
        .to_string();
        assert_eq!(msg, "cannot write report /x/report.json: entity not found");
    }
}
