//! Persistence adapters.

mod env_file;
mod filesystem;
mod history;
pub mod http_file;
mod inline;

pub use filesystem::FileSystemWorkspace;
pub use history::{MAX_STORED_BODY, NullHistory, SqliteHistory};
pub use http_file::{HttpFile, ParseError};
pub use inline::InlineWorkspace;
