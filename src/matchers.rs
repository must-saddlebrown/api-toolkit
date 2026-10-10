//! Response matcher (PLAN §4, §9). Implemented directly on `MatcherSpec`; no extra type.

use serde_json::Value;

use crate::config::MatcherSpec;

impl MatcherSpec {
    /// True when ALL present conditions hold (AND):
    /// - `status` is in `status_in`;
    /// - every `json_equals` pointer resolves to a value equal to the expected one
    ///   (serde_json equality: `1` != `1.0` != `"1"`);
    /// - every `json_exists` pointer resolves to some value (an explicit `null` EXISTS).
    ///
    /// Any `json_*` condition that is present (even an empty one) with no body
    /// (`None`: absent or not JSON) is a non-match.
    /// A missing or malformed pointer is a non-match. Returns only a bool, never body values.
    pub fn matches(&self, status: u16, body: Option<&Value>) -> bool {
        if !self.status_in.contains(&status) {
            return false;
        }
        let equals = self.json_equals.as_ref();
        let exists = self.json_exists.as_ref();
        if equals.is_none() && exists.is_none() {
            return true;
        }
        let Some(body) = body else {
            return false;
        };
        let equals_ok = equals.is_none_or(|m| {
            m.iter()
                .all(|(pointer, expected)| body.pointer(pointer) == Some(expected))
        });
        let exists_ok =
            exists.is_none_or(|v| v.iter().all(|pointer| body.pointer(pointer).is_some()));
        equals_ok && exists_ok
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn spec(v: Value) -> MatcherSpec {
        serde_json::from_value(v).expect("test matcher must parse")
    }

    #[test]
    fn status_only() {
        let m = spec(json!({"status_in": [200, 201]}));
        assert!(m.matches(200, None));
        assert!(m.matches(201, Some(&json!({"a": 1}))));
        assert!(!m.matches(500, None));
        assert!(!m.matches(500, Some(&json!({}))));
        assert!(!spec(json!({"status_in": []})).matches(200, None));
    }

    #[test]
    fn json_equals_match_mismatch_and_types() {
        let m = spec(json!({"status_in": [200], "json_equals": {"/n": 1, "/s/t": "x"}}));
        assert!(m.matches(200, Some(&json!({"n": 1, "s": {"t": "x"}}))));
        assert!(!m.matches(200, Some(&json!({"n": 2, "s": {"t": "x"}}))));
        // One pair failing is enough (AND across pairs).
        assert!(!m.matches(200, Some(&json!({"n": 1, "s": {"t": "y"}}))));

        // Observed serde_json equality: integer 1 != string "1" and integer 1 != float 1.0.
        let one = spec(json!({"status_in": [200], "json_equals": {"/n": 1}}));
        assert!(one.matches(200, Some(&json!({"n": 1}))));
        assert!(!one.matches(200, Some(&json!({"n": "1"}))));
        let float: Value = serde_json::from_str(r#"{"n": 1.0}"#).expect("json");
        assert!(!one.matches(200, Some(&float)));
        let one_f = spec(json!({"status_in": [200], "json_equals": {"/n": 1.0}}));
        assert!(!one_f.matches(200, Some(&json!({"n": 1}))));
        assert!(one_f.matches(200, Some(&float)));
        // null expected equals only an explicit null, not a missing key.
        let nul = spec(json!({"status_in": [200], "json_equals": {"/n": null}}));
        assert!(nul.matches(200, Some(&json!({"n": null}))));
        assert!(!nul.matches(200, Some(&json!({}))));
    }

    #[test]
    fn json_exists_present_absent_null() {
        let m = spec(json!({"status_in": [200], "json_exists": ["/a", "/b"]}));
        assert!(m.matches(200, Some(&json!({"a": 1, "b": null}))));
        assert!(!m.matches(200, Some(&json!({"a": 1}))));
        assert!(!m.matches(200, Some(&json!({}))));
    }

    #[test]
    fn all_conditions_are_anded() {
        let m = spec(json!({
            "status_in": [200],
            "json_equals": {"/ok": true},
            "json_exists": ["/id"]
        }));
        let good = json!({"ok": true, "id": 7});
        assert!(m.matches(200, Some(&good)));
        assert!(!m.matches(404, Some(&good)));
        assert!(!m.matches(200, Some(&json!({"ok": false, "id": 7}))));
        assert!(!m.matches(200, Some(&json!({"ok": true}))));
    }

    #[test]
    fn body_none_with_and_without_json_conditions() {
        assert!(spec(json!({"status_in": [200]})).matches(200, None));
        let eq = spec(json!({"status_in": [200], "json_equals": {"/a": 1}}));
        let ex = spec(json!({"status_in": [200], "json_exists": ["/a"]}));
        assert!(!eq.matches(200, None));
        assert!(!ex.matches(200, None));
        // Present-but-empty json_* containers still need a JSON body (no false PASS without one).
        let empty = spec(json!({"status_in": [200], "json_equals": {}, "json_exists": []}));
        assert!(!empty.matches(200, None));
        assert!(empty.matches(200, Some(&json!({}))));
    }

    #[test]
    fn non_object_bodies_and_bad_pointers_do_not_match() {
        let ex = spec(json!({"status_in": [200], "json_exists": ["/a"]}));
        assert!(!ex.matches(200, Some(&json!([1, 2]))));
        assert!(!ex.matches(200, Some(&json!("text"))));
        assert!(!ex.matches(200, Some(&json!(5))));
        assert!(!ex.matches(200, Some(&Value::Null)));
        // Pointer without a leading "/" is invalid per RFC 6901: no match, no panic.
        let bad = spec(json!({"status_in": [200], "json_exists": ["a"]}));
        assert!(!bad.matches(200, Some(&json!({"a": 1}))));
        // Index out of range / non-numeric index on an array.
        let idx = spec(json!({"status_in": [200], "json_exists": ["/9", "/x"]}));
        assert!(!idx.matches(200, Some(&json!([1, 2]))));
    }

    #[test]
    fn pointer_escapes_and_array_index() {
        let body = json!({"a/b": 1, "m~n": 2, "items": [{"id": "x"}, {"id": "y"}]});
        let m = spec(json!({
            "status_in": [200],
            "json_equals": {"/a~1b": 1, "/m~0n": 2, "/items/1/id": "y"},
            "json_exists": ["/items/0"]
        }));
        assert!(m.matches(200, Some(&body)));
        let wrong = spec(json!({"status_in": [200], "json_equals": {"/items/0/id": "y"}}));
        assert!(!wrong.matches(200, Some(&body)));
    }
}
