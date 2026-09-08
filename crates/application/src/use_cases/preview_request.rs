//! Use case: previewing a request without sending it.
//!
//! It is the equivalent of a `--dry-run`, and the basis of the UIs' "explain
//! this variable" view: it returns both the final request and the variable
//! context that produced it.

use std::sync::Arc;

use crate::error::ApplicationError;
use crate::ports::{CollectionRepository, DynamicSource, EnvironmentRepository, SecretProvider};
use crate::use_cases::preparation::{RequestPreparation, RequestPreparer};
use crate::use_cases::send_request::SendRequestInput;

/// Resolves a request and returns it without touching the network.
pub struct PreviewRequest {
    preparer: RequestPreparer,
}

impl PreviewRequest {
    /// Injects the ports it needs.
    #[must_use]
    pub fn new(
        collections: Arc<dyn CollectionRepository>,
        environments: Arc<dyn EnvironmentRepository>,
        secrets: Arc<dyn SecretProvider>,
        dynamic: Arc<dyn DynamicSource>,
    ) -> Self {
        Self {
            preparer: RequestPreparer::new(collections, environments, secrets, dynamic),
        }
    }

    /// Returns the resolved request and the variable context applied to it.
    ///
    /// # Errors
    ///
    /// Those of [`RequestPreparer::prepare`].
    pub async fn execute(
        &self,
        input: SendRequestInput,
    ) -> Result<RequestPreparation, ApplicationError> {
        self.preparer.prepare(&input).await
    }
}
