//! End to end: config parse -> resolve -> build URL -> transport -> local healthy fixture.
//! Only 127.0.0.1 (ephemeral ports).
mod support;
use std::sync::Arc;

use api_toolkit::config::{self, Config, Secret};
use api_toolkit::http::request::ResolvedRequest;
use api_toolkit::http::{Transport, TransportResponse, send};
use api_toolkit::report::TransportErrorCode;
use support::cli::{example_text, with_base_url};
use support::fixture::*;

struct Env {
    cfg: Config,
    reqs: Arc<config::ResolvedRequests>,
    transport: Arc<Transport>,
}

fn setup(base_url: &str) -> Env {
    let cfg = config::parse(&with_base_url(&example_text(), base_url)).unwrap();
    config::validate(&cfg, config::Selection::All).unwrap();
    let lookup = |name: &str| -> Option<String> {
        match name {
            "LOGIN_USER" => Some(LOGIN_USER),
            "LOGIN_VALID_PASSWORD" => Some(LOGIN_VALID_PASSWORD),
            "LOGIN_INVALID_PASSWORD" => Some(LOGIN_INVALID_PASSWORD),
            "TEST_ACTION_ID" => Some(TEST_ACTION_ID),
            "TEST_AUTHORIZATION" => Some(TEST_AUTHORIZATION),
            _ => None,
        }
        .map(String::from)
    };
    let reqs = Arc::new(config::resolve(&cfg, config::Selection::All, &lookup).unwrap());
    let transport = Arc::new(Transport::new(&cfg.limits).unwrap());
    Env {
        cfg,
        reqs,
        transport,
    }
}

async fn go(env: &Env, r: &ResolvedRequest) -> TransportResponse {
    send(&env.transport, r).await.unwrap()
}

fn reset_request(base: &str) -> ResolvedRequest {
    ResolvedRequest {
        method: "POST".into(),
        url: Secret::new(format!("{base}/__admin/reset")),
        headers: vec![],
        body: None,
    }
}

fn counter(r: &TransportResponse) -> i64 {
    r.body
        .as_ref()
        .unwrap()
        .pointer("/execution_count")
        .unwrap()
        .as_i64()
        .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pipeline_against_healthy_fixture() {
    let fx = Fixture::start(0).await.unwrap();
    assert!(fx.addr.ip().is_loopback()); // P0-AC17
    let base = fx.base_url();
    let env = setup(&base);
    let bf = env.cfg.brute_force.as_ref().unwrap();
    let bfr = env.reqs.brute_force.as_ref().unwrap();
    let racer = env.reqs.race.as_ref().unwrap();

    let bfr_baseline = bfr.baseline.as_ref().expect("baseline is require_success");
    // Login valid: 200 + /access_token, body parsed as JSON, elapsed set.
    let r = go(&env, bfr_baseline).await;
    assert_eq!(r.status, 200);
    assert!(bf.success_match.matches(r.status, r.body.as_ref()));
    assert!(r.body.is_some());
    assert!(r.elapsed > std::time::Duration::ZERO);

    // Five invalid logins: 401 and rejection match; the sixth: 429 and protection match.
    for _ in 0..5 {
        let r = go(&env, &bfr.probe).await;
        assert_eq!(r.status, 401);
        assert!(bf.rejection_match.matches(r.status, r.body.as_ref()));
        assert!(!bf.success_match.matches(r.status, r.body.as_ref()));
    }
    let r = go(&env, &bfr.probe).await;
    assert_eq!(r.status, 429);
    assert!(bf.protection_match.matches(r.status, r.body.as_ref()));

    // Verify before: 0, with matcher.
    let race = env.cfg.race.as_ref().unwrap();
    let r = go(&env, &racer.verify).await;
    assert!(race.verify_match.matches(r.status, r.body.as_ref()));
    assert_eq!(counter(&r), 0);

    // Apply: 200 then 409, both in expected_action_statuses.
    let a1 = go(&env, &racer.action).await;
    let a2 = go(&env, &racer.action).await;
    assert_eq!((a1.status, a2.status), (200, 409));
    assert!(race.expected_action_statuses.contains(&a1.status));
    assert!(race.expected_action_statuses.contains(&a2.status));
    let r = go(&env, &racer.verify).await;
    assert!(race.verify_match.matches(r.status, r.body.as_ref()));
    assert_eq!(counter(&r), 1);

    // Load endpoint.
    let r = go(&env, env.reqs.load.as_ref().unwrap()).await;
    assert_eq!(r.status, 200);
    assert!(
        env.cfg
            .load
            .as_ref()
            .unwrap()
            .response_match
            .matches(r.status, r.body.as_ref())
    );

    // Reset restores state (P0-AC19): counter 0, failed login count 0.
    assert_eq!(go(&env, &reset_request(&base)).await.status, 200);
    assert_eq!(counter(&go(&env, &racer.verify).await), 0);
    assert_eq!(go(&env, bfr_baseline).await.status, 200);
    assert_eq!(go(&env, &bfr.probe).await.status, 401); // first failure again

    // Debug of a response never shows body text.
    let dbg = format!("{:?}", go(&env, bfr_baseline).await);
    assert!(!dbg.contains(ACCESS_TOKEN), "{dbg}");
    assert!(!dbg.contains("access_token"), "{dbg}");

    fx.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn thirty_two_concurrent_applies_yield_one_success() {
    let fx = Fixture::start(0).await.unwrap();
    let env = setup(&fx.base_url());

    let mut tasks = Vec::new();
    for _ in 0..32 {
        let (t, reqs) = (env.transport.clone(), env.reqs.clone());
        tasks.push(tokio::spawn(async move {
            send(&t, &reqs.race.as_ref().unwrap().action)
                .await
                .unwrap()
                .status
        }));
    }
    let mut statuses = Vec::new();
    for t in tasks {
        statuses.push(t.await.unwrap());
    }
    assert_eq!(
        statuses.iter().filter(|s| **s == 200).count(),
        1,
        "{statuses:?}"
    );
    assert_eq!(
        statuses.iter().filter(|s| **s == 409).count(),
        31,
        "{statuses:?}"
    );
    assert_eq!(
        counter(&go(&env, &env.reqs.race.as_ref().unwrap().verify).await),
        1
    );

    fx.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn closed_port_is_connect_failed_without_url() {
    // Bind then drop: the port is (almost certainly) closed right after.
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let env = setup(&format!("http://127.0.0.1:{port}"));
    let err = send(
        &env.transport,
        env.reqs
            .brute_force
            .as_ref()
            .unwrap()
            .baseline
            .as_ref()
            .expect("baseline is require_success"),
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, TransportErrorCode::ConnectFailed);
    for text in [format!("{err}"), format!("{err:?}")] {
        assert!(!text.contains("127.0.0.1"), "{text}");
        assert!(!text.contains(&port.to_string()), "{text}");
        assert!(!text.contains("http"), "{text}");
        assert!(!text.contains("/login"), "{text}");
    }
}
