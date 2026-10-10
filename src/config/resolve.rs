//! `${ENV:NAME}` resolution (PLAN §9) and the `Secret` type.
//!
//! A value is substituted only when the WHOLE string is one placeholder (no
//! interpolation). Resolution works on a copy: the raw `Config` is never changed.
//! Errors name env vars and field paths, never values.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde_json::Value;

use super::{Config, ConfigError, MatcherSpec, RequestSpec};
use crate::http::request::{ResolvedRequest, build_url};

/// A string that must never be printed. `Debug` is redacted on purpose; there is no
/// `Display`, no `Serialize` and no `Clone`. Use `expose` only where the value is
/// really sent.
pub struct Secret(String);

impl Secret {
    pub fn new(value: String) -> Self {
        Secret(value)
    }

    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(<redacted>)")
    }
}

/// All requests of a config with env resolved and URLs built.
#[derive(Debug)]
pub struct ResolvedRequests {
    pub baseline: ResolvedRequest,
    pub probe: ResolvedRequest,
    pub action: ResolvedRequest,
    pub verify: ResolvedRequest,
    pub load: ResolvedRequest,
    /// Sorted, de-duplicated NAMES of every env var `resolve` looked up (never values).
    pub env_names: Vec<String>,
}

/// Whole-string placeholder shape, same rule as `parse_placeholder`.
fn looks_like_placeholder(s: &str) -> bool {
    s.starts_with("${ENV:") && s.ends_with('}')
}

fn reject_placeholder(path: &str, s: &str) -> Result<(), ConfigError> {
    if looks_like_placeholder(s) {
        return Err(ConfigError::Invalid {
            path: path.to_string(),
            rule: "${ENV:NAME} is only substituted in path_params, query, headers and json body; \
                   it is not allowed here"
                .to_string(),
        });
    }
    Ok(())
}

fn reject_in_json(path: &str, v: &Value) -> Result<(), ConfigError> {
    match v {
        Value::String(s) => reject_placeholder(path, s),
        Value::Array(items) => items
            .iter()
            .enumerate()
            .try_for_each(|(i, v)| reject_in_json(&format!("{path}[{i}]"), v)),
        Value::Object(map) => map
            .iter()
            .try_for_each(|(k, v)| reject_in_json(&format!("{path}.{k}"), v)),
        _ => Ok(()),
    }
}

fn reject_in_matcher(path: &str, m: &MatcherSpec) -> Result<(), ConfigError> {
    for (k, v) in m.json_equals.iter().flatten() {
        // ponytail: the key (a pointer) is checked by name only; the path never prints it.
        reject_placeholder(&format!("{path}.json_equals"), k)?;
        reject_in_json(&format!("{path}.json_equals.{k}"), v)?;
    }
    for (i, p) in m.json_exists.iter().flatten().enumerate() {
        reject_placeholder(&format!("{path}.json_exists[{i}]"), p)?;
    }
    Ok(())
}

/// Reject whole-string placeholders where `resolve` never substitutes: a silent literal
/// would let a check pass or fail against the wrong expectation.
fn reject_unresolved_placeholders(cfg: &Config) -> Result<(), ConfigError> {
    reject_placeholder("base_url", &cfg.base_url)?;
    let bf = &cfg.brute_force;
    for (f, r) in [
        ("brute_force.baseline_request", &bf.baseline_request),
        ("brute_force.probe_request", &bf.probe_request),
        ("race.action_request", &cfg.race.action_request),
        ("race.verify_request", &cfg.race.verify_request),
        ("load.request", &cfg.load.request),
    ] {
        reject_placeholder(&format!("{f}.path"), &r.path)?;
    }
    reject_in_matcher("brute_force.success_match", &bf.success_match)?;
    reject_in_matcher("brute_force.rejection_match", &bf.rejection_match)?;
    reject_in_matcher("brute_force.protection_match", &bf.protection_match)?;
    reject_in_matcher("race.verify_match", &cfg.race.verify_match)?;
    reject_placeholder("race.counter_pointer", &cfg.race.counter_pointer)?;
    reject_in_matcher("load.response_match", &cfg.load.response_match)
}

/// `Ok(Some(name))` for a whole-string `${ENV:name}`, `Ok(None)` for a plain literal,
/// `Err(())` for a string that is shaped `${ENV:...}` (starts with `${ENV:`, ends with
/// `}`) but has an invalid name. Anything not shaped like that is a literal, e.g.
/// `prefix-${ENV:X}` or `${ENV:X}-suffix` (no interpolation).
fn parse_placeholder(raw: &str) -> Result<Option<&str>, ()> {
    let Some(name) = raw
        .strip_prefix("${ENV:")
        .and_then(|rest| rest.strip_suffix('}'))
    else {
        return Ok(None);
    };
    let mut chars = name.chars();
    let valid = chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_');
    if valid { Ok(Some(name)) } else { Err(()) }
}

/// Resolves strings and remembers every problem so all of them are reported at once.
struct Resolver<'a, F: Fn(&str) -> Option<String>> {
    lookup: &'a F,
    missing: BTreeSet<String>,
    used: BTreeSet<String>,
    malformed: Vec<String>,
}

impl<F: Fn(&str) -> Option<String>> Resolver<'_, F> {
    fn lookup_name(&mut self, name: &str) -> Option<String> {
        self.used.insert(name.to_string());
        (self.lookup)(name)
    }

    /// On a problem the error is recorded and an empty string is returned; the caller
    /// discards the result because `finish` fails.
    fn string(&mut self, field: &str, raw: &str) -> String {
        match parse_placeholder(raw) {
            Ok(None) => raw.to_string(),
            Err(()) => {
                self.malformed.push(field.to_string());
                String::new()
            }
            // An empty env var counts as missing (conservative: an empty credential
            // or id is almost never intended).
            Ok(Some(name)) => match self.lookup_name(name) {
                Some(v) if !v.is_empty() => v,
                _ => {
                    self.missing.insert(name.to_string());
                    String::new()
                }
            },
        }
    }

    fn map(
        &mut self,
        field: &str,
        raw: &Option<BTreeMap<String, String>>,
    ) -> BTreeMap<String, String> {
        let mut out = BTreeMap::new();
        for (k, v) in raw.iter().flatten() {
            out.insert(k.clone(), self.string(&format!("{field}.{k}"), v));
        }
        out
    }

    /// Only string leaves are touched; numbers, booleans, null and object keys stay.
    fn json(&mut self, field: &str, value: &Value) -> Value {
        match value {
            Value::String(s) => Value::String(self.string(field, s)),
            Value::Array(items) => Value::Array(
                items
                    .iter()
                    .enumerate()
                    .map(|(i, v)| self.json(&format!("{field}[{i}]"), v))
                    .collect(),
            ),
            Value::Object(map) => Value::Object(
                map.iter()
                    .map(|(k, v)| (k.clone(), self.json(&format!("{field}.{k}"), v)))
                    .collect(),
            ),
            other => other.clone(),
        }
    }

    fn request(
        &mut self,
        field: &str,
        base_url: &str,
        spec: &RequestSpec,
    ) -> Result<ResolvedRequest, ConfigError> {
        let path_params = self.map(&format!("{field}.path_params"), &spec.path_params);
        let query = self.map(&format!("{field}.query"), &spec.query);
        let raw_headers = self.map(&format!("{field}.headers"), &spec.headers);
        let body = spec
            .json
            .as_ref()
            .map(|v| Secret::new(self.json(&format!("{field}.json"), v).to_string()));
        // Checked after everything else is resolved so missing env vars are still all
        // collected. Only CR, LF and NUL are rejected (header injection); the error
        // names the field, never the value.
        if let Some((name, _)) = raw_headers
            .iter()
            .find(|(_, v)| v.contains(['\r', '\n', '\0']))
        {
            return Err(ConfigError::Invalid {
                path: format!("{field}.headers.{name}"),
                rule: "header value must not contain CR, LF or NUL".to_string(),
            });
        }
        let headers = raw_headers
            .into_iter()
            .map(|(k, v)| (k, Secret::new(v)))
            .collect();
        let url = build_url(base_url, &spec.path, &path_params, &query).map_err(|e| {
            ConfigError::Invalid {
                path: format!("{field}.path"),
                rule: e.to_string(),
            }
        })?;
        Ok(ResolvedRequest {
            method: spec.method.clone(),
            url: Secret::new(url),
            headers,
            body,
        })
    }
}

/// Resolve every request in `cfg`. `lookup` is injected (`std::env::var(..).ok()` in
/// the CLI, a fake in tests). Run `validate` first.
pub fn resolve<F: Fn(&str) -> Option<String>>(
    cfg: &Config,
    lookup: &F,
) -> Result<ResolvedRequests, ConfigError> {
    reject_unresolved_placeholders(cfg)?;
    let mut r = Resolver {
        lookup,
        missing: BTreeSet::new(),
        used: BTreeSet::new(),
        malformed: Vec::new(),
    };
    let base = cfg.base_url.as_str();
    let bf = &cfg.brute_force;
    let baseline = r.request("brute_force.baseline_request", base, &bf.baseline_request);
    let probe = r.request("brute_force.probe_request", base, &bf.probe_request);
    let action = r.request("race.action_request", base, &cfg.race.action_request);
    let verify = r.request("race.verify_request", base, &cfg.race.verify_request);
    let load = r.request("load.request", base, &cfg.load.request);

    if !r.malformed.is_empty() {
        return Err(ConfigError::BadPlaceholder { paths: r.malformed });
    }
    if !r.missing.is_empty() {
        return Err(ConfigError::MissingEnv {
            names: r.missing.into_iter().collect(),
        });
    }
    Ok(ResolvedRequests {
        baseline: baseline?,
        probe: probe?,
        action: action?,
        verify: verify?,
        load: load?,
        env_names: r.used.into_iter().collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::parse;

    const EXAMPLE: &str = include_str!("../../examples/core-checks.json");
    const SENTINEL: &str = "SENTINEL-SECRET-VALUE";

    fn base() -> Config {
        parse(EXAMPLE).expect("example must parse")
    }

    /// Fake env: every example variable set, secrets carry the sentinel.
    fn full_env(name: &str) -> Option<String> {
        match name {
            "LOGIN_USER" => Some("demo".into()),
            "LOGIN_VALID_PASSWORD" => Some(format!("valid-{SENTINEL}")),
            "LOGIN_INVALID_PASSWORD" => Some(format!("invalid-{SENTINEL}")),
            "TEST_ACTION_ID" => Some("act-1".into()),
            "TEST_AUTHORIZATION" => Some(format!("Bearer {SENTINEL}")),
            _ => None,
        }
    }

    fn map(pairs: &[(&str, &str)]) -> Option<BTreeMap<String, String>> {
        Some(
            pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        )
    }

    #[test]
    fn example_resolves_and_debug_is_redacted() {
        let reqs = resolve(&base(), &full_env).expect("resolves");
        assert_eq!(
            reqs.action.url.expose(),
            "http://127.0.0.1:8080/actions/act-1/apply"
        );
        let body = reqs.baseline.body.as_ref().expect("body").expose();
        assert!(
            body.contains("valid-SENTINEL"),
            "body is really substituted"
        );

        for dump in [
            format!("{reqs:?}"),
            format!("{:?}", Secret::new(SENTINEL.into())),
        ] {
            assert!(!dump.contains(SENTINEL), "{dump}");
            assert!(!dump.contains("act-1"), "{dump}");
            assert!(!dump.contains("127.0.0.1"), "{dump}");
        }
        assert_eq!(
            format!("{:?}", Secret::new("x".into())),
            "Secret(<redacted>)"
        );
    }

    #[test]
    fn all_missing_envs_are_reported_together_without_values() {
        let partial = |n: &str| (n == "LOGIN_USER").then(|| SENTINEL.to_string());
        let err = resolve(&base(), &partial).unwrap_err();
        let ConfigError::MissingEnv { names } = &err else {
            panic!("expected MissingEnv, got {err:?}");
        };
        assert_eq!(
            names,
            &[
                "LOGIN_INVALID_PASSWORD",
                "LOGIN_VALID_PASSWORD",
                "TEST_ACTION_ID",
                "TEST_AUTHORIZATION"
            ]
        );
        let msg = err.to_string();
        assert!(msg.contains("TEST_ACTION_ID") && msg.contains("LOGIN_VALID_PASSWORD"));
        assert!(!msg.contains(SENTINEL) && !format!("{err:?}").contains(SENTINEL));
    }

    #[test]
    fn empty_env_var_counts_as_missing() {
        let empty = |n: &str| {
            if n == "TEST_ACTION_ID" {
                Some(String::new())
            } else {
                full_env(n)
            }
        };
        match resolve(&base(), &empty) {
            Err(ConfigError::MissingEnv { names }) => assert_eq!(names, ["TEST_ACTION_ID"]),
            other => panic!("expected MissingEnv, got {other:?}"),
        }
    }

    #[test]
    fn placeholder_only_counts_as_whole_string() {
        let mut cfg = base();
        cfg.load.request.headers = map(&[
            ("X-A", "prefix-${ENV:TEST_ACTION_ID}"),
            ("X-B", "${ENV:TEST_ACTION_ID}-s"),
        ]);
        // `UNSET` is not in the env: if it were interpolated, resolve would fail.
        cfg.load.request.query = map(&[("q", "a${ENV:UNSET}")]);
        let reqs = resolve(&cfg, &full_env).expect("literals do not need env");
        let h: BTreeMap<_, _> = reqs
            .load
            .headers
            .iter()
            .map(|(k, v)| (k.as_str(), v.expose()))
            .collect();
        assert_eq!(h["X-A"], "prefix-${ENV:TEST_ACTION_ID}");
        assert_eq!(h["X-B"], "${ENV:TEST_ACTION_ID}-s");
        assert!(reqs.load.url.expose().ends_with("?q=a%24%7BENV%3AUNSET%7D"));
    }

    #[test]
    fn numbers_bools_null_and_nested_leaves() {
        let mut cfg = base();
        cfg.load.request.json = Some(serde_json::json!({
            "n": 5, "b": true, "z": null, "f": 1.5,
            "nested": {"list": ["${ENV:LOGIN_USER}", 7, "lit"]},
            "${ENV:LOGIN_USER}": "key-stays"
        }));
        let reqs = resolve(&cfg, &full_env).expect("resolves");
        let body: Value =
            serde_json::from_str(reqs.load.body.as_ref().expect("body").expose()).expect("json");
        assert_eq!(
            body,
            serde_json::json!({
                "n": 5, "b": true, "z": null, "f": 1.5,
                "nested": {"list": ["demo", 7, "lit"]},
                "${ENV:LOGIN_USER}": "key-stays"
            })
        );
    }

    #[test]
    fn json_leaf_missing_env_is_reported() {
        let mut cfg = base();
        cfg.load.request.json = Some(serde_json::json!({"a": ["${ENV:NOPE_1}"]}));
        match resolve(&cfg, &full_env) {
            Err(ConfigError::MissingEnv { names }) => assert_eq!(names, ["NOPE_1"]),
            other => panic!("expected MissingEnv, got {other:?}"),
        }
    }

    #[test]
    fn malformed_placeholders_name_the_field_not_the_value() {
        for bad in [
            "${ENV:}",
            "${ENV:A B}",
            "${ENV:1A}",
            "${ENV:A}${ENV:B}",
            "${ENV:a-b}",
        ] {
            let mut cfg = base();
            cfg.load.request.headers = map(&[("X-Bad", bad)]);
            let err = resolve(&cfg, &full_env).unwrap_err();
            let ConfigError::BadPlaceholder { paths } = &err else {
                panic!("{bad}: expected BadPlaceholder, got {err:?}");
            };
            assert_eq!(paths, &["load.request.headers.X-Bad"], "{bad}");
            assert!(!err.to_string().contains(bad), "{err}");
        }
        // Valid names: underscore start, digits after.
        let mut cfg = base();
        cfg.load.request.headers = map(&[("X-Ok", "${ENV:_A1}")]);
        let env = |n: &str| (n == "_A1").then(|| "v".to_string());
        // other requests still need their env, so only check this one is not malformed
        assert!(!matches!(
            resolve(&cfg, &env),
            Err(ConfigError::BadPlaceholder { .. })
        ));
    }

    #[test]
    fn env_names_are_exactly_the_looked_up_names() {
        let reqs = resolve(&base(), &full_env).expect("resolves");
        assert_eq!(
            reqs.env_names,
            [
                "LOGIN_INVALID_PASSWORD",
                "LOGIN_USER",
                "LOGIN_VALID_PASSWORD",
                "TEST_ACTION_ID",
                "TEST_AUTHORIZATION"
            ]
        );
    }

    #[test]
    fn placeholder_in_unresolved_position_is_rejected_by_path_only() {
        let ph = "${ENV:ZED}";
        let mut cases: Vec<(Config, &str)> = Vec::new();
        let mut c = base();
        c.brute_force.success_match.json_equals =
            Some([("/a".to_string(), serde_json::json!({"x": [ph]}))].into());
        cases.push((c, "brute_force.success_match.json_equals./a.x[0]"));
        let mut c = base();
        c.load.response_match.json_exists = Some(vec![ph.to_string()]);
        cases.push((c, "load.response_match.json_exists[0]"));
        let mut c = base();
        c.race.counter_pointer = ph.to_string();
        cases.push((c, "race.counter_pointer"));
        let mut c = base();
        c.base_url = ph.to_string();
        cases.push((c, "base_url"));
        let mut c = base();
        c.load.request.path = ph.to_string();
        cases.push((c, "load.request.path"));
        for (cfg, want) in cases {
            let err = resolve(&cfg, &full_env).unwrap_err();
            let ConfigError::Invalid { path, .. } = &err else {
                panic!("{want}: expected Invalid, got {err:?}");
            };
            assert_eq!(path, want);
            assert!(!err.to_string().contains("ZED"), "{err}");
        }
    }

    #[test]
    fn raw_config_stays_unresolved() {
        let cfg = base();
        let _ = resolve(&cfg, &full_env).expect("resolves");
        assert_eq!(
            cfg.race
                .action_request
                .path_params
                .as_ref()
                .expect("params")["id"],
            "${ENV:TEST_ACTION_ID}"
        );
        let json = cfg
            .brute_force
            .baseline_request
            .json
            .as_ref()
            .expect("json");
        assert_eq!(json["username"], "${ENV:LOGIN_USER}");
    }

    #[test]
    fn path_param_value_is_encoded_in_the_resolved_url() {
        let env = |n: &str| match n {
            "TEST_ACTION_ID" => Some("a/b c?d#e%2F..".to_string()),
            other => full_env(other),
        };
        let reqs = resolve(&base(), &env).expect("resolves");
        assert_eq!(
            reqs.action.url.expose(),
            "http://127.0.0.1:8080/actions/a%2Fb%20c%3Fd%23e%252F../apply"
        );
    }

    #[test]
    fn header_with_cr_lf_nul_is_rejected_at_resolve_without_value() {
        for bad in ["a\rb", "a\nb", "a\0b"] {
            let tainted = format!("{SENTINEL}{bad}");
            // via env
            let mut cfg = base();
            cfg.load.request.headers = map(&[("X-Env", "${ENV:TAINTED}")]);
            let t = tainted.clone();
            let env = move |n: &str| {
                if n == "TAINTED" {
                    Some(t.clone())
                } else {
                    full_env(n)
                }
            };
            // via literal
            let mut lit = base();
            lit.load.request.headers = map(&[("X-Lit", &tainted)]);
            for (cfg, env_fn, name) in [
                (&cfg, &env as &dyn Fn(&str) -> Option<String>, "X-Env"),
                (&lit, &full_env, "X-Lit"),
            ] {
                let err = resolve(cfg, &env_fn).unwrap_err();
                let ConfigError::Invalid { path, .. } = &err else {
                    panic!("expected Invalid, got {err:?}");
                };
                assert_eq!(path, &format!("load.request.headers.{name}"));
                assert!(!err.to_string().contains(SENTINEL), "{err}");
                assert!(!format!("{err:?}").contains(SENTINEL), "{err:?}");
            }
        }
        // Tab and non-ASCII are not part of the rule.
        let mut ok = base();
        ok.load.request.headers = map(&[("X-Ok", "a\tb é")]);
        resolve(&ok, &full_env).expect("tab and non-ASCII accepted");
    }

    #[test]
    fn empty_literal_path_param_is_rejected() {
        let mut cfg = base();
        cfg.race.action_request.path_params = map(&[("id", "")]);
        let err = resolve(&cfg, &full_env).unwrap_err();
        assert!(matches!(err, ConfigError::Invalid { .. }), "{err:?}");
    }

    #[test]
    fn dot_dot_path_param_is_rejected_without_value() {
        let env = |n: &str| match n {
            "TEST_ACTION_ID" => Some("..".to_string()),
            other => full_env(other),
        };
        let err = resolve(&base(), &env).unwrap_err();
        assert!(matches!(err, ConfigError::Invalid { .. }), "{err:?}");
    }
}
