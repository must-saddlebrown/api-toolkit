//! Small in-process smoke test of the healthy fixture (full suite is S10).
mod support;
use serde_json::{Value, json};
use support::fixture::*;

async fn login(c: &reqwest::Client, url: &str, pw: &str) -> u16 {
    c.post(format!("{url}/login"))
        .json(&json!({"username": LOGIN_USER, "password": pw}))
        .send()
        .await
        .unwrap()
        .status()
        .as_u16()
}

async fn apply(c: &reqwest::Client, url: &str) -> u16 {
    c.post(format!("{url}/actions/{TEST_ACTION_ID}/apply"))
        .header("Authorization", TEST_AUTHORIZATION)
        .send()
        .await
        .unwrap()
        .status()
        .as_u16()
}

async fn count(c: &reqwest::Client, url: &str) -> Value {
    let v: Value = c
        .get(format!("{url}/actions/{TEST_ACTION_ID}"))
        .header("Authorization", TEST_AUTHORIZATION)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    v["execution_count"].clone()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn fixture_healthy_behaviour() {
    let fx = Fixture::start(0).await.unwrap();
    assert!(fx.addr.ip().is_loopback());
    let url = fx.base_url();
    let c = reqwest::Client::builder().no_proxy().build().unwrap();

    assert_eq!(login(&c, &url, LOGIN_VALID_PASSWORD).await, 200);
    for _ in 0..5 {
        assert_eq!(login(&c, &url, LOGIN_INVALID_PASSWORD).await, 401);
    }
    assert_eq!(login(&c, &url, LOGIN_INVALID_PASSWORD).await, 429);

    // Valid login after protection started is still blocked.
    assert_eq!(login(&c, &url, LOGIN_VALID_PASSWORD).await, 429);

    // Missing/wrong auth; correct auth passes.
    let r = c
        .get(format!("{url}/critical"))
        .header("Authorization", TEST_AUTHORIZATION)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let r = c.get(format!("{url}/critical")).send().await.unwrap();
    assert_eq!(r.status(), 401);
    let r = c.get(format!("{url}/nope")).send().await.unwrap();
    assert_eq!(r.status(), 404);

    assert_eq!(count(&c, &url).await, json!(0));
    assert_eq!(apply(&c, &url).await, 200);
    assert_eq!(apply(&c, &url).await, 409);
    assert_eq!(count(&c, &url).await, json!(1));

    // Reset restores the initial state.
    let r = c.post(format!("{url}/__admin/reset")).send().await.unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(count(&c, &url).await, json!(0));
    assert_eq!(login(&c, &url, LOGIN_VALID_PASSWORD).await, 200);

    // Valid login (count < 6) does not reset the failed counter.
    for _ in 0..3 {
        assert_eq!(login(&c, &url, LOGIN_INVALID_PASSWORD).await, 401);
    }
    assert_eq!(login(&c, &url, LOGIN_VALID_PASSWORD).await, 200);
    assert_eq!(login(&c, &url, LOGIN_INVALID_PASSWORD).await, 401);
    assert_eq!(login(&c, &url, LOGIN_INVALID_PASSWORD).await, 401);
    assert_eq!(login(&c, &url, LOGIN_INVALID_PASSWORD).await, 429);

    // Wrong-typed / malformed bodies: fixed 400, input never echoed.
    c.post(format!("{url}/__admin/reset")).send().await.unwrap();
    for body in [r#"{"username":"demo","password":12345}"#, "not json"] {
        let r = c
            .post(format!("{url}/login"))
            .header("Content-Type", "application/json")
            .body(body)
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 400);
        let text = r.text().await.unwrap();
        assert_eq!(text, r#"{"error":"bad_request"}"#);
        assert!(!text.contains("12345"));
    }

    // Concurrent applies: exactly one 200, rest 409.
    c.post(format!("{url}/__admin/reset")).send().await.unwrap();
    let mut tasks = Vec::new();
    for _ in 0..32 {
        let (c, url) = (c.clone(), url.clone());
        tasks.push(tokio::spawn(async move { apply(&c, &url).await }));
    }
    let mut ok = 0;
    for t in tasks {
        match t.await.unwrap() {
            200 => ok += 1,
            409 => {}
            other => panic!("unexpected status {other}"),
        }
    }
    assert_eq!(ok, 1);
    assert_eq!(count(&c, &url).await, json!(1));

    fx.shutdown().await;
}

async fn post_login(c: &reqwest::Client, url: &str, path: &str, user: &str, pw: &str) -> Value {
    let resp = c
        .post(format!("{url}{path}"))
        .json(&json!({"username": user, "password": pw}))
        .send()
        .await
        .unwrap();
    let status = resp.status().as_u16();
    let body: Value = resp.json().await.unwrap();
    json!({"status": status, "body": body})
}

async fn get_stats(c: &reqwest::Client, url: &str) -> Value {
    c.get(format!("{url}/__admin/stats"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap()
}

fn client() -> reqwest::Client {
    reqwest::Client::builder().no_proxy().build().unwrap()
}

#[tokio::test]
async fn unprotected_never_returns_429() {
    let fx = Fixture::start(0).await.unwrap();
    let (c, url) = (client(), fx.base_url());
    for _ in 0..20 {
        let r = post_login(&c, &url, "/broken/login/unprotected", LOGIN_USER, "x").await;
        assert_eq!(r["status"], 401);
        assert_eq!(r["body"], json!({"error": "invalid_credentials"}));
    }
    let ok = post_login(
        &c,
        &url,
        "/broken/login/unprotected",
        LOGIN_USER,
        LOGIN_VALID_PASSWORD,
    )
    .await;
    assert_eq!(ok["status"], 200);
    fx.shutdown().await;
}

#[tokio::test]
async fn accepts_invalid_gives_200_for_wrong_password() {
    let fx = Fixture::start(0).await.unwrap();
    let (c, url) = (client(), fx.base_url());
    let path = "/broken/login/accepts-invalid";
    let r = post_login(&c, &url, path, LOGIN_USER, LOGIN_INVALID_PASSWORD).await;
    assert_eq!(r["status"], 200);
    assert_eq!(r["body"], json!({"access_token": ACCESS_TOKEN}));
    let other = post_login(&c, &url, path, "someone-else", "x").await;
    assert_eq!(other["status"], 401);
    fx.shutdown().await;
}

#[tokio::test]
async fn broken_routes_answer_bad_json_with_fixed_400() {
    let fx = Fixture::start(0).await.unwrap();
    let (c, url) = (client(), fx.base_url());
    for path in ["/broken/login/unprotected", "/broken/login/accepts-invalid"] {
        let resp = c
            .post(format!("{url}{path}"))
            .body("not-json-SENTINEL")
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status().as_u16(), 400);
        let text = resp.text().await.unwrap();
        assert_eq!(text, r#"{"error":"bad_request"}"#);
    }
    fx.shutdown().await;
}

#[tokio::test]
async fn stats_count_login_requests_on_all_login_routes() {
    let fx = Fixture::start(0).await.unwrap();
    let (c, url) = (client(), fx.base_url());
    assert_eq!(
        get_stats(&c, &url).await,
        json!({
            "login_requests": 0,
            "load_requests": 0,
            "action_requests": 0,
            "verify_requests": 0
        })
    );
    post_login(&c, &url, "/login", LOGIN_USER, "x").await;
    post_login(&c, &url, "/broken/login/unprotected", LOGIN_USER, "x").await;
    post_login(&c, &url, "/broken/login/accepts-invalid", LOGIN_USER, "x").await;
    // A malformed body still counts as a login request.
    c.post(format!("{url}/login"))
        .body("nope")
        .send()
        .await
        .unwrap();
    // Reading stats does not count.
    let stats = get_stats(&c, &url).await;
    assert_eq!(stats["login_requests"], 4);
    assert_eq!(get_stats(&c, &url).await["login_requests"], 4);
    fx.shutdown().await;
}

#[tokio::test]
async fn reset_zeroes_login_counter_and_healthy_state() {
    let fx = Fixture::start(0).await.unwrap();
    let (c, url) = (client(), fx.base_url());
    for _ in 0..PROTECT_BY_ATTEMPT {
        post_login(&c, &url, "/login", LOGIN_USER, "x").await;
    }
    assert_eq!(get_stats(&c, &url).await["login_requests"], 6);
    c.post(format!("{url}/__admin/reset")).send().await.unwrap();
    assert_eq!(get_stats(&c, &url).await["login_requests"], 0);
    let r = post_login(&c, &url, "/login", LOGIN_USER, "x").await;
    assert_eq!(r["status"], 401);
    fx.shutdown().await;
}

async fn post_apply(c: &reqwest::Client, url: &str, path: &str, auth: bool) -> u16 {
    let mut req = c.post(format!("{url}{path}"));
    if auth {
        req = req.header("Authorization", TEST_AUTHORIZATION);
    }
    req.send().await.unwrap().status().as_u16()
}

#[tokio::test]
async fn racy_route_duplicates_when_concurrent_and_idempotent_applies_once() {
    let fx = Fixture::start(0).await.unwrap();
    let (c, url) = (client(), fx.base_url());
    let path = "/broken/actions/r1/apply-racy";
    let (a, b, d) = tokio::join!(
        post_apply(&c, &url, path, true),
        post_apply(&c, &url, path, true),
        post_apply(&c, &url, path, true)
    );
    assert!([a, b, d].iter().filter(|s| **s == 200).count() >= 2);
    let v: Value = c
        .get(format!("{url}/actions/r1"))
        .header("Authorization", TEST_AUTHORIZATION)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(v["execution_count"].as_u64().unwrap() >= 2);
    // After the increments, a late request is 409.
    assert_eq!(post_apply(&c, &url, path, true).await, 409);
    assert_eq!(post_apply(&c, &url, path, false).await, 401);

    let idem = "/idempotent/actions/i1/apply";
    for _ in 0..3 {
        assert_eq!(post_apply(&c, &url, idem, true).await, 200);
    }
    assert_eq!(post_apply(&c, &url, idem, false).await, 401);
    let v: Value = c
        .get(format!("{url}/actions/i1"))
        .header("Authorization", TEST_AUTHORIZATION)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(v["execution_count"], 1);

    let stats = get_stats(&c, &url).await;
    assert_eq!(stats["action_requests"], 3 + 2 + 3 + 1);
    assert_eq!(stats["verify_requests"], 2);
    c.post(format!("{url}/__admin/reset")).send().await.unwrap();
    let stats = get_stats(&c, &url).await;
    assert_eq!(stats["action_requests"], 0);
    assert_eq!(stats["verify_requests"], 0);
    assert_eq!(count(&c, &url).await, 0);
    fx.shutdown().await;
}
