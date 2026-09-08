//! Domain service: turns a [`RequestDefinition`] into a [`ResolvedRequest`].
//!
//! All the "what actually gets sent" logic lives here:
//!
//! - interpolating variables in the URL, headers, query and body,
//! - merging the separately declared query with whatever the URL already
//!   carries,
//! - deriving the implied `Content-Type` from the body type without stepping on
//!   one the user wrote,
//! - validating that the resulting URL is absolute.
//!
//! It is a pure function over data: it is tested in full without network or
//! files, and any UI can call it to show a faithful preview of the request.

use crate::error::DomainError;
use crate::model::{Body, Header, RequestDefinition, ResolvedRequest};
use crate::variables::{VariableContext, interpolate};

/// The header we never overwrite when the user has already written it.
const CONTENT_TYPE: &str = "content-type";

/// Resolves a definition against a variable context.
///
/// # Errors
///
/// Returns a [`DomainError`] when an interpolation fails, or when the resulting
/// URL is not a valid absolute URL.
///
/// # Examples
///
/// ```
/// use http_studio_domain::model::{HttpMethod, RequestDefinition, RequestId};
/// use http_studio_domain::resolver::resolve;
/// use http_studio_domain::{VariableContext, VariableScope};
///
/// let definition = RequestDefinition {
///     id: RequestId::new("health"),
///     name: "Health".into(),
///     method: HttpMethod::Get,
///     url: "{{base_url}}/health".into(),
///     headers: vec![],
///     query: vec![],
///     body: Default::default(),
///     variables: Default::default(),
///     description: None,
///     options: Default::default(),
/// };
/// let ctx = VariableContext::new()
///     .with_scope(VariableScope::new("env").with("base_url", "https://api.example.com"));
///
/// let resolved = resolve(&definition, &ctx)?;
/// assert_eq!(resolved.url.as_str(), "https://api.example.com/health");
/// # Ok::<(), http_studio_domain::DomainError>(())
/// ```
pub fn resolve(
    definition: &RequestDefinition,
    context: &VariableContext,
) -> Result<ResolvedRequest, DomainError> {
    let url = resolve_url(definition, context)?;
    let body = resolve_body(&definition.body, context)?;
    let headers = resolve_headers(definition, &body, context)?;

    Ok(ResolvedRequest {
        id: definition.id.clone(),
        method: definition.method,
        url,
        headers,
        body,
        options: definition.options,
    })
}

/// Interpolates the URL and appends the separately declared `query` parameters.
fn resolve_url(
    definition: &RequestDefinition,
    context: &VariableContext,
) -> Result<url::Url, DomainError> {
    let raw = interpolate(&definition.url, context)?;

    let mut url = url::Url::parse(&raw).map_err(|error| DomainError::InvalidUrl {
        url: raw.clone(),
        reason: error.to_string(),
    })?;

    // `query_pairs_mut` keeps the parameters the URL already carried, so both
    // ways of declaring them can be mixed without surprises.
    if !definition.query.is_empty() {
        let mut pairs = url.query_pairs_mut();
        for (key, value) in &definition.query {
            pairs.append_pair(&interpolate(key, context)?, &interpolate(value, context)?);
        }
    }

    Ok(url)
}

/// Interpolates the body's content while preserving its type.
fn resolve_body(body: &Body, context: &VariableContext) -> Result<Body, DomainError> {
    Ok(match body {
        Body::Empty => Body::Empty,
        Body::Text { content } => Body::Text {
            content: interpolate(content, context)?,
        },
        Body::Json { content } => Body::Json {
            content: interpolate(content, context)?,
        },
        Body::Form { fields } => Body::Form {
            fields: fields
                .iter()
                .map(|(key, value)| Ok((interpolate(key, context)?, interpolate(value, context)?)))
                .collect::<Result<Vec<_>, DomainError>>()?,
        },
    })
}

/// Interpolates the headers and adds the implied `Content-Type` when needed.
fn resolve_headers(
    definition: &RequestDefinition,
    body: &Body,
    context: &VariableContext,
) -> Result<Vec<Header>, DomainError> {
    let mut headers = definition
        .headers
        .iter()
        .map(|header| {
            Ok(Header::new(
                interpolate(&header.name, context)?,
                interpolate(&header.value, context)?,
            ))
        })
        .collect::<Result<Vec<_>, DomainError>>()?;

    let declared_content_type = headers
        .iter()
        .any(|header| header.name.eq_ignore_ascii_case(CONTENT_TYPE));

    if let Some(implied) = body.implied_content_type()
        && !declared_content_type
    {
        headers.push(Header::new("Content-Type", implied));
    }

    Ok(headers)
}

#[cfg(test)]
mod tests {
    // In tests, `unwrap` documents the expectation and its panic IS the failure.
    #![allow(clippy::unwrap_used)]

    use super::*;
    use crate::model::{HttpMethod, RequestId, RequestOptions};
    use crate::variables::VariableScope;
    use std::collections::BTreeMap;

    fn definition() -> RequestDefinition {
        RequestDefinition {
            id: RequestId::new("demo"),
            name: "Demo".into(),
            method: HttpMethod::Post,
            url: "{{base_url}}/users".into(),
            headers: vec![],
            query: vec![],
            body: Body::Empty,
            variables: BTreeMap::new(),
            description: None,
            options: RequestOptions::default(),
        }
    }

    fn context() -> VariableContext {
        VariableContext::new().with_scope(
            VariableScope::new("test")
                .with("base_url", "https://api.example.com")
                .with("token", "s3cr3t"),
        )
    }

    #[test]
    fn interpolates_the_url() {
        let resolved = resolve(&definition(), &context()).unwrap();
        assert_eq!(resolved.url.as_str(), "https://api.example.com/users");
    }

    #[test]
    fn transport_options_survive_resolution() {
        // The transport only receives the resolved request: if the options
        // stayed behind in the definition, `# @insecure` would never apply.
        let mut def = definition();
        def.options.insecure_tls = true;

        assert!(resolve(&def, &context()).unwrap().options.insecure_tls);
    }

    #[test]
    fn interpolates_the_headers() {
        let mut def = definition();
        def.headers = vec![Header::new("Authorization", "Bearer {{token}}")];

        let resolved = resolve(&def, &context()).unwrap();
        assert_eq!(resolved.headers[0].value, "Bearer s3cr3t");
    }

    #[test]
    fn adds_the_implied_content_type_for_json() {
        let mut def = definition();
        def.body = Body::Json {
            content: r#"{"name":"{{token}}"}"#.into(),
        };

        let resolved = resolve(&def, &context()).unwrap();
        assert!(
            resolved
                .headers
                .iter()
                .any(|h| h.name == "Content-Type" && h.value == "application/json")
        );
    }

    #[test]
    fn respects_a_content_type_the_user_declared() {
        let mut def = definition();
        def.body = Body::Json {
            content: "{}".into(),
        };
        def.headers = vec![Header::new("content-type", "application/vnd.api+json")];

        let resolved = resolve(&def, &context()).unwrap();
        let content_types: Vec<_> = resolved
            .headers
            .iter()
            .filter(|h| h.name.eq_ignore_ascii_case("content-type"))
            .collect();

        assert_eq!(content_types.len(), 1);
        assert_eq!(content_types[0].value, "application/vnd.api+json");
    }

    #[test]
    fn merges_the_query_with_the_one_the_url_carries() {
        let mut def = definition();
        def.url = "{{base_url}}/users?page=1".into();
        def.query = vec![("limit".into(), "10".into())];

        let resolved = resolve(&def, &context()).unwrap();
        assert_eq!(resolved.url.query(), Some("page=1&limit=10"));
    }

    #[test]
    fn interpolates_a_json_body() {
        let mut def = definition();
        def.body = Body::Json {
            content: r#"{"token":"{{token}}"}"#.into(),
        };

        let resolved = resolve(&def, &context()).unwrap();
        assert_eq!(
            resolved.body,
            Body::Json {
                content: r#"{"token":"s3cr3t"}"#.into()
            }
        );
    }

    #[test]
    fn a_relative_url_is_an_error() {
        let mut def = definition();
        def.url = "/solo/ruta".into();

        assert!(matches!(
            resolve(&def, &context()),
            Err(DomainError::InvalidUrl { .. })
        ));
    }

    #[test]
    fn propagates_the_undefined_variable_error() {
        let mut def = definition();
        def.url = "{{ausente}}/x".into();

        assert!(matches!(
            resolve(&def, &context()),
            Err(DomainError::UndefinedVariable { .. })
        ));
    }
}
