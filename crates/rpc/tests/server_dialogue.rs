//! A whole dialogue against the JSON-RPC server using fake adapters.
//!
//! That these doubles fit in a short file is itself the proof that the ports
//! are the right size: if half of `reqwest` had to be simulated to exercise
//! the server, the boundary would be in the wrong place.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use http_studio_application::{
    ApplicationError, CollectionRepository, DynamicSource, Engine, EnginePorts,
    EnvironmentRepository, ExchangeRecorder, ExecutionStream, HistoryMaintenance, HistoryQuery,
    HistoryReader, HttpTransport, LocatedRequest, PruneScope, RequestSource, SecretProvider,
};
use http_studio_domain::{
    Body, Collection, DynamicSeed, Environment, Exchange, ExecutionEvent, Header, HistoryDetail,
    HistoryEntry, HistoryStats, HttpMethod, RequestDefinition, RequestId, RequestOptions,
    ResolvedRequest, ResponseBody, ResponseHead, Timings,
};

// --- Test doubles --------------------------------------------------------

/// An in-memory workspace with one collection holding one request.
struct FakeWorkspace;

impl FakeWorkspace {
    fn collection() -> Collection {
        Collection {
            name: "demo".to_owned(),
            variables: BTreeMap::from([("accept".to_owned(), "application/json".to_owned())]),
            requests: vec![RequestDefinition {
                id: RequestId::new("demo/ping"),
                name: "Ping".to_owned(),
                method: HttpMethod::Get,
                url: "{{base_url}}/ping".to_owned(),
                headers: vec![
                    Header::new("Accept", "{{accept}}"),
                    Header::new("X-Sent-At", "{{$timestamp}}"),
                ],
                query: vec![],
                body: Body::Empty,
                variables: BTreeMap::new(),
                description: None,
                options: RequestOptions::default(),
            }],
        }
    }
}

#[async_trait]
impl CollectionRepository for FakeWorkspace {
    async fn list_collections(&self) -> Result<Vec<Collection>, ApplicationError> {
        Ok(vec![Self::collection()])
    }

    async fn find_request(
        &self,
        id: &RequestId,
    ) -> Result<Option<LocatedRequest>, ApplicationError> {
        let collection = Self::collection();
        Ok(collection
            .requests
            .iter()
            .find(|request| &request.id == id)
            .map(|definition| LocatedRequest {
                collection_name: collection.name.clone(),
                collection_variables: collection.variables.clone(),
                definition: definition.clone(),
            }))
    }
}

#[async_trait]
impl EnvironmentRepository for FakeWorkspace {
    async fn list_environments(&self) -> Result<Vec<Environment>, ApplicationError> {
        Ok(vec![Environment {
            name: "dev".to_owned(),
            variables: BTreeMap::from([("base_url".to_owned(), "https://api.test".to_owned())]),
        }])
    }

    async fn find_environment(&self, name: &str) -> Result<Option<Environment>, ApplicationError> {
        Ok(self
            .list_environments()
            .await?
            .into_iter()
            .find(|environment| environment.name == name))
    }
}

/// An empty secret provider.
struct NoSecrets;

#[async_trait]
impl SecretProvider for NoSecrets {
    async fn secrets(&self) -> Result<BTreeMap<String, String>, ApplicationError> {
        Ok(BTreeMap::new())
    }
}

/// A transport that answers `200` without touching the network.
///
/// `delay` keeps the execution in flight long enough to exercise
/// `request/cancel` deterministically, without depending on the speed of the
/// machine running the tests.
struct FakeTransport {
    delay: Duration,
}

impl HttpTransport for FakeTransport {
    fn execute(&self, request: ResolvedRequest) -> ExecutionStream {
        let delay = self.delay;

        Box::pin(async_stream::stream! {
            let head = ResponseHead {
                status: 200,
                version: "HTTP/1.1".to_owned(),
                headers: vec![Header::new("content-type", "application/json")],
            };
            yield ExecutionEvent::Head { head: Box::new(head.clone()) };

            tokio::time::sleep(delay).await;

            let body = ResponseBody { bytes: b"{\"pong\":true}".to_vec() };
            yield ExecutionEvent::BodyChunk { len: body.len() };
            yield ExecutionEvent::Completed {
                exchange: Box::new(Exchange {
                    request,
                    head,
                    body,
                    timings: Timings::default(),
                }),
            };
        })
    }
}

/// A recorder that ignores everything.
struct NullRecorder;

#[async_trait]
impl ExchangeRecorder for NullRecorder {
    async fn record(&self, _exchange: &Exchange) -> Result<(), ApplicationError> {
        Ok(())
    }
}

/// A history reader with one fixed row.
struct FakeHistory;

#[async_trait]
impl HistoryReader for FakeHistory {
    async fn query(&self, _query: &HistoryQuery) -> Result<Vec<HistoryEntry>, ApplicationError> {
        Ok(vec![HistoryEntry {
            id: 1,
            request_id: RequestId::new("demo/ping"),
            method: "GET".to_owned(),
            url: "https://api.test/ping".to_owned(),
            status: 200,
            duration_ms: 12,
            response_bytes: 13,
            executed_at: "2026-01-01 00:00:00".to_owned(),
        }])
    }

    async fn detail(&self, id: i64) -> Result<Option<HistoryDetail>, ApplicationError> {
        if id != 1 {
            return Ok(None);
        }

        let entry = self.query(&HistoryQuery::default()).await?.remove(0);
        Ok(Some(HistoryDetail {
            request: ResolvedRequest {
                id: entry.request_id.clone(),
                method: HttpMethod::Get,
                url: "https://api.test/ping".parse().expect("a valid URL"),
                headers: Vec::new(),
                body: Body::Empty,
                options: RequestOptions::default(),
            },
            head: ResponseHead {
                status: 200,
                version: "HTTP/1.1".to_owned(),
                headers: Vec::new(),
            },
            body: ResponseBody {
                bytes: b"pong".to_vec(),
            },
            body_truncated: false,
            entry,
        }))
    }
}

#[async_trait]
impl HistoryMaintenance for FakeHistory {
    async fn stats(&self) -> Result<HistoryStats, ApplicationError> {
        Ok(HistoryStats {
            executions: 1,
            requests: 1,
            ..HistoryStats::default()
        })
    }

    async fn prune(&self, _scope: PruneScope) -> Result<u64, ApplicationError> {
        Ok(0)
    }
}

/// A fixed seed, so a test can assert the exact value of a `{{$timestamp}}`.
struct FixedSeed;

impl DynamicSource for FixedSeed {
    fn seed(&self) -> DynamicSeed {
        DynamicSeed::fixed(1_788_606_912, 7)
    }
}

/// A text source returning one fixed `.http` block.
struct FakeSource;

#[async_trait]
impl RequestSource for FakeSource {
    async fn read(&self, id: &RequestId) -> Result<Option<String>, ApplicationError> {
        Ok((id.as_str() == "demo/ping")
            .then(|| "### Ping\n# @name ping\nGET {{base_url}}/ping".to_owned()))
    }

    async fn save(&self, _id: &RequestId, _source: &str) -> Result<(), ApplicationError> {
        Ok(())
    }
}

/// Assembles the engine out of every double.
fn fake_engine(delay: Duration) -> Engine {
    let workspace = Arc::new(FakeWorkspace);
    Engine::new(EnginePorts {
        collections: workspace.clone(),
        environments: workspace,
        secrets: Arc::new(NoSecrets),
        // A fixed seed: this way a test can assert the exact value of a
        // `{{$timestamp}}` instead of settling for "something like now".
        dynamic: Arc::new(FixedSeed),
        transport: Arc::new(FakeTransport { delay }),
        recorder: Arc::new(NullRecorder),
        history: Arc::new(FakeHistory),
        maintenance: Arc::new(FakeHistory),
        source: Arc::new(FakeSource),
    })
}

// --- Dialogue helpers ----------------------------------------------------

/// A shared buffer standing in for the server's output.
///
/// `serve` spawns the writing in its own task, so it demands a `'static`
/// `writer`: a borrowed `&mut Vec<u8>` will not do. This buffer clones and
/// keeps access to what was written once the dialogue is over.
#[derive(Clone, Default)]
struct SharedBuffer(Arc<std::sync::Mutex<Vec<u8>>>);

impl SharedBuffer {
    fn contents(&self) -> Vec<u8> {
        self.0.lock().map(|guard| guard.clone()).unwrap_or_default()
    }
}

impl tokio::io::AsyncWrite for SharedBuffer {
    fn poll_write(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        if let Ok(mut guard) = self.0.lock() {
            guard.extend_from_slice(buf);
        }
        std::task::Poll::Ready(Ok(buf.len()))
    }

    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::task::Poll::Ready(Ok(()))
    }

    fn poll_shutdown(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::task::Poll::Ready(Ok(()))
    }
}

/// Frames several messages the way a client would send them.
fn frame(messages: &[&str]) -> Vec<u8> {
    let mut out = Vec::new();
    for message in messages {
        out.extend_from_slice(format!("Content-Length: {}\r\n\r\n", message.len()).as_bytes());
        out.extend_from_slice(message.as_bytes());
    }
    out
}

/// Unframes the server's output into JSON objects.
fn unframe(raw: &[u8]) -> Vec<serde_json::Value> {
    let mut out = Vec::new();
    let mut rest = raw;

    while let Some(position) = rest.windows(4).position(|w| w == b"\r\n\r\n") {
        let headers = std::str::from_utf8(&rest[..position]).unwrap();
        let length: usize = headers.split(':').nth(1).unwrap().trim().parse().unwrap();

        let body = &rest[position + 4..position + 4 + length];
        out.push(serde_json::from_slice(body).unwrap());
        rest = &rest[position + 4 + length..];
    }

    out
}

/// Runs a whole dialogue and returns everything the server answered.
async fn dialogue(engine: Engine, messages: &[&str]) -> Vec<serde_json::Value> {
    let input = frame(messages);
    let output = SharedBuffer::default();

    http_studio_rpc::serve(engine, input.as_slice(), output.clone())
        .await
        .unwrap();

    unframe(&output.contents())
}

/// Finds the response matching a request identifier.
fn response_for(messages: &[serde_json::Value], id: u64) -> &serde_json::Value {
    messages
        .iter()
        .find(|message| message.get("id").and_then(serde_json::Value::as_u64) == Some(id))
        .expect("there must be a response for that id")
}

/// Collects the `execution/event` notifications emitted.
fn events(messages: &[serde_json::Value]) -> Vec<&serde_json::Value> {
    messages
        .iter()
        .filter(|message| {
            message.get("method").and_then(serde_json::Value::as_str) == Some("execution/event")
        })
        .collect()
}

// --- Tests ---------------------------------------------------------------

#[tokio::test]
async fn lists_the_collections() {
    let responses = dialogue(
        fake_engine(Duration::ZERO),
        &[r#"{"jsonrpc":"2.0","id":1,"method":"workspace/collections"}"#],
    )
    .await;

    let result = &response_for(&responses, 1)["result"]["collections"];
    assert_eq!(result[0]["name"], "demo");
}

#[tokio::test]
async fn lists_the_environments() {
    let responses = dialogue(
        fake_engine(Duration::ZERO),
        &[r#"{"jsonrpc":"2.0","id":1,"method":"workspace/environments"}"#],
    )
    .await;

    let result = &response_for(&responses, 1)["result"]["environments"];
    assert_eq!(result[0]["name"], "dev");
}

#[tokio::test]
async fn the_preview_resolves_the_variables_and_explains_their_origin() {
    let responses = dialogue(
        fake_engine(Duration::ZERO),
        &[
            r#"{"jsonrpc":"2.0","id":1,"method":"request/preview","params":{"requestId":"demo/ping","environment":"dev"}}"#,
        ],
    )
    .await;

    let result = &response_for(&responses, 1)["result"];
    assert_eq!(result["request"]["url"], "https://api.test/ping");

    let origin = result["variables"]
        .as_array()
        .unwrap()
        .iter()
        .find(|variable| variable["name"] == "base_url")
        .unwrap()["origin"]
        .as_str()
        .unwrap()
        .to_owned();

    assert_eq!(origin, "environment:dev");
}

#[tokio::test]
async fn the_preview_executes_nothing() {
    let responses = dialogue(
        fake_engine(Duration::ZERO),
        &[
            r#"{"jsonrpc":"2.0","id":1,"method":"request/preview","params":{"requestId":"demo/ping","environment":"dev"}}"#,
        ],
    )
    .await;

    assert!(events(&responses).is_empty());
}

#[tokio::test]
async fn sending_returns_an_id_and_emits_the_event_sequence() {
    let responses = dialogue(
        fake_engine(Duration::ZERO),
        &[
            r#"{"jsonrpc":"2.0","id":1,"method":"request/send","params":{"requestId":"demo/ping","environment":"dev"}}"#,
        ],
    )
    .await;

    let execution_id = response_for(&responses, 1)["result"]["executionId"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(execution_id, "exec-1");

    let names: Vec<&str> = events(&responses)
        .iter()
        .map(|event| event["params"]["event"].as_str().unwrap())
        .collect();
    assert_eq!(names, vec!["resolved", "head", "body_chunk", "completed"]);

    for event in events(&responses) {
        assert_eq!(event["params"]["executionId"], "exec-1");
    }
}

#[tokio::test]
async fn the_variable_overrides_reach_the_engine() {
    let responses = dialogue(
        fake_engine(Duration::ZERO),
        &[
            r#"{"jsonrpc":"2.0","id":1,"method":"request/preview","params":{"requestId":"demo/ping","environment":"dev","variables":{"base_url":"https://forced.test"}}}"#,
        ],
    )
    .await;

    let result = &response_for(&responses, 1)["result"];
    assert_eq!(result["request"]["url"], "https://forced.test/ping");
}

#[tokio::test]
async fn a_request_that_does_not_exist_is_an_application_error() {
    let responses = dialogue(
        fake_engine(Duration::ZERO),
        &[r#"{"jsonrpc":"2.0","id":1,"method":"request/send","params":{"requestId":"does/not-exist"}}"#],
    )
    .await;

    assert_eq!(response_for(&responses, 1)["error"]["code"], -32000);
}

#[tokio::test]
async fn an_environment_that_does_not_exist_is_an_application_error() {
    let responses = dialogue(
        fake_engine(Duration::ZERO),
        &[
            r#"{"jsonrpc":"2.0","id":1,"method":"request/preview","params":{"requestId":"demo/ping","environment":"staging"}}"#,
        ],
    )
    .await;

    assert_eq!(response_for(&responses, 1)["error"]["code"], -32000);
}

#[tokio::test]
async fn queries_the_history() {
    let responses = dialogue(
        fake_engine(Duration::ZERO),
        &[r#"{"jsonrpc":"2.0","id":1,"method":"history/query","params":{"limit":5}}"#],
    )
    .await;

    let entries = &response_for(&responses, 1)["result"]["entries"];
    assert_eq!(entries[0]["request_id"], "demo/ping");
}

#[tokio::test]
async fn history_query_accepts_arriving_without_parameters() {
    let responses = dialogue(
        fake_engine(Duration::ZERO),
        &[r#"{"jsonrpc":"2.0","id":1,"method":"history/query"}"#],
    )
    .await;

    assert!(response_for(&responses, 1)["result"]["entries"].is_array());
}

#[tokio::test]
async fn resolves_the_dynamic_variables_with_the_engines_seed() {
    // The seed being a port is what makes this checkable: with the clock read
    // inside the domain, all that could be asserted here is "a number".
    let responses = dialogue(
        fake_engine(Duration::ZERO),
        &[
            r#"{"jsonrpc":"2.0","id":1,"method":"request/preview","params":{"requestId":"demo/ping","environment":"dev"}}"#,
        ],
    )
    .await;

    let headers = &response_for(&responses, 1)["result"]["request"]["headers"];
    let sent_at = headers
        .as_array()
        .expect("the headers are a list")
        .iter()
        .find(|header| header["name"] == "X-Sent-At")
        .expect("the resolved header must be there");

    assert_eq!(sent_at["value"], "1788606912");
}

#[tokio::test]
async fn queries_the_detail_of_an_execution() {
    let responses = dialogue(
        fake_engine(Duration::ZERO),
        &[r#"{"jsonrpc":"2.0","id":1,"method":"history/detail","params":{"id":1}}"#],
    )
    .await;

    let detail = &response_for(&responses, 1)["result"]["detail"];
    assert_eq!(detail["entry"]["request_id"], "demo/ping");
    assert_eq!(detail["body_truncated"], false);
}

#[tokio::test]
async fn the_detail_of_a_missing_execution_is_null_and_not_an_error() {
    // An identifier already pruned from the index is normal, not a badly made
    // call: the client tells "not there" apart from "something failed".
    let responses = dialogue(
        fake_engine(Duration::ZERO),
        &[r#"{"jsonrpc":"2.0","id":1,"method":"history/detail","params":{"id":99}}"#],
    )
    .await;

    let response = response_for(&responses, 1);
    assert!(response["error"].is_null());
    assert!(response["result"]["detail"].is_null());
}

#[tokio::test]
async fn an_unknown_method_returns_32601() {
    let responses = dialogue(
        fake_engine(Duration::ZERO),
        &[r#"{"jsonrpc":"2.0","id":1,"method":"does/not-exist"}"#],
    )
    .await;

    assert_eq!(response_for(&responses, 1)["error"]["code"], -32601);
}

#[tokio::test]
async fn a_method_requiring_parameters_returns_32602_without_them() {
    let responses = dialogue(
        fake_engine(Duration::ZERO),
        &[r#"{"jsonrpc":"2.0","id":1,"method":"request/send"}"#],
    )
    .await;

    assert_eq!(response_for(&responses, 1)["error"]["code"], -32602);
}

#[tokio::test]
async fn broken_json_returns_32700_with_a_null_id() {
    let responses = dialogue(fake_engine(Duration::ZERO), &["{this is not json"]).await;

    assert_eq!(responses[0]["error"]["code"], -32700);
    assert!(responses[0]["id"].is_null());
}

#[tokio::test]
async fn a_wrong_jsonrpc_version_returns_32600() {
    let responses = dialogue(
        fake_engine(Duration::ZERO),
        &[r#"{"jsonrpc":"1.0","id":1,"method":"workspace/collections"}"#],
    )
    .await;

    assert_eq!(response_for(&responses, 1)["error"]["code"], -32600);
}

#[tokio::test]
async fn a_client_notification_gets_no_answer() {
    let responses = dialogue(
        fake_engine(Duration::ZERO),
        &[r#"{"jsonrpc":"2.0","method":"workspace/collections"}"#],
    )
    .await;

    assert!(responses.is_empty());
}

#[tokio::test]
async fn cancelling_an_unknown_execution_is_not_an_error() {
    let responses = dialogue(
        fake_engine(Duration::ZERO),
        &[r#"{"jsonrpc":"2.0","id":1,"method":"request/cancel","params":{"executionId":"exec-99"}}"#],
    )
    .await;

    assert_eq!(response_for(&responses, 1)["result"]["cancelled"], false);
}

#[tokio::test]
async fn cancelling_an_execution_in_flight_emits_a_terminal_event() {
    // The transport is slow, so the cancellation arrives with the execution alive.
    let responses = dialogue(
        fake_engine(Duration::from_secs(30)),
        &[
            r#"{"jsonrpc":"2.0","id":1,"method":"request/send","params":{"requestId":"demo/ping","environment":"dev"}}"#,
            r#"{"jsonrpc":"2.0","id":2,"method":"request/cancel","params":{"executionId":"exec-1"}}"#,
        ],
    )
    .await;

    assert_eq!(response_for(&responses, 2)["result"]["cancelled"], true);

    let last = events(&responses).last().copied().unwrap().clone();
    assert_eq!(last["params"]["event"], "failed");
    assert!(
        last["params"]["message"]
            .as_str()
            .unwrap()
            .contains("cancelled")
    );
}
