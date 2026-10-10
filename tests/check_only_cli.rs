//! End-to-end `check --only brute_force` against the real binary (plan only-flag, S5).
//! Network: the local fixture (random port), counting listeners and hanging listeners on
//! 127.0.0.1 only. Every `check --only` case must exit non-zero (a partial run never exits 0).
mod support;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use support::cli::{CountingListener, Run};
use support::fixture::{Fixture, RunningFixture};

const SENTINEL: &str = "SENTINEL-s3cr3t-9f2c";
const BF_EXAMPLE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/examples/brute-force-only.json"
);
const CORE_EXAMPLE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/examples/core-checks.json");
const PASS_LINE: &str =
    "brute_force PASS first_protected=6 tail=3/2 attempts=8/8 reasons=protection_observed";

// ---------- config and job helpers ----------

fn config_value(base: &str) -> Value {
    let mut v: Value = serde_json::from_str(&std::fs::read_to_string(BF_EXAMPLE).unwrap()).unwrap();
    v["base_url"] = json!(base);
    v
}

fn core_section(name: &str) -> Value {
    let v: Value = serde_json::from_str(&std::fs::read_to_string(CORE_EXAMPLE).unwrap()).unwrap();
    v[name].clone()
}

fn cfg_text(base: &str, mutate: impl FnOnce(&mut Value)) -> String {
    let mut v = config_value(base);
    mutate(&mut v);
    serde_json::to_string_pretty(&v).unwrap()
}

fn set_path(v: &mut Value, path: &str) {
    v["brute_force"]["baseline_request"]["path"] = json!(path);
    v["brute_force"]["probe_request"]["path"] = json!(path);
}

type EnvVars = Vec<(String, String)>;

fn env_with(valid: &str, invalid: &str) -> EnvVars {
    [
        ("LOGIN_USER", "demo"),
        ("LOGIN_VALID_PASSWORD", valid),
        ("LOGIN_INVALID_PASSWORD", invalid),
    ]
    .iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect()
}

/// Healthy fixture needs the real valid password; the sentinel is the invalid one.
fn healthy_env() -> EnvVars {
    env_with("demo-pass", SENTINEL)
}

/// `accepts-invalid` takes any password for `demo`, so both can be the sentinel.
fn sentinel_env() -> EnvVars {
    env_with(SENTINEL, SENTINEL)
}

#[derive(Clone)]
struct Job {
    cfg: PathBuf,
    out: PathBuf,
    only: Option<String>,
    env: EnvVars,
}

struct Work {
    dir: tempfile::TempDir,
    cfg: PathBuf,
    out: PathBuf,
}

impl Work {
    fn new(text: &str) -> Work {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join("c.json");
        std::fs::write(&cfg, text).unwrap();
        let out = dir.path().join("result.json");
        Work { dir, cfg, out }
    }
    fn job(&self, env: EnvVars) -> Job {
        Job {
            cfg: self.cfg.clone(),
            out: self.out.clone(),
            only: Some("brute_force".into()),
            env,
        }
    }
    fn report(&self) -> Value {
        serde_json::from_str(&std::fs::read_to_string(&self.out).unwrap()).unwrap()
    }
    fn file_names(&self) -> Vec<String> {
        names_in(self.dir.path())
    }
}

fn names_in(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}

fn command(job: &Job) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_api-toolkit"));
    c.arg("check").arg("--config").arg(&job.cfg);
    c.arg("--out").arg(&job.out);
    if let Some(o) = &job.only {
        c.arg("--only").arg(o);
    }
    c.env_clear()
        .envs(job.env.iter().map(|(k, v)| (k.as_str(), v.as_str())));
    c
}

fn to_run(out: Output) -> Run {
    Run {
        code: out.status.code().expect("exit by signal, not by code"),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

/// Never exit 0, and no env secret value in stdout, stderr or the report file.
fn verify(job: &Job, run: &Run) {
    assert_ne!(run.code, 0, "check --only must never exit 0");
    let mut hay = vec![run.stdout.clone(), run.stderr.clone()];
    if let Ok(t) = std::fs::read_to_string(&job.out) {
        hay.push(t);
    }
    for (k, v) in &job.env {
        if k == "LOGIN_USER" || v.len() < 6 {
            continue;
        }
        for h in &hay {
            assert!(!h.contains(v.as_str()), "secret value of {k} leaked: {h}");
        }
    }
}

async fn exec(job: &Job) -> Run {
    let j = job.clone();
    let out = tokio::task::spawn_blocking(move || command(&j).output().unwrap())
        .await
        .unwrap();
    let run = to_run(out);
    verify(job, &run);
    run
}

// ---------- tiny blocking HTTP client for the fixture admin endpoints ----------

fn http_blocking(addr: SocketAddr, method: &str, path: &str, body: &str) -> String {
    let mut s = std::net::TcpStream::connect(addr).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    let req = format!(
        "{method} {path} HTTP/1.1\r\nHost: x\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    s.write_all(req.as_bytes()).unwrap();
    let mut resp = String::new();
    s.read_to_string(&mut resp).unwrap();
    resp
}

async fn http(
    fx: &RunningFixture,
    method: &'static str,
    path: &'static str,
    body: String,
) -> String {
    let addr = fx.addr;
    tokio::task::spawn_blocking(move || http_blocking(addr, method, path, &body))
        .await
        .unwrap()
}

async fn login_requests(fx: &RunningFixture) -> u64 {
    let resp = http(fx, "GET", "/__admin/stats", String::new()).await;
    let body = resp.split("\r\n\r\n").nth(1).expect("http body");
    let v: Value = serde_json::from_str(body.trim()).unwrap();
    v["login_requests"].as_u64().unwrap()
}

// ---------- hanging listener: accepts, never answers ----------

struct Hang {
    port: u16,
    count: Arc<AtomicUsize>,
    task: tokio::task::JoinHandle<()>,
}

impl Hang {
    async fn start() -> Hang {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = l.local_addr().unwrap().port();
        let count = Arc::new(AtomicUsize::new(0));
        let c = count.clone();
        let task = tokio::spawn(async move {
            let mut held = Vec::new();
            while let Ok((s, _)) = l.accept().await {
                held.push(s); // keep open, never reply
                c.fetch_add(1, Ordering::SeqCst);
            }
        });
        Hang { port, count, task }
    }
    fn base_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }
    fn seen(&self) -> usize {
        self.count.load(Ordering::SeqCst)
    }
}

impl Drop for Hang {
    fn drop(&mut self) {
        self.task.abort();
    }
}

// ---------- report assertions ----------

fn hex_sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn assert_whole_second(ts: &str) {
    assert!(ts.ends_with('Z') && !ts.contains('.'), "timestamp: {ts}");
    assert_eq!(ts.len(), 20, "timestamp: {ts}");
}

/// Shape shared by every partial report (PA-AC11).
fn assert_partial_shape(rep: &Value) {
    assert_eq!(rep["report_version"], 1);
    assert_eq!(rep["scope"], "partial");
    let checks = rep["checks"].as_array().unwrap();
    let names: Vec<&str> = checks
        .iter()
        .map(|c| c["check"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["race", "load", "brute_force"]);
    for (i, n) in ["race", "load"].iter().enumerate() {
        assert_eq!(
            checks[i],
            json!({"check": n, "verdict": "not_run", "reasons": ["excluded_by_only"], "completion": "incomplete"}),
            "excluded entry {n}"
        );
        assert!(checks[i].get("evidence").is_none());
    }
    assert_whole_second(rep["started_at"].as_str().unwrap());
    assert_whole_second(rep["finished_at"].as_str().unwrap());
}

fn bf(rep: &Value) -> &Value {
    &rep["checks"][2]
}

fn assert_bf(rep: &Value, verdict: &str, reasons: Value, completion: &str) {
    assert_partial_shape(rep);
    assert_eq!(bf(rep)["verdict"], verdict, "report: {rep}");
    assert_eq!(bf(rep)["reasons"], reasons);
    assert_eq!(bf(rep)["completion"], completion);
}

fn one_line(run: &Run) -> &str {
    let lines: Vec<&str> = run.stdout.lines().collect();
    assert_eq!(
        lines.len(),
        1,
        "stdout must be exactly one line: {:?}",
        run.stdout
    );
    lines[0]
}

// ---------- fixture-backed runs ----------

#[tokio::test(flavor = "multi_thread")]
async fn healthy_run_is_exit_2_with_pass_line_and_partial_report() {
    let fx = Fixture::start(0).await.unwrap();
    let w = Work::new(&cfg_text(&fx.base_url(), |_| {}));
    let run = exec(&w.job(healthy_env())).await;
    assert_eq!(run.code, 2, "stderr: {}", run.stderr);
    assert_eq!(one_line(&run), PASS_LINE);
    assert!(
        run.stderr
            .starts_with("note: partial run (--only brute_force)")
    );
    let rep = w.report();
    assert_bf(&rep, "pass", json!(["protection_observed"]), "complete");
    assert_eq!(rep["completion"], "complete");
    assert!(bf(&rep).get("evidence").is_some());
    // PA-AC21: no retry, request count equals the budget.
    assert_eq!(login_requests(&fx).await, 9);
    // PA-AC11 identity fields.
    assert_eq!(
        rep["config_hash"],
        hex_sha256(&std::fs::read(&w.cfg).unwrap())
    );
    let id = rep["run_id"].as_str().unwrap();
    let (ms, hex8) = id.split_once('-').expect("run_id format");
    assert_eq!(ms.len(), 13, "run_id: {id}");
    assert!(ms.bytes().all(|b| b.is_ascii_digit()));
    assert_eq!(hex8.len(), 8);
    assert!(hex8.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')));
    // No temp file left next to the report.
    assert_eq!(w.file_names(), ["c.json", "result.json"]);
    fx.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn second_run_without_reset_overwrites_report_and_is_inconclusive_with_one_request() {
    let fx = Fixture::start(0).await.unwrap();
    let w = Work::new(&cfg_text(&fx.base_url(), |_| {}));
    let first = exec(&w.job(healthy_env())).await;
    assert_eq!(first.code, 2);
    assert_eq!(w.report()["checks"][2]["verdict"], "pass");
    let before = login_requests(&fx).await;
    assert_eq!(before, 9);

    let second = exec(&w.job(healthy_env())).await;
    assert_eq!(second.code, 2, "stderr: {}", second.stderr);
    assert!(one_line(&second).starts_with("brute_force INCONCLUSIVE"));
    let rep = w.report(); // old report atomically replaced
    assert_bf(
        &rep,
        "inconclusive",
        json!(["account_already_locked"]),
        "complete",
    );
    assert_eq!(login_requests(&fx).await - before, 1);
    assert_eq!(w.file_names(), ["c.json", "result.json"]);
    fx.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn prelocked_account_is_inconclusive_with_one_request() {
    let fx = Fixture::start(0).await.unwrap();
    for _ in 0..6 {
        let body = json!({"username": "demo", "password": "nope"}).to_string();
        http(&fx, "POST", "/login", body).await;
    }
    assert_eq!(login_requests(&fx).await, 6);
    let w = Work::new(&cfg_text(&fx.base_url(), |_| {}));
    let run = exec(&w.job(healthy_env())).await;
    assert_eq!(run.code, 2, "stderr: {}", run.stderr);
    assert!(one_line(&run).starts_with("brute_force INCONCLUSIVE"));
    assert_bf(
        &w.report(),
        "inconclusive",
        json!(["account_already_locked"]),
        "complete",
    );
    assert_eq!(login_requests(&fx).await, 7);
    fx.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn broken_unprotected_login_is_fail_exit_1() {
    let fx = Fixture::start(0).await.unwrap();
    let w = Work::new(&cfg_text(&fx.base_url(), |v| {
        set_path(v, "/broken/login/unprotected")
    }));
    let run = exec(&w.job(healthy_env())).await;
    assert_eq!(run.code, 1, "stderr: {}", run.stderr);
    assert!(one_line(&run).starts_with("brute_force FAIL"));
    let rep = w.report();
    assert_partial_shape(&rep);
    assert_eq!(bf(&rep)["verdict"], "fail");
    assert_eq!(bf(&rep)["completion"], "complete");
    assert_eq!(rep["completion"], "complete");
    fx.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn broken_accepts_invalid_login_is_fail_exit_1() {
    let fx = Fixture::start(0).await.unwrap();
    let w = Work::new(&cfg_text(&fx.base_url(), |v| {
        set_path(v, "/broken/login/accepts-invalid")
    }));
    let run = exec(&w.job(sentinel_env())).await;
    assert_eq!(run.code, 1, "stderr: {}", run.stderr);
    assert!(one_line(&run).starts_with("brute_force FAIL"));
    let rep = w.report();
    assert_partial_shape(&rep);
    assert_eq!(bf(&rep)["verdict"], "fail");
    assert_eq!(bf(&rep)["completion"], "complete");
    fx.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn wrong_protection_matcher_is_error_exit_2() {
    let fx = Fixture::start(0).await.unwrap();
    let w = Work::new(&cfg_text(&fx.base_url(), |v| {
        v["brute_force"]["protection_match"]["status_in"] = json!([403])
    }));
    let run = exec(&w.job(healthy_env())).await;
    assert_eq!(run.code, 2, "stderr: {}", run.stderr);
    assert!(
        one_line(&run).starts_with("brute_force ERROR"),
        "{}",
        run.stdout
    );
    let rep = w.report();
    assert_partial_shape(&rep);
    assert_eq!(bf(&rep)["verdict"], "error");
    fx.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn closed_port_is_error_exit_2() {
    let port = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    }; // dropped: nothing listens here any more
    let w = Work::new(&cfg_text(&format!("http://127.0.0.1:{port}"), |_| {}));
    let run = exec(&w.job(healthy_env())).await;
    assert_eq!(run.code, 2, "stderr: {}", run.stderr);
    assert!(one_line(&run).starts_with("brute_force ERROR"));
    let rep = w.report();
    assert_partial_shape(&rep);
    assert_eq!(bf(&rep)["verdict"], "error");
    assert_eq!(w.file_names(), ["c.json", "result.json"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn unselected_sections_with_bad_semantics_and_no_test_env_do_not_block_the_run() {
    let fx = Fixture::start(0).await.unwrap();
    let w = Work::new(&cfg_text(&fx.base_url(), |v| {
        let mut race = core_section("race");
        race["concurrency"] = json!(1); // semantically invalid, but not selected
        let mut load = core_section("load");
        load["requests"] = json!(0);
        v["race"] = race;
        v["load"] = load;
    }));
    // env has no TEST_* names (race/load placeholders stay unresolved).
    let run = exec(&w.job(healthy_env())).await;
    assert_eq!(run.code, 2, "stderr: {}", run.stderr);
    assert_eq!(one_line(&run), PASS_LINE);
    assert_bf(
        &w.report(),
        "pass",
        json!(["protection_observed"]),
        "complete",
    );
    assert_eq!(login_requests(&fx).await, 9);
    fx.shutdown().await;
}

// ---------- rejected before any request ----------

struct Case {
    name: &'static str,
    only: Option<&'static str>,
    cfg: String,
    env: EnvVars,
    needle: &'static str,
}

fn rejected_cases(base: &str) -> Vec<Case> {
    let plain = |name, only, needle| Case {
        name,
        only,
        cfg: cfg_text(base, |_| {}),
        env: healthy_env(),
        needle,
    };
    let race_core = |extra: fn(&mut Value)| {
        cfg_text(base, move |v| {
            let mut r = core_section("race");
            extra(&mut r);
            v["race"] = r;
        })
    };
    let dup_field = {
        let t = cfg_text(base, |v| v["race"] = core_section("race"));
        let out = t.replacen("\"race\": {", "\"race\": {\"concurrency\": 5,", 1);
        assert_ne!(out, t);
        out
    };
    let dup_section = {
        let t = cfg_text(base, |v| v["race"] = core_section("race"));
        let full = serde_json::to_string(&core_section("race")).unwrap();
        let out = t.replacen("\"race\": {", &format!("\"race\": {full}, \"race\": {{"), 1);
        assert_ne!(out, t);
        out
    };
    vec![
        plain("no --only", None, "available in P4"),
        plain("bogus", Some("bogus"), "unknown check"),
        plain("loadx", Some("loadx"), "unknown check"),
        Case {
            name: "selected section missing",
            only: Some("brute_force"),
            cfg: cfg_text(base, |v| {
                v.as_object_mut().unwrap().remove("brute_force");
            }),
            env: healthy_env(),
            needle: "invalid config at `brute_force`",
        },
        Case {
            name: "env missing",
            only: Some("brute_force"),
            cfg: cfg_text(base, |_| {}),
            env: vec![
                ("LOGIN_USER".into(), "demo".into()),
                ("LOGIN_VALID_PASSWORD".into(), SENTINEL.into()),
            ],
            needle: "LOGIN_INVALID_PASSWORD",
        },
        Case {
            name: "unknown field in unselected race",
            only: Some("brute_force"),
            cfg: race_core(|r| r["typo_field"] = json!(1)),
            env: healthy_env(),
            needle: "unknown field",
        },
        Case {
            name: "wrong type in unselected race",
            only: Some("brute_force"),
            cfg: race_core(|r| r["concurrency"] = json!(SENTINEL)),
            env: healthy_env(),
            needle: "invalid value",
        },
        Case {
            name: "duplicate field in unselected race",
            only: Some("brute_force"),
            cfg: dup_field,
            env: healthy_env(),
            needle: "duplicate",
        },
        Case {
            name: "duplicate unselected section",
            only: Some("brute_force"),
            cfg: dup_section,
            env: healthy_env(),
            needle: "duplicate",
        },
    ]
}

#[tokio::test(flavor = "multi_thread")]
async fn rejected_inputs_exit_2_with_zero_connections_and_out_untouched() {
    let l = CountingListener::start();
    for existing in [false, true] {
        for case in rejected_cases(&l.base_url()) {
            let w = Work::new(&case.cfg);
            if existing {
                std::fs::write(&w.out, "OLD").unwrap();
            }
            let mut job = w.job(case.env.clone());
            job.only = case.only.map(String::from);
            let run = exec(&job).await;
            let ctx = format!("case `{}` existing_out={existing}", case.name);
            assert_eq!(run.code, 2, "{ctx}: stderr {}", run.stderr);
            assert!(
                run.stderr.contains(case.needle),
                "{ctx}: stderr {}",
                run.stderr
            );
            assert!(run.stdout.is_empty(), "{ctx}: stdout {}", run.stdout);
            if existing {
                assert_eq!(std::fs::read_to_string(&w.out).unwrap(), "OLD", "{ctx}");
            } else {
                assert!(!w.out.exists(), "{ctx}: out was created");
            }
            assert_eq!(w.file_names().len(), if existing { 2 } else { 1 }, "{ctx}");
        }
    }
    assert_eq!(l.finish(), 0, "a rejected input opened a connection");
}

#[tokio::test(flavor = "multi_thread")]
async fn counting_listener_control_sees_a_valid_run() {
    let l = CountingListener::start();
    let w = Work::new(&cfg_text(&l.base_url(), |_| {}));
    let run = exec(&w.job(healthy_env())).await;
    assert_eq!(run.code, 2);
    assert!(
        l.finish() >= 1,
        "control: a valid run must reach the listener"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn out_precheck_failures_exit_2_with_zero_connections_and_config_intact() {
    let l = CountingListener::start();
    let text = cfg_text(&l.base_url(), |_| {});
    let mut names: Vec<&str> = vec![
        "parent missing",
        "parent is a file",
        "out is a directory",
        "out is config",
    ];
    if cfg!(unix) {
        names.push("symlink to config");
    }
    for name in names {
        let w = Work::new(&text);
        let mut job = w.job(healthy_env());
        job.out = match name {
            "parent missing" => w.dir.path().join("nope").join("r.json"),
            "parent is a file" => {
                let f = w.dir.path().join("plain.txt");
                std::fs::write(&f, "keep").unwrap();
                f.join("r.json")
            }
            "out is a directory" => {
                let d = w.dir.path().join("sub");
                std::fs::create_dir(&d).unwrap();
                d
            }
            "out is config" => w.cfg.clone(),
            _ => {
                let link = w.dir.path().join("link.json");
                #[cfg(unix)]
                std::os::unix::fs::symlink(&w.cfg, &link).unwrap();
                link
            }
        };
        let run = exec(&job).await;
        assert_eq!(run.code, 2, "case `{name}`: stderr {}", run.stderr);
        assert!(
            run.stderr.starts_with("error:"),
            "case `{name}`: {}",
            run.stderr
        );
        assert!(run.stdout.is_empty(), "case `{name}`");
        assert_eq!(
            std::fs::read_to_string(&w.cfg).unwrap(),
            text,
            "case `{name}`: config changed"
        );
        assert!(!w.dir.path().join("nope").exists(), "case `{name}`");
    }
    assert_eq!(
        l.finish(),
        0,
        "an --out precheck failure opened a connection"
    );
}

// ---------- deadline and cancel ----------

#[tokio::test(flavor = "multi_thread")]
async fn deadline_with_hanging_server_is_exit_2_not_run_deadline_exceeded() {
    let h = Hang::start().await;
    let w = Work::new(&cfg_text(&h.base_url(), |v| {
        v["limits"]["request_timeout_ms"] = json!(5000);
        v["limits"]["suite_deadline_ms"] = json!(300);
        let b = &mut v["brute_force"];
        b["attempts"] = json!(2);
        b["interval_ms"] = json!(100);
        b["protect_by_attempt"] = json!(2);
        b["protected_tail_count"] = json!(1);
    }));
    let run = exec(&w.job(healthy_env())).await;
    assert_eq!(run.code, 2, "stderr: {}", run.stderr);
    assert!(
        one_line(&run).starts_with("brute_force NOT_RUN"),
        "{}",
        run.stdout
    );
    let rep = w.report();
    assert_bf(&rep, "not_run", json!(["deadline_exceeded"]), "incomplete");
    assert_eq!(rep["completion"], "incomplete");
    // Not asserted: `h.seen() >= 1`. Under heavy load the deadline can fire before the first
    // connection is accepted; the server never replies, so no verdict is possible either way.
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn sigint_while_request_hangs_is_exit_130_cancelled_without_new_connections() {
    let h = Hang::start().await;
    let w = Work::new(&cfg_text(&h.base_url(), |_| {})); // request_timeout 5000, deadline 120000
    let job = w.job(sentinel_env());
    let mut child = command(&job)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let pid = child.id();
    // Observed condition, not a guessed sleep: the first request reached the server, so the
    // SIGINT handler (registered before the first request) is installed.
    let mut waited = 0;
    while h.seen() == 0 {
        if waited >= 10_000 {
            let _ = child.kill(); // do not leak the child when the assertion below fails
            let _ = child.wait();
            panic!("the binary never connected");
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
        waited += 10;
    }
    let st = Command::new("kill")
        .args(["-INT", &pid.to_string()])
        .status()
        .unwrap();
    if !st.success() {
        let _ = child.kill();
        let _ = child.wait();
        panic!("kill -INT failed");
    }
    let out = tokio::task::spawn_blocking(move || child.wait_with_output().unwrap())
        .await
        .unwrap();
    let run = to_run(out);
    verify(&job, &run);
    assert_eq!(run.code, 130, "stderr: {}", run.stderr);
    assert!(one_line(&run).starts_with("brute_force NOT_RUN"));
    let rep = w.report();
    assert_bf(&rep, "not_run", json!(["cancelled"]), "incomplete");
    assert_eq!(rep["completion"], "incomplete");
    assert_eq!(bf(&rep)["evidence"]["attempts_dispatched"], 1);
    // Process has exited: give any stray connection time to arrive, then it must still be 1.
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(h.seen(), 1, "a request was sent after cancel");
}

// ---------- report writing ----------

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn unwritable_report_dir_keeps_console_line_and_old_content_exit_2() {
    use std::os::unix::fs::PermissionsExt;
    let fx = Fixture::start(0).await.unwrap();
    let w = Work::new(&cfg_text(&fx.base_url(), |_| {}));
    let ro = w.dir.path().join("ro");
    std::fs::create_dir(&ro).unwrap();
    let out = ro.join("result.json");
    std::fs::write(&out, "OLD").unwrap();
    std::fs::set_permissions(&ro, std::fs::Permissions::from_mode(0o555)).unwrap();
    let restore =
        |p: &Path| std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o755)).unwrap();
    if std::fs::write(ro.join("probe"), "x").is_ok() {
        restore(&ro);
        eprintln!("skipped: directory permissions are not enforced (root?)");
        fx.shutdown().await;
        return;
    }
    let mut job = w.job(healthy_env());
    job.out = out.clone();
    let run = exec(&job).await;
    restore(&ro);
    assert_eq!(run.code, 2, "stderr: {}", run.stderr);
    assert_eq!(one_line(&run), PASS_LINE);
    assert!(run.stderr.contains("cannot write report"), "{}", run.stderr);
    assert_eq!(std::fs::read_to_string(&out).unwrap(), "OLD");
    assert_eq!(names_in(&ro), ["result.json"], "temp file left behind");
    fx.shutdown().await;
}

// ---------- check --only race (P2 S8) ----------

const RACE_EXAMPLE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/examples/race-only.json");
const RACE_AUTH: &str = support::fixture::TEST_AUTHORIZATION;

fn race_cfg(base: &str, mutate: impl FnOnce(&mut Value)) -> String {
    let mut v: Value =
        serde_json::from_str(&std::fs::read_to_string(RACE_EXAMPLE).unwrap()).unwrap();
    v["base_url"] = json!(base);
    v["race"]["settle_ms"] = json!(50);
    v["race"]["max_dispatch_skew_ms"] = json!(1000); // skew must never decide these cases
    mutate(&mut v);
    serde_json::to_string_pretty(&v).unwrap()
}

fn race_env(id: &str, auth: &str) -> EnvVars {
    vec![
        ("TEST_ACTION_ID".to_string(), id.to_string()),
        ("TEST_AUTHORIZATION".to_string(), auth.to_string()),
    ]
}

fn race_job(w: &Work, env: EnvVars) -> Job {
    let mut j = w.job(env);
    j.only = Some("race".into());
    j
}

async fn stat(fx: &RunningFixture, key: &str) -> u64 {
    let resp = http(fx, "GET", "/__admin/stats", String::new()).await;
    let body = resp.split("\r\n\r\n").nth(1).expect("http body");
    let v: Value = serde_json::from_str(body.trim()).unwrap();
    v[key].as_u64().unwrap()
}

/// (action_requests, verify_requests)
async fn race_stats(fx: &RunningFixture) -> (u64, u64) {
    (
        stat(fx, "action_requests").await,
        stat(fx, "verify_requests").await,
    )
}

fn race_entry(rep: &Value) -> &Value {
    &rep["checks"][0]
}

/// Partial shape for `--only race`: order race, load, brute_force; the two others excluded.
fn assert_race_partial(rep: &Value) {
    assert_eq!(rep["report_version"], 1);
    assert_eq!(rep["scope"], "partial");
    let checks = rep["checks"].as_array().unwrap();
    let names: Vec<&str> = checks
        .iter()
        .map(|c| c["check"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["race", "load", "brute_force"]);
    for (i, n) in [(1, "load"), (2, "brute_force")] {
        assert_eq!(
            checks[i],
            json!({"check": n, "verdict": "not_run", "reasons": ["excluded_by_only"], "completion": "incomplete"}),
            "excluded entry {n}"
        );
    }
}

fn assert_race(rep: &Value, verdict: &str, reasons: Value, completion: &str) {
    assert_race_partial(rep);
    assert_eq!(race_entry(rep)["verdict"], verdict, "report: {rep}");
    assert_eq!(race_entry(rep)["reasons"], reasons);
    assert_eq!(race_entry(rep)["completion"], completion);
}

#[tokio::test(flavor = "multi_thread")]
async fn race_healthy_is_pass_line_exit_2_partial_report() {
    let fx = Fixture::start(0).await.unwrap();
    let w = Work::new(&race_cfg(&fx.base_url(), |_| {}));
    let run = exec(&race_job(&w, race_env("s8-healthy", RACE_AUTH))).await;
    assert_eq!(run.code, 2, "stderr: {}", run.stderr);
    let line = one_line(&run);
    // Measured skew varies per run; only the stable parts are pinned.
    assert!(
        line.starts_with("race PASS delta=1 before=0 after=1 actions=5/5 skew="),
        "{line}"
    );
    assert!(
        line.ends_with("/1000ms reasons=single_side_effect"),
        "{line}"
    );
    assert!(
        run.stderr.starts_with("note: partial run (--only race)"),
        "stderr: {}",
        run.stderr
    );
    let rep = w.report();
    assert_race(&rep, "pass", json!(["single_side_effect"]), "complete");
    assert_eq!(rep["completion"], "complete");
    let ev = &race_entry(&rep)["evidence"];
    assert_eq!(ev["counter_before"], 0);
    assert_eq!(ev["counter_after"], 1);
    assert_eq!(ev["delta"], 1);
    assert_eq!(ev["actions"].as_array().unwrap().len(), 5);
    // No retry: 1 verifier before + 5 actions + 1 verifier after.
    assert_eq!(race_stats(&fx).await, (5, 2));
    assert_eq!(w.file_names(), ["c.json", "result.json"]);
    fx.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn race_racy_variant_is_fail_exit_1_duplicate_side_effect() {
    let fx = Fixture::start(0).await.unwrap();
    let w = Work::new(&race_cfg(&fx.base_url(), |v| {
        v["race"]["action_request"]["path"] = json!("/broken/actions/{id}/apply-racy");
        v["limits"]["request_timeout_ms"] = json!(5000); // > the 2 s racy gate
    }));
    let run = exec(&race_job(&w, race_env("s8-racy", RACE_AUTH))).await;
    assert_eq!(run.code, 1, "stderr: {}", run.stderr);
    let line = one_line(&run);
    assert!(line.starts_with("race FAIL"), "{line}");
    assert!(line.ends_with("reasons=duplicate_side_effect"), "{line}");
    let rep = w.report();
    assert_race(&rep, "fail", json!(["duplicate_side_effect"]), "complete");
    assert!(race_entry(&rep)["evidence"]["delta"].as_u64().unwrap() >= 2);
    assert_eq!(race_stats(&fx).await, (5, 2));
    fx.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn race_used_resource_second_run_is_inconclusive_with_no_action() {
    let fx = Fixture::start(0).await.unwrap();
    let w = Work::new(&race_cfg(&fx.base_url(), |_| {}));
    let job = race_job(&w, race_env("s8-reuse", RACE_AUTH));
    let first = exec(&job).await;
    assert_eq!(first.code, 2, "stderr: {}", first.stderr);
    assert!(one_line(&first).starts_with("race PASS"));
    let (a0, v0) = race_stats(&fx).await;
    assert_eq!((a0, v0), (5, 2));

    let second = exec(&job).await;
    assert_eq!(second.code, 2, "stderr: {}", second.stderr);
    assert!(one_line(&second).starts_with("race INCONCLUSIVE"));
    let rep = w.report();
    assert_race(
        &rep,
        "inconclusive",
        json!(["initial_counter_not_zero"]),
        "complete",
    );
    assert_eq!(race_entry(&rep)["evidence"]["counter_before"], 1);
    let (a1, v1) = race_stats(&fx).await;
    assert_eq!((a1 - a0, v1 - v0), (0, 1), "no action, one verifier");
    fx.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn race_rejected_inputs_exit_2_with_zero_connections_and_out_untouched() {
    let fx = Fixture::start(0).await.unwrap();
    let base = fx.base_url();
    let ok_env = || race_env("s8-rej", RACE_AUTH);
    let cases: Vec<(&str, Option<&str>, String, EnvVars, &str)> = vec![
        (
            "202 in expected_action_statuses",
            Some("race"),
            race_cfg(&base, |v| {
                v["race"]["expected_action_statuses"] = json!([200, 202, 409])
            }),
            ok_env(),
            "expected_action_statuses",
        ),
        (
            "both env missing",
            Some("race"),
            race_cfg(&base, |_| {}),
            vec![],
            "TEST_ACTION_ID",
        ),
        (
            "TEST_AUTHORIZATION missing",
            Some("race"),
            race_cfg(&base, |_| {}),
            vec![("TEST_ACTION_ID".into(), "s8-rej".into())],
            "TEST_AUTHORIZATION",
        ),
        (
            "TEST_ACTION_ID missing",
            Some("race"),
            race_cfg(&base, |_| {}),
            vec![("TEST_AUTHORIZATION".into(), RACE_AUTH.into())],
            "TEST_ACTION_ID",
        ),
        (
            "--only loadx",
            Some("loadx"),
            race_cfg(&base, |_| {}),
            ok_env(),
            "unknown check",
        ),
    ];
    for existing in [false, true] {
        for (name, only, cfg, env, needle) in &cases {
            let w = Work::new(cfg);
            if existing {
                std::fs::write(&w.out, "OLD").unwrap();
            }
            let mut job = w.job(env.clone());
            job.only = only.map(String::from);
            let run = exec(&job).await;
            let ctx = format!("case `{name}` existing_out={existing}");
            assert_eq!(run.code, 2, "{ctx}: stderr {}", run.stderr);
            assert!(run.stderr.contains(needle), "{ctx}: stderr {}", run.stderr);
            assert!(run.stdout.is_empty(), "{ctx}: stdout {}", run.stdout);
            if existing {
                assert_eq!(std::fs::read_to_string(&w.out).unwrap(), "OLD", "{ctx}");
            } else {
                assert!(!w.out.exists(), "{ctx}: out was created");
            }
            assert_eq!(race_stats(&fx).await, (0, 0), "{ctx}: touched the fixture");
            assert_eq!(login_requests(&fx).await, 0, "{ctx}");
        }
    }
    fx.shutdown().await;
}

/// Raw listener that records the bytes of every request and never answers.
struct Recorder {
    port: u16,
    wire: Arc<std::sync::Mutex<Vec<u8>>>,
    task: tokio::task::JoinHandle<()>,
}

impl Recorder {
    async fn start() -> Recorder {
        use tokio::io::AsyncReadExt;
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = l.local_addr().unwrap().port();
        let wire = Arc::new(std::sync::Mutex::new(Vec::new()));
        let w = wire.clone();
        let task = tokio::spawn(async move {
            while let Ok((mut s, _)) = l.accept().await {
                let w = w.clone();
                tokio::spawn(async move {
                    let mut buf = [0u8; 4096];
                    while let Ok(n) = s.read(&mut buf).await {
                        if n == 0 {
                            break;
                        }
                        w.lock().unwrap().extend_from_slice(&buf[..n]);
                    }
                });
            }
        });
        Recorder { port, wire, task }
    }
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.wire.lock().unwrap()).into_owned()
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        self.task.abort();
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn race_sentinel_is_on_the_wire_but_not_in_cli_output() {
    let rec = Recorder::start().await;
    let w = Work::new(&race_cfg(&format!("http://127.0.0.1:{}", rec.port), |v| {
        v["limits"]["request_timeout_ms"] = json!(500);
        for k in ["action_request", "verify_request"] {
            v["race"][k]["headers"]["X-Sentinel"] = json!("${ENV:TEST_ACTION_ID}");
        }
    }));
    // exec() also searches stdout, stderr and the report for every env value (>= 6 chars).
    let run = exec(&race_job(&w, race_env(SENTINEL, SENTINEL))).await;
    // The server never answers: verifier before times out, a non-PASS result.
    assert_eq!(run.code, 2, "stderr: {}", run.stderr);
    let line = one_line(&run);
    assert!(line.starts_with("race ERROR"), "{line}");
    assert!(line.contains("verifier_failed"), "{line}");
    let rep = w.report();
    assert_race(
        &rep,
        "error",
        json!(["verifier_failed", "transport_error"]),
        "complete",
    );
    // Positive control: the sentinel really was sent (path, Authorization, extra header).
    let wire = rec.text().to_ascii_lowercase();
    let s = SENTINEL.to_ascii_lowercase();
    assert!(wire.contains(&format!("get /actions/{s} ")), "wire: {wire}");
    assert!(
        wire.contains(&format!("authorization: {s}")),
        "wire: {wire}"
    );
    assert!(wire.contains(&format!("x-sentinel: {s}")), "wire: {wire}");
    // Negative: nowhere in console output or report (checked again explicitly).
    let report_text = std::fs::read_to_string(&w.out).unwrap();
    for hay in [&run.stdout, &run.stderr, &report_text] {
        assert!(!hay.contains(SENTINEL), "sentinel leaked: {hay}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn race_sentinel_id_against_fixture_does_not_leak_on_pass_or_fail() {
    for (path, want_code) in [
        ("/actions/{id}/apply", 2),
        ("/broken/actions/{id}/apply-racy", 1),
    ] {
        let fx = Fixture::start(0).await.unwrap();
        let w = Work::new(&race_cfg(&fx.base_url(), |v| {
            v["race"]["action_request"]["path"] = json!(path);
            v["limits"]["request_timeout_ms"] = json!(5000);
        }));
        let run = exec(&race_job(&w, race_env(SENTINEL, RACE_AUTH))).await;
        assert_eq!(run.code, want_code, "{path}: stderr {}", run.stderr);
        // A real verdict proves the sentinel id was used end to end.
        let rep = w.report();
        assert_eq!(race_entry(&rep)["evidence"]["counter_before"], 0, "{path}");
        let report_text = std::fs::read_to_string(&w.out).unwrap();
        for hay in [&run.stdout, &run.stderr, &report_text] {
            assert!(
                !hay.contains(SENTINEL) && !hay.contains(RACE_AUTH),
                "{path}: {hay}"
            );
        }
        fx.shutdown().await;
    }
}

#[test]
fn validate_only_race_example_exits_0() {
    let r = support::cli::run_bin(
        &["validate", "--config", RACE_EXAMPLE, "--only", "race"],
        &[
            ("TEST_ACTION_ID", "act-1"),
            ("TEST_AUTHORIZATION", RACE_AUTH),
        ],
    );
    assert_eq!(r.code, 0, "stderr: {} stdout: {}", r.stderr, r.stdout);
}

#[test]
fn validate_only_race_reports_seven_planned_requests() {
    let r = support::cli::run_bin(
        &["validate", "--config", RACE_EXAMPLE, "--only", "race"],
        &[
            ("TEST_ACTION_ID", "act-1"),
            ("TEST_AUTHORIZATION", RACE_AUTH),
        ],
    );
    assert_eq!(r.code, 0, "stderr: {} stdout: {}", r.stderr, r.stdout);
    assert!(r.stdout.contains("planned requests: 7"), "{}", r.stdout);
}
