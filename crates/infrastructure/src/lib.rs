//! The infrastructure layer: concrete adapters for the ports.
//!
//! Everything that "touches the world" lives here and only here: `reqwest` for
//! the network, the file system for the workspace, SQLite for the history, the
//! process environment for secrets.
//!
//! None of those dependencies surface in `http_studio_application`'s public
//! signatures, so swapping an adapter — or writing a fake one in a test — never
//! forces a use case to change.
//!
//! # The hybrid storage model
//!
//! - **Source of truth**: `.http` files, versioned in git
//!   ([`storage::FileSystemWorkspace`]). This is what gets reviewed in a pull
//!   request.
//! - **Index**: a SQLite database with the execution history
//!   ([`storage::SqliteHistory`]). It is a cache: it can be deleted without
//!   losing anything that cannot be obtained again.

pub mod dynamic;
pub mod http;
pub mod secrets;
pub mod storage;

pub use dynamic::SystemDynamicSource;
pub use http::ReqwestTransport;
pub use secrets::EnvSecretProvider;
pub use storage::{
    FileSystemWorkspace, InlineWorkspace, MAX_STORED_BODY, NullHistory, SqliteHistory,
};
