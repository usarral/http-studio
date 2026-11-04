//! The `.http` format writer: the parser's inverse.
//!
//! It exists because importing is the first operation that has to *produce*
//! `.http` text rather than read it. Everything written here goes back through
//! [`super::http_file::parse`] before it reaches the disk, so the round trip is
//! the contract, not a nicety.
//!
//! # What a round trip preserves
//!
//! Name, method, URL, headers, variables, description, the `@insecure`
//! directive and the body's bytes. What it does not preserve is anything the
//! model does not hold: a [`Body::Form`] comes back as [`Body::Text`] carrying
//! the same encoded bytes, and a [`RequestDefinition::query`] comes back merged
//! into the URL. Both send the same request, which is the property that
//! matters.
//!
//! # The one thing that cannot round trip
//!
//! A body holding a line that starts with `###` would be read back as a new
//! request, because the format has no escape for its separator. No client in
//! the ecosystem can represent that either, so it is written verbatim rather
//! than mangled: the file says what was asked for, and reading it back says
//! where it broke.

use std::fmt::Write as _;

use http_studio_domain::{Body, Collection, Header, RequestDefinition};

/// Renders a whole collection as the contents of a `.http` file.
///
/// The collection's variables become the file's preamble, and each request a
/// `###` block, in order.
#[must_use]
pub fn render_collection(collection: &Collection) -> String {
    let mut out = String::new();

    for (name, value) in &collection.variables {
        // Writing into a `String` cannot fail, so the result is dropped here
        // and in the rest of the module rather than threading a `fmt::Result`
        // through functions that have no way to fail.
        let _ = writeln!(out, "@{name} = {value}");
    }

    for request in &collection.requests {
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(&render_request(request));
    }

    out
}

/// Renders one request as a `###` block, newline-terminated.
///
/// The name is written as the block's title instead of as `# @name`: the
/// parser reads a title back as the name, and one line says what two would.
/// The directive is only added when the name would not survive as a title.
#[must_use]
pub fn render_request(request: &RequestDefinition) -> String {
    let mut out = String::new();

    if title_safe(&request.name) {
        let _ = writeln!(out, "### {}", request.name.trim());
    } else {
        // A name carrying `#` or a line break cannot be a title: the separator
        // would eat it. `# @name` holds it instead, and wins over the title.
        out.push_str("###\n");
        let _ = writeln!(out, "# @name {}", single_line(&request.name));
    }

    if let Some(description) = &request.description
        && !description.trim().is_empty()
    {
        let _ = writeln!(out, "# @description {}", single_line(description));
    }

    if request.options.insecure_tls {
        out.push_str("# @insecure\n");
    }

    for (name, value) in &request.variables {
        let _ = writeln!(out, "@{name} = {value}");
    }

    let _ = writeln!(out, "{} {}", request.method, request.url);

    // Query declared apart from the URL goes on continuation lines, which is
    // how the format writes a long query and how the parser reads it back.
    for (index, (key, value)) in request.query.iter().enumerate() {
        let prefix = if index == 0 && !request.url.contains('?') {
            '?'
        } else {
            '&'
        };
        let _ = writeln!(out, "  {prefix}{key}={value}");
    }

    for Header { name, value } in &request.headers {
        let _ = writeln!(out, "{name}: {value}");
    }

    if let Some(body) = render_body(&request.body) {
        out.push('\n');
        out.push_str(&body);
        out.push('\n');
    }

    out
}

/// Renders the body, or `None` when there is nothing to write.
fn render_body(body: &Body) -> Option<String> {
    match body {
        Body::Empty => None,
        Body::Text { content } | Body::Json { content } => {
            (!content.is_empty()).then(|| content.clone())
        }
        // A form is written already encoded, on one line. Writing the pairs
        // raw would be prettier and wrong: the bytes on the wire are the
        // encoded ones, and the point of the round trip is sending the same
        // request.
        Body::Form { fields } => (!fields.is_empty()).then(|| {
            url::form_urlencoded::Serializer::new(String::new())
                .extend_pairs(fields)
                .finish()
        }),
    }
}

/// `true` when the name can be written as a `###` title unchanged.
fn title_safe(name: &str) -> bool {
    let trimmed = name.trim();
    !trimmed.is_empty() && !trimmed.contains('#') && !trimmed.contains(['\n', '\r'])
}

/// Collapses a value into the single line a metadata directive can hold.
fn single_line(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    // In tests, `unwrap` documents the expectation and its panic IS the failure.
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use std::collections::BTreeMap;

    use http_studio_domain::{HttpMethod, RequestId, RequestOptions};

    use super::*;
    use crate::storage::http_file;

    fn request(name: &str) -> RequestDefinition {
        RequestDefinition {
            id: RequestId::new(format!("demo/{name}")),
            name: name.to_owned(),
            method: HttpMethod::Get,
            url: "{{base_url}}/health".to_owned(),
            headers: Vec::new(),
            query: Vec::new(),
            body: Body::Empty,
            variables: BTreeMap::new(),
            description: None,
            options: RequestOptions::default(),
        }
    }

    /// Renders a request and reads it straight back, which is the contract.
    fn round_trip(request: &RequestDefinition) -> RequestDefinition {
        let text = render_request(request);
        let file = http_file::parse(&text, "demo")
            .unwrap_or_else(|error| panic!("what was written does not parse: {error}\n{text}"));

        assert_eq!(file.requests.len(), 1, "one block was written:\n{text}");
        file.requests.into_iter().next().unwrap().definition
    }

    #[test]
    fn a_rendered_request_parses_back_into_the_same_definition() {
        let mut original = request("sign-in");
        original.method = HttpMethod::Post;
        original.url = "{{base_url}}/auth/login".to_owned();
        original.headers = vec![
            Header::new("Content-Type", "application/json"),
            Header::new("Accept", "application/json"),
        ];
        original.body = Body::Json {
            content: "{\n  \"email\": \"{{email}}\"\n}".to_owned(),
        };
        original.variables = BTreeMap::from([("email".to_owned(), "demo@example.com".to_owned())]);
        original.description = Some("Signs in and returns a token.".to_owned());

        assert_eq!(round_trip(&original), original);
    }

    #[test]
    fn the_name_travels_as_the_block_title() {
        let text = render_request(&request("sign-in"));

        assert!(text.starts_with("### sign-in\n"), "{text}");
        assert_eq!(round_trip(&request("sign-in")).name, "sign-in");
    }

    #[test]
    fn a_name_that_cannot_be_a_title_travels_as_a_directive() {
        // `#` would start a comment and the name would be lost halfway.
        let original = request("count #1");
        let text = render_request(&original);

        assert!(text.contains("# @name count #1"), "{text}");
        assert_eq!(round_trip(&original).name, "count #1");
    }

    #[test]
    fn the_insecure_directive_survives() {
        let mut original = request("internal");
        original.options = RequestOptions { insecure_tls: true };

        assert!(round_trip(&original).options.insecure_tls);
    }

    #[test]
    fn a_multiline_description_is_collapsed_to_one_line() {
        // A directive is read up to the end of its line, so a description with
        // line breaks would leave its tail behind as loose comments.
        let mut original = request("notes");
        original.description = Some("First line.\nSecond line.".to_owned());

        assert_eq!(
            round_trip(&original).description.as_deref(),
            Some("First line. Second line.")
        );
    }

    #[test]
    fn a_query_declared_apart_comes_back_merged_into_the_url() {
        let mut original = request("search");
        original.url = "https://api.example.com/search".to_owned();
        original.query = vec![
            ("q".to_owned(), "rust".to_owned()),
            ("page".to_owned(), "2".to_owned()),
        ];

        let parsed = round_trip(&original);

        assert_eq!(parsed.url, "https://api.example.com/search?q=rust&page=2");
        assert!(parsed.query.is_empty());
    }

    #[test]
    fn a_query_is_appended_when_the_url_already_has_one() {
        let mut original = request("search");
        original.url = "https://api.example.com/search?lang=es".to_owned();
        original.query = vec![("q".to_owned(), "rust".to_owned())];

        assert_eq!(
            round_trip(&original).url,
            "https://api.example.com/search?lang=es&q=rust"
        );
    }

    #[test]
    fn a_form_body_is_written_already_encoded() {
        let mut original = request("submit");
        original.method = HttpMethod::Post;
        original.headers = vec![Header::new(
            "Content-Type",
            "application/x-www-form-urlencoded",
        )];
        original.body = Body::Form {
            fields: vec![
                ("name".to_owned(), "a b".to_owned()),
                ("role".to_owned(), "admin&root".to_owned()),
            ],
        };

        // It comes back as text, which is what the parser produces, carrying
        // the same bytes the transport would have sent.
        assert_eq!(
            round_trip(&original).body,
            Body::Text {
                content: "name=a+b&role=admin%26root".to_owned(),
            }
        );
    }

    #[test]
    fn an_empty_body_writes_no_blank_line_at_the_end() {
        let text = render_request(&request("health"));

        assert!(text.ends_with("/health\n"), "{text}");
    }

    #[test]
    fn a_collection_writes_its_variables_as_the_preamble() {
        let collection = Collection {
            name: "demo".to_owned(),
            variables: BTreeMap::from([(
                "base_url".to_owned(),
                "https://api.example.com".to_owned(),
            )]),
            requests: vec![request("health"), request("ready")],
        };

        let text = render_collection(&collection);
        let file = http_file::parse(&text, "demo").unwrap();

        assert_eq!(
            file.variables.get("base_url").map(String::as_str),
            Some("https://api.example.com")
        );
        assert_eq!(file.requests.len(), 2);
    }

    #[test]
    fn requests_are_separated_by_a_blank_line() {
        let collection = Collection {
            name: "demo".to_owned(),
            variables: BTreeMap::new(),
            requests: vec![request("health"), request("ready")],
        };

        assert!(render_collection(&collection).contains("\n\n### ready"));
    }
}
