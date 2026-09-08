//! Protocol types: the JSON-RPC 2.0 envelope and each method's parameters.
//!
//! Results are not redefined here: the domain entities, which already derive
//! `Serialize`, are serialized directly. Keeping a parallel hierarchy of
//! outgoing DTOs would be the fastest way to make the wire and the engine
//! drift apart.

use std::collections::BTreeMap;

use http_studio_domain::{ExecutionEvent, RequestId};
use serde::{Deserialize, Serialize};

/// The JSON-RPC version announced and required.
pub const JSONRPC_VERSION: &str = "2.0";

/// A message received from the client.
///
/// When `id` is `None` the message is a notification and must not be answered,
/// as JSON-RPC 2.0 requires.
#[derive(Debug, Clone, Deserialize)]
pub struct Incoming {
    /// Must be exactly `"2.0"`.
    pub jsonrpc: String,
    /// The request's identifier; absent on notifications.
    #[serde(default)]
    pub id: Option<serde_json::Value>,
    /// The method invoked.
    pub method: String,
    /// The parameters, whose shape depends on the method.
    #[serde(default)]
    pub params: Option<serde_json::Value>,
}

/// The answer to a request.
#[derive(Debug, Clone, Serialize)]
pub struct Response {
    /// Always `"2.0"`.
    pub jsonrpc: &'static str,
    /// The same identifier the request carried.
    pub id: serde_json::Value,
    /// The result, if the call succeeded.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    /// The error, if the call failed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError>,
}

impl Response {
    /// A successful response.
    #[must_use]
    pub fn success(id: serde_json::Value, result: serde_json::Value) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION,
            id,
            result: Some(result),
            error: None,
        }
    }

    /// A failing response.
    #[must_use]
    pub fn failure(id: serde_json::Value, error: RpcError) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION,
            id,
            result: None,
            error: Some(error),
        }
    }
}

/// An error in the shape JSON-RPC 2.0 defines.
#[derive(Debug, Clone, Serialize)]
pub struct RpcError {
    /// The numeric code.
    pub code: i32,
    /// A readable message.
    pub message: String,
}

impl RpcError {
    /// The JSON received could not be parsed (`-32700`).
    #[must_use]
    pub fn parse_error(detail: impl std::fmt::Display) -> Self {
        Self {
            code: -32700,
            message: format!("invalid JSON: {detail}"),
        }
    }

    /// The message is not a valid JSON-RPC request (`-32600`).
    #[must_use]
    pub fn invalid_request(detail: impl std::fmt::Display) -> Self {
        Self {
            code: -32600,
            message: detail.to_string(),
        }
    }

    /// The method does not exist (`-32601`).
    #[must_use]
    pub fn method_not_found(method: &str) -> Self {
        Self {
            code: -32601,
            message: format!("unknown method: `{method}`"),
        }
    }

    /// The parameters do not fit the method (`-32602`).
    #[must_use]
    pub fn invalid_params(detail: impl std::fmt::Display) -> Self {
        Self {
            code: -32602,
            message: format!("invalid parameters: {detail}"),
        }
    }

    /// An internal server failure while serializing the response (`-32603`).
    #[must_use]
    pub fn internal(detail: impl std::fmt::Display) -> Self {
        Self {
            code: -32603,
            message: detail.to_string(),
        }
    }

    /// The engine refused the operation (`-32000`).
    ///
    /// The range reserved for application errors is used so the client can
    /// tell "you called me wrong" apart from "your workspace has a problem",
    /// which are fixed in very different ways.
    #[must_use]
    pub fn application(detail: impl std::fmt::Display) -> Self {
        Self {
            code: -32000,
            message: detail.to_string(),
        }
    }
}

/// A notification the server sends without anyone asking for it.
#[derive(Debug, Clone, Serialize)]
pub struct Notification<T> {
    /// Always `"2.0"`.
    pub jsonrpc: &'static str,
    /// The notification's method, e.g. `execution/event`.
    pub method: &'static str,
    /// The payload.
    pub params: T,
}

impl<T> Notification<T> {
    /// Builds a notification.
    #[must_use]
    pub fn new(method: &'static str, params: T) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION,
            method,
            params,
        }
    }
}

/// The parameters shared by `request/preview` and `request/send`.
///
/// They are the same on purpose, as in the CLI: if previewing accepted a
/// different input than sending, the preview would stop being trustworthy.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RequestParams {
    /// The request's identifier, e.g. `auth/login`.
    pub request_id: String,
    /// The environment to apply.
    #[serde(default)]
    pub environment: Option<String>,
    /// Forced variables, the equivalent of `--var`.
    #[serde(default)]
    pub variables: BTreeMap<String, String>,
    /// Skips TLS certificate verification, the equivalent of `--insecure`.
    ///
    /// As in the CLI, it can only relax what the file declares: a `false` here
    /// does not switch off a `# @insecure` written in the request.
    #[serde(default)]
    pub insecure_tls: bool,
}

/// The parameters of `request/cancel`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CancelParams {
    /// The execution to abort.
    pub execution_id: String,
}

/// The parameters of `history/query`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HistoryParams {
    /// Narrows to one request.
    #[serde(default)]
    pub request_id: Option<String>,
    /// Narrows to an exact status code.
    #[serde(default)]
    pub status: Option<u16>,
    /// The maximum number of rows.
    #[serde(default)]
    pub limit: Option<u32>,
}

/// The parameters of `history/detail`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HistoryDetailParams {
    /// The row's identifier, exactly as `history/query` returns it.
    pub id: i64,
}

/// A resolved variable and the scope it came from.
///
/// It is what lets a client offer "where does this value come from?" without
/// reimplementing precedence: the engine computes the answer.
#[derive(Debug, Clone, Serialize)]
pub struct ResolvedVariable {
    /// The variable's name.
    pub name: String,
    /// The effective value once precedence has been applied.
    pub value: String,
    /// The scope that contributed the winning value (`environment:prod`, `cli`…).
    pub origin: String,
}

/// The payload of the `execution/event` notification.
///
/// The event is flattened onto the object, so the client sees the same keys as
/// in the CLI's JSON Lines output plus the `executionId`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecutionEventParams {
    /// The execution the event belongs to.
    pub execution_id: String,
    /// The event exactly as the engine emits it.
    #[serde(flatten)]
    pub event: ExecutionEvent,
}

/// Turns the identifier received into the domain's type.
#[must_use]
pub fn request_id(raw: &str) -> RequestId {
    RequestId::new(raw)
}
