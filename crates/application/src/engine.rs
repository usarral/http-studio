//! The engine facade: one entry point for every interface.
//!
//! [`Engine`] gathers the already built ports and makes use cases on demand. It
//! exists so the CLI, the JSON-RPC server and the TUI share exactly the same
//! object instead of each keeping its own version of the wiring, which is where
//! behavioural differences between one product's interfaces usually creep in.
//!
//! Building the concrete adapters stays the job of each binary's composition
//! root; [`Engine`] only receives them ready-made.
//!
//! It is cheap to `Clone`: every field is an `Arc`, so each task serving a
//! request can keep its own copy.

use std::sync::Arc;

use http_studio_domain::{HistoryDetail, HistoryStats, RequestId};

use crate::ports::{
    CollectionImporter, CollectionRepository, CollectionWriter, DocumentLoader, DynamicSource,
    EnvironmentRepository, ExchangeRecorder, HistoryMaintenance, HistoryQuery, HistoryReader,
    HttpTransport, PruneScope, RequestSource, SecretProvider,
};
use crate::use_cases::{ImportCollections, ListRequests, PreviewRequest, SendRequest};
use crate::{ApplicationError, HistoryEntryList};

/// The ports an [`Engine`] is assembled from.
///
/// They are passed in a named struct rather than as eight positional
/// parameters because nearly all of them are `Arc<dyn …>`: two of them swapped
/// compile just as well and fail at runtime. With fields, the compiler says
/// which one is missing.
pub struct EnginePorts {
    /// Reading the workspace's collections.
    pub collections: Arc<dyn CollectionRepository>,
    /// Reading the environments.
    pub environments: Arc<dyn EnvironmentRepository>,
    /// Where the secrets come from.
    pub secrets: Arc<dyn SecretProvider>,
    /// Where the clock and randomness for dynamic variables come from.
    pub dynamic: Arc<dyn DynamicSource>,
    /// The real HTTP transport.
    pub transport: Arc<dyn HttpTransport>,
    /// Writing the execution index.
    pub recorder: Arc<dyn ExchangeRecorder>,
    /// Querying the index.
    pub history: Arc<dyn HistoryReader>,
    /// Maintaining the index.
    pub maintenance: Arc<dyn HistoryMaintenance>,
    /// Access to the requests' source text.
    pub source: Arc<dyn RequestSource>,
    /// Writing collections into the workspace.
    pub writer: Arc<dyn CollectionWriter>,
    /// Reading documents the user points at, from outside the workspace.
    pub documents: Arc<dyn DocumentLoader>,
}

/// The HTTP Studio engine with all of its dependencies resolved.
#[derive(Clone)]
pub struct Engine {
    collections: Arc<dyn CollectionRepository>,
    environments: Arc<dyn EnvironmentRepository>,
    secrets: Arc<dyn SecretProvider>,
    dynamic: Arc<dyn DynamicSource>,
    transport: Arc<dyn HttpTransport>,
    recorder: Arc<dyn ExchangeRecorder>,
    history: Arc<dyn HistoryReader>,
    maintenance: Arc<dyn HistoryMaintenance>,
    source: Arc<dyn RequestSource>,
    writer: Arc<dyn CollectionWriter>,
    documents: Arc<dyn DocumentLoader>,
}

impl Engine {
    /// Assembles the engine from its ports.
    #[must_use]
    pub fn new(ports: EnginePorts) -> Self {
        Self {
            collections: ports.collections,
            environments: ports.environments,
            secrets: ports.secrets,
            dynamic: ports.dynamic,
            transport: ports.transport,
            recorder: ports.recorder,
            history: ports.history,
            maintenance: ports.maintenance,
            source: ports.source,
            writer: ports.writer,
            documents: ports.documents,
        }
    }

    /// The send use case.
    #[must_use]
    pub fn send_request(&self) -> SendRequest {
        SendRequest::new(
            Arc::clone(&self.collections),
            Arc::clone(&self.environments),
            Arc::clone(&self.secrets),
            Arc::clone(&self.dynamic),
            Arc::clone(&self.transport),
            Arc::clone(&self.recorder),
        )
    }

    /// The preview use case.
    #[must_use]
    pub fn preview_request(&self) -> PreviewRequest {
        PreviewRequest::new(
            Arc::clone(&self.collections),
            Arc::clone(&self.environments),
            Arc::clone(&self.secrets),
            Arc::clone(&self.dynamic),
        )
    }

    /// The import use case, for the format `importer` understands.
    ///
    /// The importer arrives here rather than in [`EnginePorts`] because it is
    /// the only dependency that changes per invocation: `hts import curl` and
    /// `hts import openapi` are the same operation over two different readers,
    /// and the engine has no reason to learn either format's name.
    #[must_use]
    pub fn import_collections(&self, importer: Arc<dyn CollectionImporter>) -> ImportCollections {
        ImportCollections::new(
            importer,
            Arc::clone(&self.writer),
            Arc::clone(&self.documents),
            Arc::clone(&self.transport),
        )
    }

    /// The workspace listing use case.
    #[must_use]
    pub fn list_requests(&self) -> ListRequests {
        ListRequests::new(
            Arc::clone(&self.collections),
            Arc::clone(&self.environments),
        )
    }

    /// Queries the execution index.
    ///
    /// # Errors
    ///
    /// [`ApplicationError::Repository`] when the index cannot be queried.
    pub async fn history(
        &self,
        query: &HistoryQuery,
    ) -> Result<HistoryEntryList, ApplicationError> {
        self.history.query(query).await
    }

    /// Returns one execution's detail from the index, or `None` when absent.
    ///
    /// # Errors
    ///
    /// [`ApplicationError::Repository`] when the index cannot be queried.
    pub async fn history_detail(&self, id: i64) -> Result<Option<HistoryDetail>, ApplicationError> {
        self.history.detail(id).await
    }

    /// Summarises what the index holds.
    ///
    /// # Errors
    ///
    /// [`ApplicationError::Repository`] when the index cannot be queried.
    pub async fn index_stats(&self) -> Result<HistoryStats, ApplicationError> {
        self.maintenance.stats().await
    }

    /// Prunes the index and returns how many executions were deleted.
    ///
    /// # Errors
    ///
    /// [`ApplicationError::Repository`] when the index cannot be written.
    pub async fn prune_index(&self, scope: PruneScope) -> Result<u64, ApplicationError> {
        self.maintenance.prune(scope).await
    }

    /// Returns a request's source text, or `None` when it does not exist.
    ///
    /// # Errors
    ///
    /// [`ApplicationError::Repository`] when the workspace cannot be read.
    pub async fn read_source(&self, id: &RequestId) -> Result<Option<String>, ApplicationError> {
        self.source.read(id).await
    }

    /// Saves a request's source text.
    ///
    /// # Errors
    ///
    /// Those of [`RequestSource::save`]: no such identifier, a write failure,
    /// or text that no longer parses.
    pub async fn save_source(&self, id: &RequestId, source: &str) -> Result<(), ApplicationError> {
        self.source.save(id, source).await
    }
}
