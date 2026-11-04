//! Persistence adapters.

mod collection_writer;
mod documents;
mod env_file;
mod filesystem;
mod history;
pub mod http_file;
pub mod http_writer;
mod inline;

pub use documents::LocalDocuments;
pub use filesystem::FileSystemWorkspace;
pub use history::{MAX_STORED_BODY, NullHistory, SqliteHistory};
pub use http_file::{HttpFile, ParseError};
pub use http_writer::{render_collection, render_request};
pub use inline::InlineWorkspace;
