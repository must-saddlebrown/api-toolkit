//! Helpers for tests that run the real `api-toolkit` binary.
#![allow(dead_code)]

use std::net::TcpListener;
use std::path::Path;
use std::process::{Command, Output};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::thread::{self, JoinHandle};
use std::time::Duration;

pub const EXAMPLE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/examples/core-checks.json");

/// The five dev values from docs/fixture.md.
pub const DEV_ENV: [(&str, &str); 5] = [
    ("LOGIN_USER", "demo"),
    ("LOGIN_VALID_PASSWORD", "demo-pass"),
    ("LOGIN_INVALID_PASSWORD", "wrong-pass"),
    ("TEST_ACTION_ID", "act-1"),
    ("TEST_AUTHORIZATION", "Bearer fixture-token"),
];

pub struct Run {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

/// Run the binary with an empty environment plus `env`.
pub fn run_bin(args: &[&str], env: &[(&str, &str)]) -> Run {
    let out: Output = Command::new(env!("CARGO_BIN_EXE_api-toolkit"))
        .args(args)
        .env_clear()
        .envs(env.iter().copied())
        .output()
        .expect("run binary");
    Run {
        code: out.status.code().expect("exit code"),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

pub fn validate(path: &Path, env: &[(&str, &str)]) -> Run {
    run_bin(&["validate", "--config", path.to_str().unwrap()], env)
}

pub fn example_text() -> String {
    std::fs::read_to_string(EXAMPLE).unwrap()
}

/// Example config with `base_url` pointed at `base`.
pub fn with_base_url(text: &str, base: &str) -> String {
    let out = text.replace("http://127.0.0.1:8080", base);
    assert_ne!(out, text, "base_url not rewritten");
    out
}

/// Counts TCP connections made to 127.0.0.1:<port>. Non-blocking accept loop in a thread.
pub struct CountingListener {
    pub port: u16,
    count: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl CountingListener {
    pub fn start() -> CountingListener {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let port = listener.local_addr().unwrap().port();
        let count = Arc::new(AtomicUsize::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let (c, s) = (count.clone(), stop.clone());
        let handle = thread::spawn(move || {
            loop {
                let stopping = s.load(Ordering::SeqCst);
                // Drain everything pending (also after `stop`, to catch queued connections).
                while listener.accept().is_ok() {
                    c.fetch_add(1, Ordering::SeqCst);
                }
                if stopping {
                    break;
                }
                thread::sleep(Duration::from_millis(2));
            }
        });
        CountingListener {
            port,
            count,
            stop,
            handle: Some(handle),
        }
    }

    pub fn base_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    /// Call after the command exited: short bounded wait, stop flag, join, final count.
    pub fn finish(mut self) -> usize {
        thread::sleep(Duration::from_millis(100));
        self.stop.store(true, Ordering::SeqCst);
        if let Some(h) = self.handle.take() {
            h.join().unwrap();
        }
        self.count.load(Ordering::SeqCst)
    }
}
