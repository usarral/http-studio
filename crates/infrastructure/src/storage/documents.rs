//! Reading documents that live outside the workspace.
//!
//! The adapter behind [`DocumentLoader`], and deliberately the thinnest thing
//! in this layer: an OpenAPI description being imported is a file the person
//! named on their own command line, so there is nothing to resolve and nothing
//! to confine. The workspace's own boundary rules — the ones that refuse
//! `< ../../.ssh/id_rsa` inside a `.http` — do not apply here, because the
//! threat they answer is a file *somebody else wrote* reaching for a path. A
//! path typed into a shell is not that.

use async_trait::async_trait;
use http_studio_application::{ApplicationError, DocumentLoader};

/// Reads documents from the local file system.
#[derive(Debug, Clone, Copy, Default)]
pub struct LocalDocuments;

impl LocalDocuments {
    /// Builds the loader. It holds no state.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl DocumentLoader for LocalDocuments {
    async fn load(&self, path: &str) -> Result<String, ApplicationError> {
        let path = path.to_owned();

        tokio::task::spawn_blocking(move || {
            std::fs::read_to_string(&path).map_err(|error| {
                if error.kind() == std::io::ErrorKind::InvalidData {
                    // Worth naming: the usual cause is pointing at a `.zip` or
                    // a `.tar.gz` of a specification, and "stream did not
                    // contain valid UTF-8" does not say that.
                    ApplicationError::repository(format!("{path}: not UTF-8 text"))
                } else {
                    ApplicationError::repository(format!("{path}: {error}"))
                }
            })
        })
        .await
        .map_err(|error| ApplicationError::repository(format!("task interrupted: {error}")))?
    }
}

#[cfg(test)]
mod tests {
    // In tests, `unwrap` documents the expectation and its panic IS the failure.
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[tokio::test]
    async fn reads_a_document_from_disk() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("openapi.yaml");
        std::fs::write(&path, "openapi: 3.1.0\n").unwrap();

        let document = LocalDocuments::new()
            .load(path.to_str().unwrap())
            .await
            .unwrap();

        assert_eq!(document, "openapi: 3.1.0\n");
    }

    #[tokio::test]
    async fn a_missing_document_names_the_path_it_looked_for() {
        // With a relative path, which directory it was resolved against is the
        // whole answer, so the path has to be in the message.
        let error = LocalDocuments::new()
            .load("./nowhere/openapi.yaml")
            .await
            .unwrap_err();

        assert!(
            error.to_string().contains("./nowhere/openapi.yaml"),
            "{error}"
        );
    }

    #[tokio::test]
    async fn a_binary_document_says_it_is_not_text() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("spec.zip");
        std::fs::write(&path, [0x50, 0x4b, 0x03, 0x04, 0xff, 0xfe]).unwrap();

        let error = LocalDocuments::new()
            .load(path.to_str().unwrap())
            .await
            .unwrap_err();

        assert!(error.to_string().contains("not UTF-8"), "{error}");
    }
}
