//! Spike: proves the library and the planned dependencies build, link and initialise.

// Module paths exist (empty stubs for now).
#[allow(unused_imports)]
use api_toolkit::{
    checks::{brute_force, load, race},
    cli, config, http, matchers, report,
};

use sha2::{Digest, Sha256};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

#[tokio::test]
async fn dependencies_compile_and_run() {
    // reqwest with rustls (aws-lc-rs): building the client initialises the TLS stack.
    reqwest::Client::builder().build().expect("rustls client");

    // sha2 0.11: hex via byte loop, as planned.
    let digest = Sha256::digest(b"abc");
    let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    assert!(hex.starts_with("ba7816bf"));

    // time: RFC3339 formatting.
    assert!(OffsetDateTime::now_utc().format(&Rfc3339).is_ok());

    // clap derive-free builder, serde_json, axum, tempfile.
    clap::Command::new("api-toolkit").debug_assert();
    assert_eq!(
        serde_json::json!({"a": 1}).pointer("/a"),
        Some(&serde_json::json!(1))
    );
    let _router: axum::Router = axum::Router::new();
    tempfile::tempdir().expect("tempdir");
}
