//! Domain service: resolving and interpolating variables.
//!
//! This is pure business logic, which is why it lives in the domain and not in
//! the HTTP engine: which value wins when `base_url` is defined in three places
//! at once is a product decision, not a transport detail.
//!
//! # Precedence
//!
//! A [`VariableContext`] is a stack of [`VariableScope`]s. **The last scope
//! pushed wins.** The order the application assembles, from lowest to highest
//! priority:
//!
//! 1. Collection variables
//! 2. Request variables (more specific than their collection)
//! 3. Variables of the selected environment (`--env prod`)
//! 4. Secrets (the OS keychain, or the process environment)
//! 5. Overrides from the invocation (`--var token=abc`)
//!
//! That concrete order is assembled by `http_studio_application::context`; the
//! domain only guarantees that the last scope pushed wins.
//!
//! # Syntax
//!
//! - `{{name}}` is replaced by the value of `name`.
//! - Spaces are allowed: `{{ name }}` is equivalent.
//! - Values may themselves contain placeholders; they are resolved in cascade,
//!   with cycle detection.
//! - `\{{` produces a literal `{{`.
//! - A name starting with `$` is a **dynamic variable** (`{{$timestamp}}`,
//!   `{{$uuid}}`, `{{$randomInt 1 100}}`) and is not looked up in the scopes:
//!   [`crate::dynamic`] generates it from the seed the context carries.

use std::collections::BTreeMap;

use crate::dynamic::{DynamicSeed, resolve_dynamic};
use crate::error::DomainError;

/// One named layer of variables, so a value's origin can be explained.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct VariableScope {
    /// A readable label for the origin (`environment:prod`, `cli`, `collection:auth`…).
    pub origin: String,
    /// The variables this layer contributes.
    pub values: BTreeMap<String, String>,
}

impl VariableScope {
    /// Creates an empty scope with the given origin label.
    #[must_use]
    pub fn new(origin: impl Into<String>) -> Self {
        Self {
            origin: origin.into(),
            values: BTreeMap::new(),
        }
    }

    /// Creates a scope from an already built map.
    #[must_use]
    pub fn from_map(origin: impl Into<String>, values: BTreeMap<String, String>) -> Self {
        Self {
            origin: origin.into(),
            values,
        }
    }

    /// Adds a variable to the scope, builder style.
    #[must_use]
    pub fn with(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.values.insert(key.into(), value.into());
        self
    }

    /// `true` when the scope contributes no variables.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }
}

/// A stack of scopes that resolves names according to the precedence rules.
#[derive(Debug, Clone, Default)]
pub struct VariableContext {
    /// Scopes ordered from lowest to highest precedence.
    scopes: Vec<VariableScope>,
    /// The dynamic variables' seed, when whoever built the context supplied one.
    ///
    /// It is optional so a test context can still be built with
    /// `VariableContext::new()`. Without it, a `{{$timestamp}}` fails saying the
    /// variable is undefined rather than inventing a time.
    dynamic: Option<DynamicSeed>,
}

impl VariableContext {
    /// Creates an empty context.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Pushes a scope; the ones added later take priority.
    pub fn push(&mut self, scope: VariableScope) {
        self.scopes.push(scope);
    }

    /// The builder-style variant of [`VariableContext::push`].
    #[must_use]
    pub fn with_scope(mut self, scope: VariableScope) -> Self {
        self.push(scope);
        self
    }

    /// Sets the seed the dynamic variables are generated from.
    #[must_use]
    pub fn with_dynamic(mut self, seed: DynamicSeed) -> Self {
        self.dynamic = Some(seed);
        self
    }

    /// The context's dynamic seed, when it has one.
    #[must_use]
    pub fn dynamic(&self) -> Option<DynamicSeed> {
        self.dynamic
    }

    /// Looks up a variable's value, honouring precedence.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&str> {
        self.lookup(name).map(|(_, value)| value)
    }

    /// Like [`VariableContext::get`], but also saying which scope the value
    /// came from, so UIs can explain how it resolved.
    #[must_use]
    pub fn lookup(&self, name: &str) -> Option<(&str, &str)> {
        self.scopes.iter().rev().find_map(|scope| {
            scope
                .values
                .get(name)
                .map(|v| (scope.origin.as_str(), v.as_str()))
        })
    }

    /// The scopes, from lowest to highest precedence.
    #[must_use]
    pub fn scopes(&self) -> &[VariableScope] {
        &self.scopes
    }

    /// Every visible variable name, deduplicated and sorted.
    #[must_use]
    pub fn names(&self) -> Vec<&str> {
        let mut names: Vec<&str> = self
            .scopes
            .iter()
            .flat_map(|scope| scope.values.keys().map(String::as_str))
            .collect();
        names.sort_unstable();
        names.dedup();
        names
    }
}

/// Replaces the `{{...}}` placeholders in `template` using `context`.
///
/// # Errors
///
/// - [`DomainError::UndefinedVariable`] when a name that does not exist is used.
/// - [`DomainError::UnterminatedPlaceholder`] when the closing `}}` is missing.
/// - [`DomainError::CircularReference`] when two variables reference each other.
///
/// # Examples
///
/// ```
/// use http_studio_domain::{VariableContext, VariableScope, interpolate};
///
/// let ctx = VariableContext::new()
///     .with_scope(VariableScope::new("environment").with("host", "api.example.com"));
///
/// assert_eq!(interpolate("https://{{host}}/v1", &ctx)?, "https://api.example.com/v1");
/// # Ok::<(), http_studio_domain::DomainError>(())
/// ```
pub fn interpolate(template: &str, context: &VariableContext) -> Result<String, DomainError> {
    let mut visiting = Vec::new();
    expand(template, context, &mut visiting)
}

/// The recursive implementation of [`interpolate`].
///
/// `visiting` keeps the chain of variables currently being expanded, so cycles
/// like `a -> b -> a` can be spotted.
fn expand(
    template: &str,
    context: &VariableContext,
    visiting: &mut Vec<String>,
) -> Result<String, DomainError> {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;

    loop {
        let Some(open) = rest.find("{{") else {
            out.push_str(rest);
            return Ok(out);
        };

        // `\{{` escapes the delimiter and produces a literal `{{`.
        if open > 0 && rest.as_bytes()[open - 1] == b'\\' {
            out.push_str(&rest[..open - 1]);
            out.push_str("{{");
            rest = &rest[open + 2..];
            continue;
        }

        out.push_str(&rest[..open]);
        let after_open = &rest[open + 2..];

        let Some(close) = after_open.find("}}") else {
            // The position is reported relative to the whole template.
            let position = template.len() - rest.len() + open;
            return Err(DomainError::UnterminatedPlaceholder { position });
        };

        let name = after_open[..close].trim();
        out.push_str(&resolve_name(name, context, visiting)?);
        rest = &after_open[close + 2..];
    }
}

/// Resolves a name and recursively expands its value.
fn resolve_name(
    name: &str,
    context: &VariableContext,
    visiting: &mut Vec<String>,
) -> Result<String, DomainError> {
    // A leading `$` takes it out of the scope game: it is not a value someone
    // wrote in a file, it is one generated now. It is checked before the lookup
    // so that declaring `@$timestamp = x` cannot shadow it.
    if name.starts_with('$') {
        let seed = context
            .dynamic
            .ok_or_else(|| DomainError::UndefinedVariable {
                name: name.to_owned(),
            })?;
        return resolve_dynamic(name, seed);
    }

    let value = context
        .get(name)
        .ok_or_else(|| DomainError::UndefinedVariable {
            name: name.to_owned(),
        })?;

    // With no nested placeholders there is no need to recurse or touch the stack.
    if !value.contains("{{") {
        return Ok(value.to_owned());
    }

    if visiting.iter().any(|seen| seen == name) {
        let mut chain = visiting.clone();
        chain.push(name.to_owned());
        return Err(DomainError::CircularReference { chain });
    }

    visiting.push(name.to_owned());
    let owned = value.to_owned();
    let expanded = expand(&owned, context, visiting);
    visiting.pop();
    expanded
}

#[cfg(test)]
mod tests {
    // In tests, `unwrap` documents the expectation and its panic IS the failure.
    #![allow(clippy::unwrap_used)]

    use super::*;

    fn ctx() -> VariableContext {
        VariableContext::new().with_scope(
            VariableScope::new("test")
                .with("host", "api.example.com")
                .with("version", "v1")
                .with("base", "https://{{host}}/{{version}}"),
        )
    }

    #[test]
    fn substitutes_a_simple_variable() {
        assert_eq!(
            interpolate("https://{{host}}/health", &ctx()).unwrap(),
            "https://api.example.com/health"
        );
    }

    #[test]
    fn allows_spaces_inside_the_placeholder() {
        assert_eq!(
            interpolate("{{  host  }}", &ctx()).unwrap(),
            "api.example.com"
        );
    }

    #[test]
    fn expands_nested_variables_in_cascade() {
        assert_eq!(
            interpolate("{{base}}/users", &ctx()).unwrap(),
            "https://api.example.com/v1/users"
        );
    }

    #[test]
    fn leaves_text_without_placeholders_untouched() {
        assert_eq!(
            interpolate("no variables here", &ctx()).unwrap(),
            "no variables here"
        );
    }

    #[test]
    fn the_last_scope_pushed_wins() {
        let context = VariableContext::new()
            .with_scope(VariableScope::new("collection").with("host", "dev.local"))
            .with_scope(VariableScope::new("cli").with("host", "prod.example.com"));

        assert_eq!(
            interpolate("{{host}}", &context).unwrap(),
            "prod.example.com"
        );
    }

    #[test]
    fn lookup_reports_the_scope_a_value_came_from() {
        let context = VariableContext::new()
            .with_scope(VariableScope::new("collection").with("host", "dev.local"))
            .with_scope(VariableScope::new("cli").with("host", "prod.example.com"));

        assert_eq!(context.lookup("host"), Some(("cli", "prod.example.com")));
    }

    #[test]
    fn an_undefined_variable_is_an_error() {
        let err = interpolate("{{ausente}}", &ctx()).unwrap_err();
        assert_eq!(
            err,
            DomainError::UndefinedVariable {
                name: "ausente".to_owned()
            }
        );
    }

    #[test]
    fn an_unclosed_placeholder_is_an_error() {
        let err = interpolate("hola {{host", &ctx()).unwrap_err();
        assert_eq!(err, DomainError::UnterminatedPlaceholder { position: 5 });
    }

    #[test]
    fn detects_circular_references() {
        let context = VariableContext::new().with_scope(
            VariableScope::new("test")
                .with("a", "{{b}}")
                .with("b", "{{a}}"),
        );

        let err = interpolate("{{a}}", &context).unwrap_err();
        assert!(matches!(err, DomainError::CircularReference { .. }));
    }

    #[test]
    fn a_backslash_escapes_the_delimiter() {
        assert_eq!(
            interpolate(r"literal \{{host}}", &ctx()).unwrap(),
            "literal {{host}}"
        );
    }

    #[test]
    fn names_returns_unique_sorted_names() {
        let context = VariableContext::new()
            .with_scope(VariableScope::new("a").with("z", "1").with("a", "2"))
            .with_scope(VariableScope::new("b").with("a", "3"));

        assert_eq!(context.names(), vec!["a", "z"]);
    }
}
