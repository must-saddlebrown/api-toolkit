//! Public-API contract of the report module (P0-AC13/14/15). Golden unit tests live in
//! src/report.rs; this file checks the same contract from outside the crate.
use api_toolkit::report::*;
use serde_json::{Value, json};
use time::OffsetDateTime;

const SENTINEL_CONFIG: &str = r#"{"password": "SENTINEL-s3cr3t-9f2c"}"#;

fn to_str<T: serde::Serialize>(v: T) -> String {
    match serde_json::to_value(v).unwrap() {
        Value::String(s) => s,
        other => panic!("not a string: {other}"),
    }
}

fn check(name: CheckName, verdict: Verdict, reasons: &[ReasonCode]) -> CheckResult {
    CheckResult {
        check: name,
        verdict,
        reasons: reasons.to_vec(),
        completion: Completion::Complete,
        evidence: None,
    }
}

fn pass_checks() -> Vec<CheckResult> {
    vec![
        check(
            CheckName::BruteForce,
            Verdict::Pass,
            &[ReasonCode::ProtectionObserved],
        ),
        check(
            CheckName::Race,
            Verdict::Pass,
            &[ReasonCode::SingleSideEffect],
        ),
        check(CheckName::Load, Verdict::Pass, &[ReasonCode::ThresholdsMet]),
    ]
}

fn report(completion: Completion, checks: Vec<CheckResult>) -> Report {
    let t = OffsetDateTime::UNIX_EPOCH;
    Report::new(
        "1-abcdef12".into(),
        config_hash(SENTINEL_CONFIG.as_bytes()),
        t,
        t,
        completion,
        Scope::Full,
        checks,
    )
    .unwrap()
}

fn is_snake(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_lowercase() || c == '_' || c.is_ascii_digit())
}

#[test]
fn enums_serialize_to_stable_snake_case() {
    assert_eq!(
        Verdict::ALL.map(to_str),
        ["pass", "fail", "inconclusive", "error", "not_run"]
    );
    assert_eq!(CheckName::ALL.map(to_str), ["brute_force", "race", "load"]);
    assert_eq!(Completion::ALL.map(to_str), ["complete", "incomplete"]);
    assert_eq!(
        Outcome::ALL.map(to_str),
        [
            "success",
            "normal_rejection",
            "protection",
            "unexpected",
            "transport_error"
        ]
    );
    assert_eq!(
        TransportErrorCode::ALL.map(to_str),
        [
            "timeout",
            "connect_failed",
            "tls_failed",
            "body_too_large",
            "body_read_failed",
            "request_invalid",
            "other"
        ]
    );
    // ReasonCode: the exact documented set (P0 plan), serde string == as_str, all unique.
    let expected = [
        "config_invalid",
        "deadline_exceeded",
        "cancelled",
        "not_started",
        "report_write_failed",
        "transport_error",
        "overlapping_rules",
        "protection_observed",
        "invalid_credentials_accepted",
        "protection_policy_not_met",
        "account_already_locked",
        "baseline_failed",
        "probe_unclassifiable",
        "single_side_effect",
        "duplicate_side_effect",
        "action_not_executed",
        "initial_counter_not_zero",
        "dispatch_incomplete",
        "dispatch_skew_exceeded",
        "action_outcome_ambiguous",
        "verifier_failed",
        "counter_invalid",
        "thresholds_met",
        "error_rate_exceeded",
        "latency_exceeded",
        "target_not_ready",
        "workload_incomplete",
        "preflight_failed",
        "runner_error",
        "excluded_by_only",
    ];
    let mut got: Vec<String> = ReasonCode::ALL.iter().map(|r| to_str(*r)).collect();
    for (r, s) in ReasonCode::ALL.iter().zip(&got) {
        assert_eq!(r.as_str(), s);
        assert!(is_snake(s));
    }
    got.sort();
    let mut want: Vec<String> = expected.iter().map(|s| s.to_string()).collect();
    want.sort();
    assert_eq!(got, want);
}

#[test]
fn exit_code_table() {
    use CheckName::*;
    use ReasonCode as R;
    let c = |n, v, r: &[ReasonCode]| check(n, v, r);

    assert_eq!(exit_code(&report(Completion::Complete, pass_checks())), 0);

    let mut fail_only = pass_checks();
    fail_only[1] = c(Race, Verdict::Fail, &[R::DuplicateSideEffect]);
    assert_eq!(
        exit_code(&report(Completion::Complete, fail_only.clone())),
        1
    );

    let mut fail_error = fail_only;
    fail_error[2] = c(Load, Verdict::Error, &[R::RunnerError]);
    assert_eq!(exit_code(&report(Completion::Complete, fail_error)), 2);

    let mut cancelled = pass_checks();
    cancelled[2] = c(Load, Verdict::NotRun, &[R::Cancelled]);
    assert_eq!(exit_code(&report(Completion::Incomplete, cancelled)), 130);

    // Empty or partial check list, incomplete suite, inconclusive / not_run: never 0 or 1.
    assert_eq!(exit_code(&report(Completion::Complete, vec![])), 2);
    assert_eq!(
        exit_code(&report(Completion::Complete, pass_checks()[..2].to_vec())),
        2
    );
    assert_eq!(exit_code(&report(Completion::Incomplete, pass_checks())), 2);
    let mut inc = pass_checks();
    inc[0] = c(
        BruteForce,
        Verdict::Inconclusive,
        &[R::AccountAlreadyLocked],
    );
    assert_eq!(exit_code(&report(Completion::Complete, inc)), 2);
    let mut nr = pass_checks();
    nr[0] = c(BruteForce, Verdict::NotRun, &[R::NotStarted]);
    assert_eq!(exit_code(&report(Completion::Complete, nr)), 2);
}

#[test]
fn written_report_has_contract_keys_and_no_secret_fields() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("report.json");
    let rep = report(Completion::Complete, pass_checks());
    write_report_atomic(&path, &rep).unwrap();

    let text = std::fs::read_to_string(&path).unwrap();
    let v: Value = serde_json::from_str(&text).unwrap();
    let mut keys: Vec<&str> = v.as_object().unwrap().keys().map(String::as_str).collect();
    keys.sort();
    assert_eq!(
        keys,
        [
            "checks",
            "completion",
            "config_hash",
            "finished_at",
            "report_version",
            "run_id",
            "scope",
            "started_at",
            "tool_version"
        ]
    );
    assert_eq!(v["report_version"], json!(1));
    assert_eq!(v["checks"].as_array().unwrap().len(), 3);
    assert_eq!(v["checks"][0]["check"], "brute_force");
    assert_eq!(v["checks"][0]["reasons"], json!(["protection_observed"]));
    assert_eq!(v["started_at"], "1970-01-01T00:00:00Z");

    // No key anywhere (any depth) looks like a body/header/secret field.
    fn walk(v: &Value, bad: &mut Vec<String>) {
        match v {
            Value::Object(m) => {
                for (k, v) in m {
                    let k = k.to_lowercase();
                    if ["body", "header", "secret", "password", "token"]
                        .iter()
                        .any(|w| k.contains(w))
                    {
                        bad.push(k);
                    }
                    walk(v, bad);
                }
            }
            Value::Array(a) => a.iter().for_each(|v| walk(v, bad)),
            _ => {}
        }
    }
    let mut bad = vec![];
    walk(&v, &mut bad);
    assert!(bad.is_empty(), "suspicious keys: {bad:?}");

    // The sentinel config was only hashed: neither it nor its pieces are in the file.
    assert!(!text.contains("SENTINEL"));
    assert!(!text.contains("s3cr3t"));
    assert_eq!(v["config_hash"], config_hash(SENTINEL_CONFIG.as_bytes()));
    assert_eq!(config_hash(SENTINEL_CONFIG.as_bytes()).len(), 64);
}

#[test]
fn write_failure_is_an_error_and_old_file_survives() {
    let dir = tempfile::tempdir().unwrap();
    let rep = report(Completion::Complete, pass_checks());

    // Missing directory: error, nothing created.
    let missing = dir.path().join("nope").join("r.json");
    assert!(write_report_atomic(&missing, &rep).is_err());

    // Target is a non-empty directory: rename fails, error, directory content intact, no temp left.
    let target = dir.path().join("r.json");
    std::fs::create_dir(&target).unwrap();
    std::fs::write(target.join("keep"), "x").unwrap();
    assert!(write_report_atomic(&target, &rep).is_err());
    assert!(target.join("keep").exists());
    let names: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(names.len(), 1, "leftover temp files: {names:?}");
}
