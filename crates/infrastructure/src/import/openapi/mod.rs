//! Reading an OpenAPI description as a workspace of requests.
//!
//! It takes OpenAPI 3.0 and 3.1, and Swagger 2.0, in JSON or in YAML — the
//! shapes descriptions actually arrive in. One operation becomes one request,
//! and each tag becomes a collection, which is the grouping the document
//! already chose for itself.
//!
//! # What ends up in the file
//!
//! - `@base_url`, from the first server the document names. It is a collection
//!   variable, so an environment overrides it: importing a public description
//!   and then running it against staging with `--env` needs no editing.
//! - The path, with `{petId}` rewritten as `{{petId}}` and declared as a
//!   request variable with an example value. That is what makes an imported
//!   request runnable rather than a template to fill in.
//! - **Required** parameters only. Optional ones are left out on purpose:
//!   there is no way to write a parameter that is present but disabled, and a
//!   filter nobody asked for silently changing a response is worse than
//!   having to add it by hand.
//! - A body built from the schema, with every property present and `$ref`s
//!   followed.
//! - The authentication the operation declares, as a header with a
//!   placeholder: `{{token}}`, `{{api_key}}` or `{{basic_auth}}`. Nothing is
//!   declared for them, so the engine says "undefined variable" until the
//!   value is put in `http-client.private.env.json` — which is where a secret
//!   belongs, and not in the collection that gets committed.
//!
//! # What it leaves out
//!
//! `trace` operations, which the engine has no method for, and external
//! `$ref`s, which live in a document this importer will not go and fetch. A
//! description is otherwise imported whole: refusing all of it over one
//! operation nobody wanted would be the wrong trade.

mod example;

use std::collections::BTreeMap;

use http_studio_application::{ApplicationError, CollectionImporter};
use http_studio_domain::{
    Body, Collection, Header, HttpMethod, RequestDefinition, RequestId, RequestOptions,
};
use serde_json::Value;

use crate::import::openapi::example::Examples;
use crate::storage::http_file::slug;

/// The format's name, as it appears in error messages.
const FORMAT: &str = "OpenAPI";

/// The variable the server's URL is written to.
const BASE_URL: &str = "base_url";

/// The collection operations with no tag land in.
const UNTAGGED: &str = "openapi";

/// The methods an operation can be, in the order they are emitted.
///
/// `trace` is deliberately absent: the engine has no method for it, and the
/// honest thing is to leave the operation out rather than send something else.
const METHODS: &[(&str, HttpMethod)] = &[
    ("get", HttpMethod::Get),
    ("post", HttpMethod::Post),
    ("put", HttpMethod::Put),
    ("patch", HttpMethod::Patch),
    ("delete", HttpMethod::Delete),
    ("head", HttpMethod::Head),
    ("options", HttpMethod::Options),
];

/// Reads OpenAPI descriptions as collections.
#[derive(Debug, Clone, Default)]
pub struct OpenApiImporter {
    tags: Vec<String>,
}

impl OpenApiImporter {
    /// An importer that takes every operation.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Keeps only the operations carrying one of `tags`.
    ///
    /// A public description can run to hundreds of operations, and wanting the
    /// four that concern you is the normal case rather than the exception.
    #[must_use]
    pub fn with_tags(mut self, tags: Vec<String>) -> Self {
        self.tags = tags;
        self
    }

    /// `true` when the operation's tag passes the filter.
    fn wanted(&self, tag: Option<&str>) -> bool {
        if self.tags.is_empty() {
            return true;
        }

        self.tags.iter().any(|wanted| {
            tag.is_some_and(|tag| tag.eq_ignore_ascii_case(wanted) || slug(tag) == slug(wanted))
        })
    }
}

impl CollectionImporter for OpenApiImporter {
    fn import(&self, document: &str) -> Result<Vec<Collection>, ApplicationError> {
        let value = parse(document)?;
        let document = Document::read(&value)?;

        let paths = value
            .get("paths")
            .and_then(Value::as_object)
            .ok_or_else(|| fail("it declares no `paths`"))?;

        let mut groups: BTreeMap<String, Vec<RequestDefinition>> = BTreeMap::new();

        for (path, item) in paths {
            let shared = document.parameters(item.get("parameters"));

            for (name, method) in METHODS {
                let Some(operation) = item.get(name) else {
                    continue;
                };

                let tag = operation
                    .get("tags")
                    .and_then(Value::as_array)
                    .and_then(|tags| tags.first())
                    .and_then(Value::as_str);

                if !self.wanted(tag) {
                    continue;
                }

                let collection = tag.map_or_else(|| document.fallback.clone(), slug);
                let request = document.request(&collection, path, *method, operation, &shared);
                groups.entry(collection).or_default().push(request);
            }
        }

        if groups.is_empty() && !self.tags.is_empty() {
            return Err(fail(format!(
                "no operation carries any of the tags {}",
                self.tags.join(", ")
            )));
        }

        Ok(groups
            .into_iter()
            .map(|(name, requests)| Collection {
                name,
                // The server travels as a collection variable, which is the
                // layer an environment overrides: that is what lets a
                // description imported from a public URL be pointed at
                // staging without editing a single request.
                variables: BTreeMap::from([(BASE_URL.to_owned(), document.base_url.clone())]),
                requests,
            })
            .collect())
    }
}

/// Reads the document as JSON, and failing that as YAML.
///
/// JSON first because it is the cheaper parse and the commoner download; YAML
/// second because it is what descriptions kept in a repository look like. A
/// document that is neither is reported as neither.
fn parse(document: &str) -> Result<Value, ApplicationError> {
    if let Ok(value) = serde_json::from_str::<Value>(document) {
        return Ok(value);
    }

    serde_norway::from_str::<Value>(document).map_err(|error| {
        fail(format!(
            "it is neither JSON nor YAML: {}",
            error.to_string().replace('\n', " ")
        ))
    })
}

/// An import error for this format.
fn fail(message: impl std::fmt::Display) -> ApplicationError {
    ApplicationError::import(FORMAT, message)
}

/// A description, with everything the walk needs resolved up front.
struct Document<'a> {
    root: &'a Value,
    /// The URL every request hangs off.
    base_url: String,
    /// The collection untagged operations land in.
    fallback: String,
    /// The document's own `security`, applied when an operation states none.
    security: Option<&'a Value>,
}

impl<'a> Document<'a> {
    /// Reads the document's header: version, server, title, security.
    fn read(root: &'a Value) -> Result<Self, ApplicationError> {
        let swagger = root.get("swagger").and_then(Value::as_str);
        let openapi = root.get("openapi").and_then(Value::as_str);

        if openapi.is_none() && swagger.is_none() {
            return Err(fail(
                "it declares neither `openapi` nor `swagger`, so it is not a description",
            ));
        }

        let base_url = if swagger.is_some() {
            swagger_base_url(root)
        } else {
            openapi_base_url(root)
        };

        let fallback = root
            .get("info")
            .and_then(|info| info.get("title"))
            .and_then(Value::as_str)
            .map(slug)
            .filter(|title| !title.is_empty())
            .unwrap_or_else(|| UNTAGGED.to_owned());

        Ok(Self {
            root,
            base_url,
            fallback,
            security: root.get("security"),
        })
    }

    /// Follows a `$ref` one level, so callers can treat both forms alike.
    fn resolve(&self, value: &'a Value) -> &'a Value {
        let Some(reference) = value.get("$ref").and_then(Value::as_str) else {
            return value;
        };

        reference
            .strip_prefix("#/")
            .and_then(|pointer| self.root.pointer(&format!("/{pointer}")))
            .unwrap_or(value)
    }

    /// The parameters of a list, with their `$ref`s resolved.
    fn parameters(&self, value: Option<&'a Value>) -> Vec<&'a Value> {
        value
            .and_then(Value::as_array)
            .map(|parameters| {
                parameters
                    .iter()
                    .map(|parameter| self.resolve(parameter))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Turns one operation into a request.
    fn request(
        &self,
        collection: &str,
        path: &str,
        method: HttpMethod,
        operation: &'a Value,
        shared: &[&'a Value],
    ) -> RequestDefinition {
        let name = operation
            .get("operationId")
            .and_then(Value::as_str)
            .map(|id| slug(&split_camel_case(id)))
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| slug(&format!("{method} {path}")));

        let parameters = merge_parameters(shared, &self.parameters(operation.get("parameters")));
        let mut variables = BTreeMap::new();
        let mut headers = Vec::new();
        let mut query = Vec::new();
        let mut cookies = Vec::new();
        let examples = Examples::new(self.root);

        for parameter in &parameters {
            let Some(name) = parameter.get("name").and_then(Value::as_str) else {
                continue;
            };
            let location = parameter.get("in").and_then(Value::as_str).unwrap_or("");
            let required = parameter.get("required").and_then(Value::as_bool) == Some(true);

            // A path parameter is part of the URL whether it says it is
            // required or not; everything else only travels when it must.
            if location != "path" && !required {
                continue;
            }

            let placeholder = slug_variable(name);
            variables.insert(placeholder.clone(), example_for(&examples, parameter));

            match location {
                "path" => {}
                "query" => query.push(format!("{name}={{{{{placeholder}}}}}")),
                "header" => headers.push(Header::new(name, format!("{{{{{placeholder}}}}}"))),
                "cookie" => cookies.push(format!("{name}={{{{{placeholder}}}}}")),
                // Swagger 2.0's `body` and `formData` are handled with the
                // body, not here.
                _ => {
                    variables.remove(&placeholder);
                }
            }
        }

        let (body, content_type) = self.body(operation, &parameters, &examples);
        if let Some(content_type) = content_type {
            headers.push(Header::new("Content-Type", content_type));
        }

        self.authenticate(operation, &mut headers, &mut query, &mut cookies);

        if !cookies.is_empty() {
            headers.push(Header::new("Cookie", cookies.join("; ")));
        }

        RequestDefinition {
            id: RequestId::new(format!("{collection}/{name}")),
            name,
            method,
            url: url_for(path, &query),
            headers,
            query: Vec::new(),
            body,
            variables,
            description: describe(operation),
            options: RequestOptions::default(),
        }
    }

    /// The body an operation declares, and the `Content-Type` that goes with it.
    fn body(
        &self,
        operation: &'a Value,
        parameters: &[&'a Value],
        examples: &Examples<'_>,
    ) -> (Body, Option<String>) {
        // Swagger 2.0 puts the body among the parameters; 3.x gives it a field
        // of its own. Both are read, so one importer covers both versions.
        if let Some(body) = parameters
            .iter()
            .find(|parameter| parameter.get("in").and_then(Value::as_str) == Some("body"))
        {
            let schema = body.get("schema").cloned().unwrap_or(Value::Null);
            let content_type = consumes(operation, self.root).unwrap_or("application/json");
            return (json_body(examples, &schema), Some(content_type.to_owned()));
        }

        let form: Vec<&Value> = parameters
            .iter()
            .copied()
            .filter(|parameter| parameter.get("in").and_then(Value::as_str) == Some("formData"))
            .collect();
        if !form.is_empty() {
            return (
                form_body(examples, &form),
                Some("application/x-www-form-urlencoded".to_owned()),
            );
        }

        let Some(request_body) = operation.get("requestBody").map(|body| self.resolve(body)) else {
            return (Body::Empty, None);
        };

        let Some(content) = request_body.get("content").and_then(Value::as_object) else {
            return (Body::Empty, None);
        };

        // JSON when it is on offer: it is the type whose example can actually
        // be built, and the one nearly every API prefers.
        let (content_type, media) = content
            .iter()
            .find(|(name, _)| name.contains("json"))
            .or_else(|| content.iter().next())
            .map_or_else(
                || ("application/json".to_owned(), &Value::Null),
                |(name, media)| (name.clone(), media),
            );

        let schema = media
            .get("example")
            .cloned()
            .or_else(|| {
                media
                    .get("examples")
                    .and_then(Value::as_object)
                    .and_then(|examples| examples.values().next())
                    .and_then(|first| first.get("value"))
                    .cloned()
            })
            .map_or_else(
                || media.get("schema").cloned().unwrap_or(Value::Null),
                |example| serde_json::json!({ "example": example }),
            );

        let body = if content_type.contains("json") {
            json_body(examples, &schema)
        } else if content_type.contains("x-www-form-urlencoded") {
            urlencoded_body(examples, &schema)
        } else {
            // XML, plain text, multipart: there is no example to build, and
            // inventing one would be worse than an empty body next to a
            // header that says what is expected.
            match examples.value(&schema) {
                Value::String(text) => Body::Text { content: text },
                _ => Body::Empty,
            }
        };

        (body, Some(content_type))
    }

    /// Adds the authentication the operation declares.
    ///
    /// The value is always a placeholder, never a value: a token in a
    /// versioned collection is a token in a pull request.
    fn authenticate(
        &self,
        operation: &'a Value,
        headers: &mut Vec<Header>,
        query: &mut Vec<String>,
        cookies: &mut Vec<String>,
    ) {
        let requirements = operation
            .get("security")
            .or(self.security)
            .and_then(Value::as_array);

        let Some(name) = requirements
            .and_then(|requirements| requirements.iter().find_map(first_scheme))
            .as_deref()
            .map(str::to_owned)
        else {
            return;
        };

        let Some(scheme) = self.scheme(&name) else {
            return;
        };

        let kind = scheme
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default();

        match kind {
            "http" => {
                let http_scheme = scheme
                    .get("scheme")
                    .and_then(Value::as_str)
                    .unwrap_or("bearer")
                    .to_ascii_lowercase();

                if http_scheme == "basic" {
                    headers.push(Header::new("Authorization", "Basic {{basic_auth}}"));
                } else {
                    headers.push(Header::new("Authorization", "Bearer {{token}}"));
                }
            }
            // Swagger 2.0's name for Basic auth.
            "basic" => headers.push(Header::new("Authorization", "Basic {{basic_auth}}")),
            "apiKey" => {
                let field = scheme
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("X-API-Key");

                match scheme.get("in").and_then(Value::as_str) {
                    Some("query") => query.push(format!("{field}={{{{api_key}}}}")),
                    Some("cookie") => cookies.push(format!("{field}={{{{api_key}}}}")),
                    _ => headers.push(Header::new(field, "{{api_key}}")),
                }
            }
            // OAuth and OIDC both end as a bearer token once the dance is
            // over, and the dance is not something a `.http` can hold.
            "oauth2" | "openIdConnect" => {
                headers.push(Header::new("Authorization", "Bearer {{token}}"));
            }
            _ => {}
        }
    }

    /// Looks a security scheme up, in either version's place for it.
    fn scheme(&self, name: &str) -> Option<&'a Value> {
        self.root
            .get("components")
            .and_then(|components| components.get("securitySchemes"))
            .or_else(|| self.root.get("securityDefinitions"))
            .and_then(|schemes| schemes.get(name))
            .map(|scheme| self.resolve(scheme))
    }
}

/// The first scheme named by one security requirement.
fn first_scheme(requirement: &Value) -> Option<String> {
    requirement
        .as_object()
        .and_then(|schemes| schemes.keys().next())
        .cloned()
}

/// The URL a request is written with.
///
/// The server never appears literally: the request points at `{{base_url}}`,
/// which is the collection variable, so an environment can redirect every
/// imported request at once.
fn url_for(path: &str, query: &[String]) -> String {
    let mut url = format!("{{{{{BASE_URL}}}}}{}", placeholders(path));

    if !query.is_empty() {
        url.push('?');
        url.push_str(&query.join("&"));
    }

    // A server URL that already ends in `/` plus a path that starts with one
    // is a double slash, which some servers answer and others 404.
    url.replace("}}//", "}}/")
}

/// Rewrites `{petId}` as the `{{petId}}` the format uses.
fn placeholders(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    let mut rest = path;

    while let Some(start) = rest.find('{') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];

        let Some(end) = after.find('}') else {
            // An unclosed brace is a typo in the description, and passing it
            // through unchanged is what shows it in the file.
            out.push_str(&rest[start..]);
            return out;
        };

        out.push_str("{{");
        out.push_str(&slug_variable(&after[..end]));
        out.push_str("}}");
        rest = &after[end + 1..];
    }

    out.push_str(rest);
    out
}

/// Puts a space at every camel-case boundary.
///
/// `operationId`s are written `listPets`, and slugging that straight gives
/// `listpets` — a name nobody wants to type at `hts run`. An uppercase run is
/// kept together up to its last letter, so `getHTTPStatus` comes out as
/// `get-http-status` rather than `get-h-t-t-p-status`.
fn split_camel_case(name: &str) -> String {
    let characters: Vec<char> = name.chars().collect();
    let mut out = String::with_capacity(name.len() + 4);

    for (index, character) in characters.iter().enumerate() {
        let previous = index.checked_sub(1).and_then(|at| characters.get(at));
        let next = characters.get(index + 1);

        let after_lowercase =
            previous.is_some_and(|previous| previous.is_lowercase() || previous.is_ascii_digit());
        let ends_an_uppercase_run = previous.is_some_and(|previous| previous.is_uppercase())
            && next.is_some_and(|next| next.is_lowercase());

        if character.is_uppercase() && (after_lowercase || ends_an_uppercase_run) {
            out.push(' ');
        }

        out.push(*character);
    }

    out
}

/// A parameter's name as a variable the interpolator accepts.
///
/// Names in a description carry dots and dashes (`filter.status`), and a
/// placeholder with those in it is read as a different name than the
/// declaration would be.
fn slug_variable(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '_' {
                character
            } else {
                '_'
            }
        })
        .collect();

    if cleaned.is_empty() {
        "value".to_owned()
    } else {
        cleaned
    }
}

/// The example value a parameter declares, or one built from its schema.
fn example_for(examples: &Examples<'_>, parameter: &Value) -> String {
    if let Some(example) = parameter.get("example") {
        return match example {
            Value::String(text) => text.clone(),
            other => other.to_string(),
        };
    }

    parameter.get("schema").map_or_else(
        || examples.scalar(parameter),
        |schema| examples.scalar(schema),
    )
}

/// An operation's one-line description.
fn describe(operation: &Value) -> Option<String> {
    operation
        .get("summary")
        .or_else(|| operation.get("description"))
        .and_then(Value::as_str)
        .map(|text| text.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|text| !text.is_empty())
}

/// Swagger 2.0's `consumes`, from the operation or the document.
fn consumes<'a>(operation: &'a Value, root: &'a Value) -> Option<&'a str> {
    operation
        .get("consumes")
        .or_else(|| root.get("consumes"))
        .and_then(Value::as_array)
        .and_then(|types| types.first())
        .and_then(Value::as_str)
}

/// A JSON body, pretty-printed so it can be edited in place.
fn json_body(examples: &Examples<'_>, schema: &Value) -> Body {
    let value = examples.value(schema);
    if value.is_null() {
        return Body::Empty;
    }

    serde_json::to_string_pretty(&value).map_or(Body::Empty, |content| Body::Json { content })
}

/// A form body out of Swagger 2.0's `formData` parameters.
fn form_body(examples: &Examples<'_>, parameters: &[&Value]) -> Body {
    let fields: Vec<String> = parameters
        .iter()
        .filter_map(|parameter| {
            let name = parameter.get("name").and_then(Value::as_str)?;
            Some(format!("{name}={}", example_for(examples, parameter)))
        })
        .collect();

    if fields.is_empty() {
        Body::Empty
    } else {
        Body::Text {
            content: fields.join("&"),
        }
    }
}

/// A urlencoded body out of a schema's properties.
fn urlencoded_body(examples: &Examples<'_>, schema: &Value) -> Body {
    let Value::Object(fields) = examples.value(schema) else {
        return Body::Empty;
    };

    let content = fields
        .iter()
        .map(|(name, value)| match value {
            Value::String(text) => format!("{name}={text}"),
            other => format!("{name}={other}"),
        })
        .collect::<Vec<_>>()
        .join("&");

    if content.is_empty() {
        Body::Empty
    } else {
        Body::Text { content }
    }
}

/// Merges path-level and operation-level parameters.
///
/// An operation may redeclare one of the path's, and when it does, its version
/// is the one that counts — which is what the specification says and also what
/// anyone reading the document would assume.
fn merge_parameters<'a>(shared: &[&'a Value], own: &[&'a Value]) -> Vec<&'a Value> {
    let key = |parameter: &Value| {
        format!(
            "{}:{}",
            parameter.get("in").and_then(Value::as_str).unwrap_or(""),
            parameter.get("name").and_then(Value::as_str).unwrap_or("")
        )
    };

    let overridden: Vec<String> = own.iter().map(|parameter| key(parameter)).collect();
    let mut merged: Vec<&Value> = shared
        .iter()
        .copied()
        .filter(|parameter| !overridden.contains(&key(parameter)))
        .collect();

    merged.extend(own.iter().copied());
    merged
}

/// The server URL of an OpenAPI 3.x description.
///
/// Server variables are substituted with their defaults: a URL still carrying
/// `{region}` resolves to nothing the transport can use, and the default is
/// the document's own answer to what it should be.
fn openapi_base_url(root: &Value) -> String {
    let Some(server) = root
        .get("servers")
        .and_then(Value::as_array)
        .and_then(|servers| servers.first())
    else {
        return String::new();
    };

    let url = server
        .get("url")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim_end_matches('/')
        .to_owned();

    let Some(variables) = server.get("variables").and_then(Value::as_object) else {
        return url;
    };

    variables.iter().fold(url, |url, (name, variable)| {
        let default = variable
            .get("default")
            .and_then(Value::as_str)
            .unwrap_or_default();
        url.replace(&format!("{{{name}}}"), default)
    })
}

/// The server URL of a Swagger 2.0 description, assembled from three fields.
fn swagger_base_url(root: &Value) -> String {
    let scheme = root
        .get("schemes")
        .and_then(Value::as_array)
        .and_then(|schemes| schemes.iter().find_map(Value::as_str))
        // 2.0 lets the scheme be left out, meaning "the one the description
        // was served over". HTTPS is the assumption worth making.
        .unwrap_or("https");

    let Some(host) = root.get("host").and_then(Value::as_str) else {
        return String::new();
    };

    let base = root
        .get("basePath")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim_end_matches('/');

    format!("{scheme}://{host}{base}")
}

#[cfg(test)]
mod tests;
