//! Extracting one value of the JSON body by path.
//!
//! It exists so that capturing a datum in a script requires neither having
//! `jq` installed nor setting up a pipe:
//!
//! ```sh
//! TOKEN=$(hts run auth/login --extract token)
//! ```
//!
//! It is a deliberately small subset: dotted paths and array indices. For
//! anything more complicated, `hts run ... -o body | jq` is still the right
//! answer, and no language has to be invented.

use anyhow::{Result, bail};

/// Returns the value at `path` inside the JSON body.
///
/// Strings come back **unquoted**, which is what a script wants to assign to a
/// variable. Every other value is serialized as JSON.
///
/// # Errors
///
/// If the body is not valid JSON, or if the path does not exist.
pub(crate) fn extract(body: &str, path: &str) -> Result<String> {
    let root: serde_json::Value = serde_json::from_str(body.trim())
        .map_err(|error| anyhow::anyhow!("the response is not valid JSON: {error}"))?;

    let mut current = &root;
    let mut walked = String::new();

    for segment in path.split('.').filter(|segment| !segment.is_empty()) {
        if !walked.is_empty() {
            walked.push('.');
        }
        walked.push_str(segment);

        current = match current {
            serde_json::Value::Object(map) => map
                .get(segment)
                .ok_or_else(|| anyhow::anyhow!("there is no `{walked}` in the response"))?,
            serde_json::Value::Array(items) => {
                let index: usize = segment.parse().map_err(|_| {
                    anyhow::anyhow!("`{walked}` indexes an array, but `{segment}` is not a number")
                })?;
                items
                    .get(index)
                    .ok_or_else(|| anyhow::anyhow!("`{walked}` is out of the array"))?
            }
            _ => bail!("`{walked}` cannot be walked: the value is neither object nor array"),
        };
    }

    Ok(match current {
        // Unquoted: this is the case assigned to a shell variable.
        serde_json::Value::String(text) => text.clone(),
        serde_json::Value::Null => String::new(),
        other => other.to_string(),
    })
}

#[cfg(test)]
mod tests {
    // In tests, `unwrap` documents the expectation and its panic IS the failure.
    #![allow(clippy::unwrap_used)]

    use super::*;

    const BODY: &str = r#"{
        "token": "abc123",
        "expires": 3600,
        "active": true,
        "empty_value": null,
        "user": { "id": 7, "name": "demo" },
        "items": [ { "id": 1 }, { "id": 2 } ],
        "blank": {}
    }"#;

    #[test]
    fn extracts_a_string_unquoted() {
        assert_eq!(extract(BODY, "token").unwrap(), "abc123");
    }

    #[test]
    fn extracts_a_number() {
        assert_eq!(extract(BODY, "expires").unwrap(), "3600");
    }

    #[test]
    fn extracts_a_boolean() {
        assert_eq!(extract(BODY, "active").unwrap(), "true");
    }

    #[test]
    fn null_comes_back_as_an_empty_string() {
        assert_eq!(extract(BODY, "empty_value").unwrap(), "");
    }

    #[test]
    fn walks_nested_objects() {
        assert_eq!(extract(BODY, "user.name").unwrap(), "demo");
    }

    #[test]
    fn indexes_arrays() {
        assert_eq!(extract(BODY, "items.1.id").unwrap(), "2");
    }

    #[test]
    fn a_whole_object_comes_out_as_json() {
        assert_eq!(extract(BODY, "user").unwrap(), r#"{"id":7,"name":"demo"}"#);
    }

    #[test]
    fn an_empty_path_returns_the_root() {
        assert!(extract(BODY, "").unwrap().starts_with('{'));
    }

    #[test]
    fn a_key_that_does_not_exist_is_an_error() {
        let error = extract(BODY, "user.email").unwrap_err().to_string();
        assert!(error.contains("user.email"), "{error}");
    }

    #[test]
    fn an_index_out_of_range_is_an_error() {
        assert!(extract(BODY, "items.9").is_err());
    }

    #[test]
    fn indexing_an_array_with_text_is_an_error() {
        let error = extract(BODY, "items.first").unwrap_err().to_string();
        assert!(error.contains("is not a number"), "{error}");
    }

    #[test]
    fn walking_into_a_scalar_is_an_error() {
        let error = extract(BODY, "token.more").unwrap_err().to_string();
        assert!(error.contains("neither object nor array"), "{error}");
    }

    #[test]
    fn a_body_that_is_not_json_is_an_error() {
        let error = extract("<html></html>", "token").unwrap_err().to_string();
        assert!(error.contains("not valid JSON"), "{error}");
    }

    #[test]
    fn accepts_whitespace_around_the_body() {
        assert_eq!(extract("  {\"a\":1}  ", "a").unwrap(), "1");
    }
}
