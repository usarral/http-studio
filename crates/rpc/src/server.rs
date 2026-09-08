//! The server loop and method dispatch.
//!
//! # Concurrency
//!
//! The read loop executes nothing: it parses the message and spawns one task
//! per request. That is what lets a `request/cancel` arrive and be served
//! while the execution it wants to abort is still downloading.
//!
//! Every outgoing message goes through a single channel into a single writer
//! task. With several tasks writing straight to stdout, messages would
//! interleave halfway through a header and the framing would stop being
//! valid.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use futures_util::StreamExt;
use http_studio_application::{Engine, HistoryQuery, SendRequestInput};
use http_studio_domain::ExecutionEvent;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::mpsc::{self, UnboundedSender};
use tokio::task::AbortHandle;

use crate::codec::{MessageReader, write_message};
use crate::protocol::{
    CancelParams, ExecutionEventParams, HistoryDetailParams, HistoryParams, Incoming,
    JSONRPC_VERSION, Notification, RequestParams, ResolvedVariable, Response, RpcError, request_id,
};

/// The method name of the execution-event notification.
const EXECUTION_EVENT: &str = "execution/event";

/// The executions in flight, so they can be aborted by `executionId`.
type Executions = Arc<Mutex<HashMap<String, AbortHandle>>>;

/// The state shared by every task serving requests.
#[derive(Clone)]
struct ServerState {
    engine: Engine,
    outgoing: UnboundedSender<String>,
    executions: Executions,
    next_execution: Arc<AtomicU64>,
}

impl ServerState {
    /// Serializes a message and queues it for the client.
    ///
    /// A failure to send can only mean the writer task already finished, that
    /// is, that the client closed: there is nothing to report and nothing to
    /// salvage, so it is dropped silently.
    fn send<T: serde::Serialize>(&self, message: &T) {
        if let Ok(raw) = serde_json::to_string(message) {
            let _ = self.outgoing.send(raw);
        }
    }

    /// Reserves an execution identifier unique within this process.
    fn allocate_execution_id(&self) -> String {
        let n = self.next_execution.fetch_add(1, Ordering::Relaxed);
        format!("exec-{n}")
    }
}

/// Serves JSON-RPC requests until the input stream closes.
///
/// Returns `Ok(())` when the client closes cleanly, which is the normal way to
/// finish: the editor kills the child process on exit.
///
/// # Errors
///
/// If reading the input stream fails, or a badly framed message arrives. A
/// well-framed but invalid message is **not** a server error: it is answered
/// with an `RpcError` and the loop carries on, because a client that gets one
/// call wrong should not bring down the whole session.
pub async fn serve<R, W>(engine: Engine, reader: R, writer: W) -> std::io::Result<()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin + Send + 'static,
{
    let (outgoing, mut rx) = mpsc::unbounded_channel::<String>();

    let writer_task = tokio::spawn(async move {
        let mut writer = writer;
        while let Some(message) = rx.recv().await {
            if write_message(&mut writer, &message).await.is_err() {
                break;
            }
        }
    });

    let state = ServerState {
        engine,
        outgoing,
        executions: Arc::new(Mutex::new(HashMap::new())),
        next_execution: Arc::new(AtomicU64::new(1)),
    };

    let mut reader = MessageReader::new(reader);
    while let Some(raw) = reader.read_message().await? {
        let state = state.clone();
        tokio::spawn(async move { handle_raw(&state, &raw).await });
    }

    // Closing the channel lets the writer task flush what is pending and end.
    drop(state);
    let _ = writer_task.await;
    Ok(())
}

/// Parses a raw message and answers whatever it calls for.
async fn handle_raw(state: &ServerState, raw: &str) {
    let incoming: Incoming = match serde_json::from_str(raw) {
        Ok(incoming) => incoming,
        Err(error) => {
            // With no parseable `id`, JSON-RPC requires answering `id: null`.
            state.send(&Response::failure(
                serde_json::Value::Null,
                RpcError::parse_error(error),
            ));
            return;
        }
    };

    let Some(id) = incoming.id.clone() else {
        // A notification from the client: by contract it gets no answer. None
        // is defined today, so it is ignored rather than inventing an error.
        return;
    };

    if incoming.jsonrpc != JSONRPC_VERSION {
        state.send(&Response::failure(
            id,
            RpcError::invalid_request(format!(
                "expected jsonrpc `{JSONRPC_VERSION}`, got `{}`",
                incoming.jsonrpc
            )),
        ));
        return;
    }

    let response = match dispatch(state, &incoming).await {
        Ok(result) => Response::success(id, result),
        Err(error) => Response::failure(id, error),
    };

    state.send(&response);
}

/// Routes the request to the matching use case.
async fn dispatch(state: &ServerState, incoming: &Incoming) -> Result<serde_json::Value, RpcError> {
    match incoming.method.as_str() {
        "workspace/collections" => {
            let collections = state
                .engine
                .list_requests()
                .collections()
                .await
                .map_err(RpcError::application)?;
            to_value(&serde_json::json!({ "collections": collections }))
        }
        "workspace/environments" => {
            let environments = state
                .engine
                .list_requests()
                .environments()
                .await
                .map_err(RpcError::application)?;
            to_value(&serde_json::json!({ "environments": environments }))
        }
        "request/preview" => preview(state, parse_params(incoming)?).await,
        "request/send" => send(state, parse_params(incoming)?).await,
        "request/cancel" => Ok(cancel(state, parse_params(incoming)?)),
        "history/query" => history(state, parse_optional_params(incoming)?).await,
        "history/detail" => history_detail(state, parse_params(incoming)?).await,
        unknown => Err(RpcError::method_not_found(unknown)),
    }
}

/// Deserializes the parameters of a method that requires them.
fn parse_params<T: serde::de::DeserializeOwned>(incoming: &Incoming) -> Result<T, RpcError> {
    match &incoming.params {
        None | Some(serde_json::Value::Null) => Err(RpcError::invalid_params(format!(
            "`{}` requires parameters",
            incoming.method
        ))),
        Some(value) => serde_json::from_value(value.clone()).map_err(RpcError::invalid_params),
    }
}

/// Deserializes the parameters of a method where all of them are optional.
///
/// Omitting `params` is the same as sending them all empty, which is what
/// whoever calls `history/query` without filters expects.
fn parse_optional_params<T: serde::de::DeserializeOwned + Default>(
    incoming: &Incoming,
) -> Result<T, RpcError> {
    match &incoming.params {
        None | Some(serde_json::Value::Null) => Ok(T::default()),
        Some(value) => serde_json::from_value(value.clone()).map_err(RpcError::invalid_params),
    }
}

/// Serializes an already built value, turning a failure into an internal error.
fn to_value(value: &serde_json::Value) -> Result<serde_json::Value, RpcError> {
    serde_json::to_value(value).map_err(RpcError::internal)
}

/// Turns the protocol's parameters into the use case's input.
fn to_input(params: &RequestParams) -> SendRequestInput {
    SendRequestInput::new(request_id(&params.request_id))
        .with_environment(params.environment.clone())
        .with_overrides(params.variables.clone())
        .with_insecure_tls(params.insecure_tls)
}

/// `request/preview`: resolves without sending and explains the variables applied.
async fn preview(
    state: &ServerState,
    params: RequestParams,
) -> Result<serde_json::Value, RpcError> {
    let prepared = state
        .engine
        .preview_request()
        .execute(to_input(&params))
        .await
        .map_err(RpcError::application)?;

    // Each variable's origin is returned so the client can answer "where does
    // this value come from?" without replicating precedence.
    let variables: Vec<ResolvedVariable> = prepared
        .context
        .names()
        .into_iter()
        .filter_map(|name| {
            let (origin, value) = prepared.context.lookup(name)?;
            Some(ResolvedVariable {
                name: name.to_owned(),
                value: value.to_owned(),
                origin: origin.to_owned(),
            })
        })
        .collect();

    let request = serde_json::to_value(&prepared.request).map_err(RpcError::internal)?;
    let variables = serde_json::to_value(&variables).map_err(RpcError::internal)?;

    Ok(serde_json::json!({ "request": request, "variables": variables }))
}

/// `request/send`: starts the execution and returns its identifier.
///
/// Resolution (reading the workspace, interpolating) happens before answering:
/// if it fails, the client gets an immediate error instead of an `executionId`
/// whose only use would be receiving a failure event an instant later.
async fn send(state: &ServerState, params: RequestParams) -> Result<serde_json::Value, RpcError> {
    let mut events = state
        .engine
        .send_request()
        .execute(to_input(&params))
        .await
        .map_err(RpcError::application)?;

    let execution_id = state.allocate_execution_id();
    let task_state = state.clone();
    let task_id = execution_id.clone();

    let handle = tokio::spawn(async move {
        while let Some(event) = events.next().await {
            task_state.send(&Notification::new(
                EXECUTION_EVENT,
                ExecutionEventParams {
                    execution_id: task_id.clone(),
                    event,
                },
            ));
        }
        forget_execution(&task_state.executions, &task_id);
    });

    remember_execution(
        &state.executions,
        execution_id.clone(),
        handle.abort_handle(),
    );

    Ok(serde_json::json!({ "executionId": execution_id }))
}

/// `request/cancel`: aborts an execution in flight.
///
/// After aborting, a failure `execution/event` is emitted from here: the
/// aborted task can no longer emit anything, and the client needs a terminal
/// event to close whatever spinner it has drawn.
/// Cancelling something that already finished is not an error: it is a normal
/// race between the client and the response, so it is reported with
/// `cancelled: false`.
fn cancel(state: &ServerState, params: CancelParams) -> serde_json::Value {
    let Some(handle) = take_execution(&state.executions, &params.execution_id) else {
        return serde_json::json!({ "cancelled": false });
    };

    handle.abort();
    state.send(&Notification::new(
        EXECUTION_EVENT,
        ExecutionEventParams {
            execution_id: params.execution_id,
            event: ExecutionEvent::Failed {
                message: "execution cancelled by the client".to_owned(),
            },
        },
    ));

    serde_json::json!({ "cancelled": true })
}

/// `history/query`: queries the execution index.
async fn history(
    state: &ServerState,
    params: HistoryParams,
) -> Result<serde_json::Value, RpcError> {
    let defaults = HistoryQuery::default();
    let query = HistoryQuery {
        request_id: params.request_id.as_deref().map(request_id),
        status: params.status,
        limit: params.limit.unwrap_or(defaults.limit),
    };

    let entries = state
        .engine
        .history(&query)
        .await
        .map_err(RpcError::application)?;

    let entries = serde_json::to_value(&entries).map_err(RpcError::internal)?;
    Ok(serde_json::json!({ "entries": entries }))
}

/// Returns the detail of a stored execution.
///
/// Its existing here and not only in the CLI is the usual criterion: if a Lua
/// client had to query the SQLite database on its own to show an old
/// response, that logic would be in the wrong layer.
async fn history_detail(
    state: &ServerState,
    params: HistoryDetailParams,
) -> Result<serde_json::Value, RpcError> {
    let detail = state
        .engine
        .history_detail(params.id)
        .await
        .map_err(RpcError::application)?;

    // "Does not exist" is a result, not an error: an identifier already
    // pruned from the index is normal, not a badly made call.
    let detail = serde_json::to_value(&detail).map_err(RpcError::internal)?;
    Ok(serde_json::json!({ "detail": detail }))
}

/// Registers an execution in flight.
///
/// If the mutex is poisoned it carries on without registering: losing the
/// ability to cancel is preferable to bringing the server down.
fn remember_execution(executions: &Executions, id: String, handle: AbortHandle) {
    if let Ok(mut guard) = executions.lock() {
        guard.insert(id, handle);
    }
}

/// Deregisters a finished execution.
fn forget_execution(executions: &Executions, id: &str) {
    if let Ok(mut guard) = executions.lock() {
        guard.remove(id);
    }
}

/// Takes and deregisters an execution, if it is still alive.
fn take_execution(executions: &Executions, id: &str) -> Option<AbortHandle> {
    executions.lock().ok()?.remove(id)
}
