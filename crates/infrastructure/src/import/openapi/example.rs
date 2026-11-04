//! Building an example value out of a JSON Schema.
//!
//! An imported request is only worth having if its body is close enough to
//! send, and a description's schema is the only material there is for that. So
//! the schema is walked and a value of the right shape comes out: every
//! property present, every type plausible, `$ref`s followed inside the
//! document.
//!
//! # What it does not try to be
//!
//! Valid against the schema. `minLength`, `pattern` and `format`s nobody
//! agreed on are ignored, because a body that is *shaped* right is editable
//! and a body that is almost valid is misleading. Where the engine has a
//! dynamic variable for a format — `uuid`, `date-time` — the placeholder goes
//! in instead of a frozen value, so the request stays right the second time it
//! is sent.

use serde_json::{Map, Value};

/// How deep the walk goes.
///
/// A self-referencing schema is caught by the `$ref` stack, but a deeply
/// nested one is not cyclic, just unhelpful: nobody edits a body eight levels
/// down, and the alternative is a page of JSON per request.
const MAX_DEPTH: usize = 8;

/// Builds example values against one document's definitions.
pub(super) struct Examples<'a> {
    document: &'a Value,
}

impl<'a> Examples<'a> {
    /// Binds the builder to the document whose `$ref`s it will resolve.
    pub(super) fn new(document: &'a Value) -> Self {
        Self { document }
    }

    /// An example value for `schema`.
    pub(super) fn value(&self, schema: &Value) -> Value {
        self.build(schema, 0, &mut Vec::new())
    }

    /// An example for a scalar, as the text a URL or a header can carry.
    ///
    /// Objects and arrays come back as their JSON, which is what OpenAPI's
    /// default `style` does to them anyway.
    pub(super) fn scalar(&self, schema: &Value) -> String {
        match self.value(schema) {
            Value::String(text) => text,
            Value::Null => String::new(),
            other => other.to_string(),
        }
    }

    /// The recursive walk, carrying the `$ref`s already on the stack.
    fn build(&self, schema: &Value, depth: usize, seen: &mut Vec<String>) -> Value {
        if depth > MAX_DEPTH || !schema.is_object() {
            return Value::Null;
        }

        if let Some(reference) = schema.get("$ref").and_then(Value::as_str) {
            return self.follow(reference, depth, seen);
        }

        // What the document says beats anything that can be inferred: an
        // author who wrote an example wrote the one they wanted sent.
        if let Some(example) = explicit_example(schema) {
            return example;
        }

        if let Some(variants) = schema.get("allOf").and_then(Value::as_array) {
            return self.merge(variants, depth, seen);
        }

        if let Some(first) = schema
            .get("oneOf")
            .or_else(|| schema.get("anyOf"))
            .and_then(Value::as_array)
            .and_then(|variants| variants.first())
        {
            return self.build(first, depth + 1, seen);
        }

        match type_of(schema) {
            Some("object") | None if schema.get("properties").is_some() => {
                self.object(schema, depth, seen)
            }
            Some("object") => Value::Object(Map::new()),
            Some("array") => {
                let items = schema.get("items").cloned().unwrap_or(Value::Null);
                Value::Array(vec![self.build(&items, depth + 1, seen)])
            }
            Some("integer") => Value::from(0),
            Some("number") => Value::from(0.0),
            Some("boolean") => Value::Bool(true),
            Some("string") => Value::String(string_for(schema)),
            // `null`, and a schema with neither type nor properties, say
            // nothing; `null` is the honest rendering of that, and it shows up
            // in the body for whoever has to fill it in.
            _ => Value::Null,
        }
    }

    /// Resolves a `$ref` and builds its target.
    fn follow(&self, reference: &str, depth: usize, seen: &mut Vec<String>) -> Value {
        // A schema that refers to itself — a tree node, a comment thread — is
        // legal and common, and following it twice is how an importer hangs.
        if seen.iter().any(|visited| visited == reference) {
            return Value::Null;
        }

        let Some(pointer) = reference.strip_prefix("#/") else {
            // An external `$ref` lives in another document, which this
            // importer deliberately does not go and fetch.
            return Value::Null;
        };

        let Some(target) = self.document.pointer(&format!("/{pointer}")) else {
            return Value::Null;
        };

        seen.push(reference.to_owned());
        let value = self.build(target, depth + 1, seen);
        seen.pop();
        value
    }

    /// Builds an object out of the schema's properties.
    fn object(&self, schema: &Value, depth: usize, seen: &mut Vec<String>) -> Value {
        let Some(properties) = schema.get("properties").and_then(Value::as_object) else {
            return Value::Object(Map::new());
        };

        let mut object = Map::new();
        for (name, property) in properties {
            // `readOnly` is what a server sends back, not what it accepts; an
            // `id` in a creation body is the usual way to get a 400.
            if property.get("readOnly").and_then(Value::as_bool) == Some(true) {
                continue;
            }
            object.insert(name.clone(), self.build(property, depth + 1, seen));
        }

        Value::Object(object)
    }

    /// Merges the members of an `allOf` into one value.
    fn merge(&self, variants: &[Value], depth: usize, seen: &mut Vec<String>) -> Value {
        let mut merged = Map::new();

        for variant in variants {
            if let Value::Object(fields) = self.build(variant, depth + 1, seen) {
                for (name, value) in fields {
                    merged.insert(name, value);
                }
            }
        }

        Value::Object(merged)
    }
}

/// The example the document states, in any of the places it may state it.
fn explicit_example(schema: &Value) -> Option<Value> {
    if let Some(example) = schema.get("example") {
        return Some(example.clone());
    }

    // OpenAPI 3.1 follows JSON Schema here and takes a list.
    if let Some(first) = schema
        .get("examples")
        .and_then(Value::as_array)
        .and_then(|examples| examples.first())
    {
        return Some(first.clone());
    }

    if let Some(default) = schema.get("default") {
        return Some(default.clone());
    }

    // An enum is a closed list, so its first member is the only example that
    // is certain to be accepted.
    schema
        .get("enum")
        .and_then(Value::as_array)
        .and_then(|values| values.first())
        .cloned()
}

/// The schema's type, with 3.1's list form reduced to one name.
fn type_of(schema: &Value) -> Option<&str> {
    match schema.get("type")? {
        Value::String(name) => Some(name.as_str()),
        // `["string", "null"]` is 3.1's way of writing nullable; the useful
        // half is the one that is not null.
        Value::Array(names) => names
            .iter()
            .filter_map(Value::as_str)
            .find(|name| *name != "null"),
        _ => None,
    }
}

/// An example string, taking the format into account.
///
/// The two formats the engine can generate get a placeholder rather than a
/// value: a frozen timestamp in a versioned file is wrong the moment after it
/// is written, and `{{$isoTimestamp}}` is right every time the request runs.
fn string_for(schema: &Value) -> String {
    let format = schema
        .get("format")
        .and_then(Value::as_str)
        .unwrap_or_default();

    match format {
        "uuid" => "{{$uuid}}".to_owned(),
        "date-time" => "{{$isoTimestamp}}".to_owned(),
        "date" => "1970-01-01".to_owned(),
        "email" | "idn-email" => "user@example.com".to_owned(),
        "uri" | "url" | "uri-reference" => "https://example.com".to_owned(),
        "hostname" => "example.com".to_owned(),
        "ipv4" => "192.0.2.1".to_owned(),
        "ipv6" => "2001:db8::1".to_owned(),
        "password" => "secret".to_owned(),
        "byte" | "binary" => String::new(),
        _ => "string".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    // In tests, `unwrap` documents the expectation and its panic IS the failure.
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use serde_json::json;

    use super::*;

    /// Builds an example with no document behind it.
    fn example(schema: &Value) -> Value {
        let document = json!({});
        Examples::new(&document).value(schema)
    }

    #[test]
    fn an_object_gets_every_property() {
        let value = example(&json!({
            "type": "object",
            "properties": {
                "name": { "type": "string" },
                "age": { "type": "integer" },
                "tame": { "type": "boolean" },
            },
        }));

        assert_eq!(value, json!({ "name": "string", "age": 0, "tame": true }));
    }

    #[test]
    fn an_object_with_no_declared_type_is_still_an_object() {
        // Plenty of real descriptions leave `type` out when `properties` is
        // there, and refusing those would refuse half the specifications.
        let value = example(&json!({ "properties": { "name": { "type": "string" } } }));

        assert_eq!(value, json!({ "name": "string" }));
    }

    #[test]
    fn an_array_carries_one_item() {
        let value = example(&json!({ "type": "array", "items": { "type": "string" } }));

        assert_eq!(value, json!(["string"]));
    }

    #[test]
    fn a_stated_example_wins_over_the_inferred_one() {
        let value = example(&json!({ "type": "string", "example": "lila" }));

        assert_eq!(value, json!("lila"));
    }

    #[test]
    fn an_enum_uses_its_first_member() {
        let value = example(&json!({ "type": "string", "enum": ["available", "sold"] }));

        assert_eq!(value, json!("available"));
    }

    #[test]
    fn a_read_only_property_is_left_out() {
        // Sending back what the server owns is the usual way to earn a 400.
        let value = example(&json!({
            "type": "object",
            "properties": {
                "id": { "type": "integer", "readOnly": true },
                "name": { "type": "string" },
            },
        }));

        assert_eq!(value, json!({ "name": "string" }));
    }

    #[test]
    fn a_uuid_becomes_the_engines_own_placeholder() {
        let value = example(&json!({ "type": "string", "format": "uuid" }));

        assert_eq!(value, json!("{{$uuid}}"));
    }

    #[test]
    fn a_date_time_becomes_a_placeholder_rather_than_a_frozen_instant() {
        let value = example(&json!({ "type": "string", "format": "date-time" }));

        assert_eq!(value, json!("{{$isoTimestamp}}"));
    }

    #[test]
    fn a_reference_is_followed_inside_the_document() {
        let document = json!({
            "components": { "schemas": { "Pet": {
                "type": "object",
                "properties": { "name": { "type": "string" } },
            } } },
        });

        let value = Examples::new(&document).value(&json!({ "$ref": "#/components/schemas/Pet" }));

        assert_eq!(value, json!({ "name": "string" }));
    }

    #[test]
    fn a_self_referencing_schema_does_not_hang() {
        // A tree node, a comment with replies: legal, common, and the way an
        // importer that followed every `$ref` would never return.
        let document = json!({
            "components": { "schemas": { "Node": {
                "type": "object",
                "properties": {
                    "name": { "type": "string" },
                    "child": { "$ref": "#/components/schemas/Node" },
                },
            } } },
        });

        let value = Examples::new(&document).value(&json!({ "$ref": "#/components/schemas/Node" }));

        assert_eq!(value, json!({ "name": "string", "child": null }));
    }

    #[test]
    fn an_external_reference_is_not_fetched() {
        let value = example(&json!({ "$ref": "https://example.com/schemas/pet.json" }));

        assert_eq!(value, Value::Null);
    }

    #[test]
    fn all_of_members_are_merged() {
        let value = example(&json!({
            "allOf": [
                { "type": "object", "properties": { "id": { "type": "integer" } } },
                { "type": "object", "properties": { "name": { "type": "string" } } },
            ],
        }));

        assert_eq!(value, json!({ "id": 0, "name": "string" }));
    }

    #[test]
    fn one_of_takes_the_first_variant() {
        let value = example(&json!({
            "oneOf": [{ "type": "string" }, { "type": "integer" }],
        }));

        assert_eq!(value, json!("string"));
    }

    #[test]
    fn a_nullable_type_list_uses_the_half_that_is_not_null() {
        let value = example(&json!({ "type": ["string", "null"] }));

        assert_eq!(value, json!("string"));
    }

    #[test]
    fn a_scalar_comes_back_as_the_text_a_url_can_carry() {
        let document = json!({});
        let examples = Examples::new(&document);

        assert_eq!(examples.scalar(&json!({ "type": "integer" })), "0");
        assert_eq!(examples.scalar(&json!({ "type": "string" })), "string");
        assert_eq!(examples.scalar(&json!({})), "");
    }
}
