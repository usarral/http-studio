//! HTTP Studio's application layer: **ports and use cases**.
//!
//! This layer answers "what the system can do" without deciding "with which
//! technology". It holds:
//!
//! - [`ports`]: the traits infrastructure has to implement (repositories, the
//!   HTTP transport, the history, secrets). They are the boundary: the
//!   application programs against them, never against `reqwest` or `rusqlite`.
//! - [`use_cases`]: the orchestration of each product operation — sending a
//!   request, previewing it, listing the workspace.
//! - [`context`]: the service that assembles the variable stack in the right
//!   precedence order.
//!
//! # Why this matters for the UIs
//!
//! A use case returns a stream of [`http_studio_domain::ExecutionEvent`]. The
//! CLI, a TUI, a Neovim plugin or a JSON-RPC server consume that same stream
//! and only decide how to draw it. No UI reimplements variable resolution,
//! precedence or error handling: if a new client needs logic that is not here,
//! that is an architecture bug.

pub mod context;
pub mod engine;
pub mod error;
pub mod ports;
pub mod use_cases;

pub use engine::{Engine, EnginePorts};

/// Alias for a history query's result, so the `Vec` is not spelled out twice.
pub type HistoryEntryList = Vec<http_studio_domain::HistoryEntry>;
pub use error::ApplicationError;
pub use ports::{
    CollectionRepository, DynamicSource, EnvironmentRepository, ExchangeRecorder, ExecutionStream,
    HistoryMaintenance, HistoryQuery, HistoryReader, HttpTransport, LocatedRequest, PruneScope,
    RequestSource, SecretProvider,
};
pub use use_cases::{ListRequests, PreviewRequest, SendRequest, SendRequestInput};
