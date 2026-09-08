//! The JSON-RPC 2.0 server for the HTTP Studio engine.
//!
//! It is the project's second *driving adapter*, sibling to the CLI, and it
//! exists for the clients that are not Rust: Neovim, VS Code, or anything that
//! knows how to spawn a child process and talk to it over stdio.
//!
//! # Why stdio and not a socket
//!
//! The same decision LSP made, for the same reasons: the editor already knows
//! how to spawn and supervise child processes, there are no ports to negotiate
//! and no authentication to invent, and the server dies with the editor
//! instead of leaving orphan processes behind.
//!
//! # Shape of the dialogue
//!
//! JSON-RPC 2.0 messages with LSP-style framing:
//!
//! ```text
//! Content-Length: 71\r\n
//! \r\n
//! {"jsonrpc":"2.0","id":1,"method":"workspace/collections"}
//! ```
//!
//! `request/send` replies immediately with an `executionId` and then emits
//! `execution/event` notifications. It is the literal translation of the
//! [`http_studio_domain::ExecutionEvent`] stream onto an untyped channel: the
//! Lua client sees the same sequence of events an in-process Rust client sees,
//! so there are not two behaviours to keep in sync.
//!
//! # Example
//!
//! `serve` accepts any asynchronous read/write pair, not just stdio. That is
//! what makes it possible to exercise a whole dialogue in memory, without
//! spawning processes:
//!
//! ```no_run
//! # async fn example(engine: http_studio_application::Engine) -> std::io::Result<()> {
//! let request = b"Content-Length: 57\r\n\r\n//!                  {\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"workspace/collections\"}";
//!
//! http_studio_rpc::serve(engine, &request[..], Vec::new()).await
//! # }
//! ```
//!
//! The `hts serve` binary invokes it with `tokio::io::stdin()` and
//! `tokio::io::stdout()`.

pub mod codec;
pub mod protocol;
pub mod server;

pub use server::serve;
