//! Semantic validation of a parsed `Config` (PLAN §4, §5, §8, §9).
//!
//! `parse` only checks shape; `validate` checks meaning: limits, thresholds, JSON
//! pointers, status overlap, URL/path rules and fixed waits. It stops at the first
//! problem. Error messages name the field path and the rule, never a config value.

use std::collections::BTreeSet;

use reqwest::Url;

use super::{BruteForce, Config, ConfigError, Limits, Load, MatcherSpec, Race, RequestSpec};

/// Max probe count, race concurrency, load count and load concurrency (PLAN §8).
const MAX_PROBES: u32 = 100;
const MAX_RACE_CONCURRENCY: u32 = 32;
const MAX_LOAD_REQUESTS: u32 = 1000;
const MAX_LOAD_CONCURRENCY: u32 = 32;

/// What a run will cost before it starts (PLAN §8): planned request count and the
/// waits that cannot be avoided (sleeps between probes, race settle delay).
/// Worst-case timeout based estimates are not computed here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RequestBudget {
    pub requests: u64,
    pub fixed_wait_ms: u64,
}

fn invalid(path: &str, rule: &str) -> ConfigError {
    ConfigError::Invalid {
        path: path.to_string(),
        rule: rule.to_string(),
    }
}

/// Check `value` is within `1..=max`.
fn check_range(path: &str, value: u32, max: u32) -> Result<(), ConfigError> {
    if (1..=max).contains(&value) {
        Ok(())
    } else {
        Err(invalid(path, &format!("must be in 1..={max}")))
    }
}

fn check_positive(path: &str, value: u64) -> Result<(), ConfigError> {
    if value > 0 {
        Ok(())
    } else {
        Err(invalid(path, "must be greater than 0"))
    }
}

pub fn validate(cfg: &Config) -> Result<RequestBudget, ConfigError> {
    check_base_url(&cfg.base_url)?;
    check_limits(&cfg.limits)?;
    check_brute_force(&cfg.brute_force)?;
    check_race(&cfg.race)?;
    check_load(&cfg.load)?;

    let bf = &cfg.brute_force;
    // u128 cannot overflow here (max about 2^32 * 2^64 + 2^64), so a huge interval can
    // never wrap into a false accept. Anything above the deadline (a u64) is rejected,
    // which also guarantees the value fits `RequestBudget::fixed_wait_ms`.
    let fixed_wait =
        u128::from(bf.attempts - 1) * u128::from(bf.interval_ms) + u128::from(cfg.race.settle_ms);
    let fixed_wait_ms = match u64::try_from(fixed_wait) {
        Ok(ms) if ms <= cfg.limits.suite_deadline_ms => ms,
        _ => {
            return Err(invalid(
                "limits.suite_deadline_ms",
                "fixed waits ((attempts-1)*interval_ms + settle_ms) exceed the suite deadline",
            ));
        }
    };
    // Brute force: baseline + attempts. Race: verify before + verify after + N actions.
    // Load: preflight + requests.
    let requests = 1
        + u64::from(bf.attempts)
        + 2
        + u64::from(cfg.race.concurrency)
        + 1
        + u64::from(cfg.load.requests);
    Ok(RequestBudget {
        requests,
        fixed_wait_ms,
    })
}

/// Origin only: http(s), a host, no userinfo/query/fragment. A base path is allowed
/// because PLAN §8 says the base path is preserved.
fn check_base_url(base_url: &str) -> Result<(), ConfigError> {
    let path = "base_url";
    if base_url.chars().any(|c| c.is_ascii_control() || c == ' ') {
        return Err(invalid(
            path,
            "must not contain whitespace or control characters",
        ));
    }
    if base_url.contains('?') || base_url.contains('#') {
        return Err(invalid(path, "must not contain a query or fragment"));
    }
    let url = Url::parse(base_url).map_err(|_| invalid(path, "must be an absolute URL"))?;
    if url.scheme() != "http" && url.scheme() != "https" {
        return Err(invalid(path, "scheme must be http or https"));
    }
    // `Url::parse` is lenient (`http:host`, `http:///x`); require a literal `scheme://host`.
    let literal_host = base_url.split_once("://").is_some_and(|(scheme, rest)| {
        scheme.eq_ignore_ascii_case(url.scheme()) && !rest.starts_with(['/', ':', '@', '\\'])
    });
    if !literal_host || url.host_str().is_none_or(str::is_empty) {
        return Err(invalid(path, "must have a host after `://`"));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(invalid(path, "must not contain userinfo"));
    }
    Ok(())
}

fn check_limits(l: &Limits) -> Result<(), ConfigError> {
    check_positive("limits.connect_timeout_ms", l.connect_timeout_ms)?;
    check_positive("limits.request_timeout_ms", l.request_timeout_ms)?;
    check_positive("limits.suite_deadline_ms", l.suite_deadline_ms)?;
    check_positive(
        "limits.response_body_limit_bytes",
        l.response_body_limit_bytes,
    )
}

fn check_brute_force(bf: &BruteForce) -> Result<(), ConfigError> {
    check_request("brute_force.baseline_request", &bf.baseline_request)?;
    check_request("brute_force.probe_request", &bf.probe_request)?;
    check_matcher("brute_force.success_match", &bf.success_match)?;
    check_matcher("brute_force.rejection_match", &bf.rejection_match)?;
    check_matcher("brute_force.protection_match", &bf.protection_match)?;

    check_range("brute_force.attempts", bf.attempts, MAX_PROBES)?;
    check_positive("brute_force.interval_ms", bf.interval_ms)?;
    // Attempt 1 must be a normal rejection, so protection can start at 2 at the earliest.
    if !(2..=bf.attempts).contains(&bf.protect_by_attempt) {
        return Err(invalid(
            "brute_force.protect_by_attempt",
            "must be in 2..=attempts",
        ));
    }
    // The tail cannot cover attempt 1. `attempts >= 2` here (protect_by_attempt passed).
    if !(1..bf.attempts).contains(&bf.protected_tail_count) {
        return Err(invalid(
            "brute_force.protected_tail_count",
            "must be in 1..=attempts-1",
        ));
    }

    // Conservative: the three outcomes must be told apart by status alone.
    let sets = [
        ("success_match", &bf.success_match.status_in),
        ("rejection_match", &bf.rejection_match.status_in),
        ("protection_match", &bf.protection_match.status_in),
    ];
    for i in 0..sets.len() {
        for j in (i + 1)..sets.len() {
            if sets[i].1.iter().any(|s| sets[j].1.contains(s)) {
                return Err(invalid(
                    "brute_force",
                    &format!(
                        "status_in of {} and {} overlap (overlapping_rules)",
                        sets[i].0, sets[j].0
                    ),
                ));
            }
        }
    }
    Ok(())
}

fn check_race(r: &Race) -> Result<(), ConfigError> {
    check_request("race.action_request", &r.action_request)?;
    check_request("race.verify_request", &r.verify_request)?;
    check_matcher("race.verify_match", &r.verify_match)?;
    check_pointer("race.counter_pointer", &r.counter_pointer)?;
    // Write-once contract (PLAN §6). Negative values fall under the same rule.
    if r.initial_counter != 0 {
        return Err(invalid("race.initial_counter", "must be 0"));
    }
    if r.expected_delta != 1 {
        return Err(invalid("race.expected_delta", "must be 1"));
    }
    check_range("race.concurrency", r.concurrency, MAX_RACE_CONCURRENCY)?;
    check_statuses("race.expected_action_statuses", &r.expected_action_statuses)?;
    check_positive("race.max_dispatch_skew_ms", r.max_dispatch_skew_ms)?;
    check_positive("race.settle_ms", r.settle_ms)
}

fn check_load(l: &Load) -> Result<(), ConfigError> {
    check_request("load.request", &l.request)?;
    check_matcher("load.response_match", &l.response_match)?;
    check_range("load.requests", l.requests, MAX_LOAD_REQUESTS)?;
    check_range("load.concurrency", l.concurrency, MAX_LOAD_CONCURRENCY)?;
    // `contains` is false for NaN, so NaN is rejected too.
    if !(0.0..=1.0).contains(&l.max_error_rate) {
        return Err(invalid("load.max_error_rate", "must be in 0..=1"));
    }
    check_positive("load.max_p95_ms", l.max_p95_ms)
}

fn check_statuses(path: &str, statuses: &[u16]) -> Result<(), ConfigError> {
    if statuses.is_empty() {
        return Err(invalid(path, "must not be empty"));
    }
    if statuses.iter().any(|s| !(100..=599).contains(s)) {
        return Err(invalid(path, "status codes must be in 100..=599"));
    }
    Ok(())
}

/// `status_in` is mandatory (a missing one already fails `parse`; an empty one fails
/// here). Provided-but-empty `json_equals` / `json_exists` are rejected as likely
/// mistakes; omitting them is fine (`status_in` alone is a valid matcher).
fn check_matcher(path: &str, m: &MatcherSpec) -> Result<(), ConfigError> {
    check_statuses(&format!("{path}.status_in"), &m.status_in)?;
    if let Some(equals) = &m.json_equals {
        let p = format!("{path}.json_equals");
        if equals.is_empty() {
            return Err(invalid(&p, "must not be empty when present"));
        }
        for pointer in equals.keys() {
            check_pointer(&p, pointer)?;
        }
    }
    if let Some(exists) = &m.json_exists {
        let p = format!("{path}.json_exists");
        if exists.is_empty() {
            return Err(invalid(&p, "must not be empty when present"));
        }
        for pointer in exists {
            check_pointer(&p, pointer)?;
        }
    }
    Ok(())
}

/// RFC 6901 pointer, but the empty pointer (whole document) is rejected because it
/// selects nothing useful here. `~` may only be followed by `0` or `1`.
fn check_pointer(path: &str, pointer: &str) -> Result<(), ConfigError> {
    let bad = || invalid(path, "contains an invalid JSON Pointer");
    if !pointer.starts_with('/') {
        return Err(bad());
    }
    let mut chars = pointer.chars();
    while let Some(c) = chars.next() {
        if c == '~' && !matches!(chars.next(), Some('0' | '1')) {
            return Err(bad());
        }
    }
    Ok(())
}

fn check_request(path: &str, r: &RequestSpec) -> Result<(), ConfigError> {
    if reqwest::Method::from_bytes(r.method.as_bytes()).is_err() {
        return Err(invalid(
            &format!("{path}.method"),
            "is not a valid HTTP method",
        ));
    }
    check_path(path, r)?;

    if let Some(headers) = &r.headers {
        let mut seen = BTreeSet::new();
        for name in headers.keys() {
            if reqwest::header::HeaderName::from_bytes(name.as_bytes()).is_err() {
                return Err(invalid(
                    &format!("{path}.headers"),
                    "contains an invalid header name",
                ));
            }
            if !seen.insert(name.to_ascii_lowercase()) {
                return Err(invalid(
                    &format!("{path}.headers"),
                    &format!("header name `{name}` is repeated with different letter case"),
                ));
            }
        }
    }
    // A literal `null` body is almost certainly a mistake; omit `json` for no body.
    if r.json == Some(serde_json::Value::Null) {
        return Err(invalid(
            &format!("{path}.json"),
            "must not be null (omit it for no body)",
        ));
    }
    Ok(())
}

/// Relative path starting with `/`; every `{name}` placeholder must have a matching
/// `path_params` key and the other way around.
fn check_path(path: &str, r: &RequestSpec) -> Result<(), ConfigError> {
    let field = format!("{path}.path");
    let p = &r.path;
    if p.chars().any(|c| c.is_ascii_control() || c == '\\') {
        return Err(invalid(
            &field,
            "must not contain control characters or backslashes",
        ));
    }
    if !p.starts_with('/') || p.starts_with("//") || p.contains("://") {
        return Err(invalid(
            &field,
            "must be a relative path starting with a single `/`",
        ));
    }
    if p.contains('?') || p.contains('#') {
        return Err(invalid(
            &field,
            "must not contain a query or fragment (use `query`)",
        ));
    }

    let mut placeholders = BTreeSet::new();
    let mut rest = p.as_str();
    while let Some(open) = rest.find(['{', '}']) {
        if !rest[open..].starts_with('{') {
            return Err(invalid(&field, "has an unmatched `}`"));
        }
        let after = &rest[open + 1..];
        let Some(close) = after.find(['{', '}']) else {
            return Err(invalid(&field, "has an unclosed `{`"));
        };
        if !after[close..].starts_with('}') || close == 0 {
            return Err(invalid(&field, "has a malformed `{name}` placeholder"));
        }
        placeholders.insert(&after[..close]);
        rest = &after[close + 1..];
    }

    let params: BTreeSet<&str> = r
        .path_params
        .iter()
        .flat_map(|m| m.keys().map(String::as_str))
        .collect();
    if placeholders != params {
        return Err(invalid(
            &format!("{path}.path_params"),
            "keys must match the `{name}` placeholders in path exactly",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::parse;
    use std::collections::BTreeMap;

    const EXAMPLE: &str = include_str!("../../examples/core-checks.json");

    fn base() -> Config {
        parse(EXAMPLE).expect("example must parse")
    }

    fn map(pairs: &[(&str, &str)]) -> Option<BTreeMap<String, String>> {
        Some(
            pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        )
    }

    type Mutate = fn(&mut Config);

    /// Apply `f` to the example config, then validate.
    fn run(f: Mutate) -> Result<RequestBudget, ConfigError> {
        let mut cfg = base();
        f(&mut cfg);
        validate(&cfg)
    }

    #[test]
    fn example_is_valid_and_budget_is_computed() {
        let budget = validate(&base()).expect("example valid");
        // 1+8 brute force, 2+5 race, 1+100 load; 7*100 + 250 ms waits.
        assert_eq!(
            budget,
            RequestBudget {
                requests: 117,
                fixed_wait_ms: 950
            }
        );
    }

    #[test]
    fn rejected_cases() {
        // (name, mutation, expected field path in the error)
        let cases: Vec<(&str, Mutate, &str)> = vec![
            (
                "probe 101",
                |c| c.brute_force.attempts = 101,
                "brute_force.attempts",
            ),
            (
                "probe 0",
                |c| c.brute_force.attempts = 0,
                "brute_force.attempts",
            ),
            ("race 33", |c| c.race.concurrency = 33, "race.concurrency"),
            ("race 0", |c| c.race.concurrency = 0, "race.concurrency"),
            ("load 1001", |c| c.load.requests = 1001, "load.requests"),
            ("load 0", |c| c.load.requests = 0, "load.requests"),
            (
                "load conc 33",
                |c| c.load.concurrency = 33,
                "load.concurrency",
            ),
            (
                "load conc 0",
                |c| c.load.concurrency = 0,
                "load.concurrency",
            ),
            (
                "error rate 1.01",
                |c| c.load.max_error_rate = 1.01,
                "load.max_error_rate",
            ),
            (
                "error rate -0.1",
                |c| c.load.max_error_rate = -0.1,
                "load.max_error_rate",
            ),
            (
                "error rate nan",
                |c| c.load.max_error_rate = f64::NAN,
                "load.max_error_rate",
            ),
            ("p95 0", |c| c.load.max_p95_ms = 0, "load.max_p95_ms"),
            (
                "request timeout 0",
                |c| c.limits.request_timeout_ms = 0,
                "limits.request_timeout_ms",
            ),
            (
                "connect timeout 0",
                |c| c.limits.connect_timeout_ms = 0,
                "limits.connect_timeout_ms",
            ),
            (
                "deadline 0",
                |c| c.limits.suite_deadline_ms = 0,
                "limits.suite_deadline_ms",
            ),
            (
                "body limit 0",
                |c| c.limits.response_body_limit_bytes = 0,
                "limits.response_body_limit_bytes",
            ),
            (
                "interval 0",
                |c| c.brute_force.interval_ms = 0,
                "brute_force.interval_ms",
            ),
            ("settle 0", |c| c.race.settle_ms = 0, "race.settle_ms"),
            (
                "skew 0",
                |c| c.race.max_dispatch_skew_ms = 0,
                "race.max_dispatch_skew_ms",
            ),
            (
                "protect_by 1",
                |c| c.brute_force.protect_by_attempt = 1,
                "brute_force.protect_by_attempt",
            ),
            (
                "protect_by > attempts",
                |c| c.brute_force.protect_by_attempt = 9,
                "brute_force.protect_by_attempt",
            ),
            (
                "tail 0",
                |c| c.brute_force.protected_tail_count = 0,
                "brute_force.protected_tail_count",
            ),
            (
                "tail > attempts",
                |c| c.brute_force.protected_tail_count = 9,
                "brute_force.protected_tail_count",
            ),
            (
                "tail == attempts",
                |c| c.brute_force.protected_tail_count = 8,
                "brute_force.protected_tail_count",
            ),
            (
                "attempts 1",
                |c| {
                    c.brute_force.attempts = 1;
                    c.brute_force.protect_by_attempt = 1;
                    c.brute_force.protected_tail_count = 1;
                },
                "brute_force.protect_by_attempt",
            ),
            (
                "fixed waits overflow",
                |c| {
                    c.brute_force.interval_ms = u64::MAX;
                    c.race.settle_ms = u64::MAX;
                    c.limits.suite_deadline_ms = u64::MAX;
                },
                "limits.suite_deadline_ms",
            ),
            (
                "fixed waits just over u64",
                |c| {
                    c.brute_force.attempts = 2;
                    c.brute_force.protect_by_attempt = 2;
                    c.brute_force.protected_tail_count = 1;
                    c.brute_force.interval_ms = u64::MAX;
                    c.race.settle_ms = 1;
                    c.limits.suite_deadline_ms = u64::MAX;
                },
                "limits.suite_deadline_ms",
            ),
            (
                "path backslash",
                |c| c.load.request.path = "/\\evil.test".into(),
                "load.request.path",
            ),
            (
                "path newline",
                |c| c.load.request.path = "/a\n".into(),
                "load.request.path",
            ),
            (
                "path nul",
                |c| c.load.request.path = "/a\0b".into(),
                "load.request.path",
            ),
            (
                "path del",
                |c| c.load.request.path = "/a\x7f".into(),
                "load.request.path",
            ),
            (
                "base_url leading space",
                |c| c.base_url = " http://h".into(),
                "base_url",
            ),
            (
                "base_url trailing newline",
                |c| c.base_url = "http://h\n".into(),
                "base_url",
            ),
            (
                "base_url empty host",
                |c| c.base_url = "http:///x".into(),
                "base_url",
            ),
            (
                "base_url no slashes",
                |c| c.base_url = "http:127.0.0.1:8080".into(),
                "base_url",
            ),
            (
                "initial_counter 1",
                |c| c.race.initial_counter = 1,
                "race.initial_counter",
            ),
            (
                "initial_counter -1",
                |c| c.race.initial_counter = -1,
                "race.initial_counter",
            ),
            (
                "expected_delta 2",
                |c| c.race.expected_delta = 2,
                "race.expected_delta",
            ),
            (
                "expected_delta 0",
                |c| c.race.expected_delta = 0,
                "race.expected_delta",
            ),
            (
                "fixed waits over deadline",
                |c| c.limits.suite_deadline_ms = 949,
                "limits.suite_deadline_ms",
            ),
            (
                "empty status_in",
                |c| c.brute_force.success_match.status_in.clear(),
                "success_match.status_in",
            ),
            (
                "status 99",
                |c| c.load.response_match.status_in = vec![99],
                "load.response_match.status_in",
            ),
            (
                "status 600",
                |c| c.race.expected_action_statuses = vec![600],
                "race.expected_action_statuses",
            ),
            (
                "empty expected_action_statuses",
                |c| c.race.expected_action_statuses.clear(),
                "race.expected_action_statuses",
            ),
            (
                "overlap success/rejection",
                |c| c.brute_force.rejection_match.status_in = vec![200, 401],
                "brute_force",
            ),
            (
                "overlap rejection/protection",
                |c| c.brute_force.protection_match.status_in = vec![401, 429],
                "brute_force",
            ),
            (
                "overlap success/protection",
                |c| c.brute_force.protection_match.status_in = vec![429, 200],
                "brute_force",
            ),
            (
                "pointer without slash",
                |c| c.race.counter_pointer = "execution_count".into(),
                "race.counter_pointer",
            ),
            (
                "pointer empty",
                |c| c.race.counter_pointer = String::new(),
                "race.counter_pointer",
            ),
            (
                "pointer bad escape",
                |c| c.race.counter_pointer = "/a~2".into(),
                "race.counter_pointer",
            ),
            (
                "pointer dangling tilde",
                |c| c.race.counter_pointer = "/a~".into(),
                "race.counter_pointer",
            ),
            (
                "json_exists bad pointer",
                |c| c.brute_force.success_match.json_exists = Some(vec!["token".into()]),
                "success_match.json_exists",
            ),
            (
                "json_exists empty",
                |c| c.brute_force.success_match.json_exists = Some(vec![]),
                "success_match.json_exists",
            ),
            (
                "json_equals bad pointer",
                |c| c.load.response_match.json_equals = Some([("x".to_string(), 1.into())].into()),
                "load.response_match.json_equals",
            ),
            (
                "json_equals empty",
                |c| c.load.response_match.json_equals = Some(BTreeMap::new()),
                "load.response_match.json_equals",
            ),
            (
                "base_url no scheme",
                |c| c.base_url = "127.0.0.1:8080".into(),
                "base_url",
            ),
            (
                "base_url ftp",
                |c| c.base_url = "ftp://example.com".into(),
                "base_url",
            ),
            (
                "base_url userinfo",
                |c| c.base_url = "http://u:p@example.com".into(),
                "base_url",
            ),
            (
                "base_url user only",
                |c| c.base_url = "http://u@example.com".into(),
                "base_url",
            ),
            (
                "base_url query",
                |c| c.base_url = "http://example.com?a=1".into(),
                "base_url",
            ),
            (
                "base_url empty query",
                |c| c.base_url = "http://example.com/?".into(),
                "base_url",
            ),
            (
                "base_url fragment",
                |c| c.base_url = "http://example.com/#x".into(),
                "base_url",
            ),
            (
                "path absolute url",
                |c| c.load.request.path = "http://evil.test/x".into(),
                "load.request.path",
            ),
            (
                "path no slash",
                |c| c.load.request.path = "critical".into(),
                "load.request.path",
            ),
            (
                "path double slash",
                |c| c.load.request.path = "//evil.test/x".into(),
                "load.request.path",
            ),
            (
                "path with query",
                |c| c.load.request.path = "/critical?a=1".into(),
                "load.request.path",
            ),
            (
                "placeholder without param",
                |c| c.load.request.path = "/a/{id}".into(),
                "load.request.path_params",
            ),
            (
                "param without placeholder",
                |c| c.load.request.path_params = map(&[("id", "x")]),
                "load.request.path_params",
            ),
            (
                "param name mismatch",
                |c| c.race.action_request.path_params = map(&[("other", "x")]),
                "race.action_request.path_params",
            ),
            (
                "unclosed brace",
                |c| c.load.request.path = "/a/{id".into(),
                "load.request.path",
            ),
            (
                "unmatched close brace",
                |c| c.load.request.path = "/a/id}".into(),
                "load.request.path",
            ),
            (
                "empty placeholder",
                |c| c.load.request.path = "/a/{}".into(),
                "load.request.path",
            ),
            (
                "nested brace",
                |c| c.load.request.path = "/a/{a{b}}".into(),
                "load.request.path",
            ),
            (
                "bad method",
                |c| c.load.request.method = "GE T".into(),
                "load.request.method",
            ),
            (
                "header case duplicate",
                |c| c.load.request.headers = map(&[("Authorization", "a"), ("authorization", "b")]),
                "load.request.headers",
            ),
            (
                "bad header name",
                |c| c.load.request.headers = map(&[("Bad Name", "a")]),
                "load.request.headers",
            ),
            (
                "json null",
                |c| c.load.request.json = Some(serde_json::Value::Null),
                "load.request.json",
            ),
        ];
        for (name, mutate, want_path) in cases {
            match run(mutate) {
                Err(ConfigError::Invalid { path, .. }) => {
                    assert!(path.contains(want_path), "{name}: got path {path}");
                }
                other => panic!("{name}: expected Invalid, got {other:?}"),
            }
        }
    }

    #[test]
    fn accepted_boundary_cases() {
        let cases: Vec<(&str, Mutate)> = vec![
            ("probe 100", |c| {
                c.brute_force.attempts = 100;
                c.brute_force.protect_by_attempt = 100;
                c.limits.suite_deadline_ms = 1_000_000;
            }),
            ("probe 2 minimal", |c| {
                c.brute_force.attempts = 2;
                c.brute_force.protect_by_attempt = 2;
                c.brute_force.protected_tail_count = 1;
            }),
            ("race 32", |c| c.race.concurrency = 32),
            ("race 1", |c| c.race.concurrency = 1),
            ("load 1000", |c| c.load.requests = 1000),
            ("load 1", |c| c.load.requests = 1),
            ("load conc 32", |c| c.load.concurrency = 32),
            ("load conc 1", |c| c.load.concurrency = 1),
            ("error rate 0", |c| c.load.max_error_rate = 0.0),
            ("error rate 1", |c| c.load.max_error_rate = 1.0),
            ("p95 1", |c| c.load.max_p95_ms = 1),
            ("interval 1", |c| c.brute_force.interval_ms = 1),
            ("settle 1", |c| c.race.settle_ms = 1),
            ("skew 1", |c| c.race.max_dispatch_skew_ms = 1),
            ("protect_by 2", |c| c.brute_force.protect_by_attempt = 2),
            ("protect_by == attempts", |c| {
                c.brute_force.protect_by_attempt = 8
            }),
            ("tail 1", |c| c.brute_force.protected_tail_count = 1),
            ("tail == attempts-1", |c| {
                c.brute_force.protected_tail_count = 7
            }),
            ("fixed waits u64 fit", |c| {
                c.brute_force.attempts = 2;
                c.brute_force.protect_by_attempt = 2;
                c.brute_force.protected_tail_count = 1;
                c.brute_force.interval_ms = u64::MAX - 1;
                c.race.settle_ms = 1;
                c.limits.suite_deadline_ms = u64::MAX;
            }),
            ("fixed waits == deadline", |c| {
                c.limits.suite_deadline_ms = 950
            }),
            ("status 100 and 599", |c| {
                c.load.response_match.status_in = vec![100, 599]
            }),
            ("status_in alone", |c| {
                c.brute_force.success_match.json_exists = None;
                c.brute_force.success_match.json_equals = None;
            }),
            ("json_equals ok", |c| {
                c.load.response_match.json_equals =
                    Some([("/a~1b/0".to_string(), serde_json::json!(null))].into());
            }),
            ("pointer escapes", |c| {
                c.race.counter_pointer = "/a~0b~1c".into()
            }),
            ("base_url https with base path", |c| {
                c.base_url = "https://example.com:8443/api".into()
            }),
            ("base_url trailing slash", |c| {
                c.base_url = "http://127.0.0.1:8080/".into()
            }),
            ("path with two params", |c| {
                c.load.request.path = "/a/{x}/b/{y}".into();
                c.load.request.path_params = map(&[("x", "1"), ("y", "2")]);
            }),
            ("same placeholder twice", |c| {
                c.load.request.path = "/a/{x}/{x}".into();
                c.load.request.path_params = map(&[("x", "1")]);
            }),
            ("different headers", |c| {
                c.load.request.headers = map(&[("Authorization", "a"), ("X-Other", "b")])
            }),
            ("json object body", |c| {
                c.load.request.json = Some(serde_json::json!({}))
            }),
        ];
        for (name, mutate) in cases {
            if let Err(e) = run(mutate) {
                panic!("{name}: expected Ok, got {e}");
            }
        }
    }

    #[test]
    fn errors_name_no_values() {
        let sentinel = "SENTINEL-SECRET-VALUE";
        let mut cfg = base();
        cfg.load.request.path = format!("/{sentinel}?a=1");
        let msg = validate(&cfg).unwrap_err().to_string();
        assert!(!msg.contains(sentinel), "{msg}");

        let mut cfg = base();
        cfg.base_url = format!("http://u:{sentinel}@example.com");
        let msg = validate(&cfg).unwrap_err().to_string();
        assert!(!msg.contains(sentinel), "{msg}");

        let mut cfg = base();
        cfg.load.request.path = format!("/{{{sentinel}");
        let msg = validate(&cfg).unwrap_err().to_string();
        assert!(!msg.contains(sentinel), "{msg}");
    }
}
