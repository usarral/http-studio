# Architecture

## The problem it solves

Desktop HTTP clients put the engine inside the UI. When you want a second
interface — a TUI, a Neovim plugin, a CI runner — you have to reimplement
variable resolution, environment precedence and error handling. They drift
apart, and the `curl` you copy out of the GUI is not the one the GUI sends.

HTTP Studio inverts the relationship: **the engine is the product, the UIs are
its clients**. The test is explicit and checkable: if a new UI needs logic that
isn't in the engine, that logic was in the wrong layer.

## Layers

```
        ┌──────────────────────────────────────────────────┐
        │  cli · tui · nvim · tauri        (driving)        │
        └───────────────────────┬──────────────────────────┘
                                │ uses use cases
        ┌───────────────────────▼──────────────────────────┐
        │  application    use cases + ports (traits)        │
        └───────────────────────┬──────────────────────────┘
                                │ uses entities
        ┌───────────────────────▼──────────────────────────┐
        │  domain         entities + pure rules             │
        └───────────────────────▲──────────────────────────┘
                                │ implements ports
        ┌───────────────────────┴──────────────────────────┐
        │  infrastructure  reqwest · files · SQLite         │
        └──────────────────────────────────────────────────┘
```

The **dependency rule** is that the arrows always point inward. `domain` imports
nobody; `infrastructure` depends on `application` only to implement its traits.
You check it by reading each `Cargo.toml`'s `[dependencies]` — nobody's
discipline has to be taken on trust.

### `crates/domain`

Entities ([`model`]), the variable resolver ([`variables`]) and the conversion
from a definition to a sendable request ([`resolver`]). Zero I/O, zero `async`.
It holds the logic that gets tested most, because it is the logic that hurts
most when it is wrong.

The split between `RequestDefinition` (with `{{placeholders}}`) and
`ResolvedRequest` (already interpolated, with a validated `url::Url`) is
guaranteed by the type system: there is no way to send something unresolved.

### `crates/application`

Ports and use cases. The ports (`CollectionRepository`, `HttpTransport`,
`ExchangeRecorder`, `SecretProvider`, `EnvironmentRepository`, `DynamicSource`,
`CollectionImporter`, `CollectionWriter`, `DocumentLoader`) describe capabilities
in the language of the domain, not in technologies.

The key detail: **a use case returns a stream of `ExecutionEvent`**, not a
blocking `Result`.

```rust
pub enum ExecutionEvent {
    Resolved { request: Box<ResolvedRequest> },
    Head { head: Box<ResponseHead> },
    BodyChunk { len: usize },
    Completed { exchange: Box<Exchange> },
    Failed { message: String },
}
```

A TUI draws progress by consuming `BodyChunk`; the CLI ignores those and waits
for `Completed`. Same engine, same sequence, different presentation. It is what
makes adding an interface presentation work rather than engine work.

### `crates/infrastructure`

The adapters: `ReqwestTransport`, `FileSystemWorkspace`, `SqliteHistory`,
`NullHistory`, `EnvSecretProvider`, `SystemDynamicSource`, `LocalDocuments`,
`CurlImporter`, `OpenApiImporter`. None of their dependencies show up in
`application`'s signatures.

The `.http` parser (`storage::http_file`) turns text into domain entities, with
the conversion explicit and confined to one module. That isolation was tested
for real: **migrating the workspace from a homegrown YAML to standard `.http`
files did not touch a single line of `domain` or `application`.** One
infrastructure module changed, and the example files. It is exactly what the
dependency rule promises, measured against a real change rather than a diagram.

Its mirror image (`storage::http_writer`) turns entities back into text, and is
what importing is built on. The round trip is the contract: everything written
is parsed again before it reaches the disk, so no import can leave a workspace
the engine refuses to load.

The importers ([`importing.md`](importing.md)) are pure — text in, collections
out, no disk and no network — because where a document came from is the use
case's business. That is what lets one importer serve a path, a URL and a string
pasted on a command line.

### `crates/rpc`

The second driving adapter: a JSON-RPC 2.0 server over stdio with LSP-style
framing, for clients that are not Rust. It translates the `ExecutionEvent`
stream into `execution/event` notifications, so a plugin written in Lua sees the
same sequence as a client linked in process.

That it fits in three modules — codec, protocol, dispatch — and that its tests
use fake adapters a couple of dozen lines long is the measure of whether the
ports are the right size.

### `crates/tui`

The third driving adapter. It splits in four so that whatever deserves a test
can have one:

| Module | What it is | Testable without a terminal? |
|---|---|---|
| `keymap` | the vim grammar | yes, it is pure |
| `app` | state and transitions | yes: it emits commands, it doesn't run them |
| `ui` | drawing with ratatui | only its clipping functions |
| `runner` | loop, terminal, engine | no, which is why it is thin |

`app` never calls the engine: it returns a `Command` the `runner` carries out.
That is what lets an `assert_eq!` check that `3j` moves three rows, or that `:w`
with no changes writes nothing, without simulating a terminal.

The keyboard is read on its own thread because `crossterm::event::read` blocks:
leaving it in an async task would freeze the executor, and with it the events of
the request in flight — which are exactly what you want to watch move.

### `crates/cli`

The composition root. `composition.rs` is the only place in the project where
`ReqwestTransport::new(...)` is written. A future GUI will have its own
equivalent module built from the same pieces.

## Decisions and why

| Decision | Reason | Cost we accept |
|---|---|---|
| Rust | One binary, no runtime; opens up a TUI (ratatui), a GUI (Tauri), embedded Lua (mlua) and WASM from the same core | Learning curve |
| Execution as a stream of events | A TUI needs incremental progress; a CLI doesn't. Unifying it avoids having two engines | An API slightly less direct than an `async fn` |
| `Arc<dyn Port>` rather than generics | The composition root can swap adapters at runtime (`--no-history`) without propagating type parameters | One virtual indirection per call, irrelevant next to network latency |
| File DTOs kept apart from the domain | The on-disk format moves faster than the model | Explicit conversion code |
| Files as the source of truth, SQLite as an index | Collections get reviewed in pull requests; history does not deserve versioning | The index has to be disposable |
| The standard `.http` format instead of our own | The workspace opens in JetBrains or VS Code with nothing to convert; the moat is the engine, not the lock-in | The format is set by the ecosystem's consensus, not by us |
| An `Engine` facade in the application layer | The CLI, the JSON-RPC server and the TUI share the exact same wiring, instead of each having its own and drifting | One more type between the composition root and the use cases |
| Ad-hoc requests are an adapter (`InlineWorkspace`), not a special path | `hts run <url>` inherits interpolation, environments, secrets, history and exit codes with no new logic | One more `Arc<dyn>` to pick in the composition root |
| The `RequestSource` port works with text, not entities | Saving from the TUI has to keep comments and formatting, and those do not survive a round trip through the model | The editor cannot offer refactors over the model |
| `< ./body.json` is resolved in infrastructure, not in the parser or the domain | The parser stays pure and the domain never learns what a path is; whoever reads the file is who knows where the `.http` lives | A collection is read in full when listed, bodies included |
| A literal body is marked by escaping its `{{` rather than by a new `Body` variant | The escape already exists in the domain, and it spares five layers from learning one more case | The definition holds text with backslashes the file does not have |
| The clock and randomness behind `{{$timestamp}}` arrive through a port (`DynamicSource`) | The domain defines what each dynamic variable means without reading the clock, and a test can fix the seed and assert the exact value instead of "something close to now" | One more port to wire in every composition root |
| A dynamic variable has one value for the whole execution | The real case is a correlation id repeated in a header and the body; two different values there would be a bug | Two `{{$randomInt}}` in a row give the same number |
| The engine's ports are passed as `EnginePorts`, not as loose parameters | Eight positional `Arc<dyn …>` compile just as well swapped, and fail at runtime | One more type to name in every composition root |
| Importing is one use case with the format injected as a port | A cURL command and an OpenAPI description differ only in how a document is read; fetching, merging, conflict policy and reporting are written once. The next format is one trait implementation | Adding a format means touching the composition root, which is the only place allowed to name it |
| A remote document is fetched through `HttpTransport`, not through an HTTP client of its own | `--insecure` and the timeout already live there, and a description served by an internal host with its own CA is exactly when an import is needed | The import use case depends on the transport port as well as the writer |
| An import merges into the destination and skips what is there | A `.http` holds requests nobody imported, and re-importing is how a description gets followed as it grows | Two flags (`--replace`, `--dry-run`) rather than one obvious behaviour |
| `spawn_blocking` for files and SQLite | `std::fs` and `rusqlite` are synchronous; pretending to be async would be a lie | One thread hop per operation |

## How a UI gets added

1. Create the crate (`crates/tui`, `crates/nvim-host`…).
2. Copy the pattern in `crates/cli/src/composition.rs` to build the adapters.
3. Consume `SendRequest::execute` and render the `ExecutionEvent`s.

`domain`, `application` and `infrastructure` are not touched. Needing to touch
them is a design signal, not a formality.

For clients that are not Rust (Neovim, VS Code) the way in is `hts serve`,
described in [`engine-protocol.md`](engine-protocol.md).
