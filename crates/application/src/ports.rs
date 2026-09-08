//! Ports: the contracts infrastructure has to satisfy.
//!
//! Each trait describes a capability in the domain's terms, not a technology's.
//! `CollectionRepository` says "give me the requests", not "read `.http`
//! files": that is what allows an adapter tomorrow that reads them from SQLite,
//! from a remote server, or from memory in a test, without touching a single
//! use case.
//!
//! Every port requires `Send + Sync`, because use cases are shared across async
//! tasks through an `Arc`.

use std::collections::BTreeMap;
use std::pin::Pin;

use async_trait::async_trait;
use futures_core::Stream;
use http_studio_domain::{
    Collection, DynamicSeed, Environment, Exchange, ExecutionEvent, HistoryDetail, HistoryEntry,
    HistoryStats, RequestDefinition, RequestId, ResolvedRequest,
};

use crate::error::ApplicationError;

/// The stream of events a request's execution produces.
///
/// It is returned as a pinned trait object so use cases can compose it — wrap
/// it, filter it — without spreading generics across the whole API.
pub type ExecutionStream = Pin<Box<dyn Stream<Item = ExecutionEvent> + Send>>;

/// A request together with the context of the collection holding it.
///
/// The use case needs the collection's variables to build the precedence stack,
/// but not its sibling requests; so only the relevant data is copied rather than
/// dragging the whole [`Collection`] along.
#[derive(Debug, Clone)]
pub struct LocatedRequest {
    /// The containing collection's name, for messages and traces.
    pub collection_name: String,
    /// Variables declared at collection level.
    pub collection_variables: BTreeMap<String, String>,
    /// The request's definition.
    pub definition: RequestDefinition,
}

/// Read access to the workspace's collections.
#[async_trait]
pub trait CollectionRepository: Send + Sync {
    /// Returns every collection with its requests.
    ///
    /// # Errors
    ///
    /// [`ApplicationError::Repository`] when the workspace cannot be read, or
    /// one of its files is malformed.
    async fn list_collections(&self) -> Result<Vec<Collection>, ApplicationError>;

    /// Looks a request up by identifier.
    ///
    /// Returns `Ok(None)` when it does not exist: "not found" is a legitimate
    /// result, not an error, and it is the use case that decides whether it
    /// should become an [`ApplicationError::RequestNotFound`].
    ///
    /// # Errors
    ///
    /// [`ApplicationError::Repository`] when the workspace cannot be read.
    async fn find_request(
        &self,
        id: &RequestId,
    ) -> Result<Option<LocatedRequest>, ApplicationError>;
}

/// Read access to the workspace's environments.
#[async_trait]
pub trait EnvironmentRepository: Send + Sync {
    /// Returns every defined environment.
    ///
    /// # Errors
    ///
    /// [`ApplicationError::Repository`] when they cannot be read.
    async fn list_environments(&self) -> Result<Vec<Environment>, ApplicationError>;

    /// Looks an environment up by name; `Ok(None)` when it does not exist.
    ///
    /// # Errors
    ///
    /// [`ApplicationError::Repository`] when they cannot be read.
    async fn find_environment(&self, name: &str) -> Result<Option<Environment>, ApplicationError>;
}

/// Actually running an already resolved request.
///
/// It is not an `async fn` but returns an [`ExecutionStream`]: the transport
/// emits events as it goes — headers, body chunks, the end — which lets a TUI
/// show progress without waiting for the whole response.
pub trait HttpTransport: Send + Sync {
    /// Sends the request and returns the stream of its execution's events.
    ///
    /// The stream never fails as a `Result`: failures are emitted as an
    /// [`ExecutionEvent::Failed`], so every UI handles them the same way as any
    /// other event.
    fn execute(&self, request: ResolvedRequest) -> ExecutionStream;
}

/// Persistence for the exchange history.
///
/// This is the queryable index of the hybrid model: the workspace's files are
/// the source of truth for *what* gets sent, and this port records *what
/// happened*. It has to be deletable without losing anything essential.
#[async_trait]
pub trait ExchangeRecorder: Send + Sync {
    /// Records a completed exchange.
    ///
    /// # Errors
    ///
    /// [`ApplicationError::Repository`] when the write failed. The use case
    /// treats that failure as non-fatal: losing one line of history must not
    /// invalidate a request that really was sent.
    async fn record(&self, exchange: &Exchange) -> Result<(), ApplicationError>;
}

/// Where what changes on every run comes from: the clock and randomness.
///
/// It is a port for the same reason [`SecretProvider`] is: reading the clock is
/// touching the world, and the domain does not do that. Having it here also
/// lets a test pin the seed and check that `{{$timestamp}}` comes out with the
/// exact value it expects, instead of settling for "something close to now".
pub trait DynamicSource: Send + Sync {
    /// Returns the seed for one execution.
    ///
    /// It is asked for once per request, not once per placeholder: the values
    /// within one execution have to be consistent with each other.
    fn seed(&self) -> DynamicSeed;
}

/// Where sensitive values come from (tokens, passwords).
///
/// Kept apart from the repositories on purpose: secrets are **never** stored in
/// versioned files. One adapter reads them from environment variables, another
/// from the operating system's keychain, and the workspace only ever holds the
/// `{{name}}` reference.
#[async_trait]
pub trait SecretProvider: Send + Sync {
    /// Returns the available secrets as variables.
    ///
    /// # Errors
    ///
    /// [`ApplicationError::Repository`] when the secret store fails.
    async fn secrets(&self) -> Result<BTreeMap<String, String>, ApplicationError>;
}

/// Access to a request's **source text**, to read and rewrite it.
///
/// It is [`CollectionRepository`]'s sibling port, working at a deliberately
/// different level: that one returns the request already modelled, this one the
/// text exactly as it is written. An editor needs the second, because the
/// formatting, the comments and the ordering a person chose do not survive a
/// round trip through the model.
#[async_trait]
pub trait RequestSource: Send + Sync {
    /// Returns the block's text, or `None` when the identifier does not exist.
    ///
    /// # Errors
    ///
    /// [`ApplicationError::Repository`] when the workspace cannot be read.
    async fn read(&self, id: &RequestId) -> Result<Option<String>, ApplicationError>;

    /// Replaces the request's block with `source`.
    ///
    /// # Errors
    ///
    /// - [`ApplicationError::RequestNotFound`] when the identifier does not
    ///   exist.
    /// - [`ApplicationError::Repository`] when the file cannot be written, or
    ///   when `source` no longer parses: saving something the engine could not
    ///   read back would leave the workspace broken, so it is refused before
    ///   the disk is touched.
    async fn save(&self, id: &RequestId, source: &str) -> Result<(), ApplicationError>;
}

/// The filters of a history query.
///
/// Every field is optional and they combine with `AND`. A default
/// [`HistoryQuery`] means "the most recent executions, unfiltered".
#[derive(Debug, Clone)]
pub struct HistoryQuery {
    /// Narrows to one request.
    pub request_id: Option<RequestId>,
    /// Narrows to an exact status code.
    pub status: Option<u16>,
    /// The most rows to return, newest first.
    pub limit: u32,
}

impl Default for HistoryQuery {
    fn default() -> Self {
        // A bounded default keeps a careless client from pulling months of
        // history back in a single response.
        Self {
            request_id: None,
            status: None,
            limit: 50,
        }
    }
}

/// Reading the execution index.
///
/// It is [`ExchangeRecorder`]'s sibling port: one writes the index, the other
/// queries it. They stay separate because some clients only need one of the two
/// — the CLI always writes, but only `hts history` reads.
#[async_trait]
pub trait HistoryReader: Send + Sync {
    /// Returns the executions matching the filter, newest first.
    ///
    /// # Errors
    ///
    /// [`ApplicationError::Repository`] when the index cannot be queried.
    async fn query(&self, query: &HistoryQuery) -> Result<Vec<HistoryEntry>, ApplicationError>;

    /// Returns one execution's detail, or `None` when it is not there.
    ///
    /// Kept apart from [`HistoryReader::query`] because the listing is asked for
    /// often and the detail rarely: fetching fifty response bodies to draw a
    /// table would be paying for what nobody looks at.
    ///
    /// # Errors
    ///
    /// [`ApplicationError::Repository`] when the index cannot be queried.
    async fn detail(&self, id: i64) -> Result<Option<HistoryDetail>, ApplicationError>;
}

/// What is kept when the index is pruned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PruneScope {
    /// Deletes executions older than `days` days.
    OlderThan {
        /// The age past which rows are deleted.
        days: u32,
    },
    /// Keeps only the `entries` most recent executions.
    KeepLast {
        /// How many are kept.
        entries: u32,
    },
    /// Empties the whole index.
    All,
}

/// Maintenance of the execution index.
///
/// The history's third port, alongside [`ExchangeRecorder`] (which writes) and
/// [`HistoryReader`] (which queries). It exists because the hybrid model's
/// promise — "the files are the source of truth and the index is a cache" — is
/// only true if there is a way to look at it and to delete it without leaving
/// the program.
#[async_trait]
pub trait HistoryMaintenance: Send + Sync {
    /// Summarises what is stored.
    ///
    /// # Errors
    ///
    /// [`ApplicationError::Repository`] when the index cannot be queried.
    async fn stats(&self) -> Result<HistoryStats, ApplicationError>;

    /// Deletes part of the index and returns how many executions went.
    ///
    /// # Errors
    ///
    /// [`ApplicationError::Repository`] when the index cannot be written.
    async fn prune(&self, scope: PruneScope) -> Result<u64, ApplicationError>;
}
