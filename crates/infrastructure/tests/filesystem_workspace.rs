//! Integration tests for the file repository, against the repository's own
//! example workspace.
//!
//! `examples/demo` is used on purpose rather than invented fixtures: that way
//! the example a new user reads is always verified by CI and cannot go stale in
//! silence.
//!
//! The workspace is made of `.http` files with an `http-client.env.json`, the
//! ecosystem format documented at <https://http-files.org>.

// In tests, `unwrap`/`expect`/`panic!` document the expectation and their
// panic IS the failure.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::PathBuf;

use http_studio_application::{CollectionRepository, EnvironmentRepository};
use http_studio_domain::{
    Body, HttpMethod, RequestId, VariableContext, VariableScope, resolver::resolve,
};
use http_studio_infrastructure::FileSystemWorkspace;

/// The path to the example workspace shipped in the repository.
fn demo_workspace() -> FileSystemWorkspace {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../examples/demo")
        .canonicalize()
        .expect("the example workspace must exist");

    FileSystemWorkspace::new(root)
}

#[tokio::test]
async fn loads_the_examples_collections() {
    let collections = demo_workspace()
        .list_collections()
        .await
        .expect("the collections must load");

    assert_eq!(collections.len(), 1);
    assert_eq!(collections[0].name, "echo");
    assert_eq!(collections[0].requests.len(), 3);
}

#[tokio::test]
async fn the_identifier_combines_file_and_request_name() {
    let collections = demo_workspace().list_collections().await.unwrap();
    let ids: Vec<String> = collections[0]
        .requests
        .iter()
        .map(|request| request.id.to_string())
        .collect();

    assert_eq!(ids, vec!["echo/get", "echo/post-json", "echo/post-file"]);
}

#[tokio::test]
async fn file_variables_are_the_collections_variables() {
    let collections = demo_workspace().list_collections().await.unwrap();

    assert_eq!(
        collections[0].variables.get("accept").map(String::as_str),
        Some("application/json")
    );
}

#[tokio::test]
async fn the_private_environment_merges_with_the_public_one() {
    let environment = demo_workspace()
        .find_environment("dev")
        .await
        .unwrap()
        .expect("the `dev` environment must exist");

    // `base_url` comes from the public file and `api_token` from the private one.
    assert!(environment.variables.contains_key("base_url"));
    assert!(environment.variables.contains_key("api_token"));
}

#[tokio::test]
async fn an_environment_with_no_private_file_holds_only_the_public_one() {
    let environment = demo_workspace()
        .find_environment("prod")
        .await
        .unwrap()
        .expect("the `prod` environment must exist");

    assert!(!environment.variables.contains_key("api_token"));
}

#[tokio::test]
async fn finds_a_request_with_its_collections_variables() {
    let located = demo_workspace()
        .find_request(&RequestId::new("echo/post-json"))
        .await
        .expect("the lookup must not fail")
        .expect("the request must exist");

    assert_eq!(located.collection_name, "echo");
    assert_eq!(located.definition.method, HttpMethod::Post);
    assert_eq!(
        located
            .collection_variables
            .get("accept")
            .map(String::as_str),
        Some("application/json")
    );
}

#[tokio::test]
async fn a_request_that_does_not_exist_returns_none() {
    let found = demo_workspace()
        .find_request(&RequestId::new("no/existe"))
        .await
        .expect("the lookup must not fail");

    assert!(found.is_none());
}

#[tokio::test]
async fn loads_the_environments_sorted() {
    let environments = demo_workspace()
        .list_environments()
        .await
        .expect("the environments must load");

    let names: Vec<&str> = environments.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(names, vec!["dev", "prod"]);
}

#[tokio::test]
async fn finds_an_environment_by_name() {
    let environment = demo_workspace()
        .find_environment("prod")
        .await
        .unwrap()
        .expect("the `prod` environment must exist");

    assert!(environment.variables.contains_key("base_url"));
}

#[tokio::test]
async fn an_environment_that_does_not_exist_returns_none() {
    let found = demo_workspace().find_environment("staging").await.unwrap();
    assert!(found.is_none());
}

#[tokio::test]
async fn a_workspace_that_does_not_exist_is_a_repository_error() {
    let workspace = FileSystemWorkspace::new(std::env::temp_dir().join("no-existe-jamas"));
    assert!(workspace.list_collections().await.is_err());
}

#[test]
fn discover_finds_the_root_from_a_subdirectory() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../examples/demo")
        .canonicalize()
        .unwrap();

    let found = FileSystemWorkspace::discover(&root.join("collections"))
        .expect("it must find the root by walking up");

    assert_eq!(found, root);
}

/// Builds a single-`.http` workspace in a temporary directory.
///
/// The cases checked here are **broken** references, and a workspace in the
/// repository with a deliberately missing file would be a workspace nobody
/// could use for anything else.
fn workspace_with(files: &[(&str, &str)]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("a temporary directory");

    for (name, content) in files {
        let path = dir.path().join("collections").join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    dir
}

/// A request body's text, whether it is JSON or plain text.
fn body_text(body: &Body) -> &str {
    match body {
        Body::Text { content } | Body::Json { content } => content,
        other => panic!("expected a body with text, found {other:?}"),
    }
}

#[tokio::test]
async fn the_body_can_come_from_a_file() {
    let located = demo_workspace()
        .find_request(&RequestId::new("echo/post-file"))
        .await
        .unwrap()
        .expect("the example request must exist");

    // The block's `Content-Type` decides: the file is classified as JSON.
    assert!(matches!(located.definition.body, Body::Json { .. }));
    assert!(body_text(&located.definition.body).contains("\"sku\": \"ABC-1\""));
}

#[tokio::test]
async fn with_the_at_sign_the_file_goes_through_the_interpolator() {
    let located = demo_workspace()
        .find_request(&RequestId::new("echo/post-file"))
        .await
        .unwrap()
        .unwrap();

    let context = VariableContext::new().with_scope(
        VariableScope::new("test")
            .with("base_url", "https://a.test")
            .with("accept", "application/json")
            .with("user_agent", "http-studio"),
    );
    let resolved = resolve(&located.definition, &context).unwrap();

    assert!(body_text(&resolved.body).contains("\"customer\": \"http-studio\""));
}

#[tokio::test]
async fn without_the_at_sign_the_file_is_sent_literally() {
    // This is the difference that matters: a template with braces is sent as it
    // stands instead of dying with "undefined variable".
    let dir = workspace_with(&[
        (
            "template.http",
            "### X\nPOST https://a.test\n\n< ./body.txt\n",
        ),
        ("body.txt", "hello {{name}}"),
    ]);
    let workspace = FileSystemWorkspace::new(dir.path());

    let located = workspace
        .find_request(&RequestId::new("template/x"))
        .await
        .unwrap()
        .unwrap();
    let resolved = resolve(&located.definition, &VariableContext::new()).unwrap();

    assert_eq!(body_text(&resolved.body), "hello {{name}}");
}

#[tokio::test]
async fn a_reference_to_a_file_that_does_not_exist_is_an_error() {
    let dir = workspace_with(&[(
        "broken.http",
        "### X\nPOST https://a.test\n\n< ./missing.json\n",
    )]);
    let workspace = FileSystemWorkspace::new(dir.path());

    let error = workspace.list_collections().await.unwrap_err().to_string();

    // The message has to name the written path and the line, or finding it in
    // a workspace with dozens of files is guesswork.
    assert!(error.contains("./missing.json"), "message: {error}");
    assert!(error.contains("line 4"), "message: {error}");
}

#[tokio::test]
async fn a_reference_outside_the_workspace_is_an_error() {
    let dir = workspace_with(&[(
        "escape.http",
        "### X\nPOST https://a.test\n\n< ../../../../etc/hostname\n",
    )]);
    let workspace = FileSystemWorkspace::new(dir.path());

    let error = workspace.list_collections().await.unwrap_err().to_string();

    assert!(error.contains("outside the workspace"), "message: {error}");
}
