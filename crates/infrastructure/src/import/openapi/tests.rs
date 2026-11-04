//! Tests for the OpenAPI importer.
//!
//! They are in their own file because the fixtures are documents: a
//! description short enough to read is still a dozen lines, and burying the
//! assertions under them would hide the behaviour being pinned.

// In tests, `unwrap` documents the expectation and its panic IS the failure.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use super::*;

/// A minimal 3.1 description with one operation, in YAML.
const PETSTORE: &str = r##"
openapi: 3.1.0
info:
  title: Pet Store
  version: 1.0.0
servers:
  - url: https://api.example.com/v1
tags:
  - name: pets
paths:
  /pets:
    get:
      operationId: listPets
      summary: Lists every pet.
      tags: [pets]
      parameters:
        - name: limit
          in: query
          required: false
          schema: { type: integer }
        - name: X-Request-Id
          in: header
          required: true
          schema: { type: string, format: uuid }
      responses:
        "200": { description: OK }
    post:
      operationId: createPet
      tags: [pets]
      security:
        - bearerAuth: []
      requestBody:
        required: true
        content:
          application/json:
            schema:
              $ref: "#/components/schemas/Pet"
      responses:
        "201": { description: Created }
  /pets/{petId}:
    parameters:
      - name: petId
        in: path
        required: true
        schema: { type: integer }
        example: 7
    delete:
      operationId: deletePet
      tags: [pets]
      responses:
        "204": { description: Gone }
    trace:
      operationId: tracePet
      tags: [pets]
      responses:
        "200": { description: OK }
components:
  securitySchemes:
    bearerAuth:
      type: http
      scheme: bearer
  schemas:
    Pet:
      type: object
      properties:
        id: { type: integer, readOnly: true }
        name: { type: string, example: lila }
        status: { type: string, enum: [available, sold] }
"##;

/// Imports a document and returns its collections.
fn import(document: &str) -> Vec<Collection> {
    OpenApiImporter::new()
        .import(document)
        .unwrap_or_else(|error| panic!("the description did not import: {error}"))
}

/// The one request with that name, from anywhere in the import.
fn request(collections: &[Collection], name: &str) -> RequestDefinition {
    collections
        .iter()
        .flat_map(|collection| &collection.requests)
        .find(|request| request.name == name)
        .cloned()
        .unwrap_or_else(|| panic!("there is no request named `{name}`"))
}

fn header_value(request: &RequestDefinition, name: &str) -> Option<String> {
    request
        .headers
        .iter()
        .find(|header| header.name.eq_ignore_ascii_case(name))
        .map(|header| header.value.clone())
}

#[test]
fn a_camel_case_operation_id_becomes_a_name_worth_typing() {
    // `hts run pets/listpets` is the alternative, and nobody would.
    assert_eq!(split_camel_case("listPets"), "list Pets");
    assert_eq!(split_camel_case("getHTTPStatus"), "get HTTP Status");
    assert_eq!(split_camel_case("already-kebab"), "already-kebab");
}

#[test]
fn a_tag_becomes_a_collection() {
    let collections = import(PETSTORE);

    assert_eq!(collections.len(), 1);
    assert_eq!(collections[0].name, "pets");
}

#[test]
fn the_server_becomes_the_collections_base_url() {
    let collections = import(PETSTORE);

    assert_eq!(
        collections[0].variables.get(BASE_URL).map(String::as_str),
        Some("https://api.example.com/v1")
    );
}

#[test]
fn an_operation_id_becomes_the_request_name() {
    let collections = import(PETSTORE);
    let names: Vec<&str> = collections[0]
        .requests
        .iter()
        .map(|request| request.name.as_str())
        .collect();

    assert_eq!(names, ["list-pets", "create-pet", "delete-pet"]);
}

#[test]
fn a_trace_operation_is_left_out() {
    // The engine has no `TRACE`, and leaving the operation out is honest;
    // refusing the whole description over it would not be.
    let collections = import(PETSTORE);

    assert!(
        !collections[0]
            .requests
            .iter()
            .any(|request| request.name == "trace-pet")
    );
}

#[test]
fn the_summary_becomes_the_description() {
    assert_eq!(
        request(&import(PETSTORE), "list-pets")
            .description
            .as_deref(),
        Some("Lists every pet.")
    );
}

#[test]
fn a_required_parameter_travels_and_an_optional_one_does_not() {
    let request = request(&import(PETSTORE), "list-pets");

    assert_eq!(
        header_value(&request, "x-request-id").as_deref(),
        Some("{{X_Request_Id}}"),
        "a required header has to be in the request"
    );
    assert!(
        !request.url.contains("limit"),
        "an optional parameter has nowhere to be written as disabled: {}",
        request.url
    );
}

#[test]
fn a_parameter_gets_an_example_value_declared() {
    let request = request(&import(PETSTORE), "list-pets");

    assert_eq!(
        request.variables.get("X_Request_Id").map(String::as_str),
        Some("{{$uuid}}"),
        "a uuid parameter is worth generating rather than freezing"
    );
}

#[test]
fn a_path_parameter_becomes_a_placeholder_and_a_variable() {
    let request = request(&import(PETSTORE), "delete-pet");

    assert_eq!(request.url, "{{base_url}}/pets/{{petId}}");
    assert_eq!(
        request.variables.get("petId").map(String::as_str),
        Some("7")
    );
}

#[test]
fn a_path_level_parameter_reaches_every_operation_under_it() {
    // It is declared once on the path item, and both operations need it.
    let request = request(&import(PETSTORE), "delete-pet");

    assert!(request.variables.contains_key("petId"));
}

#[test]
fn a_body_is_built_from_the_referenced_schema() {
    let request = request(&import(PETSTORE), "create-pet");

    let Body::Json { content } = &request.body else {
        panic!("the body should be JSON: {:?}", request.body);
    };

    let value: Value = serde_json::from_str(content).unwrap();
    assert_eq!(
        value,
        serde_json::json!({ "name": "lila", "status": "available" }),
        "the example wins, the enum's first member stands in, and `readOnly` is left out"
    );
    assert_eq!(
        header_value(&request, "content-type").as_deref(),
        Some("application/json")
    );
}

#[test]
fn a_bearer_scheme_becomes_an_authorization_header_with_a_placeholder() {
    // And nothing declares `token`: the engine then says "undefined variable"
    // until it is put in the private environment file, which is where a
    // secret belongs.
    let request = request(&import(PETSTORE), "create-pet");

    assert_eq!(
        header_value(&request, "authorization").as_deref(),
        Some("Bearer {{token}}")
    );
    assert!(!request.variables.contains_key("token"));
}

#[test]
fn an_untagged_operation_lands_in_a_collection_named_after_the_document() {
    let collections = import(
        r#"{
            "openapi": "3.0.0",
            "info": { "title": "Billing API", "version": "1" },
            "servers": [{ "url": "https://api.example.com" }],
            "paths": { "/invoices": { "get": { "responses": {} } } }
        }"#,
    );

    assert_eq!(collections[0].name, "billing-api");
    assert_eq!(collections[0].requests[0].name, "get-invoices");
}

#[test]
fn a_json_description_imports_the_same_as_a_yaml_one() {
    let yaml = import(PETSTORE);
    let json = import(
        &serde_json::to_string(&serde_norway::from_str::<Value>(PETSTORE).unwrap()).unwrap(),
    );

    assert_eq!(json, yaml);
}

#[test]
fn tags_can_be_filtered() {
    let document = r#"
openapi: 3.0.0
info: { title: Two, version: "1" }
servers: [{ url: https://api.example.com }]
paths:
  /pets: { get: { operationId: listPets, tags: [pets], responses: {} } }
  /orders: { get: { operationId: listOrders, tags: [store], responses: {} } }
"#;

    let collections = OpenApiImporter::new()
        .with_tags(vec!["store".to_owned()])
        .import(document)
        .unwrap();

    assert_eq!(collections.len(), 1);
    assert_eq!(collections[0].name, "store");
}

#[test]
fn a_tag_filter_that_matches_nothing_says_so() {
    let error = OpenApiImporter::new()
        .with_tags(vec!["nowhere".to_owned()])
        .import(PETSTORE)
        .unwrap_err();

    assert!(error.to_string().contains("nowhere"), "{error}");
}

#[test]
fn server_variables_are_replaced_with_their_defaults() {
    // A URL still carrying `{region}` resolves to nothing the transport can
    // use, and the default is the document's own answer.
    let collections = import(
        r#"
openapi: 3.0.0
info: { title: Regional, version: "1" }
servers:
  - url: https://{region}.example.com/{version}
    variables:
      region: { default: eu-west-1 }
      version: { default: v2 }
paths:
  /ping: { get: { operationId: ping, responses: {} } }
"#,
    );

    assert_eq!(
        collections[0].variables.get(BASE_URL).map(String::as_str),
        Some("https://eu-west-1.example.com/v2")
    );
}

#[test]
fn a_swagger_2_description_is_imported_too() {
    // Three fields instead of one `servers`, the body among the parameters,
    // and `securityDefinitions` instead of `components`.
    let collections = import(
        r#"
swagger: "2.0"
info: { title: Legacy, version: "1" }
host: api.example.com
basePath: /v1
schemes: [https]
securityDefinitions:
  key: { type: apiKey, name: X-API-Key, in: header }
paths:
  /pets:
    post:
      operationId: createPet
      tags: [pets]
      security: [{ key: [] }]
      consumes: [application/json]
      parameters:
        - name: body
          in: body
          required: true
          schema:
            type: object
            properties:
              name: { type: string }
      responses: { "201": { description: Created } }
"#,
    );

    let request = request(&collections, "create-pet");
    assert_eq!(
        collections[0].variables.get(BASE_URL).map(String::as_str),
        Some("https://api.example.com/v1")
    );
    assert_eq!(request.url, "{{base_url}}/pets");
    assert_eq!(
        header_value(&request, "x-api-key").as_deref(),
        Some("{{api_key}}")
    );
    assert!(matches!(request.body, Body::Json { .. }));
}

#[test]
fn a_swagger_form_becomes_a_urlencoded_body() {
    let collections = import(
        r#"
swagger: "2.0"
info: { title: Legacy, version: "1" }
host: api.example.com
paths:
  /login:
    post:
      operationId: login
      parameters:
        - { name: user, in: formData, required: true, type: string }
        - { name: password, in: formData, required: true, type: string }
      responses: {}
"#,
    );

    let request = request(&collections, "login");
    assert_eq!(
        request.body,
        Body::Text {
            content: "user=string&password=string".to_owned()
        }
    );
    assert_eq!(
        header_value(&request, "content-type").as_deref(),
        Some("application/x-www-form-urlencoded")
    );
}

#[test]
fn an_api_key_in_the_query_goes_into_the_url() {
    let collections = import(
        r#"
openapi: 3.0.0
info: { title: Keyed, version: "1" }
servers: [{ url: https://api.example.com }]
security: [{ key: [] }]
components:
  securitySchemes:
    key: { type: apiKey, name: apikey, in: query }
paths:
  /ping: { get: { operationId: ping, responses: {} } }
"#,
    );

    assert_eq!(
        collections[0].requests[0].url,
        "{{base_url}}/ping?apikey={{api_key}}"
    );
}

#[test]
fn the_documents_security_applies_where_an_operation_states_none() {
    let collections = import(
        r#"
openapi: 3.0.0
info: { title: Guarded, version: "1" }
servers: [{ url: https://api.example.com }]
security: [{ basic: [] }]
components:
  securitySchemes:
    basic: { type: http, scheme: basic }
paths:
  /ping: { get: { operationId: ping, responses: {} } }
"#,
    );

    assert_eq!(
        header_value(&collections[0].requests[0], "authorization").as_deref(),
        Some("Basic {{basic_auth}}")
    );
}

#[test]
fn a_parameter_name_with_a_dot_becomes_a_usable_variable() {
    // `{{filter.status}}` is read as a different name than the declaration
    // would be, so the placeholder and the variable have to agree.
    let collections = import(
        r#"
openapi: 3.0.0
info: { title: Dotted, version: "1" }
servers: [{ url: https://api.example.com }]
paths:
  /pets:
    get:
      operationId: listPets
      parameters:
        - { name: filter.status, in: query, required: true, schema: { type: string } }
      responses: {}
"#,
    );

    let request = &collections[0].requests[0];
    assert_eq!(
        request.url,
        "{{base_url}}/pets?filter.status={{filter_status}}"
    );
    assert!(request.variables.contains_key("filter_status"));
}

#[test]
fn a_server_url_ending_in_a_slash_does_not_double_it() {
    let collections = import(
        r#"
openapi: 3.0.0
info: { title: Slashed, version: "1" }
servers: [{ url: "https://api.example.com/" }]
paths:
  /ping: { get: { operationId: ping, responses: {} } }
"#,
    );

    assert_eq!(collections[0].requests[0].url, "{{base_url}}/ping");
}

#[test]
fn a_document_that_is_not_a_description_says_so() {
    let error = OpenApiImporter::new()
        .import("{\"paths\": {}}")
        .unwrap_err();

    assert!(error.to_string().contains("openapi"), "{error}");
}

#[test]
fn a_document_with_no_paths_says_so() {
    let error = OpenApiImporter::new()
        .import("openapi: 3.0.0\ninfo: { title: Empty, version: \"1\" }\n")
        .unwrap_err();

    assert!(error.to_string().contains("paths"), "{error}");
}

#[test]
fn a_document_that_is_neither_json_nor_yaml_says_which_two_it_tried() {
    let error = OpenApiImporter::new()
        .import("\tthis: is: not: anything\n\t\tat all")
        .unwrap_err();

    assert!(
        error.to_string().contains("neither JSON nor YAML"),
        "{error}"
    );
}

#[test]
fn an_imported_request_resolves_against_its_own_variables() {
    // The point of the whole importer: what comes out can be sent. The
    // placeholders have to line up with the declarations, which a wrong
    // variable name would break without any test here noticing.
    use http_studio_domain::{VariableContext, VariableScope, resolve};

    let collections = import(PETSTORE);
    let collection = &collections[0];
    let request = request(&collections, "delete-pet");

    let context = VariableContext::new()
        .with_scope(VariableScope::from_map(
            "collection",
            collection.variables.clone(),
        ))
        .with_scope(VariableScope::from_map(
            "request",
            request.variables.clone(),
        ));

    let resolved = resolve(&request, &context).unwrap();
    assert_eq!(
        resolved.url.as_str(),
        "https://api.example.com/v1/pets/7",
        "every placeholder the importer wrote has to have a declaration"
    );
}
