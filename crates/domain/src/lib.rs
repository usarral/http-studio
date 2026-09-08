//! The HTTP Studio domain core.
//!
//! This layer is **pure**: it does no I/O and knows nothing of `tokio`,
//! `reqwest` or the file system. It holds only:
//!
//! - **Entities and value objects** ([`model`]): what a request, an
//!   environment, a response or a complete exchange is.
//! - **Domain services** ([`variables`], [`dynamic`]): variable interpolation
//!   and the meaning of each dynamic placeholder, which are real business
//!   logic and can therefore be tested without starting anything.
//! - **Domain errors** ([`error`]).
//!
//! The dependency rule: nothing in this crate may import from `application`,
//! `infrastructure` or `cli`. If something here needs I/O, that is the signal
//! it belongs to another layer.

pub mod dynamic;
pub mod error;
pub mod model;
pub mod resolver;
pub mod variables;

pub use dynamic::{DynamicSeed, resolve_dynamic};
pub use error::DomainError;
pub use model::{
    Body, Collection, Environment, Exchange, ExecutionEvent, Header, HistoryDetail, HistoryEntry,
    HistoryStats, HttpMethod, RequestDefinition, RequestId, RequestOptions, ResolvedRequest,
    ResponseBody, ResponseHead, Timings,
};
pub use resolver::resolve;
pub use variables::{VariableContext, VariableScope, interpolate};
