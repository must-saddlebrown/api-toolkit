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
    // `check` must not connect either, and must not create --out.
    let cfg = write_cfg(&dir, &valid);
    let out = dir.path().join("report.json");
    let r = run_bin(
        &[
            "check",
            "--config",
            cfg.to_str().unwrap(),
            "--out",
            out.to_str().unwrap(),
        ],
        &DEV_ENV,
    );
    assert_eq!(r.code, 2);
    assert!(!out.exists());

    // Positive control: the listener really counts connections.
    TcpStream::connect(("127.0.0.1", listener.port)).unwrap();
    assert_eq!(listener.finish(), 1, "only the control connection expected");
}

#[test]
fn check_stub_never_zero_and_keeps_existing_out() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = write_cfg(&dir, &example_text());
    let out = dir.path().join("report.json");
    std::fs::write(&out, "OLD CONTENT").unwrap();
    let r = run_bin(
        &[
            "check",
            "--config",
            cfg.to_str().unwrap(),
            "--out",
            out.to_str().unwrap(),
        ],
        &DEV_ENV,
    );
    assert_eq!(r.code, 2);
    assert_eq!(std::fs::read_to_string(&out).unwrap(), "OLD CONTENT");
    assert!(r.stderr.contains("not implemented"), "{}", r.stderr);

    // Also without env, and with a nonexistent --out: still 2, nothing created.
    let new_out = dir.path().join("new.json");
    let r = run_bin(
        &[
            "check",
            "--config",
            cfg.to_str().unwrap(),
            "--out",
            new_out.to_str().unwrap(),
        ],
        &[],
    );
    assert_eq!(r.code, 2);
    assert!(!new_out.exists());
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

    // check stub.
    let out = dir.path().join("o.json");
    let r = run_bin(
        &[
            "check",
            "--config",
            cfg.to_str().unwrap(),
            "--out",
            out.to_str().unwrap(),
        ],
        &full,
    );
    assert_eq!(r.code, 2);
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
