//! `${ENV:NAME}` resolution (PLAN §9) and the `Secret` type.
//!
//! A value is substituted only when the WHOLE string is one placeholder (no
//! interpolation). Resolution works on a copy: the raw `Config` is never changed.
//! Errors name env vars and field paths, never values.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde_json::Value;

use super::{BruteForce, Config, ConfigError, Load, MatcherSpec, Race, RequestSpec, Selection};
use crate::http::request::{ResolvedRequest, build_url};
use crate::report::CheckName;

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
    pub brute_force: Option<BruteForceRequests>,
    pub race: Option<RaceRequests>,
    pub load: Option<ResolvedRequest>,
    /// Sorted, de-duplicated NAMES of every env var `resolve` looked up (never values).
    pub env_names: Vec<String>,
}

/// `None` in `ResolvedRequests` means the section was not selected.
#[derive(Debug)]
pub struct BruteForceRequests {
    /// `None` when `baseline = "none"` (DECISIONS 2026-10-14 Q4): no baseline request.
    pub baseline: Option<ResolvedRequest>,
    pub probe: ResolvedRequest,
}

#[derive(Debug)]
pub struct RaceRequests {
    pub action: ResolvedRequest,
    pub verify: ResolvedRequest,
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
/// would let a check pass or fail against the wrong expectation. Selected sections only.
fn reject_unresolved_placeholders(
    cfg: &Config,
    bf: Option<&BruteForce>,
    race: Option<&Race>,
    load: Option<&Load>,
) -> Result<(), ConfigError> {
    reject_placeholder("base_url", &cfg.base_url)?;
    if let Some(bf) = bf {
        if let Some(baseline_request) = &bf.baseline_request {
            reject_placeholder("brute_force.baseline_request.path", &baseline_request.path)?;
        }
        reject_placeholder("brute_force.probe_request.path", &bf.probe_request.path)?;
        reject_in_matcher("brute_force.success_match", &bf.success_match)?;
        reject_in_matcher("brute_force.rejection_match", &bf.rejection_match)?;
        reject_in_matcher("brute_force.protection_match", &bf.protection_match)?;
    }
    if let Some(race) = race {
        reject_placeholder("race.action_request.path", &race.action_request.path)?;
        reject_placeholder("race.verify_request.path", &race.verify_request.path)?;
        reject_in_matcher("race.verify_match", &race.verify_match)?;
        reject_placeholder("race.counter_pointer", &race.counter_pointer)?;
    }
    if let Some(load) = load {
        reject_placeholder("load.request.path", &load.request.path)?;
        reject_in_matcher("load.response_match", &load.response_match)?;
    }
    Ok(())
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

/// A selected section must exist (`validate` normally guarantees it; this never panics).
fn selected<'a, T>(
    section: &'a Option<T>,
    name: &str,
    sel: Selection,
    check: CheckName,
) -> Result<Option<&'a T>, ConfigError> {
    if !sel.includes(check) {
        return Ok(None);
    }
    match section {
        Some(s) => Ok(Some(s)),
        None => Err(ConfigError::Invalid {
            path: name.to_string(),
            rule: "is required for the selected check".to_string(),
        }),
    }
}

/// Resolve the requests of the selected sections. `lookup` is injected
/// (`std::env::var(..).ok()` in the CLI, a fake in tests). Run `validate` first.
/// Sections that are not selected are not resolved, so their env vars are not needed.
pub fn resolve<F: Fn(&str) -> Option<String>>(
    cfg: &Config,
    sel: Selection,
    lookup: &F,
) -> Result<ResolvedRequests, ConfigError> {
    let bf = selected(&cfg.brute_force, "brute_force", sel, CheckName::BruteForce)?;
    let race = selected(&cfg.race, "race", sel, CheckName::Race)?;
    let load = selected(&cfg.load, "load", sel, CheckName::Load)?;
    reject_unresolved_placeholders(cfg, bf, race, load)?;
    let mut r = Resolver {
        lookup,
        missing: BTreeSet::new(),
        used: BTreeSet::new(),
        malformed: Vec::new(),
    };
    let base = cfg.base_url.as_str();
    let bf_reqs = bf.map(|bf| {
        (
            bf.baseline_request
                .as_ref()
                .map(|spec| r.request("brute_force.baseline_request", base, spec)),
            r.request("brute_force.probe_request", base, &bf.probe_request),
        )
    });
    let race_reqs = race.map(|race| {
        (
            r.request("race.action_request", base, &race.action_request),
            r.request("race.verify_request", base, &race.verify_request),
        )
    });
    let load_req = load.map(|load| r.request("load.request", base, &load.request));

    if !r.malformed.is_empty() {
        return Err(ConfigError::BadPlaceholder { paths: r.malformed });
    }
    if !r.missing.is_empty() {
        return Err(ConfigError::MissingEnv {
            names: r.missing.into_iter().collect(),
        });
    }
    let brute_force = match bf_reqs {
        Some((baseline, probe)) => Some(BruteForceRequests {
            baseline: baseline.transpose()?,
            probe: probe?,
        }),
        None => None,
    };
    let race = match race_reqs {
        Some((action, verify)) => Some(RaceRequests {
            action: action?,
            verify: verify?,
        }),
        None => None,
    };
    let load = load_req.transpose()?;
    Ok(ResolvedRequests {
        brute_force,
        race,
        load,
        env_names: r.used.into_iter().collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::parse;

    const EXAMPLE: &str = include_str!("../../examples/core-checks.json");
    const SENTINEL: &str = "SENTINEL-SECRET-VALUE";

    fn load(c: &mut Config) -> &mut Load {
        c.load.as_mut().expect("load")
    }

    fn race(c: &mut Config) -> &mut Race {
        c.race.as_mut().expect("race")
    }

    fn bf(c: &mut Config) -> &mut BruteForce {
        c.brute_force.as_mut().expect("bf")
    }

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
        let reqs = resolve(&base(), Selection::All, &full_env).expect("resolves");
        assert_eq!(
            reqs.race.as_ref().unwrap().action.url.expose(),
            "http://127.0.0.1:8080/actions/act-1/apply"
        );
        let body = reqs
            .brute_force
            .as_ref()
            .unwrap()
            .baseline
            .as_ref()
            .expect("baseline is Some for require_success")
            .body
            .as_ref()
            .expect("body")
            .expose();
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
        let err = resolve(&base(), Selection::All, &partial).unwrap_err();
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
        match resolve(&base(), Selection::All, &empty) {
            Err(ConfigError::MissingEnv { names }) => assert_eq!(names, ["TEST_ACTION_ID"]),
            other => panic!("expected MissingEnv, got {other:?}"),
        }
    }

    #[test]
    fn placeholder_only_counts_as_whole_string() {
        let mut cfg = base();
        load(&mut cfg).request.headers = map(&[
            ("X-A", "prefix-${ENV:TEST_ACTION_ID}"),
            ("X-B", "${ENV:TEST_ACTION_ID}-s"),
        ]);
        // `UNSET` is not in the env: if it were interpolated, resolve would fail.
        load(&mut cfg).request.query = map(&[("q", "a${ENV:UNSET}")]);
        let reqs = resolve(&cfg, Selection::All, &full_env).expect("literals do not need env");
        let h: BTreeMap<_, _> = reqs
            .load
            .as_ref()
            .unwrap()
            .headers
            .iter()
            .map(|(k, v)| (k.as_str(), v.expose()))
            .collect();
        assert_eq!(h["X-A"], "prefix-${ENV:TEST_ACTION_ID}");
        assert_eq!(h["X-B"], "${ENV:TEST_ACTION_ID}-s");
        assert!(
            reqs.load
                .as_ref()
                .unwrap()
                .url
                .expose()
                .ends_with("?q=a%24%7BENV%3AUNSET%7D")
        );
    }

    #[test]
    fn numbers_bools_null_and_nested_leaves() {
        let mut cfg = base();
        load(&mut cfg).request.json = Some(serde_json::json!({
            "n": 5, "b": true, "z": null, "f": 1.5,
            "nested": {"list": ["${ENV:LOGIN_USER}", 7, "lit"]},
            "${ENV:LOGIN_USER}": "key-stays"
        }));
        let reqs = resolve(&cfg, Selection::All, &full_env).expect("resolves");
        let body: Value = serde_json::from_str(
            reqs.load
                .as_ref()
                .unwrap()
                .body
                .as_ref()
                .expect("body")
                .expose(),
        )
        .expect("json");
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
        load(&mut cfg).request.json = Some(serde_json::json!({"a": ["${ENV:NOPE_1}"]}));
        match resolve(&cfg, Selection::All, &full_env) {
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
            load(&mut cfg).request.headers = map(&[("X-Bad", bad)]);
            let err = resolve(&cfg, Selection::All, &full_env).unwrap_err();
            let ConfigError::BadPlaceholder { paths } = &err else {
                panic!("{bad}: expected BadPlaceholder, got {err:?}");
            };
            assert_eq!(paths, &["load.request.headers.X-Bad"], "{bad}");
            assert!(!err.to_string().contains(bad), "{err}");
        }
        // Valid names: underscore start, digits after.
        let mut cfg = base();
        load(&mut cfg).request.headers = map(&[("X-Ok", "${ENV:_A1}")]);
        let env = |n: &str| (n == "_A1").then(|| "v".to_string());
        // other requests still need their env, so only check this one is not malformed
        assert!(!matches!(
            resolve(&cfg, Selection::All, &env),
            Err(ConfigError::BadPlaceholder { .. })
        ));
    }

    #[test]
    fn env_names_are_exactly_the_looked_up_names() {
        let reqs = resolve(&base(), Selection::All, &full_env).expect("resolves");
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
        bf(&mut c).success_match.json_equals =
            Some([("/a".to_string(), serde_json::json!({"x": [ph]}))].into());
        cases.push((c, "brute_force.success_match.json_equals./a.x[0]"));
        let mut c = base();
        load(&mut c).response_match.json_exists = Some(vec![ph.to_string()]);
        cases.push((c, "load.response_match.json_exists[0]"));
        let mut c = base();
        race(&mut c).counter_pointer = ph.to_string();
        cases.push((c, "race.counter_pointer"));
        let mut c = base();
        c.base_url = ph.to_string();
        cases.push((c, "base_url"));
        let mut c = base();
        load(&mut c).request.path = ph.to_string();
        cases.push((c, "load.request.path"));
        for (cfg, want) in cases {
            let err = resolve(&cfg, Selection::All, &full_env).unwrap_err();
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
        let _ = resolve(&cfg, Selection::All, &full_env).expect("resolves");
        assert_eq!(
            cfg.race
                .as_ref()
                .expect("race")
                .action_request
                .path_params
                .as_ref()
                .expect("params")["id"],
            "${ENV:TEST_ACTION_ID}"
        );
        let json = cfg
            .brute_force
            .as_ref()
            .expect("bf")
            .baseline_request
            .as_ref()
            .expect("baseline_request is Some for require_success")
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
        let reqs = resolve(&base(), Selection::All, &env).expect("resolves");
        assert_eq!(
            reqs.race.as_ref().unwrap().action.url.expose(),
            "http://127.0.0.1:8080/actions/a%2Fb%20c%3Fd%23e%252F../apply"
        );
    }

    #[test]
    fn header_with_cr_lf_nul_is_rejected_at_resolve_without_value() {
        for bad in ["a\rb", "a\nb", "a\0b"] {
            let tainted = format!("{SENTINEL}{bad}");
            // via env
            let mut cfg = base();
            load(&mut cfg).request.headers = map(&[("X-Env", "${ENV:TAINTED}")]);
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
            load(&mut lit).request.headers = map(&[("X-Lit", &tainted)]);
            for (cfg, env_fn, name) in [
                (&cfg, &env as &dyn Fn(&str) -> Option<String>, "X-Env"),
                (&lit, &full_env, "X-Lit"),
            ] {
                let err = resolve(cfg, Selection::All, &env_fn).unwrap_err();
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
        load(&mut ok).request.headers = map(&[("X-Ok", "a\tb é")]);
        resolve(&ok, Selection::All, &full_env).expect("tab and non-ASCII accepted");
    }

    #[test]
    fn empty_literal_path_param_is_rejected() {
        let mut cfg = base();
        race(&mut cfg).action_request.path_params = map(&[("id", "")]);
        let err = resolve(&cfg, Selection::All, &full_env).unwrap_err();
        assert!(matches!(err, ConfigError::Invalid { .. }), "{err:?}");
    }

    #[test]
    fn dot_dot_path_param_is_rejected_without_value() {
        let env = |n: &str| match n {
            "TEST_ACTION_ID" => Some("..".to_string()),
            other => full_env(other),
        };
        let err = resolve(&base(), Selection::All, &env).unwrap_err();
        assert!(matches!(err, ConfigError::Invalid { .. }), "{err:?}");
    }

    // ---- partial config / Selection (only.S2) ----

    const BF: Selection = Selection::Only(CheckName::BruteForce);

    #[test]
    fn only_bf_does_not_need_test_env_and_lists_only_login_names() {
        let login_only = |n: &str| match n {
            "LOGIN_USER" | "LOGIN_VALID_PASSWORD" | "LOGIN_INVALID_PASSWORD" => {
                Some(format!("v-{SENTINEL}"))
            }
            _ => None,
        };
        let reqs = resolve(&base(), BF, &login_only).expect("TEST_* not needed");
        assert!(reqs.brute_force.is_some());
        assert!(reqs.race.is_none() && reqs.load.is_none());
        assert_eq!(
            reqs.env_names,
            [
                "LOGIN_INVALID_PASSWORD",
                "LOGIN_USER",
                "LOGIN_VALID_PASSWORD"
            ]
        );
        // The same lookup fails in All mode.
        assert!(matches!(
            resolve(&base(), Selection::All, &login_only),
            Err(ConfigError::MissingEnv { .. })
        ));
    }

    #[test]
    fn only_bf_ignores_bad_placeholders_in_unselected_sections() {
        let mut cfg = base();
        load(&mut cfg).request.path = "${ENV:ZED}".to_string();
        race(&mut cfg).counter_pointer = "${ENV:ZED}".to_string();
        resolve(&cfg, BF, &full_env).expect("unselected sections are not resolved");
        assert!(resolve(&cfg, Selection::All, &full_env).is_err());
    }

    #[test]
    fn resolve_rejects_missing_selected_section_without_panic() {
        let mut cfg = base();
        cfg.brute_force = None;
        match resolve(&cfg, BF, &full_env) {
            Err(ConfigError::Invalid { path, .. }) => assert_eq!(path, "brute_force"),
            other => panic!("expected Invalid, got {other:?}"),
        }
    }
}
