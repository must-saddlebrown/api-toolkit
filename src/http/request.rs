//! `ResolvedRequest` (env already substituted) and the URL builder (PLAN §8).
//!
//! Anything that may carry a secret (URL with path params and query, header values,
//! body) is held as a `Secret`, so `{:?}` on a request never prints a value.

use std::collections::BTreeMap;

use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};
use reqwest::Url;

use crate::config::Secret;

/// A request ready for the transport. The body, when present, is serialized JSON
/// (the transport sets `Content-Type: application/json`).
#[derive(Debug)]
pub struct ResolvedRequest {
    pub method: String,
    pub url: Secret,
    /// Header names are config names (not secret); values are secret.
    pub headers: Vec<(String, Secret)>,
    pub body: Option<Secret>,
}

/// Why a URL could not be built. Messages name placeholders, never values.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum UrlError {
    #[error("base_url is not a plain http(s) origin with an optional base path")]
    BadBase,
    #[error(
        "path must start with a single `/` and have no query, fragment, backslash or control characters"
    )]
    BadPath,
    #[error("path placeholder `{{{0}}}` has no path_params value")]
    MissingParam(String),
    #[error("path placeholder `{{{0}}}` has an empty path_params value")]
    EmptyParam(String),
    #[error("query has an empty key")]
    EmptyQueryKey,
    #[error("path would contain a `.` or `..` segment")]
    DotSegment,
    #[error("URL structure changed while building (host, port or path segments)")]
    StructureChanged,
}

/// Everything except letters, digits and `-._~` is percent-encoded, so `/ ? # % space`
/// and non-ASCII bytes in a value can never act as URL syntax.
const ENCODE: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'.')
    .remove(b'_')
    .remove(b'~');

fn encode(value: &str) -> String {
    utf8_percent_encode(value, ENCODE).to_string()
}

fn has_bad_char(s: &str) -> bool {
    s.chars()
        .any(|c| c.is_ascii_control() || c == '\\' || c == ' ')
}

/// A segment that URL parsers treat as `.` or `..` (also in `%2e` spelling).
fn is_dot_segment(segment: &str) -> bool {
    matches!(
        segment.to_ascii_lowercase().replace("%2e", ".").as_str(),
        "." | ".."
    )
}

/// base_url (base path kept, trailing slash ignored) + `path` with `{name}` replaced by
/// the percent-encoded `path_params` value + percent-encoded `query`.
pub fn build_url(
    base_url: &str,
    path: &str,
    path_params: &BTreeMap<String, String>,
    query: &BTreeMap<String, String>,
) -> Result<String, UrlError> {
    let base = Url::parse(base_url).map_err(|_| UrlError::BadBase)?;
    if has_bad_char(base_url)
        || base_url.contains(['?', '#'])
        || !matches!(base.scheme(), "http" | "https")
        || base.host_str().is_none_or(str::is_empty)
        // `Url::parse` is lenient (`http:h`, `http:/h`); require a literal `scheme://host`.
        || !base_url.split_once("://").is_some_and(|(scheme, rest)| {
            scheme.eq_ignore_ascii_case(base.scheme()) && !rest.starts_with(['/', ':', '@'])
        })
        || !base.username().is_empty()
        || base.password().is_some()
    {
        return Err(UrlError::BadBase);
    }
    // The parser collapses `.`/`..` in the base path, so check the raw text.
    let raw_base_path = base_url
        .split_once("://")
        .map_or("", |(_, rest)| rest.find('/').map_or("", |i| &rest[i..]));
    if raw_base_path.split('/').any(is_dot_segment) {
        return Err(UrlError::DotSegment);
    }
    if has_bad_char(path)
        || path.contains(['?', '#'])
        || !path.starts_with('/')
        || path.starts_with("//")
    {
        return Err(UrlError::BadPath);
    }

    // Path part = base path (normalized by the parser) + template. One pass over the
    // template: substituted values are never scanned again.
    let mut path_part = base.path().trim_end_matches('/').to_string();
    let mut rest = path;
    while let Some(open) = rest.find('{') {
        path_part.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        let close = after.find('}').ok_or(UrlError::BadPath)?;
        let name = &after[..close];
        let value = path_params
            .get(name)
            .ok_or_else(|| UrlError::MissingParam(name.to_string()))?;
        if value.is_empty() {
            return Err(UrlError::EmptyParam(name.to_string()));
        }
        path_part.push_str(&encode(value));
        rest = &after[close + 1..];
    }
    path_part.push_str(rest);
    if path_part.split('/').any(is_dot_segment) {
        return Err(UrlError::DotSegment);
    }

    let mut url = format!("{}{}", base.origin().ascii_serialization(), path_part);
    for (i, (key, value)) in query.iter().enumerate() {
        if key.is_empty() {
            return Err(UrlError::EmptyQueryKey);
        }
        url.push(if i == 0 { '?' } else { '&' });
        url.push_str(&encode(key));
        url.push('=');
        url.push_str(&encode(value));
    }

    // Backstop: re-parse and make sure the parser saw the same origin and the same
    // number of path segments.
    let parsed = Url::parse(&url).map_err(|_| UrlError::StructureChanged)?;
    let slashes = path_part.matches('/').count();
    if parsed.scheme() != base.scheme()
        || parsed.host_str() != base.host_str()
        || parsed.port() != base.port()
        || parsed.path().matches('/').count() != slashes
    {
        return Err(UrlError::StructureChanged);
    }
    Ok(parsed.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    fn url(base: &str, path: &str, p: &[(&str, &str)]) -> Result<String, UrlError> {
        build_url(base, path, &params(p), &BTreeMap::new())
    }

    #[test]
    fn base_path_kept_with_and_without_trailing_slash() {
        for base in ["http://h:9/api", "http://h:9/api/", "http://h:9/api//"] {
            assert_eq!(
                url(base, "/x/y", &[]).unwrap(),
                "http://h:9/api/x/y",
                "{base}"
            );
        }
        assert_eq!(url("http://h:9", "/x", &[]).unwrap(), "http://h:9/x");
        assert_eq!(url("http://h:9/", "/x", &[]).unwrap(), "http://h:9/x");
    }

    #[test]
    fn hostile_path_param_values_stay_one_segment() {
        let values = [
            "a/b",
            "a b",
            "a?b",
            "a#b",
            "a%2Fb",
            "a%b",
            "a\\b",
            "é/ü",
            "日本",
            "a\nb",
            "x@evil.test",
            "..a",
            "a..",
            "a/../b",
            "%2e%2e/x",
        ];
        for v in values {
            let got = url("http://h:9/api", "/items/{id}/go", &[("id", v)]).unwrap();
            let parsed = Url::parse(&got).unwrap();
            assert_eq!(parsed.host_str(), Some("h"), "{v}");
            assert_eq!(parsed.port(), Some(9), "{v}");
            let segs: Vec<_> = parsed.path_segments().unwrap().collect();
            assert_eq!(segs.len(), 4, "{v}: {got}");
            assert_eq!(&segs[..2], ["api", "items"], "{v}");
            assert_eq!(segs[3], "go", "{v}");
            assert!(
                parsed.query().is_none() && parsed.fragment().is_none(),
                "{v}"
            );
        }
        assert_eq!(
            url("http://h:9/api", "/i/{id}", &[("id", "a/b c?d#e%2F")]).unwrap(),
            "http://h:9/api/i/a%2Fb%20c%3Fd%23e%252F"
        );
        assert_eq!(
            url("http://h", "/i/{id}", &[("id", "é")]).unwrap(),
            "http://h/i/%C3%A9"
        );
    }

    #[test]
    fn dot_segments_are_rejected() {
        for v in [".", ".."] {
            assert_eq!(
                url("http://h", "/i/{id}", &[("id", v)]).unwrap_err(),
                UrlError::DotSegment,
                "{v}"
            );
        }
        // A `%` in a value is itself encoded, so `%2e%2e` stays a literal name.
        assert_eq!(
            url("http://h", "/i/{id}", &[("id", "%2e%2e")]).unwrap(),
            "http://h/i/%252e%252e"
        );
        assert_eq!(
            url("http://h", "/a/../b", &[]).unwrap_err(),
            UrlError::DotSegment
        );
        assert_eq!(
            url("http://h/a/./b", "/c", &[]).unwrap_err(),
            UrlError::DotSegment
        );
    }

    #[test]
    fn substituted_value_is_not_rescanned_as_placeholder() {
        let got = url("http://h", "/{a}/{b}", &[("a", "{b}"), ("b", "z")]).unwrap();
        assert_eq!(got, "http://h/%7Bb%7D/z");
    }

    #[test]
    fn builder_rejects_odd_base_and_path_on_its_own() {
        let none: &[(&str, &str)] = &[];
        for base in [
            "ftp://h",
            "http:/h",
            "http:h",
            "http:///x",
            "http://h?x=1",
            "http://h/#f",
            "http://u:p@h",
            " http://h",
            "http://h\n",
            "http://h/a b",
            "http://h\\evil",
            "nonsense",
            "",
        ] {
            assert_eq!(
                url(base, "/x", none).unwrap_err(),
                UrlError::BadBase,
                "{base:?}"
            );
        }
        for path in [
            "x",
            "",
            "//evil.test/x",
            "/a?b",
            "/a#b",
            "/a\\b",
            "/a\nb",
            "/a\0b",
            "/a b",
            "/{open",
        ] {
            assert_eq!(
                url("http://h", path, none).unwrap_err(),
                UrlError::BadPath,
                "{path:?}"
            );
        }
        assert_eq!(
            url("http://h", "/{id}", none).unwrap_err(),
            UrlError::MissingParam("id".into())
        );
    }

    #[test]
    fn query_is_encoded_and_cannot_add_pairs() {
        let q = params(&[("a&b=c", "x y#z&w=1"), ("k", "é")]);
        let got = build_url("http://h/api", "/s", &BTreeMap::new(), &q).unwrap();
        assert_eq!(got, "http://h/api/s?a%26b%3Dc=x%20y%23z%26w%3D1&k=%C3%A9");
        let parsed = Url::parse(&got).unwrap();
        assert_eq!(parsed.query_pairs().count(), 2);
        assert!(parsed.fragment().is_none());
    }

    #[test]
    fn empty_path_param_and_empty_query_key_are_rejected() {
        assert_eq!(
            url("http://h", "/i/{id}", &[("id", "")]).unwrap_err(),
            UrlError::EmptyParam("id".into())
        );
        let q = params(&[("", "x")]);
        assert_eq!(
            build_url("http://h", "/s", &BTreeMap::new(), &q).unwrap_err(),
            UrlError::EmptyQueryKey
        );
    }

    #[test]
    fn resolved_request_debug_hides_values() {
        let req = ResolvedRequest {
            method: "POST".into(),
            url: Secret::new("http://h/SENTINEL-URL".into()),
            headers: vec![("Authorization".into(), Secret::new("SENTINEL-HDR".into()))],
            body: Some(Secret::new("{\"p\":\"SENTINEL-BODY\"}".into())),
        };
        let dump = format!("{req:?}");
        assert!(
            !dump.contains("SENTINEL") && !dump.contains("http://h"),
            "{dump}"
        );
        assert!(dump.contains("Authorization") && dump.contains("POST"));
    }
}
