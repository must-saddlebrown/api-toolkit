//! Command line interface: `validate` (P0) and `check` (`--only` argument layer; the run
//! itself is wired in S4, the full suite in P4).
//!
//! `run` is a plain function over injected args, env lookup and output streams so it
//! can be tested in-process. It returns the exit code; `main` only converts it.
//! `validate` never builds a transport, so it cannot send a request.

use std::ffi::OsString;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use anyhow::Context;
use clap::{Parser, Subcommand};

use crate::config::{self, Selection};
use crate::report::CheckName;

/// Exit code for invalid config, usage errors and the `check` stub (PLAN §10).
const EXIT_INVALID: u8 = 2;

#[derive(Parser)]
#[command(
    name = "api-toolkit",
    version,
    about = "Non-functional checks for an API"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Check the config and that its environment variables are set. Sends no request.
    Validate {
        #[arg(long)]
        config: PathBuf,
        /// Validate only this check's section (available: brute_force). Without it the
        /// full suite config is validated.
        #[arg(long)]
        only: Option<String>,
    },
    /// Run checks. Until P4 exactly one check must be selected with --only. A partial run
    /// never exits 0: exit 1 = FAIL, 2 = PASS or any other result, 130 = cancelled; read
    /// the verdict in the console line or the report.
    Check {
        #[arg(long)]
        config: PathBuf,
        #[arg(long)]
        out: PathBuf,
        /// Check to run (required until P4). Available: brute_force.
        #[arg(long)]
        only: Option<String>,
    },
}

/// Run the CLI and return the process exit code.
pub fn run<I, T>(
    args: I,
    env_lookup: &impl Fn(&str) -> Option<String>,
    out: &mut impl Write,
    err: &mut impl Write,
) -> u8
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let cli = match Cli::try_parse_from(args) {
        Ok(cli) => cli,
        Err(e) => {
            // clap: exit code 0 for --help/--version, 2 for usage errors.
            let code = e.exit_code();
            let text = e.render().to_string();
            let _ = if e.use_stderr() {
                write!(err, "{text}")
            } else {
                write!(out, "{text}")
            };
            return u8::try_from(code).unwrap_or(EXIT_INVALID);
        }
    };

    match cli.command {
        Command::Validate { config, only } => {
            let result = match only.as_deref().map(parse_only).transpose() {
                Ok(only) => validate_file(&config, only, env_lookup, out),
                Err(e) => Err(e),
            };
            match result {
                Ok(()) => 0,
                Err(e) => {
                    let _ = writeln!(err, "error: {e:#}");
                    EXIT_INVALID
                }
            }
        }
        Command::Check {
            config,
            out: out_path,
            only,
        } => match check_args(&config, &out_path, only.as_deref(), env_lookup) {
            Ok(()) => {
                // ponytail: argument layer only; S4 replaces this with the real run.
                let _ = writeln!(
                    err,
                    "error: `check --only` is not wired yet (the run itself comes in S4); no request was sent and no report was written"
                );
                EXIT_INVALID
            }
            Err(e) => {
                let _ = writeln!(err, "error: {e:#}");
                EXIT_INVALID
            }
        },
    }
}

/// Map the `--only` value to a check. Only `brute_force` exists so far (K6).
fn parse_only(name: &str) -> anyhow::Result<CheckName> {
    match name {
        "brute_force" => Ok(CheckName::BruteForce),
        "race" => anyhow::bail!("check `race` is not available yet (planned for P2)"),
        "load" => anyhow::bail!("check `load` is not available yet (planned for P3)"),
        _ => anyhow::bail!("unknown check `{name}`; available: brute_force"),
    }
}

/// Everything `check` can know before sending a request. Order matters (K7): `--only`
/// first, so a missing or bad value never reads the config or touches `--out`.
fn check_args(
    config: &Path,
    out: &Path,
    only: Option<&str>,
    env_lookup: &impl Fn(&str) -> Option<String>,
) -> anyhow::Result<()> {
    let Some(only) = only else {
        anyhow::bail!(
            "the full suite is not implemented until P4 (full suite available in P4); run one check with --only brute_force. The config was not read, no request was sent and no report was written."
        );
    };
    let sel = Selection::Only(parse_only(only)?);
    let text = read_config(config)?;
    let cfg = config::parse(&text)?;
    config::validate(&cfg, sel)?; // validate before resolve
    config::resolve(&cfg, sel, env_lookup)?;
    precheck_out(config, out)
}

/// Reject an unusable `--out` before any request is sent (K19): the check may change
/// account state, so a report that cannot be saved must fail early.
fn precheck_out(config: &Path, out: &Path) -> anyhow::Result<()> {
    let parent = match out.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    };
    if !parent.is_dir() {
        anyhow::bail!(
            "cannot write report `{}`: parent directory does not exist or is not a directory",
            out.display()
        );
    }
    if out.is_dir() {
        anyhow::bail!("report path `{}` is a directory", out.display());
    }
    if let (Ok(a), Ok(b)) = (std::fs::canonicalize(out), std::fs::canonicalize(config))
        && a == b
    {
        anyhow::bail!("report path `{}` is the config file", out.display());
    }
    Ok(())
}

/// Largest config file we read; a bigger file is rejected, never read to the end.
const MAX_CONFIG_BYTES: u64 = 1024 * 1024;

fn read_config(path: &Path) -> anyhow::Result<String> {
    let ctx = || format!("cannot read config file `{}`", path.display());
    // Refuse FIFOs, devices and directories up front: reading them can block forever.
    let meta = std::fs::metadata(path).with_context(ctx)?;
    if !meta.is_file() {
        anyhow::bail!("config path `{}` is not a regular file", path.display());
    }
    let file = std::fs::File::open(path).with_context(ctx)?;
    let mut bytes = Vec::new();
    file.take(MAX_CONFIG_BYTES + 1)
        .read_to_end(&mut bytes)
        .with_context(ctx)?;
    if bytes.len() as u64 > MAX_CONFIG_BYTES {
        anyhow::bail!(
            "config file `{}` is larger than the limit of {MAX_CONFIG_BYTES} bytes",
            path.display()
        );
    }
    String::from_utf8(bytes)
        .map_err(|_| anyhow::anyhow!("config file `{}` is not valid UTF-8", path.display()))
}

/// Load, parse, validate and resolve the config, then print a summary (names only).
fn validate_file(
    path: &Path,
    only: Option<CheckName>,
    env_lookup: &impl Fn(&str) -> Option<String>,
    out: &mut impl Write,
) -> anyhow::Result<()> {
    let sel = only.map_or(Selection::All, Selection::Only);
    let text = read_config(path)?;
    let cfg = config::parse(&text)?;
    let budget = config::validate(&cfg, sel)?; // validate before resolve
    let resolved = config::resolve(&cfg, sel, env_lookup)?;

    writeln!(out, "config OK")?;
    writeln!(out, "planned requests: {}", budget.requests)?;
    writeln!(out, "fixed waits: {} ms", budget.fixed_wait_ms)?;
    writeln!(
        out,
        "environment variables present: {}",
        resolved.env_names.join(", ")
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    const SENTINEL: &str = "SENTINEL-s3cr3t-value-9f2c";
    const EXAMPLE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/examples/core-checks.json");
    const ENV_NAMES: [&str; 5] = [
        "LOGIN_INVALID_PASSWORD",
        "LOGIN_USER",
        "LOGIN_VALID_PASSWORD",
        "TEST_ACTION_ID",
        "TEST_AUTHORIZATION",
    ];

    fn full_env(name: &str) -> Option<String> {
        ENV_NAMES.contains(&name).then(|| SENTINEL.to_string())
    }

    /// Returns (exit code, stdout, stderr).
    fn run_args(args: &[&str], env: &impl Fn(&str) -> Option<String>) -> (u8, String, String) {
        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = run(args.iter().copied(), env, &mut out, &mut err);
        (
            code,
            String::from_utf8(out).unwrap(),
            String::from_utf8(err).unwrap(),
        )
    }

    fn validate(path: &str, env: &impl Fn(&str) -> Option<String>) -> (u8, String, String) {
        run_args(&["api-toolkit", "validate", "--config", path], env)
    }

    /// Write `text` to a temp config file; keep the dir alive in the return value.
    fn temp_config(text: &str) -> (tempfile::TempDir, String) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("c.json");
        std::fs::write(&path, text).unwrap();
        let p = path.to_str().unwrap().to_string();
        (dir, p)
    }

    fn assert_no_sentinel(out: &str, err: &str) {
        assert!(!out.contains(SENTINEL), "sentinel in stdout: {out}");
        assert!(!err.contains(SENTINEL), "sentinel in stderr: {err}");
    }

    fn example_text() -> String {
        std::fs::read_to_string(EXAMPLE).unwrap()
    }

    #[test]
    fn validate_example_ok() {
        let (code, out, err) = validate(EXAMPLE, &full_env);
        assert_eq!(code, 0, "stderr: {err}");
        assert!(out.contains("planned requests: 117"), "{out}");
        assert!(out.contains("fixed waits: 950 ms"), "{out}");
        for name in ENV_NAMES {
            assert!(out.contains(name), "missing {name} in {out}");
        }
        assert_no_sentinel(&out, &err);
    }

    #[test]
    fn unknown_field_is_2() {
        let text = example_text().replacen("{", "{\"bogus\": 1,", 1);
        let (_d, path) = temp_config(&text);
        let (code, out, err) = validate(&path, &full_env);
        assert_eq!(code, 2);
        assert!(err.contains("unknown field"), "{err}");
        assert_no_sentinel(&out, &err);
    }

    #[test]
    fn missing_env_lists_all_names() {
        let only_user = |n: &str| (n == "LOGIN_USER").then(|| SENTINEL.to_string());
        let (code, out, err) = validate(EXAMPLE, &only_user);
        assert_eq!(code, 2);
        for name in ENV_NAMES.iter().filter(|n| **n != "LOGIN_USER") {
            assert!(err.contains(name), "{name} not listed: {err}");
        }
        assert!(out.is_empty());
        assert_no_sentinel(&out, &err);
    }

    #[test]
    fn bad_placeholder_is_2_without_sentinel() {
        let text = example_text().replacen("${ENV:LOGIN_USER}", "${ENV:bad name}", 1);
        let (_d, path) = temp_config(&text);
        let (code, out, err) = validate(&path, &full_env);
        assert_eq!(code, 2);
        assert!(err.contains("placeholder"), "{err}");
        assert_no_sentinel(&out, &err);
    }

    #[test]
    fn config_version_2_is_2() {
        let text = example_text().replacen("\"config_version\": 1", "\"config_version\": 2", 1);
        assert_ne!(text, example_text());
        let (_d, path) = temp_config(&text);
        let (code, _out, err) = validate(&path, &full_env);
        assert_eq!(code, 2);
        assert!(err.contains("config_version"), "{err}");
    }

    #[test]
    fn limit_violation_is_2() {
        let mut v: Value = serde_json::from_str(&example_text()).unwrap();
        v["brute_force"]["attempts"] = Value::from(101);
        let (_d, path) = temp_config(&v.to_string());
        let (code, _out, err) = validate(&path, &full_env);
        assert_eq!(code, 2);
        assert!(err.contains("brute_force"), "{err}");
    }

    #[test]
    fn missing_file_is_2_and_names_path() {
        let (code, out, err) = validate("/nonexistent/dir/c.json", &full_env);
        assert_eq!(code, 2);
        assert!(err.contains("/nonexistent/dir/c.json"), "{err}");
        assert!(out.is_empty());
    }

    #[test]
    fn non_json_file_does_not_echo_content() {
        let (_d, path) = temp_config(SENTINEL);
        let (code, out, err) = validate(&path, &full_env);
        assert_eq!(code, 2);
        assert_no_sentinel(&out, &err);
    }

    #[test]
    fn check_is_2_and_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let report = dir.path().join("report.json");
        let (code, out, err) = run_args(
            &[
                "api-toolkit",
                "check",
                "--config",
                EXAMPLE,
                "--out",
                report.to_str().unwrap(),
            ],
            &full_env,
        );
        assert_eq!(code, 2);
        assert!(err.contains("not implemented"), "{err}");
        assert!(!report.exists());
        assert_no_sentinel(&out, &err);
    }

    #[test]
    fn check_requires_out_and_config() {
        let (code, _, _) = run_args(&["api-toolkit", "check", "--config", EXAMPLE], &full_env);
        assert_eq!(code, 2);
        let (code, _, _) = run_args(&["api-toolkit", "check", "--out", "/tmp/x"], &full_env);
        assert_eq!(code, 2);
    }

    #[test]
    fn usage_errors_are_never_0() {
        let (code, _, err) = run_args(&["api-toolkit"], &full_env);
        assert_ne!(code, 0);
        assert!(!err.is_empty());
        let (code, _, _) = run_args(&["api-toolkit", "bogus"], &full_env);
        assert_eq!(code, 2);
        let (code, _, _) = run_args(&["api-toolkit", "validate"], &full_env);
        assert_eq!(code, 2);
        let (code, _, _) = run_args(
            &["api-toolkit", "validate", "--config", EXAMPLE, "--x"],
            &full_env,
        );
        assert_eq!(code, 2);
    }

    #[test]
    fn json_equals_placeholder_is_2_and_names_path_only() {
        let mut v: Value = serde_json::from_str(&example_text()).unwrap();
        v["brute_force"]["success_match"]["json_equals"] = serde_json::json!({"/a": "${ENV:ZED}"});
        let (_d, path) = temp_config(&v.to_string());
        let (code, out, err) = validate(&path, &full_env);
        assert_eq!(code, 2, "{err}");
        assert!(
            err.contains("brute_force.success_match.json_equals"),
            "{err}"
        );
        assert!(!err.contains("ZED") && !out.contains("ZED"), "{err}");
    }

    #[test]
    fn printed_names_equal_resolved_names() {
        let (code, out, _) = validate(EXAMPLE, &full_env);
        assert_eq!(code, 0);
        let line = out
            .lines()
            .find_map(|l| l.strip_prefix("environment variables present: "))
            .unwrap();
        assert_eq!(line, ENV_NAMES.join(", "));
    }

    #[test]
    fn oversized_config_is_2_and_names_path_and_limit() {
        let (_d, path) = temp_config(&" ".repeat(MAX_CONFIG_BYTES as usize + 1));
        let (code, out, err) = validate(&path, &full_env);
        assert_eq!(code, 2);
        assert!(err.contains(&path) && err.contains("1048576"), "{err}");
        assert!(out.is_empty());
    }

    #[test]
    fn stdout_write_error_is_2_without_panic() {
        struct Broken;
        impl Write for Broken {
            fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
                Err(std::io::Error::from(std::io::ErrorKind::BrokenPipe))
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let mut err = Vec::new();
        let args = ["api-toolkit", "validate", "--config", EXAMPLE];
        let code = run(args.iter().copied(), &full_env, &mut Broken, &mut err);
        assert_eq!(code, 2);
        assert!(!err.is_empty());
    }

    #[test]
    fn check_leaves_existing_out_file_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let report = dir.path().join("report.json");
        std::fs::write(&report, "KEEP-ME").unwrap();
        let (code, _, _) = run_args(
            &[
                "api-toolkit",
                "check",
                "--config",
                EXAMPLE,
                "--out",
                report.to_str().unwrap(),
            ],
            &full_env,
        );
        assert_eq!(code, 2);
        assert_eq!(std::fs::read_to_string(&report).unwrap(), "KEEP-ME");
    }

    const PARTIAL: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/examples/brute-force-only.json"
    );

    fn check_only(args: &[&str]) -> (u8, String, String) {
        let mut a = vec!["api-toolkit", "check"];
        a.extend_from_slice(args);
        run_args(&a, &full_env)
    }

    #[test]
    fn check_without_only_gives_p4_message_and_reads_no_config() {
        let dir = tempfile::tempdir().unwrap();
        let report = dir.path().join("r.json");
        std::fs::write(&report, "KEEP-ME").unwrap();
        let (code, out, err) = check_only(&[
            "--config",
            "/nonexistent/c.json",
            "--out",
            report.to_str().unwrap(),
        ]);
        assert_eq!(code, 2);
        assert!(err.contains("full suite available in P4"), "{err}");
        assert!(err.contains("--only"), "{err}");
        assert!(!err.contains("/nonexistent"), "config was read: {err}");
        assert_eq!(std::fs::read_to_string(&report).unwrap(), "KEEP-ME");
        assert_no_sentinel(&out, &err);
    }

    #[test]
    fn only_rejects_unknown_and_unavailable_names_in_check_and_validate() {
        let dir = tempfile::tempdir().unwrap();
        let report = dir.path().join("r.json");
        let rep = report.to_str().unwrap();
        let cases = [
            ("bogus", "unknown check"),
            ("Brute_Force", "unknown check"),
            ("", "unknown check"),
            ("race", "not available yet"),
            ("load", "not available yet"),
        ];
        for (name, want) in cases {
            // The config path does not exist: a name error must come first.
            let (code, out, err) = check_only(&[
                "--config",
                "/nonexistent/c.json",
                "--out",
                rep,
                "--only",
                name,
            ]);
            assert_eq!(code, 2, "check {name}");
            assert!(err.contains(want), "check {name}: {err}");
            assert!(!err.contains("/nonexistent"), "{err}");
            assert!(!report.exists());
            assert_no_sentinel(&out, &err);

            let (code, out, err) = run_args(
                &[
                    "api-toolkit",
                    "validate",
                    "--config",
                    PARTIAL,
                    "--only",
                    name,
                ],
                &full_env,
            );
            assert_eq!(code, 2, "validate {name}");
            assert!(err.contains(want), "validate {name}: {err}");
            assert!(out.is_empty(), "{out}");
        }
        let (_, _, err) = check_only(&["--config", "x", "--out", rep, "--only", "bogus"]);
        assert!(err.contains("available: brute_force"), "{err}");
        let (_, _, err) = check_only(&["--config", "x", "--out", rep, "--only", "race"]);
        assert!(err.contains("P2"), "{err}");
        let (_, _, err) = check_only(&["--config", "x", "--out", rep, "--only", "load"]);
        assert!(err.contains("P3"), "{err}");
    }

    #[test]
    fn validate_only_brute_force_on_partial_example() {
        let env = |n: &str| n.starts_with("LOGIN_").then(|| SENTINEL.to_string());
        let (code, out, err) = run_args(
            &[
                "api-toolkit",
                "validate",
                "--config",
                PARTIAL,
                "--only",
                "brute_force",
            ],
            &env,
        );
        assert_eq!(code, 0, "{err}");
        assert!(out.contains("planned requests: 9"), "{out}");
        assert!(out.contains("fixed waits: 700 ms"), "{out}");
        assert!(
            out.contains("environment variables present: LOGIN_INVALID_PASSWORD, LOGIN_USER, LOGIN_VALID_PASSWORD"),
            "{out}"
        );
        assert_no_sentinel(&out, &err);
    }

    #[test]
    fn validate_without_only_on_partial_example_names_race() {
        let (code, out, err) = validate(PARTIAL, &full_env);
        assert_eq!(code, 2);
        assert!(err.contains("race"), "{err}");
        assert!(out.is_empty(), "{out}");
    }

    #[test]
    fn check_only_brute_force_valid_stops_not_wired_without_report() {
        let dir = tempfile::tempdir().unwrap();
        let report = dir.path().join("r.json");
        let (code, out, err) = check_only(&[
            "--config",
            PARTIAL,
            "--out",
            report.to_str().unwrap(),
            "--only",
            "brute_force",
        ]);
        assert_eq!(code, 2);
        assert!(err.contains("not wired yet"), "{err}");
        assert!(!report.exists());
        assert_no_sentinel(&out, &err);
    }

    #[test]
    fn check_only_config_errors_are_2_without_sentinel() {
        let dir = tempfile::tempdir().unwrap();
        let report = dir.path().join("r.json");
        let rep = report.to_str().unwrap();
        let no_env = |_: &str| None;
        let (code, _, err) = run_args(
            &[
                "api-toolkit",
                "check",
                "--config",
                PARTIAL,
                "--out",
                rep,
                "--only",
                "brute_force",
            ],
            &no_env,
        );
        assert_eq!(code, 2);
        assert!(err.contains("LOGIN_USER"), "{err}");
        assert!(!report.exists());
    }

    #[test]
    fn precheck_out_rejects_missing_parent_directory_and_config_itself() {
        let dir = tempfile::tempdir().unwrap();
        let (_cd, cfg) = temp_config(&std::fs::read_to_string(PARTIAL).unwrap());
        let missing = dir.path().join("nope").join("r.json");
        let as_dir = dir.path().to_str().unwrap().to_string();
        let cases = [
            (missing.to_str().unwrap().to_string(), "parent directory"),
            (as_dir, "is a directory"),
            (cfg.clone(), "is the config file"),
        ];
        let before = std::fs::read_to_string(&cfg).unwrap();
        for (out_path, want) in cases {
            let (code, out, err) = check_only(&[
                "--config",
                &cfg,
                "--out",
                &out_path,
                "--only",
                "brute_force",
            ]);
            assert_eq!(code, 2, "{out_path}");
            assert!(err.contains(want), "{out_path}: {err}");
            assert!(!err.contains("not wired"), "{err}");
            assert_no_sentinel(&out, &err);
        }
        assert_eq!(std::fs::read_to_string(&cfg).unwrap(), before);
        assert!(!dir.path().join("nope").exists());
    }

    #[test]
    fn check_help_has_required_phrases() {
        let (code, out, _) = run_args(&["api-toolkit", "check", "--help"], &full_env);
        assert_eq!(code, 0);
        let flat = out.split_whitespace().collect::<Vec<_>>().join(" ");
        for want in [
            "--only",
            "required until P4",
            "Available: brute_force",
            "never exits 0",
            "exit 1 = FAIL",
            "2 = PASS or any other result",
            "130 = cancelled",
        ] {
            assert!(flat.contains(want), "missing `{want}` in {flat}");
        }
        let (code, out, _) = run_args(&["api-toolkit", "validate", "--help"], &full_env);
        assert_eq!(code, 0);
        assert!(out.contains("--only"), "{out}");
    }

    #[test]
    fn help_and_version_are_0() {
        let (code, out, _) = run_args(&["api-toolkit", "--help"], &full_env);
        assert_eq!(code, 0);
        assert!(out.contains("validate"));
        let (code, out, _) = run_args(&["api-toolkit", "--version"], &full_env);
        assert_eq!(code, 0);
        assert!(out.contains(env!("CARGO_PKG_VERSION")));
    }
}
