//! Use cases: one product operation per type.
//!
//! Each use case takes its dependencies as `Arc<dyn Port>` in the constructor —
//! explicit dependency injection — and exposes a single public method. The
//! composition root decides which concrete adapters get injected.

mod import_collections;
mod list_requests;
mod preparation;
mod preview_request;
mod send_request;

pub use import_collections::{ImportCollections, ImportInput, ImportOutput, ImportSource};
pub use list_requests::ListRequests;
pub use preparation::{RequestPreparation, RequestPreparer};
pub use preview_request::PreviewRequest;
pub use send_request::{SendRequest, SendRequestInput};
