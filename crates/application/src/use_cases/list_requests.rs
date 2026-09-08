//! Use case: listing the workspace's contents.

use std::sync::Arc;

use http_studio_domain::{Collection, Environment};

use crate::error::ApplicationError;
use crate::ports::{CollectionRepository, EnvironmentRepository};

/// Returns the available collections and environments, to fill a navigation
/// tree or an `hts ls`.
pub struct ListRequests {
    collections: Arc<dyn CollectionRepository>,
    environments: Arc<dyn EnvironmentRepository>,
}

impl ListRequests {
    /// Injects the ports it needs.
    #[must_use]
    pub fn new(
        collections: Arc<dyn CollectionRepository>,
        environments: Arc<dyn EnvironmentRepository>,
    ) -> Self {
        Self {
            collections,
            environments,
        }
    }

    /// Lists every collection with its requests.
    ///
    /// # Errors
    ///
    /// [`ApplicationError::Repository`] when the workspace cannot be read.
    pub async fn collections(&self) -> Result<Vec<Collection>, ApplicationError> {
        self.collections.list_collections().await
    }

    /// Lists every defined environment.
    ///
    /// # Errors
    ///
    /// [`ApplicationError::Repository`] when the workspace cannot be read.
    pub async fn environments(&self) -> Result<Vec<Environment>, ApplicationError> {
        self.environments.list_environments().await
    }
}
