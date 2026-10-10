//! Config schema v1 (PLAN §9) and `parse`.
//!
//! This module only turns JSON text into typed structs. Semantic validation (limits,
//! pointers, overlaps) is slice S3; `${ENV:NAME}` resolution is slice S4, so values
//! like `"${ENV:LOGIN_USER}"` stay plain strings here.

use std::collections::BTreeMap;

use serde::Deserialize;
use serde_json::Value;

/// The only config version this build understands.
pub const SUPPORTED_VERSION: u32 = 1;

/// Errors from `parse`. Messages never contain field values, only field names,
/// line/column and the kind of problem, because values may be secret placeholders.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("config is not valid JSON (line {line}, column {column})")]
    Syntax { line: usize, column: usize },
    #[error("unsupported config_version {found}, expected {SUPPORTED_VERSION}")]
    UnsupportedVersion { found: u64 },
    #[error("config does not match schema v1: {detail} (line {line}, column {column})")]
    Schema {
        detail: String,
        line: usize,
        column: usize,
    },
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub config_version: u32,
    pub base_url: String,
    #[serde(default)]
    pub limits: Limits,
    pub brute_force: BruteForce,
    pub race: Race,
    pub load: Load,
}

/// Execution limits (PLAN §8). Every field has a default, and the whole
/// `limits` object may be omitted.
#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields, default)]
pub struct Limits {
    pub connect_timeout_ms: u64,
    pub request_timeout_ms: u64,
    pub suite_deadline_ms: u64,
    pub response_body_limit_bytes: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            connect_timeout_ms: 3_000,
            request_timeout_ms: 5_000,
            suite_deadline_ms: 120_000,
            response_body_limit_bytes: 1_048_576,
        }
    }
}

/// One HTTP request spec. Strings may still hold `${ENV:NAME}` placeholders.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequestSpec {
    pub method: String,
    pub path: String,
    pub path_params: Option<BTreeMap<String, String>>,
    pub query: Option<BTreeMap<String, String>>,
    pub headers: Option<BTreeMap<String, String>>,
    pub json: Option<Value>,
}

/// Response matcher (PLAN §4). `status_in` is required; the rest are optional and ANDed.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MatcherSpec {
    pub status_in: Vec<u16>,
    /// JSON Pointer -> expected JSON value.
    pub json_equals: Option<BTreeMap<String, Value>>,
    /// JSON Pointers that must exist.
    pub json_exists: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BruteForce {
    pub baseline_request: RequestSpec,
    pub probe_request: RequestSpec,
    pub success_match: MatcherSpec,
    pub rejection_match: MatcherSpec,
    pub protection_match: MatcherSpec,
    pub attempts: u32,
    pub interval_ms: u64,
    pub protect_by_attempt: u32,
    pub protected_tail_count: u32,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Race {
    pub action_request: RequestSpec,
    pub verify_request: RequestSpec,
    pub verify_match: MatcherSpec,
    pub counter_pointer: String,
    // Signed on purpose: S3 rejects values other than 0 / 1 with a clear message.
    pub initial_counter: i64,
    pub expected_delta: i64,
    pub concurrency: u32,
    pub expected_action_statuses: Vec<u16>,
    pub max_dispatch_skew_ms: u64,
    pub settle_ms: u64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Load {
    pub request: RequestSpec,
    pub response_match: MatcherSpec,
    pub requests: u32,
    pub concurrency: u32,
    pub max_error_rate: f64,
    pub max_p95_ms: u64,
}

/// Only reads `config_version`; other fields are ignored so a future-version
/// file reports "unsupported version" instead of a schema error.
#[derive(Deserialize)]
struct VersionProbe {
    config_version: Option<u64>,
}

pub fn parse(text: &str) -> Result<Config, ConfigError> {
    let probe: VersionProbe = serde_json::from_str(text).map_err(to_config_error)?;
    if let Some(found) = probe.config_version
        && found != u64::from(SUPPORTED_VERSION)
    {
        return Err(ConfigError::UnsupportedVersion { found });
    }
    serde_json::from_str(text).map_err(to_config_error)
}

fn to_config_error(err: serde_json::Error) -> ConfigError {
    let (line, column) = (err.line(), err.column());
    if err.is_syntax() || err.is_eof() || err.is_io() {
        return ConfigError::Syntax { line, column };
    }
    ConfigError::Schema {
        detail: safe_detail(&err.to_string()),
        line,
        column,
    }
}

/// serde messages like "invalid type: string \"hunter2\", expected u32" quote the
/// offending value. Keep only messages that name a field, or the "expected ..." tail.
fn safe_detail(message: &str) -> String {
    // Drop serde_json's " at line X column Y" suffix; we report line/column separately.
    let message = message
        .rfind(" at line ")
        .map_or(message, |i| &message[..i]);
    let first_line = message.lines().next().unwrap_or("");
    if first_line.starts_with("unknown field ")
        || first_line.starts_with("missing field ")
        || first_line.starts_with("duplicate field ")
    {
        return first_line.to_string();
    }
    match message.rfind(", expected ") {
        Some(i) => format!(
            "invalid value, expected {}",
            &message[i + ", expected ".len()..]
        ),
        None => "invalid value".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXAMPLE: &str = include_str!("../examples/core-checks.json");

    /// Replace `from` with `to` in the example so each test changes one thing.
    fn example_with(from: &str, to: &str) -> String {
        assert!(EXAMPLE.contains(from), "test setup: pattern not in example");
        EXAMPLE.replacen(from, to, 1)
    }

    #[test]
    fn plan_example_parses() {
        let cfg = parse(EXAMPLE).expect("example must parse");
        assert_eq!(cfg.config_version, 1);
        assert_eq!(cfg.base_url, "http://127.0.0.1:8080");
        assert_eq!(cfg.brute_force.attempts, 8);
        assert_eq!(cfg.race.expected_action_statuses, vec![200, 409]);
        assert_eq!(cfg.load.requests, 100);
        // Placeholders are kept raw until S4.
        let json = cfg.brute_force.baseline_request.json.expect("json body");
        assert_eq!(json["username"], "${ENV:LOGIN_USER}");
        // Limits given in the example are used; connect timeout falls back to default.
        assert_eq!(cfg.limits.request_timeout_ms, 5000);
        assert_eq!(cfg.limits.connect_timeout_ms, 3000);
    }

    #[test]
    fn unknown_field_at_root_is_rejected() {
        let text = example_with(
            "\"config_version\": 1,",
            "\"config_version\": 1, \"extra\": 1,",
        );
        let err = parse(&text).unwrap_err();
        assert!(err.to_string().contains("unknown field `extra`"), "{err}");
    }

    #[test]
    fn unknown_field_nested_is_rejected() {
        let text = example_with("\"attempts\": 8,", "\"attempts\": 8, \"bogus\": true,");
        let err = parse(&text).unwrap_err();
        assert!(err.to_string().contains("unknown field `bogus`"), "{err}");

        let text = example_with("\"status_in\": [429]", "\"status_in\": [429], \"nope\": 1");
        assert!(parse(&text).is_err());

        let text = example_with(
            "\"request_timeout_ms\": 5000,",
            "\"request_timeout_ms\": 5000, \"x\": 1,",
        );
        assert!(parse(&text).is_err());
    }

    #[test]
    fn config_version_2_is_rejected() {
        let text = example_with("\"config_version\": 1", "\"config_version\": 2");
        assert!(matches!(
            parse(&text),
            Err(ConfigError::UnsupportedVersion { found: 2 })
        ));
    }

    #[test]
    fn missing_required_field_is_rejected() {
        let text = example_with("\"counter_pointer\": \"/execution_count\",", "");
        let err = parse(&text).unwrap_err();
        assert!(
            err.to_string().contains("missing field `counter_pointer`"),
            "{err}"
        );

        // A missing `status_in` in a matcher is also a parse error.
        let text = example_with(
            "\"rejection_match\": {\"status_in\": [401]}",
            "\"rejection_match\": {}",
        );
        assert!(parse(&text).is_err());
    }

    #[test]
    fn missing_limits_get_defaults() {
        let start = EXAMPLE.find("\"limits\"").expect("limits in example");
        let end = EXAMPLE
            .find("\"brute_force\"")
            .expect("brute_force in example");
        let text = format!("{}{}", &EXAMPLE[..start], &EXAMPLE[end..]);
        let cfg = parse(&text).expect("limits are optional");
        assert_eq!(
            cfg.limits,
            Limits {
                connect_timeout_ms: 3000,
                request_timeout_ms: 5000,
                suite_deadline_ms: 120_000,
                response_body_limit_bytes: 1_048_576,
            }
        );
    }

    #[test]
    fn errors_do_not_echo_values() {
        let sentinel = "SENTINEL-SECRET-VALUE";
        let text = example_with("\"attempts\": 8", &format!("\"attempts\": \"{sentinel}\""));
        let err = parse(&text).unwrap_err();
        assert!(!err.to_string().contains(sentinel), "{err}");
        assert!(!format!("{err:?}").contains(sentinel));

        let err = parse("{ not json SENTINEL").unwrap_err();
        assert!(!err.to_string().contains("SENTINEL"));
        assert!(matches!(err, ConfigError::Syntax { .. }));
    }
}
