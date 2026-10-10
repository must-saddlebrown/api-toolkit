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
