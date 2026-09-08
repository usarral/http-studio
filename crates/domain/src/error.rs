//! Errors from the domain layer.

use std::fmt;

/// An error produced by pure business rules (validation, interpolation…).
///
/// It carries no I/O errors: those belong to the infrastructure layer, and are
/// translated into a [`crate::error::DomainError`] only when they represent a
/// broken domain rule.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DomainError {
    /// `{{name}}` was referenced but no scope in the context defines it.
    #[error("undefined variable: `{name}`")]
    UndefinedVariable {
        /// The variable's name, exactly as it appeared in the template.
        name: String,
    },

    /// The template has a `{{` without its matching `}}`.
    #[error("unclosed `{{{{` delimiter at position {position}")]
    UnterminatedPlaceholder {
        /// Byte offset where the unclosed placeholder starts.
        position: usize,
    },

    /// A cycle was found in the chain of variable references.
    #[error("circular variable reference: {}", .chain.join(" -> "))]
    CircularReference {
        /// The chain of names walked before the cycle was detected.
        chain: Vec<String>,
    },

    /// `{{$something}}` was used with a name we do not know how to generate.
    #[error("unknown dynamic variable: `{name}`")]
    UnknownDynamicVariable {
        /// The name as it was written, `$` included.
        name: String,
    },

    /// The name is known but its arguments do not fit.
    #[error("malformed dynamic variable `{{{{{spec}}}}}`: {reason}")]
    InvalidDynamicVariable {
        /// The placeholder's full contents, arguments included.
        spec: String,
        /// What was expected instead.
        reason: String,
    },

    /// The URL left after interpolation is not a valid absolute URL.
    #[error("invalid URL `{url}`: {reason}")]
    InvalidUrl {
        /// The interpolated URL that failed to parse.
        url: String,
        /// The reason the parser gave.
        reason: String,
    },

    /// A required field of the definition is empty or malformed.
    #[error("invalid request in `{field}`: {reason}")]
    InvalidRequest {
        /// The field at fault (`method`, `url`, `header`…).
        field: &'static str,
        /// A readable explanation.
        reason: String,
    },
}

impl DomainError {
    /// Builds a [`DomainError::InvalidRequest`] from anything displayable, so
    /// callers do not have to repeat `.to_string()` every time.
    pub fn invalid_request(field: &'static str, reason: impl fmt::Display) -> Self {
        Self::InvalidRequest {
            field,
            reason: reason.to_string(),
        }
    }
}
