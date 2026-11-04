//! Use case: bringing requests in from another format.
//!
//! The format is injected as a [`CollectionImporter`], so this orchestration is
//! the same whether what arrives is a cURL command or an OpenAPI description:
//! read the document, convert it, write it into the workspace. Adding a format
//! adds an importer and nothing else.
//!
//! # Why a remote document goes through the HTTP transport
//!
//! `hts import openapi https://api.example.com/openapi.json` fetches the
//! description with the same [`HttpTransport`] every other request uses, rather
//! than with an HTTP client of its own. That is what makes `--insecure` work on
//! a specification served by an internal host with its own CA — the exact
//! situation where somebody needs to import one — without a second TLS policy
//! existing anywhere in the program.

use std::sync::Arc;

use futures_util::StreamExt;
use http_studio_domain::{Collection, ExecutionEvent, RequestId, RequestOptions, ResolvedRequest};

use crate::error::ApplicationError;
use crate::ports::{
    CollectionImporter, CollectionWriter, DocumentLoader, HttpTransport, ImportPolicy, ImportReport,
};

/// The identifier a fetched document's request carries.
///
/// It never reaches the workspace — nothing is being defined — but the
/// transport and the history both want one, and a named constant is what keeps
/// a row in `hts history` from looking like a request somebody lost.
const DOCUMENT_REQUEST_ID: &str = "import/document";

/// Where the document to import comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImportSource {
    /// The document itself, already in hand.
    ///
    /// It is what a cURL command pasted on a command line is: there is no file
    /// to read, the argument *is* the document.
    Text(String),
    /// A place to read it from: a path on disk, or an `http(s)` URL.
    ///
    /// Which of the two it is, is decided here and not by the caller, so every
    /// interface tells them apart the same way.
    Location(String),
}

/// The use case's input.
#[derive(Debug, Clone)]
pub struct ImportInput {
    /// The document, or where to find it.
    pub source: ImportSource,
    /// Collapse everything into this one collection, ignoring what the format
    /// suggested.
    pub into: Option<String>,
    /// What to do about requests the destination already holds.
    pub policy: ImportPolicy,
    /// Convert and report, without writing anything.
    pub dry_run: bool,
    /// Skip TLS verification while fetching a remote document.
    pub insecure_tls: bool,
}

impl ImportInput {
    /// The minimal input: a document, written wherever the format suggests.
    #[must_use]
    pub fn new(source: ImportSource) -> Self {
        Self {
            source,
            into: None,
            policy: ImportPolicy::Skip,
            dry_run: false,
            insecure_tls: false,
        }
    }

    /// Sets the single destination collection, builder style.
    #[must_use]
    pub fn with_into(mut self, into: Option<String>) -> Self {
        self.into = into;
        self
    }

    /// Sets the conflict policy, builder style.
    #[must_use]
    pub fn with_policy(mut self, policy: ImportPolicy) -> Self {
        self.policy = policy;
        self
    }

    /// Converts without writing, builder style.
    #[must_use]
    pub fn with_dry_run(mut self, dry_run: bool) -> Self {
        self.dry_run = dry_run;
        self
    }

    /// Skips TLS verification for a remote document, builder style.
    #[must_use]
    pub fn with_insecure_tls(mut self, insecure_tls: bool) -> Self {
        self.insecure_tls = insecure_tls;
        self
    }
}

/// What an import produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportOutput {
    /// The collections as the importer built them, before being written.
    ///
    /// They are returned even when something *was* written: a dry run needs
    /// them to show what would happen, and a UI that has just imported wants
    /// to show what arrived without reading the workspace again.
    pub collections: Vec<Collection>,
    /// What was written, one report per collection. Empty on a dry run.
    pub reports: Vec<ImportReport>,
}

/// Reads a document in a foreign format and writes it into the workspace.
pub struct ImportCollections {
    importer: Arc<dyn CollectionImporter>,
    writer: Arc<dyn CollectionWriter>,
    documents: Arc<dyn DocumentLoader>,
    transport: Arc<dyn HttpTransport>,
}

impl ImportCollections {
    /// Injects the ports it needs, the importer among them.
    ///
    /// The importer is a constructor parameter and not an [`crate::Engine`]
    /// field because it is the one dependency that changes per invocation: the
    /// engine stays unaware of which formats exist.
    #[must_use]
    pub fn new(
        importer: Arc<dyn CollectionImporter>,
        writer: Arc<dyn CollectionWriter>,
        documents: Arc<dyn DocumentLoader>,
        transport: Arc<dyn HttpTransport>,
    ) -> Self {
        Self {
            importer,
            writer,
            documents,
            transport,
        }
    }

    /// Imports the document and returns what was converted and what was written.
    ///
    /// # Errors
    ///
    /// - [`ApplicationError::Repository`] when the document cannot be read, or
    ///   the destination cannot be written.
    /// - [`ApplicationError::Transport`] when a remote document cannot be
    ///   fetched.
    /// - [`ApplicationError::Import`] when the document is not the format it
    ///   was taken for.
    pub async fn execute(&self, input: ImportInput) -> Result<ImportOutput, ApplicationError> {
        let document = self.read(&input).await?;
        let mut collections = self.importer.import(&document)?;

        if collections.is_empty() {
            return Err(ApplicationError::Import {
                format: "document".to_owned(),
                message: "it holds no requests".to_owned(),
            });
        }

        if let Some(into) = &input.into {
            collections = vec![merge(collections, into)];
        }

        // A dry run stops here on purpose: it is the same conversion, minus
        // the write. Anything it did differently would make it a worse preview
        // than no preview.
        if input.dry_run {
            return Ok(ImportOutput {
                collections,
                reports: Vec::new(),
            });
        }

        let mut reports = Vec::with_capacity(collections.len());
        for collection in &collections {
            reports.push(self.writer.write(collection, input.policy).await?);
        }

        Ok(ImportOutput {
            collections,
            reports,
        })
    }

    /// Gets the document's text, from wherever it lives.
    async fn read(&self, input: &ImportInput) -> Result<String, ApplicationError> {
        match &input.source {
            ImportSource::Text(document) => Ok(document.clone()),
            ImportSource::Location(location) if is_remote(location) => {
                self.fetch(location, input.insecure_tls).await
            }
            ImportSource::Location(path) => self.documents.load(path).await,
        }
    }

    /// Downloads a remote document with the engine's own transport.
    async fn fetch(&self, url: &str, insecure_tls: bool) -> Result<String, ApplicationError> {
        let request = ResolvedRequest::get(
            RequestId::new(DOCUMENT_REQUEST_ID),
            url,
            RequestOptions { insecure_tls },
        )?;

        let mut events = self.transport.execute(request);
        while let Some(event) = events.next().await {
            match event {
                ExecutionEvent::Completed { exchange } => {
                    if !exchange.head.is_success() {
                        return Err(ApplicationError::transport(format!(
                            "{url} answered {}",
                            exchange.head.status
                        )));
                    }
                    return Ok(exchange.body.as_text().into_owned());
                }
                ExecutionEvent::Failed { message } => {
                    return Err(ApplicationError::transport(message));
                }
                _ => {}
            }
        }

        Err(ApplicationError::transport(format!(
            "{url} closed the connection before answering"
        )))
    }
}

/// `true` when the location is a URL to fetch rather than a path to read.
///
/// Only `http` and `https` count: a Windows path (`C:\specs\api.yaml`) holds a
/// colon too, and `file://` is a path written the long way round.
fn is_remote(location: &str) -> bool {
    let lowercase = location.trim_start().to_ascii_lowercase();
    lowercase.starts_with("http://") || lowercase.starts_with("https://")
}

/// Collapses several collections into one, named `into`.
///
/// The requests keep their order and get re-homed, so their identifiers name
/// the collection they actually ended up in.
fn merge(collections: Vec<Collection>, into: &str) -> Collection {
    let mut merged = Collection {
        name: into.to_owned(),
        variables: std::collections::BTreeMap::new(),
        requests: Vec::new(),
    };

    for collection in collections {
        // First definition wins: the collections an importer produces out of
        // one document share their variables, so a later `base_url` is the
        // same value, and where it is not, the earlier one came from the
        // earlier part of the document.
        for (name, value) in collection.variables {
            merged.variables.entry(name).or_insert(value);
        }

        for mut request in collection.requests {
            request.id = request.id.in_collection(into);
            merged.requests.push(request);
        }
    }

    merged
}

#[cfg(test)]
mod tests {
    // In tests, `unwrap` documents the expectation and its panic IS the failure.
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use std::collections::BTreeMap;
    use std::sync::Mutex;

    use async_trait::async_trait;
    use http_studio_domain::{
        Body, Exchange, HttpMethod, RequestDefinition, ResponseBody, ResponseHead, Timings,
    };

    use super::*;
    use crate::ports::{ImportOutcome, ImportedRequest};

    /// An importer that hands back whatever it was built with.
    struct FakeImporter(Vec<Collection>);

    impl CollectionImporter for FakeImporter {
        fn import(&self, document: &str) -> Result<Vec<Collection>, ApplicationError> {
            if document.is_empty() {
                return Err(ApplicationError::import("fake", "the document is empty"));
            }
            Ok(self.0.clone())
        }
    }

    /// A writer that records what it was asked to write.
    #[derive(Default)]
    struct FakeWriter {
        written: Mutex<Vec<Collection>>,
    }

    #[async_trait]
    impl CollectionWriter for FakeWriter {
        async fn write(
            &self,
            collection: &Collection,
            _policy: ImportPolicy,
        ) -> Result<ImportReport, ApplicationError> {
            self.written.lock().unwrap().push(collection.clone());

            Ok(ImportReport {
                collection: collection.name.clone(),
                location: format!("collections/{}.http", collection.name),
                requests: collection
                    .requests
                    .iter()
                    .map(|request| ImportedRequest {
                        id: request.id.clone(),
                        outcome: ImportOutcome::Created,
                    })
                    .collect(),
            })
        }
    }

    /// A loader with one document at one path.
    struct FakeLoader(&'static str);

    #[async_trait]
    impl DocumentLoader for FakeLoader {
        async fn load(&self, path: &str) -> Result<String, ApplicationError> {
            if path == "spec.yaml" {
                Ok(self.0.to_owned())
            } else {
                Err(ApplicationError::repository(format!("{path}: not found")))
            }
        }
    }

    /// A transport answering every request with the same body.
    struct FakeTransport {
        status: u16,
        body: &'static str,
        seen: Mutex<Vec<ResolvedRequest>>,
    }

    impl FakeTransport {
        fn new(status: u16, body: &'static str) -> Self {
            Self {
                status,
                body,
                seen: Mutex::new(Vec::new()),
            }
        }
    }

    impl HttpTransport for FakeTransport {
        fn execute(&self, request: ResolvedRequest) -> crate::ports::ExecutionStream {
            self.seen.lock().unwrap().push(request.clone());

            let exchange = Exchange {
                request,
                head: ResponseHead {
                    status: self.status,
                    version: "HTTP/1.1".to_owned(),
                    headers: Vec::new(),
                },
                body: ResponseBody {
                    bytes: self.body.as_bytes().to_vec(),
                },
                timings: Timings::default(),
            };

            Box::pin(async_stream::stream! {
                yield ExecutionEvent::Completed { exchange: Box::new(exchange) };
            })
        }
    }

    fn request(id: &str, name: &str) -> RequestDefinition {
        RequestDefinition {
            id: RequestId::new(id),
            name: name.to_owned(),
            method: HttpMethod::Get,
            url: "https://api.example.com/x".to_owned(),
            headers: Vec::new(),
            query: Vec::new(),
            body: Body::Empty,
            variables: BTreeMap::new(),
            description: None,
            options: RequestOptions::default(),
        }
    }

    fn collection(name: &str, requests: Vec<RequestDefinition>) -> Collection {
        Collection {
            name: name.to_owned(),
            variables: BTreeMap::from([(
                "base_url".to_owned(),
                "https://api.example.com".to_owned(),
            )]),
            requests,
        }
    }

    /// The use case with a fake for every port, and the writer kept to inspect.
    fn use_case(
        collections: Vec<Collection>,
    ) -> (ImportCollections, Arc<FakeWriter>, Arc<FakeTransport>) {
        let writer = Arc::new(FakeWriter::default());
        let transport = Arc::new(FakeTransport::new(200, "remote document"));
        let import = ImportCollections::new(
            Arc::new(FakeImporter(collections)),
            writer.clone(),
            Arc::new(FakeLoader("local document")),
            transport.clone(),
        );

        (import, writer, transport)
    }

    #[tokio::test]
    async fn a_pasted_document_is_imported_without_reading_anything() {
        let (import, writer, _) = use_case(vec![collection(
            "imported",
            vec![request("imported/x", "x")],
        )]);

        let output = import
            .execute(ImportInput::new(ImportSource::Text("curl …".to_owned())))
            .await
            .unwrap();

        assert_eq!(output.reports.len(), 1);
        assert_eq!(writer.written.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn a_path_is_read_through_the_document_loader() {
        let (import, _, transport) =
            use_case(vec![collection("pets", vec![request("pets/x", "x")])]);

        import
            .execute(ImportInput::new(ImportSource::Location(
                "spec.yaml".to_owned(),
            )))
            .await
            .unwrap();

        assert!(
            transport.seen.lock().unwrap().is_empty(),
            "a local path must not reach the network"
        );
    }

    #[tokio::test]
    async fn a_url_is_fetched_with_the_engine_transport() {
        let (import, _, transport) =
            use_case(vec![collection("pets", vec![request("pets/x", "x")])]);

        import
            .execute(
                ImportInput::new(ImportSource::Location(
                    "https://api.example.com/openapi.json".to_owned(),
                ))
                .with_insecure_tls(true),
            )
            .await
            .unwrap();

        let seen = transport.seen.lock().unwrap();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].url.as_str(), "https://api.example.com/openapi.json");
        assert!(
            seen[0].options.insecure_tls,
            "`--insecure` has to reach the fetch, which is the case it exists for"
        );
    }

    #[tokio::test]
    async fn a_remote_document_that_answers_with_an_error_is_not_imported() {
        let writer = Arc::new(FakeWriter::default());
        let import = ImportCollections::new(
            Arc::new(FakeImporter(vec![collection("pets", vec![])])),
            writer.clone(),
            Arc::new(FakeLoader("")),
            Arc::new(FakeTransport::new(404, "not found")),
        );

        let error = import
            .execute(ImportInput::new(ImportSource::Location(
                "https://api.example.com/openapi.json".to_owned(),
            )))
            .await
            .unwrap_err();

        assert!(error.to_string().contains("404"), "{error}");
        assert!(writer.written.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_dry_run_converts_but_writes_nothing() {
        let (import, writer, _) = use_case(vec![collection("pets", vec![request("pets/x", "x")])]);

        let output = import
            .execute(ImportInput::new(ImportSource::Text("curl …".to_owned())).with_dry_run(true))
            .await
            .unwrap();

        assert_eq!(output.collections.len(), 1);
        assert!(output.reports.is_empty());
        assert!(writer.written.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn into_collapses_every_collection_into_one() {
        let (import, _, _) = use_case(vec![
            collection("pets", vec![request("pets/list", "list")]),
            collection("store", vec![request("store/order", "order")]),
        ]);

        let output = import
            .execute(
                ImportInput::new(ImportSource::Text("spec".to_owned()))
                    .with_into(Some("api".to_owned())),
            )
            .await
            .unwrap();

        assert_eq!(output.collections.len(), 1);
        let merged = &output.collections[0];
        assert_eq!(merged.name, "api");
        assert_eq!(merged.requests.len(), 2);

        // The identifiers have to name where the requests ended up, or
        // `hts run` would be told a collection that does not exist.
        let ids: Vec<&str> = merged
            .requests
            .iter()
            .map(|request| request.id.as_str())
            .collect();
        assert_eq!(ids, ["api/list", "api/order"]);
    }

    #[tokio::test]
    async fn a_document_with_no_requests_is_an_error_rather_than_a_silent_success() {
        let (import, _, _) = use_case(vec![]);

        let error = import
            .execute(ImportInput::new(ImportSource::Text("curl …".to_owned())))
            .await
            .unwrap_err();

        assert!(error.to_string().contains("no requests"), "{error}");
    }

    #[tokio::test]
    async fn the_importer_error_travels_untouched() {
        let (import, _, _) = use_case(vec![collection("pets", vec![])]);

        let error = import
            .execute(ImportInput::new(ImportSource::Text(String::new())))
            .await
            .unwrap_err();

        assert!(matches!(error, ApplicationError::Import { .. }), "{error}");
    }
}
