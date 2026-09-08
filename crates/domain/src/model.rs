//! The domain's entities and value objects.
//!
//! The model deliberately distinguishes two states of a request:
//!
//! - [`RequestDefinition`]: what the user writes and versions in git. It may
//!   still contain unresolved `{{variable}}` placeholders.
//! - [`ResolvedRequest`]: the result of applying a [`crate::VariableContext`]
//!   to it. It holds no placeholders and is the only thing the transport takes.
//!
//! That split is what lets a UI show an exact preview of what will be sent
//! without sending it, and spares the engine from having to guess whether a
//! string is resolved: the type system guarantees it.

use std::collections::BTreeMap;
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// A request's stable identifier within the workspace.
///
/// It is derived from the file's relative path (`auth/login`, say), which makes
/// it readable, stable across machines and usable as a CLI argument.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct RequestId(String);

impl RequestId {
    /// Builds an identifier from an already normalised logical path.
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Returns the identifier's textual form.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for RequestId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // `pad` rather than `write_str`, so `{:<20}` works when aligning listings.
        f.pad(&self.0)
    }
}

/// A supported HTTP method.
///
/// Modelled as a closed enum rather than a `String` so the domain rejects
/// invalid methods at compile time when they are built in code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "UPPERCASE")]
pub enum HttpMethod {
    /// `GET`, the default when the file names no method.
    #[default]
    Get,
    /// `POST`.
    Post,
    /// `PUT`.
    Put,
    /// `PATCH`.
    Patch,
    /// `DELETE`.
    Delete,
    /// `HEAD`.
    Head,
    /// `OPTIONS`.
    Options,
}

impl HttpMethod {
    /// The canonical uppercase form, exactly as it travels over the wire.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Get => "GET",
            Self::Post => "POST",
            Self::Put => "PUT",
            Self::Patch => "PATCH",
            Self::Delete => "DELETE",
            Self::Head => "HEAD",
            Self::Options => "OPTIONS",
        }
    }
}

impl std::fmt::Display for HttpMethod {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // `pad` rather than `write_str`, so `{:<7}` aligns the method column.
        f.pad(self.as_str())
    }
}

/// An HTTP header as a name/value pair.
///
/// A `Vec<Header>` is used rather than a map because HTTP allows repeated
/// headers (`Set-Cookie`, `Accept`) and the order can be significant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Header {
    /// The header's name. Comparison is case-insensitive in HTTP.
    pub name: String,
    /// The header's value, possibly with unresolved placeholders.
    pub value: String,
}

impl Header {
    /// Shorthand for building a header.
    #[must_use]
    pub fn new(name: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            value: value.into(),
        }
    }
}

/// A request's body.
///
/// The enum captures the *intent* — this is JSON, this is a form — and not just
/// the bytes, so the engine can derive the right `Content-Type` and UIs know how
/// to highlight the content.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Body {
    /// No body.
    #[default]
    Empty,
    /// Plain text; the `Content-Type` has to be written by hand when it matters.
    Text {
        /// The literal content.
        content: String,
    },
    /// JSON. Implies `Content-Type: application/json` unless overridden.
    Json {
        /// JSON as text, so the author's formatting and comments survive.
        content: String,
    },
    /// An `application/x-www-form-urlencoded` form.
    Form {
        /// Field/value pairs, in the order they were declared.
        fields: Vec<(String, String)>,
    },
}

impl Body {
    /// This body's implied `Content-Type`, when it has one.
    ///
    /// Returns `None` for [`Body::Empty`] and [`Body::Text`]: there the user
    /// decides, and the engine has no business inventing a header.
    #[must_use]
    pub fn implied_content_type(&self) -> Option<&'static str> {
        match self {
            Self::Empty | Self::Text { .. } => None,
            Self::Json { .. } => Some("application/json"),
            Self::Form { .. } => Some("application/x-www-form-urlencoded"),
        }
    }

    /// Whether the body contributes no bytes to the message.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        match self {
            Self::Empty => true,
            Self::Text { content } | Self::Json { content } => content.is_empty(),
            Self::Form { fields } => fields.is_empty(),
        }
    }
}

/// Transport settings that travel with a request.
///
/// They live in the domain rather than in the HTTP adapter because they are
/// decisions made by the request's author and versioned alongside it
/// (`# @insecure` in the `.http`), not configuration of the machine running it.
/// The transport merely obeys them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct RequestOptions {
    /// Do not verify the server's TLS certificate.
    ///
    /// It exists because an internal environment with its own CA, or a
    /// self-signed certificate, returns `UnknownIssuer` and leaves the request
    /// with nowhere to go. Being explicit and per request is **deliberate**:
    /// there is never a global switch that leaves it on without showing up in
    /// the file.
    #[serde(default)]
    pub insecure_tls: bool,
}

/// A request as defined on disk, with placeholders still unresolved.
///
/// This is the entity versioned in git. It is never sent directly: it first
/// becomes a [`ResolvedRequest`] through the variable service.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestDefinition {
    /// The logical identifier derived from the file's path.
    pub id: RequestId,
    /// A readable name for UIs to show.
    pub name: String,
    /// The HTTP method.
    #[serde(default)]
    pub method: HttpMethod,
    /// The URL, typically with a `{{base_url}}` at the front.
    pub url: String,
    /// The headers to send.
    #[serde(default)]
    pub headers: Vec<Header>,
    /// Query parameters added to whatever the URL already carries.
    #[serde(default)]
    pub query: Vec<(String, String)>,
    /// The request's body.
    #[serde(default)]
    pub body: Body,
    /// Variables local to this request (the lowest useful precedence layer).
    #[serde(default)]
    pub variables: BTreeMap<String, String>,
    /// Free-form description, to document the request.
    #[serde(default)]
    pub description: Option<String>,
    /// Transport settings declared in the request.
    #[serde(default)]
    pub options: RequestOptions,
}

/// A fully interpolated and validated request, ready for the transport.
///
/// The only way to obtain this type is through the resolution service, which
/// guarantees by construction that no placeholders are left.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedRequest {
    /// The identifier of the definition it came from.
    pub id: RequestId,
    /// The HTTP method.
    pub method: HttpMethod,
    /// A valid absolute URL, with the query already merged in.
    pub url: url::Url,
    /// The final headers, including any derived from the body type.
    pub headers: Vec<Header>,
    /// The body, already interpolated.
    pub body: Body,
    /// Transport settings inherited from the definition.
    ///
    /// They are carried this far so the transport never has to look back at the
    /// definition: this struct is the only thing it receives.
    #[serde(default)]
    pub options: RequestOptions,
}

/// The response's status line and headers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResponseHead {
    /// The HTTP status code.
    pub status: u16,
    /// The negotiated protocol version (`HTTP/1.1`, `HTTP/2.0`…).
    pub version: String,
    /// Response headers, in the order they arrived.
    pub headers: Vec<Header>,
}

impl ResponseHead {
    /// `true` when the status is in the 2xx range.
    #[must_use]
    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }
}

/// The response body, already materialised in memory.
///
/// For large or streaming responses, UIs should consume
/// [`ExecutionEvent::BodyChunk`] rather than wait for this value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResponseBody {
    /// The raw bytes received, already decompressed.
    pub bytes: Vec<u8>,
}

impl ResponseBody {
    /// Reads the body as UTF-8, replacing invalid sequences.
    ///
    /// It never fails: a binary response comes out as text with replacement
    /// characters, which is what a UI wants to show anyway.
    #[must_use]
    pub fn as_text(&self) -> std::borrow::Cow<'_, str> {
        String::from_utf8_lossy(&self.bytes)
    }

    /// The received body's size in bytes.
    #[must_use]
    pub fn len(&self) -> usize {
        self.bytes.len()
    }

    /// `true` when no body bytes were received.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }
}

/// An exchange's timing metrics.
///
/// They are exposed separately because they are the first thing a diagnostic UI
/// wants to show, and because they let executions be compared in the history.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Timings {
    /// Time until the status line and headers arrived.
    pub time_to_first_byte: Duration,
    /// Total time, including downloading the whole body.
    pub total: Duration,
}

/// A complete exchange: what was sent and what came back.
///
/// It is the unit persisted in the history, and what a send use case returns
/// when it finishes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Exchange {
    /// The resolved request that was sent.
    pub request: ResolvedRequest,
    /// The response's status and headers.
    pub head: ResponseHead,
    /// The response's full body.
    pub body: ResponseBody,
    /// The execution's metrics.
    pub timings: Timings,
}

/// A summary of an executed exchange, as the index returns it.
///
/// It is not a full [`Exchange`]: the index keeps what is useful for searching
/// and comparing executions, not every response's whole body. Anyone who needs
/// the full detail asks for it by [`HistoryEntry::id`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryEntry {
    /// The row's identifier in the index.
    pub id: i64,
    /// The identifier of the request that ran.
    pub request_id: RequestId,
    /// The HTTP method used.
    pub method: String,
    /// The final URL that was called.
    pub url: String,
    /// The status code received.
    pub status: u16,
    /// Total duration in milliseconds.
    pub duration_ms: u64,
    /// Body bytes received.
    pub response_bytes: u64,
    /// When it ran, in UTC and `YYYY-MM-DD HH:MM:SS` format.
    pub executed_at: String,
}

/// The full detail of one execution, as the index stored it.
///
/// It is what [`HistoryEntry`] leaves out: the request that was sent, the
/// headers that came back and the body. The body may be truncated — the index
/// is a cache, not an archive — which is why it travels with
/// [`HistoryDetail::body_truncated`]: a half response that does not announce
/// itself as one is worse than no response at all.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryDetail {
    /// The summary an index query already returns.
    pub entry: HistoryEntry,
    /// The resolved request that was sent.
    pub request: ResolvedRequest,
    /// The response's status and headers.
    pub head: ResponseHead,
    /// The stored body, possibly truncated.
    pub body: ResponseBody,
    /// `true` when the body did not fit and only its beginning was stored.
    pub body_truncated: bool,
}

/// What the execution index holds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct HistoryStats {
    /// Executions recorded.
    pub executions: u64,
    /// Distinct requests appearing among them.
    pub requests: u64,
    /// When the oldest execution ran, if there is one.
    pub oldest: Option<String>,
    /// When the most recent one ran, if there is one.
    pub newest: Option<String>,
    /// Bytes of response bodies stored.
    pub stored_body_bytes: u64,
}

/// An event emitted while a request runs.
///
/// The engine exposes execution as a stream of events rather than as a blocking
/// call. This is the key piece that lets a TUI show live progress while the CLI
/// simply ignores the intermediate events: **every UI consumes exactly the same
/// stream**, so none of them has to reimplement engine logic.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum ExecutionEvent {
    /// The request was interpolated and validated; it has not left the machine yet.
    Resolved {
        /// The request ready to send, useful for previews.
        request: Box<ResolvedRequest>,
    },
    /// The response's status and headers arrived.
    Head {
        /// The status line and headers.
        head: Box<ResponseHead>,
    },
    /// A chunk of the body arrived.
    BodyChunk {
        /// How many bytes this chunk carries.
        len: usize,
    },
    /// The execution finished successfully.
    Completed {
        /// The complete exchange.
        exchange: Box<Exchange>,
    },
    /// The execution failed; the message is already formatted for display.
    Failed {
        /// A description of the failure.
        message: String,
    },
}

/// A named set of variables (`dev`, `staging`, `prod`…).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Environment {
    /// The environment's name, used with `--env`.
    pub name: String,
    /// The variables it contributes.
    #[serde(default)]
    pub variables: BTreeMap<String, String>,
}

/// A logical grouping of requests, equivalent to one file on disk.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Collection {
    /// The collection's name.
    pub name: String,
    /// Variables shared by all of its requests.
    #[serde(default)]
    pub variables: BTreeMap<String, String>,
    /// The requests it contains.
    #[serde(default)]
    pub requests: Vec<RequestDefinition>,
}
