//! Local fixture API (healthy plus broken login variants). A development tool, NOT a product mock server.
//!
//! Binds 127.0.0.1 only. Never logs or echoes request headers or bodies.
//! Behaviour is documented in `docs/fixture.md`. Race variants (racy, idempotent) live here.
//! Load variants: `/critical` (healthy), `/broken/critical/{slow,flaky,hang}`. All four count every request
//! in `load_requests` (also 401). Flaky and hang decide by that counter (every `FLAKY_EVERY`th request),
//! never by timing; slow and hang only give a lower bound on latency.
#![allow(dead_code)] // each user (tests, example) only uses part of this module

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use axum::{
    Json, Router,
    body::Bytes,
    extract::{ConnectInfo, Path, State},
    http::{HeaderMap, StatusCode, header},
    response::IntoResponse,
    routing::{get, post},
};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::{
    net::TcpListener,
    sync::{oneshot, watch},
    task::JoinHandle,
    time::timeout,
};

// Public dev constants (not secrets). Keep in sync with docs/fixture.md.
pub const LOGIN_USER: &str = "demo";
pub const LOGIN_VALID_PASSWORD: &str = "demo-pass";
pub const LOGIN_INVALID_PASSWORD: &str = "wrong-pass";
pub const TEST_ACTION_ID: &str = "act-1";
pub const TEST_AUTHORIZATION: &str = "Bearer fixture-token";
pub const ACCESS_TOKEN: &str = "fixture-access-token";
/// Failed login number at which protection (429) starts.
pub const PROTECT_BY_ATTEMPT: u32 = 6;
/// Slow load route: fixed server delay before every 200.
pub const SLOW_DELAY: Duration = Duration::from_millis(300);
/// Flaky load route: every Nth load request (counter, not timing) answers 500. Hang: every Nth sleeps.
pub const FLAKY_EVERY: u64 = 10;
/// Hang load route: the sleep of every `FLAKY_EVERY`th request (longer than any test timeout).
pub const HANG_DELAY: Duration = Duration::from_secs(3);
/// Racy apply: how many requests must have read counter 0 before any of them may increment.
const RACY_GATE: u32 = 2;
/// Racy apply: longest wait for the gate. A sequential client times out here and the delta stays 1.
const RACY_GATE_TIMEOUT: Duration = Duration::from_secs(2);
/// Windowed login (Q3, burst): sliding window length for the time-based limiter.
pub const WINDOW: Duration = Duration::from_millis(300);
/// Windowed login: >= this many failed logins inside `WINDOW` trips 429.
pub const WINDOW_LIMIT: usize = 5;
/// Per-IP login (Q4, `baseline = "none"`): >= this many failed logins from the same peer
/// IP trips 429. Count-based (not timing), so it is deterministic for sequential AND burst.
pub const PER_IP_LIMIT: u32 = 5;

#[derive(Default)]
struct FixtureState {
    failed_logins: u32,
    /// action id -> execution_count
    actions: HashMap<String, u64>,
    load_requests: u64,
    /// Every request to the three login routes (also malformed ones). Read via /__admin/stats.
    login_requests: u64,
    /// Every request to the three apply routes (also 401). Read via /__admin/stats.
    action_requests: u64,
    /// Every GET /actions/{id} (also 401). Read via /__admin/stats.
    verify_requests: u64,
    /// action id -> number of racy requests that have read counter 0 (the explicit sync point).
    /// Reset drops the senders, which opens every gate.
    racy_seen_zero: HashMap<String, watch::Sender<u32>>,
    /// Windowed login (Q3, burst): timestamp of every failed login, oldest first.
    windowed_failed_logins: Vec<Instant>,
    /// Per-IP login (Q4): failed-login count per peer IP.
    per_ip_failures: HashMap<IpAddr, u32>,
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
            // `with_connect_info`: `/per-ip/login` (Q4) needs the real peer IP.
            let _ = axum::serve(
                listener,
                app.into_make_service_with_connect_info::<SocketAddr>(),
            )
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
        .route("/windowed/login", post(windowed_login))
        .route("/per-ip/login", post(per_ip_login))
        .route(
            "/broken/login/always-protected",
            post(login_always_protected),
        )
        .route("/actions/{id}/apply", post(apply))
        .route("/broken/actions/{id}/apply-racy", post(apply_racy))
        .route("/idempotent/actions/{id}/apply", post(apply_idempotent))
        .route("/actions/{id}", get(read_action))
        .route("/broken/actions/{id}/verify-denied", get(verify_denied))
        .route("/broken/actions/{id}/verify-not-json", get(verify_not_json))
        .route(
            "/broken/actions/{id}/verify-no-counter",
            get(verify_no_counter),
        )
        .route(
            "/broken/actions/{id}/verify-bad-counter",
            get(verify_bad_counter),
        )
        .route("/critical", get(critical))
        .route("/broken/critical/slow", get(critical_slow))
        .route("/broken/critical/flaky", get(critical_flaky))
        .route("/broken/critical/hang", get(critical_hang))
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

/// Broken variant: wrong credentials are always 429, regardless of count (proves the burst
/// `protected == attempts` line, `account_already_locked`).
async fn login_always_protected(State(state): State<Shared>, raw: Bytes) -> Reply {
    let body = match count_and_parse_login(&state, &raw) {
        Ok(body) => body,
        Err(rejection) => return rejection,
    };
    if body.username == LOGIN_USER && body.password == LOGIN_VALID_PASSWORD {
        token_reply()
    } else {
        reply(
            StatusCode::TOO_MANY_REQUESTS,
            json!({"error": "too_many_attempts"}),
        )
    }
}

/// Count of failed logins recorded within the last `WINDOW`, pruning older entries in place.
/// `now` is passed in so a single instant is shared by the prune and the decision it feeds.
fn windowed_failures_within(times: &mut Vec<Instant>, now: Instant) -> usize {
    times.retain(|t| now.duration_since(*t) <= WINDOW);
    times.len()
}

/// Real, deterministic time-based limiter: >= `WINDOW_LIMIT` failed logins within `WINDOW`
/// trips 429. Sequential pacing (`interval_ms >= 150`) can never reach the limit inside a
/// 300 ms window; a burst of probes arriving within a few ms can. See docs/fixture.md.
async fn windowed_login(State(state): State<Shared>, raw: Bytes) -> Reply {
    let body = match count_and_parse_login(&state, &raw) {
        Ok(body) => body,
        Err(rejection) => return rejection,
    };
    let valid = body.username == LOGIN_USER && body.password == LOGIN_VALID_PASSWORD;
    let now = Instant::now();
    let mut s = lock(&state);
    let failures_in_window = windowed_failures_within(&mut s.windowed_failed_logins, now);
    if valid {
        if failures_in_window >= WINDOW_LIMIT {
            return reply(
                StatusCode::TOO_MANY_REQUESTS,
                json!({"error": "too_many_attempts"}),
            );
        }
        return token_reply();
    }
    if failures_in_window >= WINDOW_LIMIT {
        return reply(
            StatusCode::TOO_MANY_REQUESTS,
            json!({"error": "too_many_attempts"}),
        );
    }
    s.windowed_failed_logins.push(now);
    invalid_credentials()
}

/// Per-IP login (Q4, `baseline = "none"`): count-based limiter keyed by peer IP, so it
/// is deterministic for both sequential and burst probing (unlike `/windowed/login`,
/// which is time-based and only reachable by a burst). Valid credentials always succeed
/// and never affect the per-IP counter (that path exists for completeness; `baseline =
/// "none"` configs never send it). The lock is held across read + increment, so a burst
/// of concurrent probes from the same IP is atomic: exactly `PER_IP_LIMIT` failures ->
/// 429 from then on, never off-by-one under concurrency.
async fn per_ip_login(
    State(state): State<Shared>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    raw: Bytes,
) -> Reply {
    let body = match count_and_parse_login(&state, &raw) {
        Ok(body) => body,
        Err(rejection) => return rejection,
    };
    if body.username == LOGIN_USER && body.password == LOGIN_VALID_PASSWORD {
        return token_reply();
    }
    let mut s = lock(&state);
    let n = s.per_ip_failures.entry(addr.ip()).or_insert(0);
    if *n >= PER_IP_LIMIT {
        return reply(
            StatusCode::TOO_MANY_REQUESTS,
            json!({"error": "too_many_attempts"}),
        );
    }
    *n += 1;
    if *n >= PER_IP_LIMIT {
        reply(
            StatusCode::TOO_MANY_REQUESTS,
            json!({"error": "too_many_attempts"}),
        )
    } else {
        invalid_credentials()
    }
}

async fn apply(State(state): State<Shared>, Path(id): Path<String>, headers: HeaderMap) -> Reply {
    lock(&state).action_requests += 1;
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

/// Broken variant: check-then-act with an explicit synchronisation point instead of lucky timing.
/// A request that reads counter 0 waits until at least `RACY_GATE` requests have read 0, and only
/// then increments. Truly concurrent clients therefore always give delta >= 2. A sequential client
/// never fills the gate: it times out, the delta stays 1, and the duplicate test goes red.
async fn apply_racy(
    State(state): State<Shared>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Reply {
    lock(&state).action_requests += 1;
    if !is_authorized(&headers) {
        return unauthorized();
    }
    let mut gate = {
        let mut s = lock(&state);
        if s.actions.get(&id).copied().unwrap_or(0) > 0 {
            return reply(StatusCode::CONFLICT, json!({"error": "already_applied"}));
        }
        // Read 0: register at the gate. The lock is released before waiting.
        let sender = s
            .racy_seen_zero
            .entry(id.clone())
            .or_insert_with(|| watch::channel(0).0);
        sender.send_modify(|seen| *seen += 1);
        sender.subscribe()
    };
    wait_for_gate(&mut gate).await;
    *lock(&state).actions.entry(id).or_insert(0) += 1;
    reply(StatusCode::OK, json!({"applied": true}))
}

/// Returns when the gate is open, the sender is gone (reset), or the timeout passed.
async fn wait_for_gate(gate: &mut watch::Receiver<u32>) {
    let _ = timeout(RACY_GATE_TIMEOUT, gate.wait_for(|seen| *seen >= RACY_GATE)).await;
}

/// Healthy idempotent variant: every request gets 200, but the side effect happens once.
async fn apply_idempotent(
    State(state): State<Shared>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Reply {
    lock(&state).action_requests += 1;
    if !is_authorized(&headers) {
        return unauthorized();
    }
    lock(&state).actions.insert(id, 1);
    reply(StatusCode::OK, json!({"applied": true}))
}

async fn read_action(
    State(state): State<Shared>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Reply {
    lock(&state).verify_requests += 1;
    if !is_authorized(&headers) {
        return unauthorized();
    }
    let mut s = lock(&state);
    let count = *s.actions.entry(id.clone()).or_insert(0);
    reply(StatusCode::OK, json!({"id": id, "execution_count": count}))
}

// Broken verifiers: stateless, no Authorization needed, each counts only `verify_requests`.

/// Always 401, regardless of headers.
async fn verify_denied(State(state): State<Shared>) -> Reply {
    lock(&state).verify_requests += 1;
    unauthorized()
}

/// 200 with a non-JSON body.
async fn verify_not_json(State(state): State<Shared>) -> impl IntoResponse {
    lock(&state).verify_requests += 1;
    ([(header::CONTENT_TYPE, "text/plain")], "ok")
}

/// 200 JSON without `execution_count`.
async fn verify_no_counter(State(state): State<Shared>, Path(id): Path<String>) -> Reply {
    lock(&state).verify_requests += 1;
    reply(StatusCode::OK, json!({"id": id}))
}

/// 200 JSON where `execution_count` is a string, not an integer.
async fn verify_bad_counter(State(state): State<Shared>, Path(id): Path<String>) -> Reply {
    lock(&state).verify_requests += 1;
    reply(StatusCode::OK, json!({"id": id, "execution_count": "0"}))
}

/// Counts a load request (also a 401 one) and returns its 1-based number.
fn count_load(state: &Shared) -> u64 {
    let mut s = lock(state);
    s.load_requests += 1;
    s.load_requests
}

fn ok_reply() -> Reply {
    reply(StatusCode::OK, json!({"ok": true}))
}

async fn critical(State(state): State<Shared>, headers: HeaderMap) -> Reply {
    count_load(&state);
    if !is_authorized(&headers) {
        return unauthorized();
    }
    ok_reply()
}

/// Broken variant: every answer is 200 but only after `SLOW_DELAY`.
async fn critical_slow(State(state): State<Shared>, headers: HeaderMap) -> Reply {
    count_load(&state);
    if !is_authorized(&headers) {
        return unauthorized();
    }
    tokio::time::sleep(SLOW_DELAY).await;
    ok_reply()
}

/// Broken variant: every `FLAKY_EVERY`th load request (by counter) is 500, the rest 200.
async fn critical_flaky(State(state): State<Shared>, headers: HeaderMap) -> Reply {
    let n = count_load(&state);
    if !is_authorized(&headers) {
        return unauthorized();
    }
    if n.is_multiple_of(FLAKY_EVERY) {
        reply(StatusCode::INTERNAL_SERVER_ERROR, json!({"error": "flaky"}))
    } else {
        ok_reply()
    }
}

/// Broken variant: every `FLAKY_EVERY`th load request sleeps `HANG_DELAY` (a client timeout fires first), the rest 200 at once.
async fn critical_hang(State(state): State<Shared>, headers: HeaderMap) -> Reply {
    let n = count_load(&state);
    if !is_authorized(&headers) {
        return unauthorized();
    }
    if n.is_multiple_of(FLAKY_EVERY) {
        tokio::time::sleep(HANG_DELAY).await;
    }
    ok_reply()
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
        json!({
            "login_requests": s.login_requests,
            "load_requests": s.load_requests,
            "action_requests": s.action_requests,
            "verify_requests": s.verify_requests,
        }),
    )
}

async fn not_found() -> Reply {
    reply(StatusCode::NOT_FOUND, json!({"error": "not_found"}))
}
