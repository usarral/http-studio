//! Use case: sending a request from the workspace.

use std::collections::BTreeMap;
use std::sync::Arc;

use futures_util::StreamExt;
use http_studio_domain::{ExecutionEvent, RequestId};

use crate::error::ApplicationError;
use crate::ports::{
    CollectionRepository, DynamicSource, EnvironmentRepository, ExchangeRecorder, ExecutionStream,
    HttpTransport, SecretProvider,
};
use crate::use_cases::preparation::RequestPreparer;

/// The use case's input.
#[derive(Debug, Clone)]
pub struct SendRequestInput {
    /// The identifier of the request to send.
    pub request_id: RequestId,
    /// The environment to apply (`--env prod`), when there is one.
    pub environment: Option<String>,
    /// Variables forced from the invocation (`--var k=v`).
    pub overrides: BTreeMap<String, String>,
    /// Skip TLS certificate verification even when the request does not ask.
    ///
    /// It can only **relax** what the file says, never tighten it: a request
    /// marked `# @insecure` stays that way even when this is `false`. That is
    /// the same semantics as `curl -k`, and it keeps an invocation from being
    /// read as a guarantee that verification did happen.
    pub insecure_tls: bool,
}

impl SendRequestInput {
    /// The minimal input: just the identifier, with no environment or overrides.
    #[must_use]
    pub fn new(request_id: RequestId) -> Self {
        Self {
            request_id,
            environment: None,
            overrides: BTreeMap::new(),
            insecure_tls: false,
        }
    }

    /// Sets the environment to apply, builder style.
    #[must_use]
    pub fn with_environment(mut self, environment: Option<String>) -> Self {
        self.environment = environment;
        self
    }

    /// Sets the variable overrides, builder style.
    #[must_use]
    pub fn with_overrides(mut self, overrides: BTreeMap<String, String>) -> Self {
        self.overrides = overrides;
        self
    }

    /// Forces TLS verification to be skipped, builder style.
    #[must_use]
    pub fn with_insecure_tls(mut self, insecure_tls: bool) -> Self {
        self.insecure_tls = insecure_tls;
        self
    }
}

/// Resolves a request, runs it and records the resulting exchange.
pub struct SendRequest {
    preparer: RequestPreparer,
    transport: Arc<dyn HttpTransport>,
    recorder: Arc<dyn ExchangeRecorder>,
}

impl SendRequest {
    /// Injects the ports it needs.
    #[must_use]
    pub fn new(
        collections: Arc<dyn CollectionRepository>,
        environments: Arc<dyn EnvironmentRepository>,
        secrets: Arc<dyn SecretProvider>,
        dynamic: Arc<dyn DynamicSource>,
        transport: Arc<dyn HttpTransport>,
        recorder: Arc<dyn ExchangeRecorder>,
    ) -> Self {
        Self {
            preparer: RequestPreparer::new(collections, environments, secrets, dynamic),
            transport,
            recorder,
        }
    }

    /// Runs the request and returns the stream of events.
    ///
    /// The stream always begins with [`ExecutionEvent::Resolved`], so a UI can
    /// draw the final request before anything arrives from the network.
    ///
    /// *Network* failures do not interrupt the stream: they arrive as an
    /// [`ExecutionEvent::Failed`]. Only failures *before the send* — no such
    /// request, an undefined variable — come back as `Err`, because in that case
    /// there is no execution to watch.
    ///
    /// # Errors
    ///
    /// Those of [`RequestPreparer::prepare`].
    pub async fn execute(
        &self,
        input: SendRequestInput,
    ) -> Result<ExecutionStream, ApplicationError> {
        let prepared = self.preparer.prepare(&input).await?;
        let resolved = prepared.request;

        let mut upstream = self.transport.execute(resolved.clone());
        let recorder = Arc::clone(&self.recorder);

        Ok(Box::pin(async_stream::stream! {
            yield ExecutionEvent::Resolved { request: Box::new(resolved) };

            while let Some(event) = upstream.next().await {
                // The history is a disposable index, not the source of truth:
                // if the write fails the request is still valid, and the user
                // should see its response anyway.
                if let ExecutionEvent::Completed { exchange } = &event {
                    let _ = recorder.record(exchange).await;
                }
                yield event;
            }
        }))
    }
}
