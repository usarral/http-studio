//! Errors from the application layer.

use http_studio_domain::DomainError;

/// The error a use case returns.
///
/// It wraps domain errors and adds the failure modes that belong to
/// orchestration: not finding something, or an adapter failing. Adapters
/// convert their concrete errors (`std::io::Error`, `reqwest::Error`…) into
/// these variants, which is how the application layer avoids depending on their
/// types.
#[derive(Debug, thiserror::Error)]
pub enum ApplicationError {
    /// A business rule was broken (an undefined variable, an invalid URL…).
    #[error(transparent)]
    Domain(#[from] DomainError),

    /// No request with that identifier exists in the workspace.
    #[error("no request named `{id}`")]
    RequestNotFound {
        /// The identifier that was asked for.
        id: String,
    },

    /// The requested environment does not exist.
    #[error("no environment named `{name}`")]
    EnvironmentNotFound {
        /// The name that was asked for.
        name: String,
    },

    /// A repository could not read or make sense of the workspace.
    #[error("storage error: {message}")]
    Repository {
        /// The failure's detail, already formatted by the adapter.
        message: String,
    },

    /// The transport could not complete the request (DNS, TLS, timeout…).
    #[error("transport error: {message}")]
    Transport {
        /// The failure's detail, already formatted by the adapter.
        message: String,
    },
}

impl ApplicationError {
    /// Shorthand for adapters building a repository error.
    pub fn repository(message: impl std::fmt::Display) -> Self {
        Self::Repository {
            message: message.to_string(),
        }
    }

    /// Shorthand for adapters building a transport error.
    pub fn transport(message: impl std::fmt::Display) -> Self {
        Self::Transport {
            message: message.to_string(),
        }
    }
}
