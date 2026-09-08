//! Building the variable stack with the product's precedence.
//!
//! It sits in its own module because the *order* is a product decision that
//! gets looked up often and should be documented in exactly one place. The
//! domain knows *how* to resolve variables; this module decides *who wins*.

use std::collections::BTreeMap;

use http_studio_domain::{DynamicSeed, Environment, VariableContext, VariableScope};

use crate::ports::LocatedRequest;

/// Assembles the variable context that applies to a request.
///
/// Scopes are stacked from lowest to highest priority, so that **the last one
/// pushed wins**:
///
/// | # | Scope | Why it sits there |
/// |---|-------|-------------------|
/// | 1 | Collection | Defaults shared by its requests. |
/// | 2 | Request | More specific than the collection holding it. |
/// | 3 | Environment | The user's explicit run-time choice (`--env`). |
/// | 4 | Secrets | Never in the file; they stand in for the placeholder. |
/// | 5 | Overrides | `--var k=v`, as explicit as it gets: it always wins. |
///
/// Each scope keeps an origin label, so a UI can explain where every value came
/// from with [`VariableContext::lookup`].
#[must_use]
pub fn build_variable_context(
    located: &LocatedRequest,
    environment: Option<&Environment>,
    secrets: BTreeMap<String, String>,
    overrides: BTreeMap<String, String>,
    dynamic: DynamicSeed,
) -> VariableContext {
    // The seed is not a scope: `{{$timestamp}}` competes with nobody for
    // precedence, so it has no place in the table above.
    let mut context = VariableContext::new().with_dynamic(dynamic);

    context.push(VariableScope::from_map(
        format!("collection:{}", located.collection_name),
        located.collection_variables.clone(),
    ));

    context.push(VariableScope::from_map(
        format!("request:{}", located.definition.id),
        located.definition.variables.clone(),
    ));

    if let Some(environment) = environment {
        context.push(VariableScope::from_map(
            format!("environment:{}", environment.name),
            environment.variables.clone(),
        ));
    }

    if !secrets.is_empty() {
        context.push(VariableScope::from_map("secrets", secrets));
    }

    if !overrides.is_empty() {
        context.push(VariableScope::from_map("cli", overrides));
    }

    context
}

#[cfg(test)]
mod tests {
    // In tests, `unwrap` documents the expectation and its panic IS the failure.
    #![allow(clippy::unwrap_used)]

    use super::*;
    use http_studio_domain::{HttpMethod, RequestDefinition, RequestId, RequestOptions};

    /// A fixed seed: these tests are about precedence, not dynamic variables.
    const SEED: DynamicSeed = DynamicSeed::fixed(0, 0);

    fn located(collection_var: &str, request_var: &str) -> LocatedRequest {
        let mut collection_variables = BTreeMap::new();
        collection_variables.insert("host".to_owned(), collection_var.to_owned());

        let mut variables = BTreeMap::new();
        variables.insert("host".to_owned(), request_var.to_owned());

        LocatedRequest {
            collection_name: "demo".to_owned(),
            collection_variables,
            definition: RequestDefinition {
                id: RequestId::new("demo/x"),
                name: "X".to_owned(),
                method: HttpMethod::Get,
                url: "{{host}}".to_owned(),
                headers: vec![],
                query: vec![],
                body: http_studio_domain::Body::Empty,
                variables,
                description: None,
                options: RequestOptions::default(),
            },
        }
    }

    fn map(key: &str, value: &str) -> BTreeMap<String, String> {
        BTreeMap::from([(key.to_owned(), value.to_owned())])
    }

    #[test]
    fn the_request_beats_the_collection() {
        let context = build_variable_context(
            &located("collection", "request"),
            None,
            BTreeMap::new(),
            BTreeMap::new(),
            SEED,
        );
        assert_eq!(context.get("host"), Some("request"));
    }

    #[test]
    fn the_environment_beats_the_request() {
        let environment = Environment {
            name: "prod".to_owned(),
            variables: map("host", "environment"),
        };
        let context = build_variable_context(
            &located("collection", "request"),
            Some(&environment),
            BTreeMap::new(),
            BTreeMap::new(),
            SEED,
        );
        assert_eq!(context.get("host"), Some("environment"));
    }

    #[test]
    fn secrets_beat_the_environment() {
        let environment = Environment {
            name: "prod".to_owned(),
            variables: map("host", "environment"),
        };
        let context = build_variable_context(
            &located("collection", "request"),
            Some(&environment),
            map("host", "secret"),
            BTreeMap::new(),
            SEED,
        );
        assert_eq!(context.get("host"), Some("secret"));
    }

    #[test]
    fn cli_overrides_beat_everything() {
        let environment = Environment {
            name: "prod".to_owned(),
            variables: map("host", "environment"),
        };
        let context = build_variable_context(
            &located("collection", "request"),
            Some(&environment),
            map("host", "secret"),
            map("host", "cli"),
            SEED,
        );
        assert_eq!(context.get("host"), Some("cli"));
    }

    #[test]
    fn the_origin_scope_is_recorded() {
        let context = build_variable_context(
            &located("collection", "request"),
            None,
            BTreeMap::new(),
            BTreeMap::new(),
            SEED,
        );
        assert_eq!(context.lookup("host").unwrap().0, "request:demo/x");
    }
}
