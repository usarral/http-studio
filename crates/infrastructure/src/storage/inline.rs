//! A single-request workspace, built in memory.
//!
//! This is what makes `hts run https://api.example.com/x` work with no file in
//! sight: the request is synthesised from the arguments and served through the
//! same port as the ones on disk.
//!
//! That this is an adapter rather than a special path inside the engine has a
//! practical consequence: an ad-hoc request **inherits everything** — variable
//! interpolation, environments, secrets, history, exit codes — with not one
//! line of new logic. `hts run '{{base_url}}/health' --env prod` works because
//! the resolver cannot tell where the definition came from.

use async_trait::async_trait;
use http_studio_application::{
    ApplicationError, CollectionRepository, LocatedRequest, RequestSource,
};
use http_studio_domain::{Collection, RequestDefinition, RequestId};

/// The synthetic collection's name.
const COLLECTION: &str = "adhoc";

/// An in-memory workspace holding a single request.
#[derive(Debug, Clone)]
pub struct InlineWorkspace {
    definition: RequestDefinition,
}

impl InlineWorkspace {
    /// The identifier given to the synthetic request.
    #[must_use]
    pub fn request_id() -> RequestId {
        RequestId::new(format!("{COLLECTION}/request"))
    }

    /// Wraps a hand-built definition.
    ///
    /// The identifier is overwritten so it is always the same: the caller has
    /// no business inventing one, and this way `--extract` or the history can
    /// refer to it unambiguously.
    #[must_use]
    pub fn new(mut definition: RequestDefinition) -> Self {
        definition.id = Self::request_id();
        Self { definition }
    }
}

#[async_trait]
impl CollectionRepository for InlineWorkspace {
    async fn list_collections(&self) -> Result<Vec<Collection>, ApplicationError> {
        Ok(vec![Collection {
            name: COLLECTION.to_owned(),
            variables: std::collections::BTreeMap::new(),
            requests: vec![self.definition.clone()],
        }])
    }

    async fn find_request(
        &self,
        id: &RequestId,
    ) -> Result<Option<LocatedRequest>, ApplicationError> {
        if id != &self.definition.id {
            return Ok(None);
        }

        Ok(Some(LocatedRequest {
            collection_name: COLLECTION.to_owned(),
            collection_variables: std::collections::BTreeMap::new(),
            definition: self.definition.clone(),
        }))
    }
}

#[async_trait]
impl RequestSource for InlineWorkspace {
    async fn read(&self, _id: &RequestId) -> Result<Option<String>, ApplicationError> {
        // An ad-hoc request has no file to read its original text from.
        Ok(None)
    }

    async fn save(&self, _id: &RequestId, _source: &str) -> Result<(), ApplicationError> {
        Err(ApplicationError::repository(
            "an ad-hoc request is in no file: there is nowhere to save it",
        ))
    }
}

#[cfg(test)]
mod tests {
    // In tests, `unwrap` documents the expectation and its panic IS the failure.
    #![allow(clippy::unwrap_used)]

    use super::*;
    use http_studio_domain::{Body, HttpMethod, RequestOptions};

    fn workspace() -> InlineWorkspace {
        InlineWorkspace::new(RequestDefinition {
            id: RequestId::new("se-sobrescribe"),
            name: "ad-hoc".to_owned(),
            method: HttpMethod::Get,
            url: "https://a.test".to_owned(),
            headers: vec![],
            query: vec![],
            body: Body::Empty,
            variables: std::collections::BTreeMap::new(),
            description: None,
            options: RequestOptions::default(),
        })
    }

    #[tokio::test]
    async fn exposes_a_single_collection_with_one_request() {
        let collections = workspace().list_collections().await.unwrap();

        assert_eq!(collections.len(), 1);
        assert_eq!(collections[0].requests.len(), 1);
    }

    #[tokio::test]
    async fn normalises_the_identifier() {
        let collections = workspace().list_collections().await.unwrap();

        assert_eq!(collections[0].requests[0].id, InlineWorkspace::request_id());
    }

    #[tokio::test]
    async fn finds_its_own_request() {
        let found = workspace()
            .find_request(&InlineWorkspace::request_id())
            .await
            .unwrap();

        assert!(found.is_some());
    }

    #[tokio::test]
    async fn does_not_find_any_other() {
        let found = workspace()
            .find_request(&RequestId::new("something/else"))
            .await
            .unwrap();

        assert!(found.is_none());
    }

    #[tokio::test]
    async fn saving_an_ad_hoc_request_is_an_error() {
        let result = workspace()
            .save(&InlineWorkspace::request_id(), "GET https://a.test")
            .await;

        assert!(result.is_err());
    }
}
