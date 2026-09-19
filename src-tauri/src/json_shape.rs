//! Describing JSON by its *structure* for error messages.
//!
//! Both things AI reactor parses — the credential blob and the usage response —
//! come from undocumented sources that can change shape without notice. When
//! one does, the error has to be diagnosable from a bug report, which means
//! naming the keys and value types that actually arrived.
//!
//! It must never include the values. The credential blob is a live token, and
//! even the usage response is account data that does not belong in a log file
//! or a screenshot. Strings are reported as `string(len=N)`.

/// Describe the structure of a JSON document.
pub fn describe_text(text: &str) -> String {
    match serde_json::from_str::<serde_json::Value>(text) {
        Ok(value) => describe(&value),
        Err(_) => format!("JSON이 아님 ({} bytes)", text.len()),
    }
}

/// Describe the structure of a parsed value.
pub fn describe(value: &serde_json::Value) -> String {
    use serde_json::Value;
    match value {
        Value::Object(map) => {
            let fields: Vec<String> = map
                .iter()
                .map(|(k, v)| format!("{k}: {}", describe(v)))
                .collect();
            format!("{{{}}}", fields.join(", "))
        }
        Value::Array(items) => format!("array[{}]", items.len()),
        Value::String(s) => format!("string(len={})", s.len()),
        Value::Number(_) => "number".into(),
        Value::Bool(_) => "bool".into(),
        Value::Null => "null".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_keys_and_types_but_never_values() {
        let described = describe_text(r#"{"token":"SUPERSECRETVALUE","n":1,"xs":[1,2]}"#);
        assert!(described.contains("token: string(len=16)"), "{described}");
        assert!(described.contains("n: number"), "{described}");
        assert!(described.contains("xs: array[2]"), "{described}");
        assert!(!described.contains("SUPERSECRET"), "value leaked: {described}");
    }

    #[test]
    fn nests() {
        assert_eq!(
            describe_text(r#"{"a":{"b":null}}"#),
            "{a: {b: null}}"
        );
    }

    #[test]
    fn non_json_is_reported_by_size() {
        assert_eq!(describe_text("<html>"), "JSON이 아님 (6 bytes)");
    }
}
