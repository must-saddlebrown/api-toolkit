//! The one place that talks HTTP (PLAN §8): one request, one connection attempt,
//! timeouts, streaming body limit, no redirects, no retry, TLS verification on.
//!
//! Errors are reduced to a `TransportErrorCode` on purpose: reqwest error text embeds
//! the URL, which may contain secrets, so it is never turned into a string.

use std::fmt;
use std::net::SocketAddr;
use std::time::{Duration, Instant};

use reqwest::header::{CONTENT_TYPE, HeaderMap, HeaderName, HeaderValue};
use reqwest::{Client, Method, Url, redirect};
use serde_json::Value;

use super::request::ResolvedRequest;
use crate::config::Limits;
use crate::report::TransportErrorCode;

/// A configured HTTP client plus the body limit. Build it once, share it by reference.
pub struct Transport {
    client: Client,
    body_limit: u64,
}

/// Why a request produced no usable response. Carries only the sanitized code.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
#[error("transport error: {code}")]
pub struct TransportError {
    pub code: TransportErrorCode,
}

impl From<TransportErrorCode> for TransportError {
    fn from(code: TransportErrorCode) -> Self {
        TransportError { code }
    }
}

/// What came back. `body` is `None` when the body was empty or not JSON; callers pass
/// `body.as_ref()` to the matcher. A body over the limit is an error, never a
/// truncated `Some`. Raw body text is dropped right after parsing.
pub struct TransportResponse {
    pub status: u16,
    /// Time from sending until the whole body was read (monotonic clock).
    pub elapsed: Duration,
    pub body: Option<Value>,
    /// Peer address of the connection this response came from (DECISIONS 2026-10-14 Q4).
    /// `None` only when reqwest cannot report it; never carries anything but IP + port.
    pub remote_addr: Option<SocketAddr>,
}

// Manual `Debug`: a parsed body may hold secrets, so only say whether one exists.
// `remote_addr` is plain IP + port (never secret) and is shown as-is.
impl fmt::Debug for TransportResponse {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TransportResponse")
            .field("status", &self.status)
            .field("elapsed", &self.elapsed)
            .field("body", &self.body.as_ref().map(|_| "<redacted>"))
            .field("remote_addr", &self.remote_addr)
            .finish()
    }
}

impl Transport {
    pub fn new(limits: &Limits) -> Result<Transport, TransportError> {
        let client = Client::builder()
            .connect_timeout(Duration::from_millis(limits.connect_timeout_ms))
            // Total timeout: covers connect, headers and reading the body.
            .timeout(Duration::from_millis(limits.request_timeout_ms))
            .redirect(redirect::Policy::none())
            // Do not let HTTP(S)_PROXY from the environment redirect traffic.
            .no_proxy()
            // ponytail: no idle connection reuse, so a stale pooled connection can never
            // trigger a hidden second attempt. P3 keeps pooling off (each load request opens a
            // new connection, so its time includes connect); enable only with proof that no
            // hidden retry on a stale connection can happen.
            .pool_max_idle_per_host(0)
            .build()
            .map_err(|_| TransportError::from(TransportErrorCode::Other))?;
        Ok(Transport {
            client,
            body_limit: limits.response_body_limit_bytes,
        })
    }
}

/// Send one request and read the response body, at most `body_limit` bytes.
pub async fn send(
    transport: &Transport,
    request: &ResolvedRequest,
) -> Result<TransportResponse, TransportError> {
    let invalid = TransportErrorCode::RequestInvalid;
    // Defensive: `Method::from_bytes` keeps case, and `post` is not `POST` on the wire.
    let method =
        Method::from_bytes(request.method.to_ascii_uppercase().as_bytes()).map_err(|_| invalid)?;
    let url = Url::parse(request.url.expose()).map_err(|_| invalid)?;

    let mut headers = HeaderMap::new();
    for (name, value) in &request.headers {
        let name = HeaderName::from_bytes(name.as_bytes()).map_err(|_| invalid)?;
        let mut value = HeaderValue::from_str(value.expose()).map_err(|_| invalid)?;
        value.set_sensitive(true);
        headers.insert(name, value);
    }

    // A user-supplied Content-Type wins; never send two.
    let add_json_type = request.body.is_some() && !headers.contains_key(CONTENT_TYPE);
    let mut builder = transport.client.request(method, url).headers(headers);
    if add_json_type {
        builder = builder.header(CONTENT_TYPE, "application/json");
    }
    if let Some(body) = &request.body {
        builder = builder.body(body.expose().to_string());
    }

    let start = Instant::now();
    let mut response = builder.send().await.map_err(map_error)?;
    let status = response.status().as_u16();
    // Captured before the body is streamed, while `response` is still the live connection.
    let remote_addr = response.remote_addr();

    // Early reject: a declared length over the limit needs no body read.
    if response
        .content_length()
        .is_some_and(|len| len > transport.body_limit)
    {
        return Err(TransportErrorCode::BodyTooLarge.into());
    }

    // Read chunk by chunk and stop as soon as the limit is crossed, so at most
    // `limit + one chunk` bytes are ever held.
    let mut bytes: Vec<u8> = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(map_error)? {
        if (bytes.len() as u64) + (chunk.len() as u64) > transport.body_limit {
            return Err(TransportErrorCode::BodyTooLarge.into());
        }
        bytes.extend_from_slice(&chunk);
    }
    let elapsed = start.elapsed();

    // Empty or non-JSON body => None (the matcher treats that as "no match").
    let body = serde_json::from_slice::<Value>(&bytes).ok();
    Ok(TransportResponse {
        status,
        elapsed,
        body,
        remote_addr,
    })
}

/// Map a reqwest error by kind. Never call `to_string()`/`{}` on it: it contains the URL.
fn map_error(error: reqwest::Error) -> TransportError {
    let code = if error.is_timeout() {
        TransportErrorCode::Timeout
    } else if error.is_connect() {
        // ponytail: reqwest does not expose TLS failures as a separate kind, so they
        // land here; `tls_failed` stays unused until a reliable signal exists.
        TransportErrorCode::ConnectFailed
    } else if error.is_builder() {
        TransportErrorCode::RequestInvalid
    } else if error.is_body() || error.is_decode() {
        TransportErrorCode::BodyReadFailed
    } else {
        // Includes redirect errors (policy is `none`, so they should not occur).
        TransportErrorCode::Other
    };
    TransportError { code }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};

    use super::*;
    use crate::config::Secret;

    const SENTINEL: &str = "SENTINEL-s3cr3t";

    /// What the raw test server does with each accepted connection.
    #[derive(Clone)]
    enum Behavior {
        /// Read the request, then write these bytes.
        Respond(Vec<u8>),
        /// Close immediately without reading or answering.
        Drop,
        /// Read the request, then never answer.
        Hang,
        /// Answer with an endless chunked body (100 bytes per chunk) until the client leaves.
        EndlessChunked,
        /// Declare 1000 body bytes, then send one byte every 50 ms.
        SlowDrip,
    }

    struct TestServer {
        port: u16,
        accepted: Arc<AtomicUsize>,
        requests: Arc<Mutex<Vec<String>>>,
    }

    impl TestServer {
        fn url(&self, path: &str) -> String {
            format!("http://127.0.0.1:{}{}", self.port, path)
        }
    }

    fn http_response(status_line: &str, extra: &str, body: &[u8]) -> Vec<u8> {
        let mut out = format!(
            "HTTP/1.1 {status_line}\r\n{extra}Content-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        )
        .into_bytes();
        out.extend_from_slice(body);
        out
    }

    /// Read headers, then `Content-Length` bytes of body. Returns the raw request text.
    async fn read_request(stream: &mut TcpStream) -> String {
        let mut buf = Vec::new();
        let mut tmp = [0u8; 1024];
        loop {
            if let Some(end) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                let head = String::from_utf8_lossy(&buf[..end]).to_lowercase();
                let len = head
                    .lines()
                    .find_map(|l| l.strip_prefix("content-length:"))
                    .and_then(|v| v.trim().parse::<usize>().ok())
                    .unwrap_or(0);
                if buf.len() >= end + 4 + len {
                    break;
                }
            }
            match stream.read(&mut tmp).await {
                Ok(0) | Err(_) => break,
                Ok(n) => buf.extend_from_slice(&tmp[..n]),
            }
        }
        String::from_utf8_lossy(&buf).to_string()
    }

    async fn spawn_server(behavior: Behavior) -> TestServer {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let accepted = Arc::new(AtomicUsize::new(0));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let (acc, reqs) = (accepted.clone(), requests.clone());
        tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    break;
                };
                acc.fetch_add(1, Ordering::SeqCst);
                let behavior = behavior.clone();
                let reqs = reqs.clone();
                tokio::spawn(async move {
                    match behavior {
                        Behavior::Drop => drop(stream),
                        Behavior::Respond(bytes) => {
                            let req = read_request(&mut stream).await;
                            reqs.lock().unwrap().push(req);
                            let _ = stream.write_all(&bytes).await;
                            let _ = stream.shutdown().await;
                        }
                        Behavior::Hang => {
                            let req = read_request(&mut stream).await;
                            reqs.lock().unwrap().push(req);
                            tokio::time::sleep(Duration::from_secs(30)).await;
                        }
                        Behavior::EndlessChunked => {
                            let _ = read_request(&mut stream).await;
                            let head = "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n";
                            if stream.write_all(head.as_bytes()).await.is_err() {
                                return;
                            }
                            let chunk = format!("64\r\n{}\r\n", "a".repeat(100));
                            // Ends when the client closes the connection (write error).
                            while stream.write_all(chunk.as_bytes()).await.is_ok() {}
                        }
                        Behavior::SlowDrip => {
                            let _ = read_request(&mut stream).await;
                            let head = "HTTP/1.1 200 OK\r\nContent-Length: 1000\r\n\r\n";
                            if stream.write_all(head.as_bytes()).await.is_err() {
                                return;
                            }
                            for _ in 0..1000 {
                                if stream.write_all(b"a").await.is_err() {
                                    return;
                                }
                                tokio::time::sleep(Duration::from_millis(50)).await;
                            }
                        }
                    }
                });
            }
        });
        TestServer {
            port,
            accepted,
            requests,
        }
    }

    fn get(url: String) -> ResolvedRequest {
        ResolvedRequest {
            method: "GET".into(),
            url: Secret::new(url),
            headers: vec![],
            body: None,
        }
    }

    fn transport(request_timeout_ms: u64, body_limit: u64) -> Transport {
        Transport::new(&Limits {
            request_timeout_ms,
            response_body_limit_bytes: body_limit,
            ..Limits::default()
        })
        .unwrap()
    }

    #[tokio::test]
    async fn json_body_is_parsed() {
        let server = spawn_server(Behavior::Respond(http_response(
            "200 OK",
            "",
            br#"{"ok":true}"#,
        )))
        .await;
        let res = send(&transport(2000, 1024), &get(server.url("/x")))
            .await
            .unwrap();
        assert_eq!(res.status, 200);
        assert_eq!(res.body, Some(serde_json::json!({"ok": true})));
        assert!(res.elapsed < Duration::from_secs(2));
    }

    /// DECISIONS 2026-10-14 Q4: `remote_addr` is the real peer of a loopback connection,
    /// and that peer is `127.0.0.1` (IPv4), never a secret, on both success and error.
    #[tokio::test]
    async fn remote_addr_is_the_real_loopback_peer() {
        let server = spawn_server(Behavior::Respond(http_response("200 OK", "", b"{}"))).await;
        let res = send(&transport(2000, 1024), &get(server.url("/")))
            .await
            .unwrap();
        let addr = res.remote_addr.expect("remote_addr set on success");
        assert!(addr.ip().is_loopback());
        assert!(addr.is_ipv4());
        assert_eq!(addr.port(), server.port);

        // A transport error never returns a TransportResponse, so there is no
        // `remote_addr` to leak: the sentinel case is `closed_port_is_connect_failed_...`.
        let dbg = format!("{res:?}");
        assert!(dbg.contains("remote_addr"));
        assert!(!dbg.contains("SENTINEL"));
    }

    #[tokio::test]
    async fn non_json_and_empty_body_are_none() {
        for body in [&b"hello <b>not json</b>"[..], &b""[..]] {
            let server = spawn_server(Behavior::Respond(http_response("200 OK", "", body))).await;
            let res = send(&transport(2000, 1024), &get(server.url("/")))
                .await
                .unwrap();
            assert_eq!(res.status, 200);
            assert!(res.body.is_none());
        }
    }

    #[tokio::test]
    async fn redirect_is_not_followed() {
        let target = spawn_server(Behavior::Respond(http_response("200 OK", "", b"{}"))).await;
        let location = format!("Location: {}\r\n", target.url("/other"));
        let server = spawn_server(Behavior::Respond(http_response(
            "302 Found",
            &location,
            b"",
        )))
        .await;
        let res = send(&transport(2000, 1024), &get(server.url("/")))
            .await
            .unwrap();
        assert_eq!(res.status, 302);
        assert_eq!(server.accepted.load(Ordering::SeqCst), 1);
        assert_eq!(target.accepted.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn redirect_to_self_is_not_followed() {
        // Bind first so the Location can point back at the server itself.
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let accepted = Arc::new(AtomicUsize::new(0));
        let acc = accepted.clone();
        tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                acc.fetch_add(1, Ordering::SeqCst);
                let _ = read_request(&mut stream).await;
                let loc = format!("Location: http://127.0.0.1:{port}/\r\n");
                let _ = stream
                    .write_all(&http_response("302 Found", &loc, b""))
                    .await;
                let _ = stream.shutdown().await;
            }
        });
        let url = format!("http://127.0.0.1:{port}/");
        let res = send(&transport(2000, 1024), &get(url)).await.unwrap();
        assert_eq!(res.status, 302);
        assert_eq!(accepted.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn body_over_limit_is_body_too_large() {
        let big = vec![b'a'; 5000];
        let server = spawn_server(Behavior::Respond(http_response("200 OK", "", &big))).await;
        let err = send(&transport(2000, 1000), &get(server.url("/")))
            .await
            .unwrap_err();
        assert_eq!(err.code, TransportErrorCode::BodyTooLarge);
    }

    #[tokio::test]
    async fn body_exactly_at_limit_is_accepted() {
        let body = br#"{"a":"bbbb"}"#;
        let server = spawn_server(Behavior::Respond(http_response("200 OK", "", body))).await;
        let limit = body.len() as u64;
        let res = send(&transport(2000, limit), &get(server.url("/")))
            .await
            .unwrap();
        assert!(res.body.is_some());
    }

    #[tokio::test]
    async fn slow_server_is_timeout() {
        let server = spawn_server(Behavior::Hang).await;
        let err = send(&transport(200, 1024), &get(server.url("/")))
            .await
            .unwrap_err();
        assert_eq!(err.code, TransportErrorCode::Timeout);
    }

    #[tokio::test]
    async fn closed_port_is_connect_failed_without_url_in_text() {
        // Bind then drop: the port is free again, so nothing listens on it.
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let url = format!("http://127.0.0.1:{port}/secret-path");
        let err = send(&transport(2000, 1024), &get(url)).await.unwrap_err();
        assert_eq!(err.code, TransportErrorCode::ConnectFailed);
        for text in [err.to_string(), format!("{err:?}")] {
            assert!(!text.contains(&port.to_string()), "{text}");
            assert!(
                !text.contains("127.0.0.1") && !text.contains("secret-path"),
                "{text}"
            );
        }
    }

    #[tokio::test]
    async fn dropped_connection_is_not_retried() {
        let server = spawn_server(Behavior::Drop).await;
        let result = send(&transport(2000, 1024), &get(server.url("/"))).await;
        assert!(result.is_err());
        assert_eq!(server.accepted.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn method_headers_and_json_body_arrive_as_sent() {
        let server = spawn_server(Behavior::Respond(http_response("200 OK", "", b"{}"))).await;
        let req = ResolvedRequest {
            method: "POST".into(),
            url: Secret::new(server.url("/items/1?a=b")),
            headers: vec![("X-Test".into(), Secret::new("hello".into()))],
            body: Some(Secret::new(r#"{"k":"v"}"#.into())),
        };
        send(&transport(2000, 1024), &req).await.unwrap();
        let seen = server.requests.lock().unwrap()[0].clone();
        let lower = seen.to_lowercase();
        assert!(seen.starts_with("POST /items/1?a=b HTTP/1.1"), "{seen}");
        assert!(lower.contains("x-test: hello"), "{seen}");
        assert!(lower.contains("content-type: application/json"), "{seen}");
        assert!(seen.ends_with(r#"{"k":"v"}"#), "{seen}");
    }

    async fn post_with_headers(headers: Vec<(String, Secret)>) -> String {
        let server = spawn_server(Behavior::Respond(http_response("200 OK", "", b"{}"))).await;
        let req = ResolvedRequest {
            method: "POST".into(),
            url: Secret::new(server.url("/")),
            headers,
            body: Some(Secret::new(r#"{"k":"v"}"#.into())),
        };
        send(&transport(2000, 1024), &req).await.unwrap();
        let seen = server.requests.lock().unwrap()[0].clone();
        seen.to_lowercase()
    }

    #[tokio::test]
    async fn user_content_type_is_sent_exactly_once() {
        let seen = post_with_headers(vec![(
            "Content-Type".into(),
            Secret::new("application/vnd.x+json".into()),
        )])
        .await;
        assert_eq!(seen.matches("content-type:").count(), 1, "{seen}");
        assert!(
            seen.contains("content-type: application/vnd.x+json"),
            "{seen}"
        );
    }

    #[tokio::test]
    async fn default_content_type_is_json_when_body_and_none_given() {
        let seen = post_with_headers(vec![]).await;
        assert_eq!(seen.matches("content-type:").count(), 1, "{seen}");
        assert!(seen.contains("content-type: application/json"), "{seen}");
    }

    #[tokio::test]
    async fn lowercase_method_is_sent_uppercase() {
        let server = spawn_server(Behavior::Respond(http_response("200 OK", "", b"{}"))).await;
        let mut req = get(server.url("/m"));
        req.method = "post".into();
        send(&transport(2000, 1024), &req).await.unwrap();
        let seen = server.requests.lock().unwrap()[0].clone();
        assert!(seen.starts_with("POST /m HTTP/1.1"), "{seen}");
    }

    #[tokio::test]
    async fn endless_chunked_body_stops_with_body_too_large() {
        let server = spawn_server(Behavior::EndlessChunked).await;
        let result = tokio::time::timeout(
            Duration::from_secs(5),
            send(&transport(4000, 1000), &get(server.url("/"))),
        )
        .await
        .expect("must not hang on an endless body");
        assert_eq!(result.unwrap_err().code, TransportErrorCode::BodyTooLarge);
    }

    #[tokio::test]
    async fn slow_drip_body_hits_total_timeout() {
        let server = spawn_server(Behavior::SlowDrip).await;
        let started = Instant::now();
        let err = send(&transport(300, 1_000_000), &get(server.url("/")))
            .await
            .unwrap_err();
        assert_eq!(err.code, TransportErrorCode::Timeout);
        // The drip alone would take 50 s; the total timeout must cut it short.
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    // ponytail: no connect-timeout test; a loopback connect never stalls reliably
    // (needs a non-routable address, i.e. external network behaviour).

    #[tokio::test]
    async fn dropped_post_is_not_retried() {
        let server = spawn_server(Behavior::Drop).await;
        let req = ResolvedRequest {
            method: "POST".into(),
            url: Secret::new(server.url("/")),
            headers: vec![],
            body: Some(Secret::new(r#"{"k":"v"}"#.into())),
        };
        assert!(send(&transport(2000, 1024), &req).await.is_err());
        // Give a hidden retry time to show up before counting.
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert_eq!(server.accepted.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn declared_length_over_limit_is_rejected_before_reading() {
        // Headers promise 5000 bytes, then the server closes without sending any. Only the
        // early check can answer body_too_large (a body read would fail instead).
        let head = b"HTTP/1.1 200 OK\r\nContent-Length: 5000\r\nConnection: close\r\n\r\n".to_vec();
        let server = spawn_server(Behavior::Respond(head)).await;
        let err = send(&transport(2000, 1000), &get(server.url("/")))
            .await
            .unwrap_err();
        assert_eq!(err.code, TransportErrorCode::BodyTooLarge);
    }

    #[tokio::test]
    async fn no_content_type_without_body() {
        let server = spawn_server(Behavior::Respond(http_response("200 OK", "", b"{}"))).await;
        send(&transport(2000, 1024), &get(server.url("/")))
            .await
            .unwrap();
        let seen = server.requests.lock().unwrap()[0].to_lowercase();
        assert!(!seen.contains("content-type"), "{seen}");
    }

    #[tokio::test]
    async fn invalid_method_and_header_value_are_request_invalid() {
        let t = transport(2000, 1024);
        let mut bad_method = get("http://127.0.0.1:1/".into());
        bad_method.method = "BAD METHOD".into();
        let mut bad_header = get("http://127.0.0.1:1/".into());
        bad_header.headers = vec![("X-A".into(), Secret::new(format!("{SENTINEL}\r\nX: 1")))];
        for req in [bad_method, bad_header] {
            let err = send(&t, &req).await.unwrap_err();
            assert_eq!(err.code, TransportErrorCode::RequestInvalid);
            assert!(!format!("{err} {err:?}").contains(SENTINEL));
        }
    }

    #[tokio::test]
    async fn sentinel_never_appears_in_response_or_error_text() {
        // Success path: the server echoes the sentinel in the body.
        let body = format!(r#"{{"echo":"{SENTINEL}"}}"#);
        let server = spawn_server(Behavior::Respond(http_response(
            "200 OK",
            "",
            body.as_bytes(),
        )))
        .await;
        let req = ResolvedRequest {
            method: "POST".into(),
            url: Secret::new(server.url(&format!("/p/{SENTINEL}"))),
            headers: vec![("X-Key".into(), Secret::new(SENTINEL.into()))],
            body: Some(Secret::new(format!(r#"{{"s":"{SENTINEL}"}}"#))),
        };
        let res = send(&transport(2000, 1024), &req).await.unwrap();
        assert!(res.body.is_some());
        assert!(!format!("{res:?}").contains(SENTINEL));

        // Error path: connection refused on a URL holding the sentinel.
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let req = ResolvedRequest {
            url: Secret::new(format!("http://127.0.0.1:{port}/p/{SENTINEL}")),
            ..req
        };
        let err = send(&transport(2000, 1024), &req).await.unwrap_err();
        assert!(!format!("{err} {err:?}").contains(SENTINEL));
    }
}
