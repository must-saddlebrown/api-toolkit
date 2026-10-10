//! Command line interface: `validate` (P0) and `check` (the full suite, or one check
//! with `--only`).
//!
//! `run` is a plain function over injected args, env lookup and output streams so it
//! can be tested in-process. It returns the exit code; `main` only converts it.
//! `validate` never builds a transport, so it cannot send a request.

use std::ffi::OsString;
use std::future::Future;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Context;
use clap::{Parser, Subcommand};
use time::OffsetDateTime;
use tokio::sync::watch;

use crate::checks::{Stop, brute_force, load, race};
use crate::config::{self, BruteForceRequests, RaceRequests, Selection};
use crate::http::Transport;
use crate::http::request::ResolvedRequest;
use crate::report::{self, CheckName, CheckResult, Completion, ReasonCode, Report, Scope, Verdict};

/// Exit code for invalid config, usage errors and every `check` outcome that is not a completed FAIL (PLAN §10).
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
        /// Validate only this check's section (available: brute_force, race, load). Without it the
        /// full suite config is validated.
        #[arg(long)]
        only: Option<String>,
    },
    /// Run the full suite (race, load, brute_force, in that order). It exits 0 only when
    /// all three checks complete with PASS; exit 1 = a FAIL and nothing worse, 2 = any
    /// other result, 130 = cancelled. With --only a single check runs as a partial run,
    /// which never exits 0: exit 1 = FAIL, 2 = PASS or any other result, 130 = cancelled;
    /// read the verdict in the console line or the report.
    Check {
        #[arg(long)]
        config: PathBuf,
        #[arg(long)]
        out: PathBuf,
        /// Run only this check (partial run). Without it the full suite runs.
        /// Available: brute_force, race, load.
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
            Ok(prepared) => check_blocking(prepared, &out_path, out, err),
            Err(e) => {
                let _ = writeln!(err, "error: {e:#}");
                EXIT_INVALID
            }
        },
    }
}

/// Map the `--only` value to a check.
fn parse_only(name: &str) -> anyhow::Result<CheckName> {
    match name {
        "brute_force" => Ok(CheckName::BruteForce),
        "race" => Ok(CheckName::Race),
        "load" => Ok(CheckName::Load),
        _ => anyhow::bail!("unknown check `{name}`; available: brute_force, race, load"),
    }
}

/// One check a run executes, with its config section and resolved requests.
enum Selected {
    BruteForce {
        cfg: config::BruteForce,
        requests: BruteForceRequests,
    },
    Race {
        cfg: config::Race,
        requests: RaceRequests,
    },
    Load {
        cfg: config::Load,
        request: ResolvedRequest,
    },
}

/// Everything a `check` run needs, ready before the first request.
struct Prepared {
    limits: config::Limits,
    /// Execution order: race, load, brute_force for the suite; one element for `--only`.
    selected: Vec<Selected>,
    scope: Scope,
    config_hash: String,
}

/// Everything `check` can know before sending a request. Order matters (K7): `--only`
/// first, so a missing or bad value never reads the config or touches `--out`.
fn check_args(
    config: &Path,
    out: &Path,
    only: Option<&str>,
    env_lookup: &impl Fn(&str) -> Option<String>,
) -> anyhow::Result<Prepared> {
    let only = only.map(parse_only).transpose()?;
    let (sel, names, scope) = match only {
        Some(name) => (Selection::Only(name), vec![name], Scope::Partial),
        None => (
            Selection::All,
            vec![CheckName::Race, CheckName::Load, CheckName::BruteForce],
            Scope::Full,
        ),
    };
    let text = read_config(config)?;
    let cfg = config::parse(&text)?;
    config::validate(&cfg, sel)?; // validate before resolve
    let mut resolved = config::resolve(&cfg, sel, env_lookup)?;
    precheck_out(config, out)?;
    let config::Config {
        limits,
        mut brute_force,
        mut race,
        mut load,
        ..
    } = cfg;
    // `validate` already rejected a missing section; the `else` arms are a backstop.
    let mut selected = Vec::new();
    for name in names {
        selected.push(match name {
            CheckName::BruteForce => match (brute_force.take(), resolved.brute_force.take()) {
                (Some(cfg), Some(requests)) => Selected::BruteForce { cfg, requests },
                _ => anyhow::bail!(
                    "invalid config at `brute_force`: section is required for the selected check"
                ),
            },
            CheckName::Race => match (race.take(), resolved.race.take()) {
                (Some(cfg), Some(requests)) => Selected::Race { cfg, requests },
                _ => anyhow::bail!(
                    "invalid config at `race`: section is required for the selected check"
                ),
            },
            CheckName::Load => match (load.take(), resolved.load.take()) {
                (Some(cfg), Some(request)) => Selected::Load { cfg, request },
                _ => anyhow::bail!(
                    "invalid config at `load`: section is required for the selected check"
                ),
            },
        });
    }
    Ok(Prepared {
        limits,
        selected,
        scope,
        config_hash: report::config_hash(text.as_bytes()),
    })
}

/// Sync wrapper: build the runtime, register SIGINT before the first request, then run.
fn check_blocking(
    prepared: Prepared,
    out_path: &Path,
    out: &mut impl Write,
    err: &mut impl Write,
) -> u8 {
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            let _ = writeln!(err, "error: cannot start the async runtime ({})", e.kind());
            return EXIT_INVALID;
        }
    };
    // `signal` needs a runtime context, so enter the runtime for this one call.
    let mut sigint = {
        let _guard = runtime.enter();
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt()) {
            Ok(s) => s,
            Err(e) => {
                let _ = writeln!(err, "error: cannot register SIGINT ({})", e.kind());
                return EXIT_INVALID;
            }
        }
    };
    let cancel = async move {
        if sigint.recv().await.is_none() {
            std::future::pending::<()>().await; // signal stream closed: never cancel
        }
    };
    runtime.block_on(run_check(prepared, out_path, cancel, out, err))
}

/// The run itself. `cancel` is injected so tests need no real signal. Not `Send`-bound:
/// it is driven by `Runtime::block_on` on the calling thread.
async fn run_check(
    prepared: Prepared,
    out_path: &Path,
    cancel: impl Future<Output = ()>,
    out: &mut impl Write,
    err: &mut impl Write,
) -> u8 {
    let started = OffsetDateTime::now_utc();
    let deadline = tokio::time::sleep(Duration::from_millis(prepared.limits.suite_deadline_ms));
    let transport = match Transport::new(&prepared.limits) {
        Ok(t) => t,
        Err(e) => {
            let _ = writeln!(err, "error: cannot build the HTTP client: {e}");
            return EXIT_INVALID;
        }
    };

    let (stop_tx, stop_rx) = watch::channel(None);
    // One future for all selected checks, run one after another. Each gets its own clone
    // of the stop receiver, so a stop that is already set makes the later checks
    // `not_run` without sending a request. An earlier FAIL never skips a later check.
    let check = async {
        let mut results = Vec::new();
        for selected in &prepared.selected {
            let stop = stop_rx.clone();
            results.push(match selected {
                Selected::BruteForce { cfg, requests } => {
                    brute_force::run(&transport, cfg, &requests.baseline, &requests.probe, stop)
                        .await
                }
                Selected::Race { cfg, requests } => {
                    race::run(&transport, cfg, &requests.action, &requests.verify, stop).await
                }
                Selected::Load { cfg, request } => load::run(&transport, cfg, request, stop).await,
            });
        }
        results
    };
    tokio::pin!(check, deadline, cancel);
    let mut stop_sent: Option<Stop> = None;
    let results = loop {
        tokio::select! {
            biased;
            _ = &mut cancel, if stop_sent.is_none() => {
                stop_sent = Some(Stop::Cancelled);
                stop_tx.send_replace(stop_sent);
            }
            _ = &mut deadline, if stop_sent.is_none() => {
                stop_sent = Some(Stop::DeadlineExceeded);
                stop_tx.send_replace(stop_sent);
            }
            r = &mut check => break r,
        }
    };
    let finished = OffsetDateTime::now_utc();

    // Console first, so the results are on screen even if the report cannot be written.
    // ponytail: printed after the whole suite; print per check if a real suite feels slow.
    for result in &results {
        let line = match result.check {
            CheckName::Race => race::console_line(result),
            CheckName::Load => load::console_line(result),
            CheckName::BruteForce => brute_force::console_line(result),
        };
        let _ = writeln!(out, "{line}");
    }
    if prepared.scope == Scope::Partial {
        let selected: Vec<CheckName> = results.iter().map(|r| r.check).collect();
        let not_run: Vec<&str> = [CheckName::Race, CheckName::Load, CheckName::BruteForce]
            .into_iter()
            .filter(|c| !selected.contains(c))
            .map(CheckName::as_str)
            .collect();
        let _ = writeln!(
            err,
            "note: partial run (--only {}): {} not run; a partial run never exits 0, read the verdict above or in {}",
            selected
                .iter()
                .map(|c| c.as_str())
                .collect::<Vec<_>>()
                .join(", "),
            not_run.join(", "),
            out_path.display()
        );
    }

    let cancelled = results
        .iter()
        .any(|r| r.reasons.contains(&ReasonCode::Cancelled));
    let built = build_report(
        prepared.scope,
        started,
        finished,
        &prepared.config_hash,
        results,
    );
    let report = match built {
        Ok(r) => r,
        Err(e) => {
            let _ = writeln!(err, "error: {e}");
            let _ = writeln!(out, "report NOT WRITTEN: {}", out_path.display());
            return if cancelled { 130 } else { EXIT_INVALID };
        }
    };
    if let Err(e) = report::write_report_atomic(out_path, &report) {
        let _ = writeln!(err, "error: {e}");
        let _ = writeln!(out, "report NOT WRITTEN: {}", out_path.display());
        return if cancelled { 130 } else { EXIT_INVALID };
    }
    u8::try_from(report::exit_code(&report)).unwrap_or(EXIT_INVALID)
}

/// A check that `--only` left out (K10): not run, incomplete, no evidence.
fn excluded(check: CheckName) -> CheckResult {
    CheckResult {
        check,
        verdict: Verdict::NotRun,
        reasons: vec![ReasonCode::ExcludedByOnly],
        completion: Completion::Incomplete,
        evidence: None,
    }
}

/// Build the report. Full: the results as given (race, load, brute_force). Partial: entries
/// in the default order race, load, brute_force, the selected one replaced by its result and
/// the rest `excluded`. Timestamps are cut to whole seconds; suite completion is
/// `incomplete` when any executed check is.
fn build_report(
    scope: Scope,
    started: OffsetDateTime,
    finished: OffsetDateTime,
    config_hash: &str,
    results: Vec<CheckResult>,
) -> Result<Report, report::ReportError> {
    let unix_ms = u64::try_from(started.unix_timestamp_nanos() / 1_000_000).unwrap_or(0);
    let run_id = report::run_id(unix_ms, config_hash);
    let whole_second = |t: OffsetDateTime| t.replace_nanosecond(0).unwrap_or(t);
    let completion = if results
        .iter()
        .any(|r| r.completion == Completion::Incomplete)
    {
        Completion::Incomplete
    } else {
        Completion::Complete
    };
    let checks = match scope {
        Scope::Full => results,
        Scope::Partial => {
            let mut results = results;
            [CheckName::Race, CheckName::Load, CheckName::BruteForce]
                .into_iter()
                .map(|name| match results.iter().position(|r| r.check == name) {
                    Some(i) => results.swap_remove(i),
                    None => excluded(name),
                })
                .collect()
        }
    };
    Report::new(
        run_id,
        config_hash.to_string(),
        whole_second(started),
        whole_second(finished),
        completion,
        scope,
        checks,
    )
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
    fn validate_broken_example_ok_and_matches_healthy_env() {
        let broken = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/examples/core-checks-broken.json"
        );
        let (code, out, err) = validate(broken, &full_env);
        assert_eq!(code, 0, "stderr: {err}");
        assert!(out.contains("planned requests: 117"), "{out}");
        assert!(out.contains("fixed waits: 950 ms"), "{out}");
        for name in ENV_NAMES {
            assert!(out.contains(name), "missing {name} in {out}");
        }
        assert_no_sentinel(&out, &err);
        // Same env placeholders as the healthy example, and no literal secret values.
        let placeholders = |text: &str| -> Vec<String> {
            text.split("${ENV:")
                .skip(1)
                .map(|s| s[..s.find('}').unwrap()].to_string())
                .collect()
        };
        let text = std::fs::read_to_string(broken).unwrap();
        assert_eq!(placeholders(&text), placeholders(&example_text()));
        for literal in ["demo-pass", "wrong-pass", "fixture-token", "act-1"] {
            assert!(
                !text.contains(literal),
                "literal {literal} in broken example"
            );
        }
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
    fn check_full_with_missing_env_is_2_and_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let report = dir.path().join("report.json");
        let no_env = |_: &str| None;
        let (code, out, err) = run_args(
            &[
                "api-toolkit",
                "check",
                "--config",
                EXAMPLE,
                "--out",
                report.to_str().unwrap(),
            ],
            &no_env,
        );
        assert_eq!(code, 2);
        assert!(
            err.contains("LOGIN_USER") && err.contains("TEST_ACTION_ID"),
            "{err}"
        );
        assert!(out.is_empty(), "{out}");
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
    fn check_full_with_missing_section_leaves_existing_out_untouched() {
        // The brute_force-only example has no race section: the suite is rejected before
        // any request, and the old report stays as it was.
        let dir = tempfile::tempdir().unwrap();
        let report = dir.path().join("report.json");
        std::fs::write(&report, "KEEP-ME").unwrap();
        let (code, out, err) = run_args(
            &[
                "api-toolkit",
                "check",
                "--config",
                PARTIAL,
                "--out",
                report.to_str().unwrap(),
            ],
            &full_env,
        );
        assert_eq!(code, 2);
        assert!(err.contains("race"), "{err}");
        assert_eq!(std::fs::read_to_string(&report).unwrap(), "KEEP-ME");
        assert_no_sentinel(&out, &err);
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
    fn check_without_only_on_config_without_load_is_2_and_connects_nowhere() {
        // Valid base_url pointing at a live local listener: validation must fail first.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let mut v: Value = serde_json::from_str(&example_text()).unwrap();
        v["base_url"] = Value::from(format!("http://{}", listener.local_addr().unwrap()));
        v.as_object_mut().unwrap().remove("load");
        let (_d, cfg) = temp_config(&v.to_string());
        let dir = tempfile::tempdir().unwrap();
        let report = dir.path().join("r.json");
        std::fs::write(&report, "KEEP-ME").unwrap();
        let (code, out, err) = check_only(&["--config", &cfg, "--out", report.to_str().unwrap()]);
        assert_eq!(code, 2);
        assert!(err.contains("load"), "{err}");
        assert!(!err.contains("full suite available in P4"), "{err}");
        assert_eq!(std::fs::read_to_string(&report).unwrap(), "KEEP-ME");
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock,
            "a connection was made"
        );
        assert_no_sentinel(&out, &err);
    }

    #[test]
    fn only_rejects_unknown_names_in_check_and_validate() {
        let dir = tempfile::tempdir().unwrap();
        let report = dir.path().join("r.json");
        let rep = report.to_str().unwrap();
        let cases = [
            ("bogus", "unknown check"),
            ("Brute_Force", "unknown check"),
            ("", "unknown check"),
            ("loadx", "unknown check"),
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
        assert!(err.contains("available: brute_force, race, load"), "{err}");
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
    fn check_only_brute_force_valid_no_longer_says_not_wired() {
        // The real run is covered by the `run_check_*` tests below (local listener).
        // Here: a valid partial config with a closed port must run, not stop at the stub.
        let dir = tempfile::tempdir().unwrap();
        let report = dir.path().join("r.json");
        let mut v: Value =
            serde_json::from_str(&std::fs::read_to_string(PARTIAL).unwrap()).unwrap();
        v["base_url"] = Value::from(closed_port_url());
        v["brute_force"]["interval_ms"] = Value::from(1);
        let (_d, cfg) = temp_config(&v.to_string());
        let (code, out, err) = check_only(&[
            "--config",
            &cfg,
            "--out",
            report.to_str().unwrap(),
            "--only",
            "brute_force",
        ]);
        assert_eq!(code, 2, "{err}");
        assert!(!err.contains("not wired"), "{err}");
        assert!(out.starts_with("brute_force ERROR"), "{out}");
        assert!(report.exists());
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
            "Run the full suite (race, load, brute_force, in that order)",
            "exits 0 only when all three checks complete with PASS",
            "With --only a single check runs as a partial run",
            "Available: brute_force, race, load",
            "never exits 0",
            "exit 1 = FAIL",
            "exit 1 = a FAIL and nothing worse",
            "2 = PASS or any other result",
            "130 = cancelled",
        ] {
            assert!(flat.contains(want), "missing `{want}` in {flat}");
        }
        let (code, out, _) = run_args(&["api-toolkit", "validate", "--help"], &full_env);
        assert_eq!(code, 0);
        assert!(out.contains("--only"), "{out}");
        let flat = out.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(
            flat.contains("available: brute_force, race, load"),
            "{flat}"
        );
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

    // ---- real run (S4): local listener on 127.0.0.1 port 0, no real signal ----

    use std::net::SocketAddr;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    fn closed_port_url() -> String {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", l.local_addr().unwrap());
        drop(l);
        url
    }

    /// Reads one request (headers and body) so the client never sees a reset.
    async fn read_request(sock: &mut tokio::net::TcpStream) {
        let mut buf = Vec::new();
        let mut chunk = [0u8; 1024];
        loop {
            let n = sock.read(&mut chunk).await.unwrap_or(0);
            if n == 0 {
                return;
            }
            buf.extend_from_slice(&chunk[..n]);
            let Some(end) = buf.windows(4).position(|w| w == b"\r\n\r\n") else {
                continue;
            };
            let head = String::from_utf8_lossy(&buf[..end]).to_lowercase();
            let len = head
                .lines()
                .find_map(|l| l.strip_prefix("content-length:"))
                .and_then(|v| v.trim().parse::<usize>().ok())
                .unwrap_or(0);
            if buf.len() >= end + 4 + len {
                return;
            }
        }
    }

    /// Answers request number i (0 = baseline) with `status_of(i)`; counts requests.
    async fn serve(status_of: fn(usize) -> u16) -> (SocketAddr, Arc<AtomicUsize>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let count = Arc::new(AtomicUsize::new(0));
        let seen = count.clone();
        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else {
                    return;
                };
                let i = seen.fetch_add(1, Ordering::SeqCst);
                read_request(&mut sock).await;
                let status = status_of(i);
                let body = r#"{"access_token":"t"}"#;
                let reply = format!(
                    "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = sock.write_all(reply.as_bytes()).await;
            }
        });
        (addr, count)
    }

    /// Accepts connections and never answers; counts them.
    async fn hang() -> (SocketAddr, Arc<AtomicUsize>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let count = Arc::new(AtomicUsize::new(0));
        let seen = count.clone();
        tokio::spawn(async move {
            let mut held = Vec::new();
            while let Ok((sock, _)) = listener.accept().await {
                seen.fetch_add(1, Ordering::SeqCst);
                held.push(sock);
            }
        });
        (addr, count)
    }

    /// Healthy login: baseline 200, failed logins 1..=5 -> 401, 6.. -> 429.
    fn healthy(i: usize) -> u16 {
        match i {
            0 => 200,
            1..=5 => 401,
            _ => 429,
        }
    }
    fn unprotected(i: usize) -> u16 {
        if i == 0 { 200 } else { 401 }
    }
    fn accepts_invalid(_: usize) -> u16 {
        200
    }

    struct RunOut {
        code: u8,
        out: String,
        err: String,
        report_path: PathBuf,
        _dir: tempfile::TempDir,
    }

    impl RunOut {
        fn report(&self) -> Value {
            serde_json::from_str(&std::fs::read_to_string(&self.report_path).unwrap()).unwrap()
        }
        fn report_text(&self) -> String {
            std::fs::read_to_string(&self.report_path).unwrap_or_default()
        }
        fn bf(&self) -> Value {
            self.report()["checks"][2].clone()
        }
    }

    /// Prepare through `check_args` (so the whole path is real), then `run_check`.
    async fn run_it(
        base_url: &str,
        tweak: impl FnOnce(&mut Value),
        cancel: impl Future<Output = ()>,
    ) -> RunOut {
        let mut v: Value =
            serde_json::from_str(&std::fs::read_to_string(PARTIAL).unwrap()).unwrap();
        v["base_url"] = Value::from(base_url);
        v["brute_force"]["interval_ms"] = Value::from(1);
        tweak(&mut v);
        let (dir, cfg) = temp_config(&v.to_string());
        let report_path = Path::new(&cfg).with_file_name("report.json");
        let prepared = check_args(
            Path::new(&cfg),
            &report_path,
            Some("brute_force"),
            &full_env,
        )
        .unwrap_or_else(|e| panic!("prepare failed: {e:#}"));
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = run_check(prepared, &report_path, cancel, &mut out, &mut err).await;
        RunOut {
            code,
            out: String::from_utf8(out).unwrap(),
            err: String::from_utf8(err).unwrap(),
            report_path,
            _dir: dir,
        }
    }

    #[tokio::test]
    async fn run_check_healthy_is_2_with_pass_report() {
        let (addr, count) = serve(healthy).await;
        let r = run_it(&format!("http://{addr}"), |_| {}, std::future::pending()).await;
        assert_eq!(r.code, 2, "{}", r.err);
        assert_eq!(
            r.out,
            "brute_force PASS first_protected=6 tail=3/2 attempts=8/8 reasons=protection_observed\n"
        );
        assert!(
            r.err.contains("partial run (--only brute_force)"),
            "{}",
            r.err
        );
        assert_eq!(count.load(Ordering::SeqCst), 9);
        let rep = r.report();
        assert_eq!(rep["report_version"], 1);
        assert_eq!(rep["scope"], "partial");
        assert_eq!(rep["completion"], "complete");
        assert_eq!(rep["checks"][2]["verdict"], "pass");
        assert_eq!(rep["checks"][2]["completion"], "complete");
        assert!(rep["checks"][2]["evidence"].is_object());
        assert_no_sentinel(&r.out, &r.err);
        assert!(!r.report_text().contains(SENTINEL));
    }

    #[tokio::test]
    async fn run_check_report_shape_order_and_ids() {
        let (addr, _) = serve(healthy).await;
        let r = run_it(&format!("http://{addr}"), |_| {}, std::future::pending()).await;
        let rep = r.report();
        let names: Vec<&str> = rep["checks"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["check"].as_str().unwrap())
            .collect();
        assert_eq!(names, ["race", "load", "brute_force"]);
        for i in 0..2 {
            let c = rep["checks"][i].as_object().unwrap();
            assert_eq!(c["verdict"], "not_run");
            assert_eq!(c["reasons"], serde_json::json!(["excluded_by_only"]));
            assert_eq!(c["completion"], "incomplete");
            assert!(!c.contains_key("evidence"));
        }
        // config_hash = SHA-256 of the config file bytes; run_id = <unix_ms>-<8 hex>.
        let cfg_bytes = std::fs::read(r.report_path.with_file_name("c.json")).unwrap();
        let hash = report::config_hash(&cfg_bytes);
        assert_eq!(rep["config_hash"], hash.as_str());
        let id = rep["run_id"].as_str().unwrap();
        let (ms, short) = id.split_once('-').unwrap();
        assert!(ms.parse::<u64>().is_ok(), "{id}");
        assert_eq!(short, &hash[..8]);
        // Timestamps: RFC3339 UTC, whole seconds.
        for key in ["started_at", "finished_at"] {
            let t = rep[key].as_str().unwrap();
            assert!(t.ends_with('Z') && !t.contains('.'), "{key}: {t}");
        }
        // No temp file left behind.
        let leftovers: Vec<_> = std::fs::read_dir(r.report_path.parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains(".tmp-"))
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
    }

    #[tokio::test]
    async fn run_check_fail_verdicts_are_1() {
        for status_of in [unprotected as fn(usize) -> u16, accepts_invalid] {
            let (addr, _) = serve(status_of).await;
            let r = run_it(&format!("http://{addr}"), |_| {}, std::future::pending()).await;
            assert_eq!(r.code, 1, "{} / {}", r.out, r.err);
            assert!(r.out.starts_with("brute_force FAIL"), "{}", r.out);
            assert_eq!(r.bf()["verdict"], "fail");
            assert_eq!(r.report()["completion"], "complete");
        }
    }

    #[tokio::test]
    async fn run_check_prelocked_is_inconclusive_2() {
        let (addr, count) = serve(|_| 429).await;
        let r = run_it(&format!("http://{addr}"), |_| {}, std::future::pending()).await;
        assert_eq!(r.code, 2);
        assert_eq!(r.bf()["verdict"], "inconclusive");
        assert_eq!(count.load(Ordering::SeqCst), 1); // baseline only, no retry
    }

    #[tokio::test]
    async fn run_check_closed_port_is_error_2() {
        let r = run_it(&closed_port_url(), |_| {}, std::future::pending()).await;
        assert_eq!(r.code, 2, "{}", r.err);
        assert!(r.out.starts_with("brute_force ERROR"), "{}", r.out);
        assert_eq!(r.bf()["verdict"], "error");
        assert_no_sentinel(&r.out, &r.err);
        assert!(!r.report_text().contains(SENTINEL));
        assert!(!r.err.contains("127.0.0.1"), "url leaked: {}", r.err);
    }

    #[tokio::test]
    async fn run_check_cancel_before_dispatch_is_130_with_report() {
        let (addr, count) = serve(healthy).await;
        let r = run_it(&format!("http://{addr}"), |_| {}, std::future::ready(())).await;
        assert_eq!(r.code, 130, "{}", r.err);
        assert_eq!(count.load(Ordering::SeqCst), 0);
        let rep = r.report();
        assert_eq!(rep["scope"], "partial");
        assert_eq!(rep["completion"], "incomplete");
        let bf = &rep["checks"][2];
        assert_eq!(bf["verdict"], "not_run");
        assert_eq!(bf["reasons"], serde_json::json!(["cancelled"]));
        assert_eq!(bf["completion"], "incomplete");
        assert!(r.out.starts_with("brute_force NOT_RUN"), "{}", r.out);
        assert!(r.out.contains("reasons=cancelled"), "{}", r.out);
    }

    #[tokio::test]
    async fn run_check_cancel_while_request_hangs_is_130() {
        let (addr, count) = hang().await;
        // Cancel only once the server has accepted the first connection: an observed
        // condition, not a guessed delay, so the request is certainly in flight.
        let seen = count.clone();
        let cancel = async move {
            while seen.load(Ordering::SeqCst) == 0 {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        };
        let r = run_it(&format!("http://{addr}"), |_| {}, cancel).await;
        assert_eq!(r.code, 130, "{}", r.err);
        assert_eq!(r.bf()["verdict"], "not_run");
        assert_eq!(r.bf()["reasons"], serde_json::json!(["cancelled"]));
        assert_eq!(r.report()["completion"], "incomplete");
        assert_eq!(count.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn run_check_deadline_is_2_not_run_deadline_exceeded() {
        let (addr, count) = hang().await;
        let r = run_it(
            &format!("http://{addr}"),
            |v| v["limits"]["suite_deadline_ms"] = Value::from(200),
            std::future::pending(),
        )
        .await;
        assert_eq!(r.code, 2, "{}", r.err);
        assert_eq!(r.bf()["verdict"], "not_run");
        assert_eq!(r.bf()["reasons"], serde_json::json!(["deadline_exceeded"]));
        assert_eq!(r.report()["completion"], "incomplete");
        // `<=`: under heavy load the deadline can fire before the first connection is
        // accepted. The server never replies, so at most one request can ever be sent.
        assert!(count.load(Ordering::SeqCst) <= 1);
    }

    #[tokio::test]
    async fn run_check_overwrites_existing_report() {
        let (addr, _) = serve(healthy).await;
        let mut v: Value =
            serde_json::from_str(&std::fs::read_to_string(PARTIAL).unwrap()).unwrap();
        v["base_url"] = Value::from(format!("http://{addr}"));
        v["brute_force"]["interval_ms"] = Value::from(1);
        let (dir, cfg) = temp_config(&v.to_string());
        let out_path = dir.path().join("report.json");
        std::fs::write(&out_path, "OLD").unwrap();
        let (code, _, err) = {
            let prepared =
                check_args(Path::new(&cfg), &out_path, Some("brute_force"), &full_env).unwrap();
            let (mut o, mut e) = (Vec::new(), Vec::new());
            let code = run_check(prepared, &out_path, std::future::pending(), &mut o, &mut e).await;
            (code, o, String::from_utf8(e).unwrap())
        };
        assert_eq!(code, 2, "{err}");
        assert!(
            std::fs::read_to_string(&out_path)
                .unwrap()
                .contains("\"scope\"")
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn run_check_report_write_failure_keeps_console_and_old_file() {
        use std::os::unix::fs::PermissionsExt;
        let (addr, _) = serve(healthy).await;
        let mut v: Value =
            serde_json::from_str(&std::fs::read_to_string(PARTIAL).unwrap()).unwrap();
        v["base_url"] = Value::from(format!("http://{addr}"));
        v["brute_force"]["interval_ms"] = Value::from(1);
        let cfg_dir = tempfile::tempdir().unwrap();
        let cfg = cfg_dir.path().join("c.json");
        std::fs::write(&cfg, v.to_string()).unwrap();
        let out_dir = tempfile::tempdir().unwrap();
        let out_path = out_dir.path().join("report.json");
        std::fs::write(&out_path, "OLD").unwrap();
        // root ignores directory permissions; skip in that case
        std::fs::set_permissions(out_dir.path(), std::fs::Permissions::from_mode(0o555)).unwrap();
        if std::fs::File::create(out_dir.path().join("probe")).is_ok() {
            std::fs::set_permissions(out_dir.path(), std::fs::Permissions::from_mode(0o755))
                .unwrap();
            return;
        }
        let prepared = check_args(&cfg, &out_path, Some("brute_force"), &full_env).unwrap();
        let (mut o, mut e) = (Vec::new(), Vec::new());
        let code = run_check(prepared, &out_path, std::future::pending(), &mut o, &mut e).await;
        std::fs::set_permissions(out_dir.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
        let (o, e) = (String::from_utf8(o).unwrap(), String::from_utf8(e).unwrap());
        assert_eq!(code, 2, "{e}");
        assert!(o.starts_with("brute_force PASS"), "{o}");
        assert_eq!(
            o.lines().last().unwrap(),
            format!("report NOT WRITTEN: {}", out_path.display())
        );
        assert!(e.contains("cannot write report"), "{e}");
        assert!(e.contains(out_path.to_str().unwrap()), "{e}");
        assert_eq!(std::fs::read_to_string(&out_path).unwrap(), "OLD");
        assert_eq!(std::fs::read_dir(out_dir.path()).unwrap().count(), 1);
        assert_no_sentinel(&o, &e);
    }

    #[test]
    fn build_report_partial_whole_seconds_and_excluded_shape() {
        let t = OffsetDateTime::from_unix_timestamp_nanos(1_700_000_000_123_456_789).unwrap();
        let bf = CheckResult {
            check: CheckName::BruteForce,
            verdict: Verdict::NotRun,
            reasons: vec![ReasonCode::Cancelled],
            completion: Completion::Incomplete,
            evidence: None,
        };
        let r = build_report(Scope::Partial, t, t, "abcdef0123456789", vec![bf]).unwrap();
        assert_eq!(r.started_at, "2023-11-14T22:13:20Z");
        assert_eq!(r.run_id, "1700000000123-abcdef01");
        assert_eq!(r.completion, Completion::Incomplete);
        assert_eq!(r.scope, Scope::Partial);
        assert_eq!(report::exit_code(&r), 130);
        assert_eq!(r.checks[0], excluded(CheckName::Race));
        assert_eq!(r.checks[1], excluded(CheckName::Load));
    }

    // ---- `--only race` (P2.S7) ----

    const RACE_EXAMPLE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/examples/race-only.json");

    #[test]
    fn parse_only_known_names_ok_and_unknown_lists_all() {
        assert_eq!(parse_only("race").unwrap(), CheckName::Race);
        assert_eq!(parse_only("brute_force").unwrap(), CheckName::BruteForce);
        assert_eq!(parse_only("load").unwrap(), CheckName::Load);
        let e = parse_only("Race").unwrap_err().to_string();
        assert!(
            e.contains("unknown check") && e.contains("brute_force, race, load"),
            "{e}"
        );
    }

    #[test]
    fn build_report_partial_race_order_and_excluded_entries() {
        let t = OffsetDateTime::from_unix_timestamp(1_700_000_000).unwrap();
        let race = CheckResult {
            check: CheckName::Race,
            verdict: Verdict::NotRun,
            reasons: vec![ReasonCode::Cancelled],
            completion: Completion::Incomplete,
            evidence: None,
        };
        let r = build_report(Scope::Partial, t, t, "abcdef0123456789", vec![race.clone()]).unwrap();
        assert_eq!(r.scope, Scope::Partial);
        assert_eq!(r.completion, Completion::Incomplete);
        assert_eq!(r.checks.len(), 3);
        assert_eq!(r.checks[0], race);
        assert_eq!(r.checks[1], excluded(CheckName::Load));
        assert_eq!(r.checks[2], excluded(CheckName::BruteForce));
        assert_eq!(report::exit_code(&r), 130);
    }

    #[test]
    fn validate_only_race_example_needs_no_brute_force_env() {
        let env = |n: &str| n.starts_with("TEST_").then(|| SENTINEL.to_string());
        let (code, out, err) = run_args(
            &[
                "api-toolkit",
                "validate",
                "--config",
                RACE_EXAMPLE,
                "--only",
                "race",
            ],
            &env,
        );
        assert_eq!(code, 0, "{err}");
        assert!(out.contains("planned requests: 7"), "{out}");
        assert!(out.contains("fixed waits: 250 ms"), "{out}");
        assert!(
            out.contains("environment variables present: TEST_ACTION_ID, TEST_AUTHORIZATION"),
            "{out}"
        );
        assert!(!out.contains("LOGIN_"), "{out}");
        assert_no_sentinel(&out, &err);
    }

    #[test]
    fn race_example_has_no_literal_secret() {
        let text = std::fs::read_to_string(RACE_EXAMPLE).unwrap();
        assert!(text.contains("${ENV:TEST_AUTHORIZATION}"));
        assert!(!text.contains("Bearer"), "literal credential in example");
    }

    #[test]
    fn only_race_without_race_section_is_2_and_sends_nothing() {
        // The brute_force example has no `race` section.
        let dir = tempfile::tempdir().unwrap();
        let report = dir.path().join("r.json");
        let rep = report.to_str().unwrap();
        let (code, out, err) = check_only(&["--config", PARTIAL, "--out", rep, "--only", "race"]);
        assert_eq!(code, 2);
        assert!(err.contains("invalid config at `race`"), "{err}");
        assert!(!report.exists());
        assert_no_sentinel(&out, &err);
        let (code, out, err) = run_args(
            &[
                "api-toolkit",
                "validate",
                "--config",
                PARTIAL,
                "--only",
                "race",
            ],
            &full_env,
        );
        assert_eq!(code, 2);
        assert!(err.contains("invalid config at `race`"), "{err}");
        assert!(out.is_empty(), "{out}");
    }

    /// Prepare `check --only race` through `check_args`, then `run_check`.
    async fn run_race(
        base_url: &str,
        tweak: impl FnOnce(&mut Value),
        cancel: impl Future<Output = ()>,
    ) -> RunOut {
        let mut v: Value =
            serde_json::from_str(&std::fs::read_to_string(RACE_EXAMPLE).unwrap()).unwrap();
        v["base_url"] = Value::from(base_url);
        v["race"]["settle_ms"] = Value::from(1);
        tweak(&mut v);
        let (dir, cfg) = temp_config(&v.to_string());
        let report_path = Path::new(&cfg).with_file_name("report.json");
        let prepared = check_args(Path::new(&cfg), &report_path, Some("race"), &full_env)
            .unwrap_or_else(|e| panic!("prepare failed: {e:#}"));
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = run_check(prepared, &report_path, cancel, &mut out, &mut err).await;
        RunOut {
            code,
            out: String::from_utf8(out).unwrap(),
            err: String::from_utf8(err).unwrap(),
            report_path,
            _dir: dir,
        }
    }

    #[tokio::test]
    async fn run_check_race_cancel_while_actions_in_flight_is_130() {
        // Verifier GETs are answered; action POSTs hang. Cancel once all 5 actions are
        // observed in flight (a condition, not a delay).
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let posts = Arc::new(AtomicUsize::new(0));
        let total = Arc::new(AtomicUsize::new(0));
        let (p, t) = (posts.clone(), total.clone());
        tokio::spawn(async move {
            let mut held = Vec::new();
            while let Ok((mut sock, _)) = listener.accept().await {
                t.fetch_add(1, Ordering::SeqCst);
                let p = p.clone();
                // Peek at the request line to tell verifier from action.
                let mut buf = [0u8; 8];
                let n = sock.peek(&mut buf).await.unwrap_or(0);
                if buf[..n].starts_with(b"POST") {
                    p.fetch_add(1, Ordering::SeqCst);
                    held.push(sock);
                } else {
                    read_request(&mut sock).await;
                    let body = r#"{"execution_count":0}"#;
                    let reply = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = sock.write_all(reply.as_bytes()).await;
                }
            }
        });
        let seen = posts.clone();
        let cancel = async move {
            while seen.load(Ordering::SeqCst) < 5 {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        };
        let r = run_race(&format!("http://{addr}"), |_| {}, cancel).await;
        assert_eq!(r.code, 130, "{}", r.err);
        assert!(r.out.starts_with("race NOT_RUN"), "{}", r.out);
        assert_eq!(r.out.lines().count(), 1, "{}", r.out);
        assert!(
            r.err
                .contains("partial run (--only race): load, brute_force not run"),
            "{}",
            r.err
        );
        let rep = r.report();
        assert_eq!(rep["scope"], "partial");
        assert_eq!(rep["completion"], "incomplete");
        assert_eq!(rep["checks"][0]["check"], "race");
        assert_eq!(rep["checks"][0]["verdict"], "not_run");
        assert_eq!(
            rep["checks"][0]["reasons"],
            serde_json::json!(["cancelled"])
        );
        assert_eq!(rep["checks"][0]["completion"], "incomplete");
        for i in 1..3 {
            assert_eq!(
                rep["checks"][i]["reasons"],
                serde_json::json!(["excluded_by_only"])
            );
        }
        assert_eq!(total.load(Ordering::SeqCst), 6); // 1 verifier before + 5 actions
        assert_no_sentinel(&r.out, &r.err);
        assert!(!r.report_text().contains(SENTINEL));
    }

    #[tokio::test]
    async fn run_check_race_deadline_is_2_not_run_deadline_exceeded() {
        let (addr, count) = hang().await;
        let r = run_race(
            &format!("http://{addr}"),
            |v| v["limits"]["suite_deadline_ms"] = Value::from(200),
            std::future::pending(),
        )
        .await;
        assert_eq!(r.code, 2, "{}", r.err);
        assert!(r.out.starts_with("race NOT_RUN"), "{}", r.out);
        let rep = r.report();
        assert_eq!(
            rep["checks"][0]["reasons"],
            serde_json::json!(["deadline_exceeded"])
        );
        assert_eq!(rep["completion"], "incomplete");
        // `<=`: under heavy load the deadline can fire before the verifier-before connection
        // is accepted. The server never replies, so no action can be sent in either case.
        assert!(count.load(Ordering::SeqCst) <= 1);
    }

    #[tokio::test]
    async fn run_check_race_cancel_before_dispatch_sends_nothing() {
        let (addr, count) = hang().await;
        let r = run_race(&format!("http://{addr}"), |_| {}, std::future::ready(())).await;
        assert_eq!(r.code, 130, "{}", r.err);
        assert_eq!(count.load(Ordering::SeqCst), 0);
        assert_eq!(
            r.report()["checks"][0]["reasons"],
            serde_json::json!(["cancelled"])
        );
    }

    // ---- `--only load` (P3.S7) ----

    const LOAD_EXAMPLE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/examples/load-only.json");

    #[test]
    fn build_report_partial_load_order_and_excluded_entries() {
        let t = OffsetDateTime::from_unix_timestamp(1_700_000_000).unwrap();
        let load = CheckResult {
            check: CheckName::Load,
            verdict: Verdict::NotRun,
            reasons: vec![ReasonCode::Cancelled, ReasonCode::WorkloadIncomplete],
            completion: Completion::Incomplete,
            evidence: None,
        };
        let r = build_report(Scope::Partial, t, t, "abcdef0123456789", vec![load.clone()]).unwrap();
        assert_eq!(r.scope, Scope::Partial);
        assert_eq!(r.checks.len(), 3);
        assert_eq!(r.checks[0], excluded(CheckName::Race));
        assert_eq!(r.checks[1], load);
        assert_eq!(r.checks[2], excluded(CheckName::BruteForce));
        assert_eq!(report::exit_code(&r), 130);
    }

    #[test]
    fn validate_only_load_example_needs_only_test_authorization() {
        // Only TEST_AUTHORIZATION is set: brute_force and race env must not be required.
        let env = |n: &str| (n == "TEST_AUTHORIZATION").then(|| SENTINEL.to_string());
        let (code, out, err) = run_args(
            &[
                "api-toolkit",
                "validate",
                "--config",
                LOAD_EXAMPLE,
                "--only",
                "load",
            ],
            &env,
        );
        assert_eq!(code, 0, "{err}");
        assert!(out.contains("planned requests: 101"), "{out}");
        assert!(out.contains("fixed waits: 0 ms"), "{out}");
        assert!(
            out.contains("environment variables present: TEST_AUTHORIZATION"),
            "{out}"
        );
        assert!(
            !out.contains("LOGIN_") && !out.contains("TEST_ACTION_ID"),
            "{out}"
        );
        assert_no_sentinel(&out, &err);
    }

    #[test]
    fn validate_only_load_on_full_example_resolves_only_load_env() {
        // EXAMPLE has all three sections; with only TEST_AUTHORIZATION set, resolving
        // brute_force/race env would fail.
        let env = |n: &str| (n == "TEST_AUTHORIZATION").then(|| SENTINEL.to_string());
        let (code, out, err) = run_args(
            &[
                "api-toolkit",
                "validate",
                "--config",
                EXAMPLE,
                "--only",
                "load",
            ],
            &env,
        );
        assert_eq!(code, 0, "{err}");
        assert!(out.contains("planned requests: 101"), "{out}");
        assert!(out.contains("fixed waits: 0 ms"), "{out}");
        assert!(
            out.contains("environment variables present: TEST_AUTHORIZATION\n"),
            "{out}"
        );
        assert_no_sentinel(&out, &err);
    }

    #[test]
    fn load_example_has_no_literal_secret() {
        let text = std::fs::read_to_string(LOAD_EXAMPLE).unwrap();
        assert!(text.contains("${ENV:TEST_AUTHORIZATION}"));
        assert!(!text.contains("Bearer"), "literal credential in example");
    }

    #[test]
    fn only_load_without_load_section_is_2_and_sends_nothing() {
        // The brute_force example has no `load` section.
        let dir = tempfile::tempdir().unwrap();
        let report = dir.path().join("r.json");
        let rep = report.to_str().unwrap();
        let (code, out, err) = check_only(&["--config", PARTIAL, "--out", rep, "--only", "load"]);
        assert_eq!(code, 2);
        assert!(err.contains("invalid config at `load`"), "{err}");
        assert!(!report.exists());
        assert_no_sentinel(&out, &err);
        let (code, out, err) = run_args(
            &[
                "api-toolkit",
                "validate",
                "--config",
                PARTIAL,
                "--only",
                "load",
            ],
            &full_env,
        );
        assert_eq!(code, 2);
        assert!(err.contains("invalid config at `load`"), "{err}");
        assert!(out.is_empty(), "{out}");
    }

    /// Prepare `check --only load` through `check_args`, then `run_check`.
    async fn run_load(
        base_url: &str,
        cancel: impl Future<Output = ()>,
        deadline_ms: u64,
    ) -> RunOut {
        let mut v: Value =
            serde_json::from_str(&std::fs::read_to_string(LOAD_EXAMPLE).unwrap()).unwrap();
        v["base_url"] = Value::from(base_url);
        v["limits"]["suite_deadline_ms"] = Value::from(deadline_ms);
        let (dir, cfg) = temp_config(&v.to_string());
        let report_path = Path::new(&cfg).with_file_name("report.json");
        let prepared = check_args(Path::new(&cfg), &report_path, Some("load"), &full_env)
            .unwrap_or_else(|e| panic!("prepare failed: {e:#}"));
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = run_check(prepared, &report_path, cancel, &mut out, &mut err).await;
        RunOut {
            code,
            out: String::from_utf8(out).unwrap(),
            err: String::from_utf8(err).unwrap(),
            report_path,
            _dir: dir,
        }
    }

    /// Answers the first connection (the preflight) with 200 `{"ok": true}`; every later
    /// connection is held open without a reply. Returns the address and a connection count.
    async fn preflight_ok_then_hang() -> (SocketAddr, Arc<AtomicUsize>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let count = Arc::new(AtomicUsize::new(0));
        let seen = count.clone();
        tokio::spawn(async move {
            let mut held = Vec::new();
            while let Ok((mut sock, _)) = listener.accept().await {
                if seen.fetch_add(1, Ordering::SeqCst) == 0 {
                    read_request(&mut sock).await;
                    let body = r#"{"ok":true}"#;
                    let reply = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = sock.write_all(reply.as_bytes()).await;
                } else {
                    held.push(sock);
                }
            }
        });
        (addr, count)
    }

    #[tokio::test]
    async fn run_check_load_cancel_while_requests_in_flight_is_130() {
        let (addr, count) = preflight_ok_then_hang().await;
        // Cancel once a load connection (the 2nd) is observed: a condition, not a delay.
        let seen = count.clone();
        let cancel = async move {
            while seen.load(Ordering::SeqCst) < 2 {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        };
        let r = run_load(&format!("http://{addr}"), cancel, 120_000).await;
        assert_eq!(r.code, 130, "{}", r.err);
        assert!(r.out.starts_with("load NOT_RUN"), "{}", r.out);
        assert_eq!(r.out.lines().count(), 1, "{}", r.out);
        assert!(
            r.err
                .contains("partial run (--only load): race, brute_force not run"),
            "{}",
            r.err
        );
        let rep = r.report();
        assert_eq!(rep["scope"], "partial");
        assert_eq!(rep["completion"], "incomplete");
        let names: Vec<&str> = rep["checks"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["check"].as_str().unwrap())
            .collect();
        assert_eq!(names, ["race", "load", "brute_force"]);
        assert_eq!(rep["checks"][1]["verdict"], "not_run");
        assert_eq!(
            rep["checks"][1]["reasons"],
            serde_json::json!(["cancelled", "workload_incomplete"])
        );
        assert_eq!(rep["checks"][1]["completion"], "incomplete");
        for i in [0, 2] {
            assert_eq!(
                rep["checks"][i]["reasons"],
                serde_json::json!(["excluded_by_only"])
            );
        }
        assert_no_sentinel(&r.out, &r.err);
        assert!(!r.report_text().contains(SENTINEL));
    }

    #[tokio::test]
    async fn run_check_load_completed_pass_is_2_with_console_line_and_report() {
        let (addr, count) = serve(|_| 200).await;
        let r = run_load(&format!("http://{addr}"), std::future::pending(), 120_000).await;
        assert_eq!(r.code, 2, "{}", r.err);
        assert_eq!(r.out.lines().count(), 1, "{}", r.out);
        assert!(
            r.out.starts_with("load PASS error_rate=0/0.01"),
            "{}",
            r.out
        );
        assert!(
            r.out.contains("samples=100 max_in_flight=10/10"),
            "{}",
            r.out
        );
        assert_eq!(count.load(Ordering::SeqCst), 101);
        let rep = r.report();
        assert_eq!(rep["checks"][1]["check"], "load");
        assert_eq!(rep["checks"][1]["verdict"], "pass");
        assert_eq!(
            rep["checks"][1]["reasons"],
            serde_json::json!(["thresholds_met"])
        );
        assert!(!rep["checks"][1]["evidence"].is_null());
        assert_no_sentinel(&r.out, &r.err);
        assert!(!r.report_text().contains(SENTINEL));
    }

    #[tokio::test]
    async fn run_check_load_deadline_is_2_not_run() {
        let (addr, _count) = preflight_ok_then_hang().await;
        let r = run_load(&format!("http://{addr}"), std::future::pending(), 300).await;
        assert_eq!(r.code, 2, "{}", r.err);
        assert!(r.out.starts_with("load NOT_RUN"), "{}", r.out);
        let rep = r.report();
        assert_eq!(rep["checks"][1]["verdict"], "not_run");
        // Under heavy load the deadline can fire before the preflight is answered; then
        // `workload_incomplete` is absent. Either way the first reason is the deadline.
        assert_eq!(rep["checks"][1]["reasons"][0], "deadline_exceeded");
        assert_eq!(rep["completion"], "incomplete");
    }

    #[tokio::test]
    async fn run_check_load_cancel_before_dispatch_sends_nothing() {
        let (addr, count) = hang().await;
        let r = run_load(&format!("http://{addr}"), std::future::ready(()), 120_000).await;
        assert_eq!(r.code, 130, "{}", r.err);
        assert_eq!(count.load(Ordering::SeqCst), 0);
        assert_eq!(
            r.report()["checks"][1]["reasons"],
            serde_json::json!(["cancelled"])
        );
    }

    // ---- full suite (P4.S1): local listeners and closed ports only ----

    /// Prepare the full suite (no `--only`) through `check_args`, then `run_check`.
    async fn run_full(
        base_url: &str,
        tweak: impl FnOnce(&mut Value),
        cancel: impl Future<Output = ()>,
    ) -> RunOut {
        let mut v: Value = serde_json::from_str(&example_text()).unwrap();
        v["base_url"] = Value::from(base_url);
        v["brute_force"]["interval_ms"] = Value::from(1);
        v["race"]["settle_ms"] = Value::from(1);
        tweak(&mut v);
        let (dir, cfg) = temp_config(&v.to_string());
        let report_path = Path::new(&cfg).with_file_name("report.json");
        let prepared = check_args(Path::new(&cfg), &report_path, None, &full_env)
            .unwrap_or_else(|e| panic!("prepare failed: {e:#}"));
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = run_check(prepared, &report_path, cancel, &mut out, &mut err).await;
        RunOut {
            code,
            out: String::from_utf8(out).unwrap(),
            err: String::from_utf8(err).unwrap(),
            report_path,
            _dir: dir,
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn run_check_full_report_write_failure_is_2_with_marker_and_old_file() {
        use std::os::unix::fs::PermissionsExt;
        let cfg_dir = tempfile::tempdir().unwrap();
        let out_dir = tempfile::tempdir().unwrap();
        let out_path = out_dir.path().join("report.json");
        std::fs::write(&out_path, "OLD").unwrap();
        let mut v: Value = serde_json::from_str(&example_text()).unwrap();
        v["base_url"] = Value::from(closed_port_url());
        v["brute_force"]["interval_ms"] = Value::from(1);
        v["race"]["settle_ms"] = Value::from(1);
        let cfg = cfg_dir.path().join("c.json");
        std::fs::write(&cfg, v.to_string()).unwrap();
        // prepare while writable, then lock the directory; root ignores permissions: skip
        let prepared = check_args(&cfg, &out_path, None, &full_env).unwrap();
        let dir_mode =
            |m| std::fs::set_permissions(out_dir.path(), std::fs::Permissions::from_mode(m));
        dir_mode(0o555).unwrap();
        if std::fs::File::create(out_dir.path().join("probe")).is_ok() {
            dir_mode(0o755).unwrap();
            return;
        }
        let (mut o, mut e) = (Vec::new(), Vec::new());
        let code = run_check(prepared, &out_path, std::future::pending(), &mut o, &mut e).await;
        dir_mode(0o755).unwrap();
        let (o, e) = (String::from_utf8(o).unwrap(), String::from_utf8(e).unwrap());
        assert_eq!(code, 2, "{e}");
        let lines: Vec<&str> = o.lines().collect();
        assert_eq!(lines.len(), 4, "{o}");
        assert!(lines[0].starts_with("race "), "{o}");
        assert!(lines[1].starts_with("load "), "{o}");
        assert!(lines[2].starts_with("brute_force "), "{o}");
        assert_eq!(
            lines[3],
            format!("report NOT WRITTEN: {}", out_path.display())
        );
        assert!(e.contains("cannot write report"), "{e}");
        assert_eq!(std::fs::read_to_string(&out_path).unwrap(), "OLD");
        assert_eq!(std::fs::read_dir(out_dir.path()).unwrap().count(), 1);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn run_check_full_cancel_and_report_write_failure_is_130_with_marker() {
        use std::os::unix::fs::PermissionsExt;
        let cfg_dir = tempfile::tempdir().unwrap();
        let out_dir = tempfile::tempdir().unwrap();
        let out_path = out_dir.path().join("report.json");
        std::fs::write(&out_path, "OLD").unwrap();
        let mut v: Value = serde_json::from_str(&example_text()).unwrap();
        v["base_url"] = Value::from(closed_port_url());
        v["brute_force"]["interval_ms"] = Value::from(1);
        v["race"]["settle_ms"] = Value::from(1);
        let cfg = cfg_dir.path().join("c.json");
        std::fs::write(&cfg, v.to_string()).unwrap();
        // prepare while writable, then lock the directory; root ignores permissions: skip
        let prepared = check_args(&cfg, &out_path, None, &full_env).unwrap();
        let dir_mode =
            |m| std::fs::set_permissions(out_dir.path(), std::fs::Permissions::from_mode(m));
        dir_mode(0o555).unwrap();
        if std::fs::File::create(out_dir.path().join("probe")).is_ok() {
            dir_mode(0o755).unwrap();
            return;
        }
        let (mut o, mut e) = (Vec::new(), Vec::new());
        let code = run_check(prepared, &out_path, std::future::ready(()), &mut o, &mut e).await;
        dir_mode(0o755).unwrap();
        let (o, e) = (String::from_utf8(o).unwrap(), String::from_utf8(e).unwrap());
        assert_eq!(code, 130, "{e}");
        let lines: Vec<&str> = o.lines().collect();
        assert_eq!(lines.len(), 4, "{o}");
        assert!(lines[0].starts_with("race "), "{o}");
        assert!(lines[1].starts_with("load "), "{o}");
        assert!(lines[2].starts_with("brute_force "), "{o}");
        assert_eq!(
            lines[3],
            format!("report NOT WRITTEN: {}", out_path.display())
        );
        assert!(e.contains("cannot write report"), "{e}");
        assert_eq!(std::fs::read_to_string(&out_path).unwrap(), "OLD");
        assert_eq!(std::fs::read_dir(out_dir.path()).unwrap().count(), 1);
    }

    fn assert_three_lines_in_order(r: &RunOut) {
        let lines: Vec<&str> = r.out.lines().collect();
        assert_eq!(lines.len(), 3, "{}", r.out);
        assert!(lines[0].starts_with("race "), "{}", r.out);
        assert!(lines[1].starts_with("load "), "{}", r.out);
        assert!(lines[2].starts_with("brute_force "), "{}", r.out);
        assert!(!r.err.contains("partial run"), "{}", r.err);
    }

    fn assert_full_report_shape(rep: &Value) {
        assert_eq!(rep["report_version"], 1);
        assert_eq!(rep["scope"], "full");
        let names: Vec<&str> = rep["checks"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["check"].as_str().unwrap())
            .collect();
        assert_eq!(names, ["race", "load", "brute_force"]);
        for c in rep["checks"].as_array().unwrap() {
            assert!(
                !c["reasons"].to_string().contains("excluded_by_only"),
                "{c}"
            );
        }
        for key in ["started_at", "finished_at"] {
            let t = rep[key].as_str().unwrap();
            assert!(t.ends_with('Z') && !t.contains('.'), "{key}: {t}");
        }
    }

    #[test]
    fn build_report_full_keeps_order_without_excluded_and_cuts_seconds() {
        let t = OffsetDateTime::from_unix_timestamp_nanos(1_700_000_000_123_456_789).unwrap();
        let done = |check| CheckResult {
            check,
            verdict: Verdict::Pass,
            reasons: vec![],
            completion: Completion::Complete,
            evidence: None,
        };
        let all = vec![
            done(CheckName::Race),
            done(CheckName::Load),
            done(CheckName::BruteForce),
        ];
        let r = build_report(Scope::Full, t, t, "abcdef0123456789", all.clone()).unwrap();
        assert_eq!(r.scope, Scope::Full);
        assert_eq!(r.checks, all);
        assert_eq!(r.completion, Completion::Complete);
        assert_eq!(r.started_at, "2023-11-14T22:13:20Z");
        assert_eq!(r.finished_at, "2023-11-14T22:13:20Z");

        // One incomplete check makes the suite incomplete.
        let mut some = all;
        some[1].completion = Completion::Incomplete;
        let r = build_report(Scope::Full, t, t, "abcdef0123456789", some).unwrap();
        assert_eq!(r.completion, Completion::Incomplete);
    }

    #[tokio::test]
    async fn run_check_full_closed_port_runs_all_three_in_order_exit_2() {
        let r = run_full(&closed_port_url(), |_| {}, std::future::pending()).await;
        assert_eq!(r.code, 2, "{}", r.err);
        assert_three_lines_in_order(&r);
        let rep = r.report();
        assert_full_report_shape(&rep);
        assert_eq!(rep["checks"][0]["verdict"], "error");
        assert_eq!(rep["checks"][1]["verdict"], "error");
        assert_eq!(
            rep["checks"][1]["reasons"],
            serde_json::json!(["preflight_failed", "transport_error"])
        );
        assert_eq!(rep["checks"][2]["verdict"], "error");
        assert_eq!(
            rep["checks"][2]["reasons"],
            serde_json::json!(["baseline_failed", "transport_error"])
        );
        assert_no_sentinel(&r.out, &r.err);
        assert!(!r.report_text().contains(SENTINEL));
    }

    #[tokio::test]
    async fn run_check_full_cancel_before_dispatch_is_130_all_cancelled_no_connection() {
        let (addr, count) = hang().await;
        let r = run_full(&format!("http://{addr}"), |_| {}, std::future::ready(())).await;
        assert_eq!(r.code, 130, "{}", r.err);
        assert_eq!(count.load(Ordering::SeqCst), 0);
        assert_three_lines_in_order(&r);
        let rep = r.report();
        assert_full_report_shape(&rep);
        assert_eq!(rep["completion"], "incomplete");
        for c in rep["checks"].as_array().unwrap() {
            assert_eq!(c["verdict"], "not_run", "{c}");
            assert!(c["reasons"].to_string().contains("cancelled"), "{c}");
        }
    }

    #[tokio::test]
    async fn run_check_full_cancel_while_race_verifier_hangs_is_130_later_checks_not_run() {
        let (addr, count) = hang().await;
        let seen = count.clone();
        let cancel = async move {
            while seen.load(Ordering::SeqCst) == 0 {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        };
        let r = run_full(&format!("http://{addr}"), |_| {}, cancel).await;
        assert_eq!(r.code, 130, "{}", r.err);
        assert_three_lines_in_order(&r);
        let rep = r.report();
        assert_full_report_shape(&rep);
        for c in rep["checks"].as_array().unwrap() {
            assert_eq!(c["verdict"], "not_run", "{c}");
            assert!(c["reasons"].to_string().contains("cancelled"), "{c}");
        }
        // Only the hanging race verifier connected; load and brute_force sent nothing.
        assert_eq!(count.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn run_check_full_deadline_while_race_hangs_is_2_all_not_run_deadline() {
        let (addr, count) = hang().await;
        let r = run_full(
            &format!("http://{addr}"),
            |v| v["limits"]["suite_deadline_ms"] = Value::from(200),
            std::future::pending(),
        )
        .await;
        assert_eq!(r.code, 2, "{}", r.err);
        assert_three_lines_in_order(&r);
        let rep = r.report();
        assert_full_report_shape(&rep);
        assert_eq!(rep["completion"], "incomplete");
        for c in rep["checks"].as_array().unwrap() {
            assert_eq!(c["verdict"], "not_run", "{c}");
            assert_eq!(c["reasons"][0], "deadline_exceeded", "{c}");
        }
        // `<=`: the deadline may fire before the first connection is accepted; the
        // server never replies, so at most the race verifier can ever connect.
        assert!(count.load(Ordering::SeqCst) <= 1);
    }

    #[tokio::test]
    async fn run_check_full_cancel_after_race_complete_keeps_race_verdict() {
        let (addr, count) = preflight_ok_then_hang().await;
        let seen = count.clone();
        let cancel = async move {
            while seen.load(Ordering::SeqCst) < 2 {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        };
        let r = run_full(&format!("http://{addr}"), |_| {}, cancel).await;
        assert_eq!(r.code, 130, "{}", r.err);
        assert_three_lines_in_order(&r);
        let rep = r.report();
        assert_full_report_shape(&rep);
        let checks = rep["checks"].as_array().unwrap();
        assert_eq!(checks[0]["verdict"], "error", "{}", checks[0]);
        assert!(checks[0]["reasons"].to_string().contains("counter_invalid"));
        assert_eq!(checks[0]["completion"], "complete", "{}", checks[0]);
        assert_eq!(checks[1]["verdict"], "not_run", "{}", checks[1]);
        assert!(checks[1]["reasons"].to_string().contains("cancelled"));
        assert_eq!(checks[2]["verdict"], "not_run", "{}", checks[2]);
        assert!(checks[2]["reasons"].to_string().contains("cancelled"));
        assert_eq!(count.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn run_check_full_deadline_after_race_complete_keeps_race_verdict() {
        let (addr, count) = preflight_ok_then_hang().await;
        let r = run_full(
            &format!("http://{addr}"),
            |v| v["limits"]["suite_deadline_ms"] = Value::from(400),
            std::future::pending(),
        )
        .await;
        assert_eq!(r.code, 2, "{}", r.err);
        assert_three_lines_in_order(&r);
        let rep = r.report();
        assert_full_report_shape(&rep);
        let checks = rep["checks"].as_array().unwrap();
        assert_eq!(checks[0]["verdict"], "error", "{}", checks[0]);
        assert!(checks[0]["reasons"].to_string().contains("counter_invalid"));
        assert_eq!(checks[0]["completion"], "complete", "{}", checks[0]);
        assert_eq!(checks[1]["verdict"], "not_run", "{}", checks[1]);
        assert_eq!(checks[1]["reasons"][0], "deadline_exceeded");
        assert_eq!(checks[2]["verdict"], "not_run", "{}", checks[2]);
        assert_eq!(checks[2]["reasons"][0], "deadline_exceeded");
        assert_eq!(count.load(Ordering::SeqCst), 2);
    }
}
