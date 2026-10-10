//! Local fixture API (healthy plus broken login variants). A development tool, NOT a product mock server.
//!
//! Binds 127.0.0.1 only. Never logs or echoes request headers or bodies.
//! Behaviour is documented in `docs/fixture.md`. Broken race/load variants arrive in P2-P3.
#![allow(dead_code)] // each user (tests, example) only uses part of this module

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex, MutexGuard};

use axum::{
    Json, Router,
    body::Bytes,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    routing::{get, post},
};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::{net::TcpListener, sync::oneshot, task::JoinHandle};

// Public dev constants (not secrets). Keep in sync with docs/fixture.md.
pub const LOGIN_USER: &str = "demo";
pub const LOGIN_VALID_PASSWORD: &str = "demo-pass";
pub const LOGIN_INVALID_PASSWORD: &str = "wrong-pass";
pub const TEST_ACTION_ID: &str = "act-1";
pub const TEST_AUTHORIZATION: &str = "Bearer fixture-token";
pub const ACCESS_TOKEN: &str = "fixture-access-token";
/// Failed login number at which protection (429) starts.
pub const PROTECT_BY_ATTEMPT: u32 = 6;

#[derive(Default)]
struct FixtureState {
    failed_logins: u32,
    /// action id -> execution_count
    actions: HashMap<String, u64>,
    load_requests: u64,
    /// Every request to the three login routes (also malformed ones). Read via /__admin/stats.
    login_requests: u64,
}

type Shared = Arc<Mutex<FixtureState>>;

/// A poisoned lock only means another handler panicked; the counters are still plain numbers.
fn lock(state: &Shared) -> MutexGuard<'_, FixtureState> {
    state.lock().unwrap_or_else(|e| e.into_inner())
}

pub struct Fixture;

pub struct RunningFixture {
    pub addr: SocketAddr,
    shutdown_tx: oneshot::Sender<()>,
    task: JoinHandle<()>,
}

impl Fixture {
    /// `port` 0 = ephemeral. Always binds 127.0.0.1, never 0.0.0.0 or ::.
    pub async fn start(port: u16) -> std::io::Result<RunningFixture> {
        let listener = TcpListener::bind(("127.0.0.1", port)).await?;
        let addr = listener.local_addr()?;
        let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
        let app = router(Arc::new(Mutex::new(FixtureState::default())));
        let task = tokio::spawn(async move {
            // Server errors after bind have nowhere useful to go in a dev tool.
            let _ = axum::serve(listener, app)
                .with_graceful_shutdown(async {
                    let _ = shutdown_rx.await;
                })
                .await;
        });
        Ok(RunningFixture {
            addr,
            shutdown_tx,
            task,
        })
    }
}

impl RunningFixture {
    pub fn base_url(&self) -> String {
        format!("http://{}", self.addr)
    }

    /// Graceful shutdown: stop accepting, finish in-flight requests, then return.
    pub async fn shutdown(self) {
        let _ = self.shutdown_tx.send(());
        let _ = self.task.await;
    }
}

fn router(state: Shared) -> Router {
    Router::new()
        .route("/login", post(login))
        .route("/broken/login/unprotected", post(login_unprotected))
        .route("/broken/login/accepts-invalid", post(login_accepts_invalid))
        .route("/actions/{id}/apply", post(apply))
        .route("/actions/{id}", get(read_action))
        .route("/critical", get(critical))
        .route("/__admin/reset", post(reset))
        .route("/__admin/stats", get(stats))
        .fallback(not_found)
        .with_state(state)
}

type Reply = (StatusCode, Json<Value>);

fn reply(status: StatusCode, body: Value) -> Reply {
    (status, Json(body))
}

fn unauthorized() -> Reply {
    reply(StatusCode::UNAUTHORIZED, json!({"error": "unauthorized"}))
}

fn is_authorized(headers: &HeaderMap) -> bool {
    headers
        .get("authorization")
        .is_some_and(|v| v.as_bytes() == TEST_AUTHORIZATION.as_bytes())
}

#[derive(Deserialize)]
struct LoginBody {
    username: String,
    password: String,
}

/// Counts the request, then parses the body. Parsing is by hand because the Json
/// extractor's rejection text echoes input values. `Err` is the fixed 400 reply.
fn count_and_parse_login(state: &Shared, raw: &[u8]) -> Result<LoginBody, Reply> {
    lock(state).login_requests += 1;
    serde_json::from_slice::<LoginBody>(raw)
        .map_err(|_| reply(StatusCode::BAD_REQUEST, json!({"error": "bad_request"})))
}

fn token_reply() -> Reply {
    reply(StatusCode::OK, json!({"access_token": ACCESS_TOKEN}))
}

fn invalid_credentials() -> Reply {
    reply(
        StatusCode::UNAUTHORIZED,
        json!({"error": "invalid_credentials"}),
    )
}

/// Broken variant: no protection. Wrong credentials are always 401, no state.
async fn login_unprotected(State(state): State<Shared>, raw: Bytes) -> Reply {
    let body = match count_and_parse_login(&state, &raw) {
        Ok(body) => body,
        Err(rejection) => return rejection,
    };
    if body.username == LOGIN_USER && body.password == LOGIN_VALID_PASSWORD {
        token_reply()
    } else {
        invalid_credentials()
    }
}

/// Broken variant: user `demo` is accepted with any password.
async fn login_accepts_invalid(State(state): State<Shared>, raw: Bytes) -> Reply {
    let body = match count_and_parse_login(&state, &raw) {
        Ok(body) => body,
        Err(rejection) => return rejection,
    };
    if body.username == LOGIN_USER {
        token_reply()
    } else {
        invalid_credentials()
    }
}

async fn login(State(state): State<Shared>, raw: Bytes) -> Reply {
    let body = match count_and_parse_login(&state, &raw) {
        Ok(body) => body,
        Err(rejection) => return rejection,
    };
    let valid = body.username == LOGIN_USER && body.password == LOGIN_VALID_PASSWORD;
    let mut s = lock(&state);
    if valid {
        // A valid login never resets the failed counter; only the admin reset does.
        if s.failed_logins >= PROTECT_BY_ATTEMPT {
            return reply(
                StatusCode::TOO_MANY_REQUESTS,
                json!({"error": "too_many_attempts"}),
            );
        }
        return token_reply();
    }
    s.failed_logins = s.failed_logins.saturating_add(1);
    if s.failed_logins >= PROTECT_BY_ATTEMPT {
        reply(
            StatusCode::TOO_MANY_REQUESTS,
            json!({"error": "too_many_attempts"}),
        )
    } else {
        invalid_credentials()
    }
}

async fn apply(State(state): State<Shared>, Path(id): Path<String>, headers: HeaderMap) -> Reply {
    if !is_authorized(&headers) {
        return unauthorized();
    }
    // The lock is held across check + increment, so concurrent applies are atomic.
    let mut s = lock(&state);
    let count = s.actions.entry(id).or_insert(0);
    if *count == 0 {
        *count = 1;
        reply(StatusCode::OK, json!({"applied": true}))
    } else {
        reply(StatusCode::CONFLICT, json!({"error": "already_applied"}))
    }
}

async fn read_action(
    State(state): State<Shared>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Reply {
    if !is_authorized(&headers) {
        return unauthorized();
    }
    let mut s = lock(&state);
    let count = *s.actions.entry(id.clone()).or_insert(0);
    reply(StatusCode::OK, json!({"id": id, "execution_count": count}))
}

async fn critical(State(state): State<Shared>, headers: HeaderMap) -> Reply {
    if !is_authorized(&headers) {
        return unauthorized();
    }
    let mut s = lock(&state);
    s.load_requests += 1;
    reply(StatusCode::OK, json!({"ok": true}))
}

/// Dev tool only: restores the initial state.
async fn reset(State(state): State<Shared>) -> Reply {
    *lock(&state) = FixtureState::default();
    reply(StatusCode::OK, json!({"reset": true}))
}

/// Dev tool only: request counters, so a test can prove how many requests reached the fixture.
async fn stats(State(state): State<Shared>) -> Reply {
    let s = lock(&state);
    reply(
        StatusCode::OK,
        json!({"login_requests": s.login_requests, "load_requests": s.load_requests}),
    )
}

async fn not_found() -> Reply {
    reply(StatusCode::NOT_FOUND, json!({"error": "not_found"}))
}
