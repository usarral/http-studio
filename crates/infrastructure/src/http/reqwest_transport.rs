//! The HTTP transport, over `reqwest`.
//!
//! Translates a domain [`ResolvedRequest`] into a `reqwest` request, runs it
//! and emits [`ExecutionEvent`]s as the response arrives.
//!
//! The streaming is not decoration: it is what lets a TUI draw the headers and
//! a progress bar while the body is still downloading, using exactly the same
//! engine as the CLI, which simply waits for the final event.

use std::time::{Duration, Instant};

use http_studio_application::{ExecutionStream, HttpTransport};
use http_studio_domain::{
    Body, Exchange, ExecutionEvent, Header, ResolvedRequest, ResponseBody, ResponseHead, Timings,
};

/// A reusable HTTP client.
///
/// Built once and shared: a `reqwest::Client` holds the connection pool and the
/// TLS session, so creating one per request would throw keep-alive away.
///
/// There are **two** clients because certificate verification is configured
/// when the client is built, not when it sends. Keeping them apart is exactly
/// what stops `# @insecure` on one request from contaminating the others: a
/// request that does not ask for it still travels over the verifying client.
pub struct ReqwestTransport {
    client: reqwest::Client,
    insecure: reqwest::Client,
}

impl ReqwestTransport {
    /// Creates the transport with a total per-request timeout.
    ///
    /// # Errors
    ///
    /// Returns `reqwest`'s error when the client cannot be built — typically a
    /// misconfigured TLS backend.
    pub fn new(timeout: Duration) -> Result<Self, reqwest::Error> {
        Ok(Self {
            client: base_builder(timeout).build()?,
            insecure: base_builder(timeout)
                .tls_danger_accept_invalid_certs(true)
                .tls_danger_accept_invalid_hostnames(true)
                .build()?,
        })
    }

    /// Picks the client according to what the request asks for.
    fn client_for(&self, request: &ResolvedRequest) -> &reqwest::Client {
        if request.options.insecure_tls {
            &self.insecure
        } else {
            &self.client
        }
    }

    /// Turns the domain request into a `reqwest` one.
    fn build(&self, request: &ResolvedRequest) -> reqwest::RequestBuilder {
        let mut builder = self
            .client_for(request)
            .request(method_of(request), request.url.clone());

        for header in &request.headers {
            builder = builder.header(&header.name, &header.value);
        }

        match &request.body {
            Body::Empty => builder,
            Body::Text { content } | Body::Json { content } => builder.body(content.clone()),
            Body::Form { fields } => {
                // Serialized by hand rather than with `.form()` so the order
                // is not serde's to choose: the declared order matters when
                // comparing against a trace, or when signing the request.
                let encoded = url::form_urlencoded::Serializer::new(String::new())
                    .extend_pairs(fields.iter().map(|(k, v)| (k.as_str(), v.as_str())))
                    .finish();
                builder.body(encoded)
            }
        }
    }
}

impl HttpTransport for ReqwestTransport {
    fn execute(&self, request: ResolvedRequest) -> ExecutionStream {
        let builder = self.build(&request);

        Box::pin(async_stream::stream! {
            let started = Instant::now();

            let mut response = match builder.send().await {
                Ok(response) => response,
                Err(error) => {
                    yield ExecutionEvent::Failed { message: describe(&error) };
                    return;
                }
            };

            let time_to_first_byte = started.elapsed();
            let head = ResponseHead {
                status: response.status().as_u16(),
                version: format!("{:?}", response.version()),
                headers: response
                    .headers()
                    .iter()
                    .map(|(name, value)| {
                        Header::new(name.as_str(), value.to_str().unwrap_or("<binario>"))
                    })
                    .collect(),
            };
            yield ExecutionEvent::Head { head: Box::new(head.clone()) };

            let mut bytes = Vec::new();
            loop {
                match response.chunk().await {
                    Ok(Some(chunk)) => {
                        yield ExecutionEvent::BodyChunk { len: chunk.len() };
                        bytes.extend_from_slice(&chunk);
                    }
                    Ok(None) => break,
                    Err(error) => {
                        yield ExecutionEvent::Failed { message: describe(&error) };
                        return;
                    }
                }
            }

            yield ExecutionEvent::Completed {
                exchange: Box::new(Exchange {
                    request,
                    head,
                    body: ResponseBody { bytes },
                    timings: Timings {
                        time_to_first_byte,
                        total: started.elapsed(),
                    },
                }),
            };
        })
    }
}

/// The settings both clients share.
///
/// Everything that is not certificate policy belongs here, so the insecure
/// client differs from the normal one in **that alone**.
fn base_builder(timeout: Duration) -> reqwest::ClientBuilder {
    reqwest::Client::builder()
        .timeout(timeout)
        .user_agent(concat!("http-studio/", env!("CARGO_PKG_VERSION")))
}

/// Translates the domain method into `reqwest`'s.
fn method_of(request: &ResolvedRequest) -> reqwest::Method {
    use http_studio_domain::HttpMethod;

    match request.method {
        HttpMethod::Get => reqwest::Method::GET,
        HttpMethod::Post => reqwest::Method::POST,
        HttpMethod::Put => reqwest::Method::PUT,
        HttpMethod::Patch => reqwest::Method::PATCH,
        HttpMethod::Delete => reqwest::Method::DELETE,
        HttpMethod::Head => reqwest::Method::HEAD,
        HttpMethod::Options => reqwest::Method::OPTIONS,
    }
}

/// Turns a `reqwest` error into a message someone can act on.
///
/// A raw `reqwest::Error` usually says no more than "error sending request";
/// the real cause — DNS, a certificate, a timeout — is down the `source` chain,
/// so the whole chain is walked.
fn describe(error: &reqwest::Error) -> String {
    let mut message = if error.is_timeout() {
        "timeout".to_owned()
    } else if error.is_connect() {
        "could not connect".to_owned()
    } else {
        error.to_string()
    };

    let mut source = std::error::Error::source(error);
    while let Some(cause) = source {
        message.push_str(": ");
        message.push_str(&cause.to_string());
        source = cause.source();
    }

    // A bare `UnknownIssuer` tells nobody what to do next. The way out is
    // always the same, so the message names it.
    if looks_like_tls_failure(&message) {
        message.push_str(
            "\n  (untrusted certificate: add `# @insecure` to the block, \
             or run with `--insecure`)",
        );
    }

    message
}

/// Spots a certificate validation failure in the already assembled message.
///
/// The text is inspected because `reqwest` hides `rustls`'s error behind an
/// opaque `Box<dyn Error>`: there is no concrete type to match on.
fn looks_like_tls_failure(message: &str) -> bool {
    const MARKERS: [&str; 5] = [
        "UnknownIssuer",
        "InvalidCertificate",
        "CertNotValidForName",
        "certificate",
        "self-signed",
    ];

    MARKERS.iter().any(|marker| message.contains(marker))
}
