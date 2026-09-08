//! The shared step: locating a request and resolving it against its variables.
//!
//! Sending and previewing need exactly the same work up to the moment the
//! socket opens. Extracting it here is what guarantees that the preview the
//! user sees is, byte for byte, what will be sent: there are not two code paths
//! that could drift apart.

use std::sync::Arc;

use http_studio_domain::{Environment, ResolvedRequest, VariableContext, resolver};

use crate::context::build_variable_context;
use crate::error::ApplicationError;
use crate::ports::{
    CollectionRepository, DynamicSource, EnvironmentRepository, LocatedRequest, SecretProvider,
};
use crate::use_cases::send_request::SendRequestInput;

/// The result of preparing a request.
///
/// The [`VariableContext`] comes back too, so UIs can explain where each value
/// came from without redoing the work of assembling it.
#[derive(Debug)]
pub struct RequestPreparation {
    /// The request, ready to send.
    pub request: ResolvedRequest,
    /// The variable context used during resolution.
    pub context: VariableContext,
    /// The environment applied, when one was asked for.
    pub environment: Option<Environment>,
}

/// The application service that resolves a request without running it.
pub struct RequestPreparer {
    collections: Arc<dyn CollectionRepository>,
    environments: Arc<dyn EnvironmentRepository>,
    secrets: Arc<dyn SecretProvider>,
    dynamic: Arc<dyn DynamicSource>,
}

impl RequestPreparer {
    /// Injects the ports it needs.
    #[must_use]
    pub fn new(
        collections: Arc<dyn CollectionRepository>,
        environments: Arc<dyn EnvironmentRepository>,
        secrets: Arc<dyn SecretProvider>,
        dynamic: Arc<dyn DynamicSource>,
    ) -> Self {
        Self {
            collections,
            environments,
            secrets,
            dynamic,
        }
    }

    /// Locates the request, assembles its variables and resolves it.
    ///
    /// # Errors
    ///
    /// - [`ApplicationError::RequestNotFound`] when the identifier does not
    ///   exist.
    /// - [`ApplicationError::EnvironmentNotFound`] when a non-existent
    ///   environment was asked for. Failing here is deliberate: running against
    ///   the wrong environment because of a typo is worse than not running.
    /// - [`ApplicationError::Domain`] when interpolation or the URL fails.
    /// - [`ApplicationError::Repository`] when the workspace cannot be read.
    pub async fn prepare(
        &self,
        input: &SendRequestInput,
    ) -> Result<RequestPreparation, ApplicationError> {
        let located = self.locate(input).await?;
        let environment = self
            .resolve_environment(input.environment.as_deref())
            .await?;
        let secrets = self.secrets.secrets().await?;

        let context = build_variable_context(
            &located,
            environment.as_ref(),
            secrets,
            input.overrides.clone(),
            // One seed per execution: every `{{$…}}` in the same request looks
            // at the same clock.
            self.dynamic.seed(),
        );

        let mut request = resolver::resolve(&located.definition, &context)?;

        // The invocation adds to what the file declares rather than replacing
        // it: see [`SendRequestInput::insecure_tls`].
        request.options.insecure_tls |= input.insecure_tls;

        Ok(RequestPreparation {
            request,
            context,
            environment,
        })
    }

    /// Finds the request and turns "does not exist" into a use-case error.
    async fn locate(&self, input: &SendRequestInput) -> Result<LocatedRequest, ApplicationError> {
        self.collections
            .find_request(&input.request_id)
            .await?
            .ok_or_else(|| ApplicationError::RequestNotFound {
                id: input.request_id.to_string(),
            })
    }

    /// Resolves the requested environment; `None` when none was asked for.
    async fn resolve_environment(
        &self,
        name: Option<&str>,
    ) -> Result<Option<Environment>, ApplicationError> {
        let Some(name) = name else {
            return Ok(None);
        };

        self.environments
            .find_environment(name)
            .await?
            .ok_or_else(|| ApplicationError::EnvironmentNotFound {
                name: name.to_owned(),
            })
            .map(Some)
    }
}
