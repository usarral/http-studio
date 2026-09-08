//! Building an ad-hoc request out of the arguments.
//!
//! It is what makes `hts run https://api.example.com/x` work with no file in
//! existence. The resulting definition enters through the same port as the
//! ones from disk, so it inherits variable interpolation, environments,
//! secrets, history and exit codes with no new logic.

use std::collections::BTreeMap;
use std::io::Read;

use anyhow::{Context, Result, bail};
use http_studio_domain::{Body, Header, HttpMethod, RequestDefinition, RequestId, RequestOptions};

use crate::cli::RunArgs;

/// Builds an ad-hoc request's definition.
///
/// # Errors
///
/// If the method is not recognised, or if the body is asked for from a file
/// that cannot be read.
pub(crate) fn build(args: &RunArgs) -> Result<RequestDefinition> {
    let body = read_body(args)?;

    // A body with no explicit method almost always wants to be a POST:
    // demanding `-X POST` there only makes the first attempt fail.
    let method = match &args.method {
        Some(raw) => parse_method(raw)?,
        None if body.is_empty() => HttpMethod::Get,
        None => HttpMethod::Post,
    };

    let mut headers: Vec<Header> = args
        .headers
        .iter()
        .map(|(name, value)| Header::new(name, value))
        .collect();

    // `--json` implies the content type, unless one has already been declared
    // by hand: whoever writes the header is in charge.
    let declared = headers
        .iter()
        .any(|header| header.name.eq_ignore_ascii_case("content-type"));
    if args.json.is_some() && !declared {
        headers.push(Header::new("Content-Type", "application/json"));
    }

    Ok(RequestDefinition {
        id: RequestId::new("adhoc/request"),
        name: "ad-hoc".to_owned(),
        method,
        url: args.request.clone(),
        headers,
        query: Vec::new(),
        body,
        variables: BTreeMap::new(),
        description: None,
        // The TLS policy is not decided here: `--insecure` travels in the
        // `SendRequestInput`, which is the only path that also serves the
        // workspace's requests.
        options: RequestOptions::default(),
    })
}

/// Translates the method asked for, case-insensitively.
fn parse_method(raw: &str) -> Result<HttpMethod> {
    match raw.to_ascii_uppercase().as_str() {
        "GET" => Ok(HttpMethod::Get),
        "POST" => Ok(HttpMethod::Post),
        "PUT" => Ok(HttpMethod::Put),
        "PATCH" => Ok(HttpMethod::Patch),
        "DELETE" => Ok(HttpMethod::Delete),
        "HEAD" => Ok(HttpMethod::Head),
        "OPTIONS" => Ok(HttpMethod::Options),
        other => bail!("unknown HTTP method: `{other}`"),
    }
}

/// Resolves the body, accepting `@file` and `@-` for standard input.
fn read_body(args: &RunArgs) -> Result<Body> {
    let (raw, is_json) = match (&args.json, &args.data) {
        (Some(json), _) => (json, true),
        (None, Some(data)) => (data, false),
        (None, None) => return Ok(Body::Empty),
    };

    let content = if let Some(source) = raw.strip_prefix('@') {
        if source == "-" {
            let mut buffer = String::new();
            std::io::stdin()
                .read_to_string(&mut buffer)
                .context("the body could not be read from standard input")?;
            buffer
        } else {
            std::fs::read_to_string(source)
                .with_context(|| format!("the body could not be read from `{source}`"))?
        }
    } else {
        raw.clone()
    };

    Ok(if is_json {
        Body::Json { content }
    } else {
        Body::Text { content }
    })
}

#[cfg(test)]
mod tests {
    // In tests, `unwrap` documents the expectation and its panic IS the failure.
    #![allow(clippy::unwrap_used)]

    use super::*;

    fn args(request: &str) -> RunArgs {
        RunArgs {
            request: request.to_owned(),
            method: None,
            headers: Vec::new(),
            data: None,
            json: None,
            environment: None,
            variables: Vec::new(),
            extract: None,
            no_history: false,
            insecure: false,
        }
    }

    #[test]
    fn with_no_body_and_no_method_it_is_a_get() {
        let definition = build(&args("https://a.test")).unwrap();

        assert_eq!(definition.method, HttpMethod::Get);
        assert_eq!(definition.body, Body::Empty);
    }

    #[test]
    fn with_a_body_and_no_method_it_is_a_post() {
        let mut args = args("https://a.test");
        args.json = Some("{}".to_owned());

        assert_eq!(build(&args).unwrap().method, HttpMethod::Post);
    }

    #[test]
    fn the_explicit_method_is_in_charge() {
        let mut args = args("https://a.test");
        args.method = Some("delete".to_owned());
        args.json = Some("{}".to_owned());

        assert_eq!(build(&args).unwrap().method, HttpMethod::Delete);
    }

    #[test]
    fn an_unknown_method_is_an_error() {
        let mut args = args("https://a.test");
        args.method = Some("TELEPORT".to_owned());

        assert!(build(&args).is_err());
    }

    #[test]
    fn json_declares_the_content_type() {
        let mut args = args("https://a.test");
        args.json = Some(r#"{"a":1}"#.to_owned());

        let definition = build(&args).unwrap();
        assert!(
            definition
                .headers
                .iter()
                .any(|h| h.name == "Content-Type" && h.value == "application/json")
        );
        assert_eq!(
            definition.body,
            Body::Json {
                content: r#"{"a":1}"#.to_owned()
            }
        );
    }

    #[test]
    fn a_header_declared_by_hand_wins() {
        let mut args = args("https://a.test");
        args.json = Some("{}".to_owned());
        args.headers = vec![(
            "Content-Type".to_owned(),
            "application/vnd.api+json".to_owned(),
        )];

        let definition = build(&args).unwrap();
        let types: Vec<_> = definition
            .headers
            .iter()
            .filter(|h| h.name.eq_ignore_ascii_case("content-type"))
            .collect();

        assert_eq!(types.len(), 1);
        assert_eq!(types[0].value, "application/vnd.api+json");
    }

    #[test]
    fn data_is_text_with_no_implicit_content_type() {
        let mut args = args("https://a.test");
        args.data = Some("hello".to_owned());

        let definition = build(&args).unwrap();
        assert_eq!(
            definition.body,
            Body::Text {
                content: "hello".to_owned()
            }
        );
        assert!(definition.headers.is_empty());
    }

    #[test]
    fn an_at_sign_reads_the_body_from_a_file() {
        let path = std::env::temp_dir().join("hts-adhoc-test.json");
        std::fs::write(&path, r#"{"from":"file"}"#).unwrap();

        let mut args = args("https://a.test");
        args.json = Some(format!("@{}", path.display()));

        assert_eq!(
            build(&args).unwrap().body,
            Body::Json {
                content: r#"{"from":"file"}"#.to_owned()
            }
        );

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn a_file_that_does_not_exist_is_an_error() {
        let mut args = args("https://a.test");
        args.data = Some("@/does/not/ever-exist.json".to_owned());

        assert!(build(&args).is_err());
    }

    #[test]
    fn the_url_is_kept_with_its_placeholders() {
        // It is stored unresolved: interpolating it is the engine's job, and
        // that way an ad-hoc request can use the environment's variables too.
        let definition = build(&args("{{base_url}}/health")).unwrap();

        assert_eq!(definition.url, "{{base_url}}/health");
    }
}
