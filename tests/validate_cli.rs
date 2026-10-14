//! Runs the real `api-toolkit` binary (P0-AC3/4/5/8/16). No network except a local
//! counting listener that must never see a connection.
mod support;
use std::net::TcpStream;
use std::path::PathBuf;
use support::cli::*;

const SENTINEL: &str = "SENTINEL-s3cr3t-9f2c";

/// (case name, config text, env, expected exit code)
type Case<'a> = (&'a str, String, &'a [(&'a str, &'a str)], i32);

fn write_cfg(dir: &tempfile::TempDir, text: &str) -> PathBuf {
    let p = dir.path().join("c.json");
    std::fs::write(&p, text).unwrap();
    p
}

/// Example config with the five dev env names replaced where noted, plus SENTINEL_VAR in
/// header, json, path_param and query positions.
fn sentinel_config() -> String {
    example_text()
        .replace("${ENV:LOGIN_USER}", "${ENV:SENTINEL_VAR}")
        .replace("${ENV:TEST_ACTION_ID}", "${ENV:SENTINEL_VAR}")
        .replace("${ENV:TEST_AUTHORIZATION}", "${ENV:SENTINEL_VAR}")
        .replacen(
            "\"method\": \"GET\", \"path\": \"/critical\",",
            "\"method\": \"GET\", \"path\": \"/critical\", \"query\": {\"q\": \"${ENV:SENTINEL_VAR}\"},",
            1,
        )
}

fn assert_no_sentinel(r: &Run) {
    assert!(
        !r.stdout.contains(SENTINEL),
        "sentinel in stdout: {}",
        r.stdout
    );
    assert!(
        !r.stderr.contains(SENTINEL),
        "sentinel in stderr: {}",
        r.stderr
    );
}

#[test]
fn validate_example_ok() {
    let r = validate(std::path::Path::new(EXAMPLE), &DEV_ENV);
    assert_eq!(r.code, 0, "stderr: {}", r.stderr);
    assert!(r.stdout.contains("117"), "{}", r.stdout);
    assert!(r.stdout.contains("950 ms"), "{}", r.stdout);
    for (name, value) in DEV_ENV {
        assert!(r.stdout.contains(name), "name {name} missing");
        // Values are short dev constants; check the distinctive ones are never printed.
        if value.contains('-') {
            assert!(!r.stdout.contains(value), "value of {name} printed");
            assert!(!r.stderr.contains(value), "value of {name} printed");
        }
    }
}

#[test]
fn validate_missing_env_lists_all_names() {
    let r = validate(std::path::Path::new(EXAMPLE), &[]);
    assert_eq!(r.code, 2);
    for (name, _) in DEV_ENV {
        assert!(r.stderr.contains(name), "{name} not listed: {}", r.stderr);
    }
}

#[test]
fn validate_never_sends_requests() {
    let listener = CountingListener::start();
    let base = listener.base_url();
    let valid = with_base_url(&example_text(), &base);
    let cases: Vec<Case> = vec![
        ("valid", valid.clone(), &DEV_ENV, 0),
        (
            "unknown_field",
            valid.replacen("{", "{\"bogus_field\": 1,", 1),
            &DEV_ENV,
            2,
        ),
        (
            "version_2",
            valid.replace("\"config_version\": 1", "\"config_version\": 2"),
            &DEV_ENV,
            2,
        ),
        ("missing_env", valid.clone(), &[], 2),
        (
            "probe_101",
            valid.replace("\"attempts\": 8", "\"attempts\": 101"),
            &DEV_ENV,
            2,
        ),
    ];
    let dir = tempfile::tempdir().unwrap();
    for (name, text, env, want) in cases {
        let r = validate(&write_cfg(&dir, &text), env);
        assert_eq!(r.code, want, "case {name}: stderr {}", r.stderr);
    }
    // `check` (full suite) must not connect either on invalid input, and must not create
    // --out: a missing env and a missing section, both against the live listener.
    let out = dir.path().join("report.json");
    let cfg = write_cfg(&dir, &valid);
    let out_s = out.to_str().unwrap();
    let check = |cfg: &PathBuf, env: &[(&str, &str)]| {
        run_bin(
            &["check", "--config", cfg.to_str().unwrap(), "--out", out_s],
            env,
        )
    };
    let r = check(&cfg, &[]);
    assert_eq!(r.code, 2, "missing env: {}", r.stderr);
    assert!(!out.exists());
    let no_load = {
        let mut v: serde_json::Value = serde_json::from_str(&valid).unwrap();
        v.as_object_mut().unwrap().remove("load");
        v.to_string()
    };
    let r = check(&write_cfg(&dir, &no_load), &DEV_ENV);
    assert_eq!(r.code, 2, "missing section: {}", r.stderr);
    assert!(r.stderr.contains("load"), "{}", r.stderr);
    assert!(!out.exists());

    // Positive control: the listener really counts connections.
    TcpStream::connect(("127.0.0.1", listener.port)).unwrap();
    assert_eq!(listener.finish(), 1, "only the control connection expected");
}

#[test]
fn check_full_invalid_input_is_2_and_keeps_existing_out() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = write_cfg(&dir, &example_text());
    let out = dir.path().join("report.json");
    std::fs::write(&out, "OLD CONTENT").unwrap();
    // No env: rejected before any request (the example points at 127.0.0.1:8080).
    let r = run_bin(
        &[
            "check",
            "--config",
            cfg.to_str().unwrap(),
            "--out",
            out.to_str().unwrap(),
        ],
        &[],
    );
    assert_eq!(r.code, 2);
    assert_eq!(std::fs::read_to_string(&out).unwrap(), "OLD CONTENT");
    assert!(r.stderr.contains("LOGIN_USER"), "{}", r.stderr);

    // Nonexistent --out parent: still 2, nothing created.
    // base_url points at a counting listener so a broken precheck cannot hit :8080.
    let listener = CountingListener::start();
    let cfg2 = write_cfg(&dir, &with_base_url(&example_text(), &listener.base_url()));
    let new_out = dir.path().join("nope").join("new.json");
    let r = run_bin(
        &[
            "check",
            "--config",
            cfg2.to_str().unwrap(),
            "--out",
            new_out.to_str().unwrap(),
        ],
        &DEV_ENV,
    );
    assert_eq!(r.code, 2);
    assert!(!new_out.exists());
    assert!(r.stderr.contains("parent directory"), "{}", r.stderr);
    assert_eq!(listener.finish(), 0, "precheck must not send requests");
}

#[test]
fn usage_errors_and_help() {
    assert_eq!(run_bin(&[], &[]).code, 2);
    assert_eq!(run_bin(&["bogus"], &[]).code, 2);
    assert_eq!(run_bin(&["validate"], &[]).code, 2); // missing --config
    assert_eq!(run_bin(&["check", "--config", "x"], &[]).code, 2); // missing --out
    let h = run_bin(&["--help"], &[]);
    assert_eq!(h.code, 0);
    assert!(h.stdout.contains("validate"));
}

#[test]
fn invalid_configs_are_exit_2_with_message() {
    let dir = tempfile::tempdir().unwrap();
    let base = example_text();
    let cases = [
        (
            "nested unknown field",
            base.replace(
                "\"attempts\": 8,",
                "\"attempts\": 8, \"nested_bogus_xyz\": 1,",
            ),
            "nested_bogus_xyz",
        ),
        (
            "version 2",
            base.replace("\"config_version\": 1", "\"config_version\": 2"),
            "config_version",
        ),
        (
            "missing required",
            base.replace("\"attempts\": 8,", ""),
            "attempts",
        ),
    ];
    for (name, text, needle) in cases {
        let r = validate(&write_cfg(&dir, &text), &DEV_ENV);
        assert_eq!(r.code, 2, "{name}");
        assert!(r.stderr.contains(needle), "{name}: stderr {}", r.stderr);
        assert!(r.stdout.is_empty(), "{name}: stdout {}", r.stdout);
    }
}

#[test]
fn sentinel_never_printed() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = write_cfg(&dir, &sentinel_config());
    let full = [
        ("SENTINEL_VAR", SENTINEL),
        ("LOGIN_VALID_PASSWORD", "demo-pass"),
        ("LOGIN_INVALID_PASSWORD", "wrong-pass"),
    ];

    // Success.
    let r = validate(&cfg, &full);
    assert_eq!(r.code, 0, "stderr: {}", r.stderr);
    assert!(r.stdout.contains("SENTINEL_VAR"));
    assert_no_sentinel(&r);

    // Another env var missing.
    let r = validate(&cfg, &[("SENTINEL_VAR", SENTINEL)]);
    assert_eq!(r.code, 2);
    assert!(r.stderr.contains("LOGIN_VALID_PASSWORD"));
    assert_no_sentinel(&r);

    // Bad config (unknown field) with the sentinel env set.
    let bad = sentinel_config().replacen("{", "{\"bogus_field\": 1,", 1);
    let r = validate(&write_cfg(&dir, &bad), &full);
    assert_eq!(r.code, 2);
    assert_no_sentinel(&r);

    // Bad value after resolution (limit exceeded) with the sentinel env set.
    let bad = sentinel_config().replace("\"attempts\": 8", "\"attempts\": 101");
    let r = validate(&write_cfg(&dir, &bad), &full);
    assert_eq!(r.code, 2);
    assert_no_sentinel(&r);

    // check (full suite), rejected before any request: a missing env with the sentinel set.
    let out = dir.path().join("o.json");
    let r = run_bin(
        &[
            "check",
            "--config",
            cfg.to_str().unwrap(),
            "--out",
            out.to_str().unwrap(),
        ],
        &[("SENTINEL_VAR", SENTINEL)],
    );
    assert_eq!(r.code, 2);
    assert!(!out.exists());
    assert_no_sentinel(&r);
}

// S9 carry-over (a): size limit is exactly 1 MiB.
#[test]
fn config_size_limit_is_one_mib() {
    const MIB: usize = 1024 * 1024;
    let dir = tempfile::tempdir().unwrap();
    let base = example_text();
    let pad = |total: usize| format!("{base}{}", " ".repeat(total - base.len()));

    let exact = pad(MIB);
    assert_eq!(exact.len(), MIB);
    let r = validate(&write_cfg(&dir, &exact), &DEV_ENV);
    assert!(
        !r.stderr.contains("larger than"),
        "exact limit rejected: {}",
        r.stderr
    );
    assert_eq!(r.code, 0, "stderr: {}", r.stderr);

    let over = pad(MIB + 1);
    let r = validate(&write_cfg(&dir, &over), &DEV_ENV);
    assert_eq!(r.code, 2);
    assert!(r.stderr.contains("larger than"), "{}", r.stderr);
}

// ---------- Q3-S5: burst mode (`brute_force.mode`) ----------

const BF_BURST: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/examples/brute-force-burst.json"
);
const BF_SEQ: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/examples/brute-force-only.json"
);

/// The two brute_force-only examples only resolve these three placeholders.
const BF_ENV: [(&str, &str); 3] = [
    ("LOGIN_USER", "demo"),
    ("LOGIN_VALID_PASSWORD", "demo-pass"),
    ("LOGIN_INVALID_PASSWORD", "wrong-pass"),
];

/// Both brute_force-only example configs have no `race`/`load` section, so `validate`
/// needs `--only brute_force` (see `docs/fixture.md`), unlike `EXAMPLE` (`core-checks.json`).
fn validate_bf_only(path: &str, env: &[(&str, &str)]) -> Run {
    run_bin(
        &["validate", "--config", path, "--only", "brute_force"],
        env,
    )
}

/// P1-AC19/21, AC31: `examples/brute-force-burst.json` is locked at 9 requests, 0 ms fixed
/// wait (burst has no interval).
#[test]
fn validate_burst_example_ok() {
    let r = validate_bf_only(BF_BURST, &BF_ENV);
    assert_eq!(r.code, 0, "stderr: {}", r.stderr);
    assert!(r.stdout.contains("planned requests: 9"), "{}", r.stdout);
    assert!(r.stdout.contains("fixed waits: 0 ms"), "{}", r.stdout);
}

/// P1-AC19/AC31 regression: the sequential-only example is unchanged by Q3.
#[test]
fn validate_sequential_only_example_unchanged() {
    let r = validate_bf_only(BF_SEQ, &BF_ENV);
    assert_eq!(r.code, 0, "stderr: {}", r.stderr);
    assert!(r.stdout.contains("planned requests: 9"), "{}", r.stdout);
    assert!(r.stdout.contains("fixed waits: 700 ms"), "{}", r.stdout);
}

/// P1-AC20/AC21: every per-mode violation is rejected before any request, with an error
/// that names only the field, never a value.
#[test]
fn validate_burst_mode_violations_are_exit_2_with_zero_requests() {
    let listener = CountingListener::start();
    let base = with_base_url(
        &std::fs::read_to_string(BF_SEQ).unwrap(),
        &listener.base_url(),
    );
    let burst_base = with_base_url(
        &std::fs::read_to_string(BF_BURST).unwrap(),
        &listener.base_url(),
    );
    let dir = tempfile::tempdir().unwrap();

    let cases: Vec<(&str, String, &str)> = vec![
        (
            "window_ms in sequential",
            base.replacen(
                "\"interval_ms\": 100,",
                "\"interval_ms\": 100, \"window_ms\": 1000,",
                1,
            ),
            "brute_force.window_ms",
        ),
        (
            "interval_ms in burst",
            burst_base.replacen(
                "\"attempts\": 8,",
                "\"attempts\": 8, \"interval_ms\": 100,",
                1,
            ),
            "brute_force.interval_ms",
        ),
        (
            "burst attempts 1",
            burst_base.replace("\"attempts\": 8,", "\"attempts\": 1,"),
            "brute_force.attempts",
        ),
        (
            "burst attempts 33",
            burst_base.replace("\"attempts\": 8,", "\"attempts\": 33,"),
            "brute_force.attempts",
        ),
        (
            "unknown mode value",
            burst_base.replace("\"mode\": \"burst\"", "\"mode\": \"parallel\""),
            "expected `sequential` or `burst`",
        ),
    ];
    assert_ne!(cases[0].1, base, "window_ms case: pattern not replaced");
    assert_ne!(
        cases[1].1, burst_base,
        "interval_ms case: pattern not replaced"
    );

    for (name, text, needle) in cases {
        let r = validate(&write_cfg(&dir, &text), &BF_ENV);
        assert_eq!(r.code, 2, "case {name}: stderr {}", r.stderr);
        assert!(
            r.stderr.contains(needle),
            "case {name}: stderr {} (want {needle})",
            r.stderr
        );
        assert!(r.stdout.is_empty(), "case {name}: stdout {}", r.stdout);
        for (_, v) in BF_ENV {
            if v.contains('-') {
                assert!(
                    !r.stderr.contains(v),
                    "case {name}: value leaked: {}",
                    r.stderr
                );
            }
        }
    }
    assert_eq!(
        listener.finish(),
        0,
        "a rejected burst config opened a connection"
    );
}

// ---------- Q4-S7: `baseline = "none"` (budget and the cross-field rejection) ----------

const BF_NONE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/examples/brute-force-baseline-none.json"
);
/// `examples/brute-force-baseline-none.json` only resolves these two placeholders
/// (no `LOGIN_VALID_PASSWORD`: `baseline = "none"` never sends a baseline request).
const BF_NONE_ENV: [(&str, &str); 2] = [
    ("LOGIN_USER", "demo"),
    ("LOGIN_INVALID_PASSWORD", "wrong-pass"),
];

/// P1-AC35: `baseline = "none"` budgets `attempts` requests, not `1 + attempts` -- no
/// baseline request is planned. `attempts: 8`, `interval_ms: 100` -> 700 ms fixed wait
/// (unchanged formula: `(attempts - 1) * interval_ms`, baseline never contributes a wait).
#[test]
fn validate_baseline_none_example_budgets_attempts_only() {
    let r = run_bin(
        &["validate", "--config", BF_NONE, "--only", "brute_force"],
        &BF_NONE_ENV,
    );
    assert_eq!(r.code, 0, "stderr: {}", r.stderr);
    assert!(r.stdout.contains("planned requests: 8"), "{}", r.stdout);
    assert!(r.stdout.contains("700 ms"), "{}", r.stdout);
}

/// P1-AC35: `baseline_request` present together with `baseline: "none"` is rejected before
/// any request, naming only the field (pattern Q3 "nothing is silently ignored").
#[test]
fn validate_baseline_none_rejects_baseline_request_present() {
    let dir = tempfile::tempdir().unwrap();
    let base = std::fs::read_to_string(BF_NONE).unwrap();
    let with_baseline_request = base.replacen(
        "\"baseline\": \"none\",",
        concat!(
            "\"baseline\": \"none\",",
            "\"baseline_request\": {\"method\": \"POST\", \"path\": \"/per-ip/login\",",
            "\"json\": {\"username\": \"${ENV:LOGIN_USER}\", \"password\": \"${ENV:LOGIN_INVALID_PASSWORD}\"}},",
        ),
        1,
    );
    assert_ne!(with_baseline_request, base, "pattern not replaced");
    let path = write_cfg(&dir, &with_baseline_request);
    let r = run_bin(
        &[
            "validate",
            "--config",
            path.to_str().unwrap(),
            "--only",
            "brute_force",
        ],
        &BF_NONE_ENV,
    );
    assert_eq!(r.code, 2, "stdout: {}", r.stdout);
    assert!(
        r.stderr.contains("brute_force.baseline_request"),
        "{}",
        r.stderr
    );
    assert!(
        r.stderr.contains("is not allowed when baseline is none"),
        "{}",
        r.stderr
    );
    assert!(r.stdout.is_empty(), "{}", r.stdout);
}

// S9 carry-over (c): known limitation, see S9 carry-over. A placeholder in a `query` key or
// a json body object key is silently kept as a literal. This pins CURRENT behavior only;
// it is not the desired behavior.
#[test]
fn placeholder_in_query_or_json_key_is_accepted_known_limitation() {
    let dir = tempfile::tempdir().unwrap();
    let base = example_text();
    let query_key = base.replacen(
        "\"method\": \"GET\", \"path\": \"/critical\",",
        "\"method\": \"GET\", \"path\": \"/critical\", \"query\": {\"${ENV:LOGIN_USER}\": \"v\"},",
        1,
    );
    let json_key = base.replacen(
        "\"json\": {\"username\"",
        "\"json\": {\"${ENV:LOGIN_USER}\": 1, \"username\"",
        1,
    );
    assert_ne!(json_key, base);
    for (name, text) in [("query key", query_key), ("json key", json_key)] {
        let r = validate(&write_cfg(&dir, &text), &DEV_ENV);
        assert_eq!(r.code, 0, "{name}: stderr {}", r.stderr);
    }
}
