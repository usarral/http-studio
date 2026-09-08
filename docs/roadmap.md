# Roadmap

Every phase closes when the engine can back a new client **without that client
adding any logic of its own**. That is the project's metric, and the reason the
engine — not the file format, not the UI — is the product.

## Shipped

| Phase | What it delivered |
|---|---|
| **F0** | Four-layer workspace with an enforceable dependency rule. `RequestDefinition` → `ResolvedRequest`, variable interpolation with nesting, escaping and cycle detection, environment precedence, `reqwest` transport streaming `ExecutionEvent`, SQLite history, `hts run`/`preview`/`ls`/`env`, CI exit codes. |
| **F1** | `.http` files as the workspace format, `http-client.env.json` environments, `hts history`, `hts serve` (JSON-RPC 2.0 over stdio) with `request/cancel` and per-variable provenance in `request/preview`. |
| **F1.5** | Bodies from files (`<` literal, `<@` interpolated), `hts info`, `hts index` (stats/prune/clear), response bodies stored in the index with `hts history --show` and `history/detail`, dynamic variables, multi-line urlencoded forms, scripts recognised and excluded from the request. |
| **F2** | `crates/tui` on ratatui with vim motions, modes, leader key, collapsible tree, `.http` highlighting, live progress, variable-provenance panel, block-scoped saving, wrapping, `Ctrl-Enter`. Tested without a terminal. |
| **F2.1** | Ad-hoc requests by URL, bodies from file and stdin, machine output modes, `--extract`, shell completions. |
| **F3** | Neovim plugin over `hts serve`, with pure tests plus an integration test against a real server, both in CI. |
| **F3.1** | TLS opt-out: `# @insecure` per request, `--insecure`/`-k` per invocation, and an error message that names the way out. |
| **English migration** | The whole repository — code, comments, test names, user-facing strings, docs, the example workspace and CI — moved to English. Decided along the way: the CLI, the TUI and the plugin speak English to everyone, with no message catalog. |

As of F1.5 the engine covers **all twelve features** the http-files.org registry
marks `universal: true` — the set its core v1 profile will be derived from.

---

## Pending

Ordered by what unblocks the most. Old phase tags are kept so nothing gets lost.

### 1. Per-workspace configuration *(was F1.5)*

Timeout, proxy and TLS settings, today hardcoded — `REQUEST_TIMEOUT` is a
30-second constant in the CLI's composition root.

**Open question, and the reason this hasn't been done:** it needs a config file,
which would be the **first file in the project that is not part of the `.http`
ecosystem**. That cuts against the "the format is not a moat" principle, so it
deserves a deliberate answer rather than a default:

- Check what the other clients do first — the registry documents their config
  conventions, and reusing one beats inventing one.
- If nothing fits, the shape (`.hts.toml`? `http-client.config.json`?) and its
  precedence against `--flags` need deciding before any code gets written.

### 2. Index scope: global or per-workspace *(was F1.5)*

The index lives in the user's data directory and is global, but the request ids
it stores (`auth/login`) are **relative to a workspace**. Two workspaces with a
request of the same name share rows in `hts history`, and nothing says which is
which.

Options: store the workspace root alongside each row and filter on it, or move
the index into the workspace and accept that it stops covering ad-hoc requests
made outside one. Worth deciding before the index grows more features.

### 3. Format coverage

Everything below is tracked by the http-files.org registry, which is the honest
list of what "compatible with `.http`" means beyond the core. Grouped by cost.

**Cheap and high value:**

- `@no-redirect` and `@no-cookie-jar` — directives four of the five registry
  clients support. Small, and squarely inside the engine.
- Save the response to a file: `>> ./out.json`, and `>>!` to overwrite. The
  parser already recognises the `>` block; it just discards it.
- Multipart form bodies with an explicit boundary.
- Import and export cURL commands. `hts` already knows the resolved request,
  which is most of the work for export.

**Medium:**

- Cross-file references (`@import` / `@ref`): running a request defined in
  another `.http`. Touches the `RequestId` model.
- Response assertions (`?? status == 200`) for CI. *(was F4)*
- Chaining: extracting values from a response into session variables. *(was F4)*
  Related to assertions — both need somewhere to keep per-run state.
- Postman and Insomnia collection import. *(was F4)*

**Large, and each deserves its own decision:**

- Executing scripts, pre-request and response handlers. We recognise and skip
  them today, which is the honest position until there is an answer to "which
  engine, and what does it get access to".
- GraphQL, WebSocket, SSE and gRPC as transports. *(was F5)* Each is a new
  port shaped like `HttpTransport`, and the streaming ones will put useful
  pressure on the `ExecutionEvent` model.

### 4. Security and authentication

- **mTLS: client certificate per environment.** *(was F3.1 and F4)* Supported by
  VS Code, JetBrains and httpyac, each with a different config format — so this
  is gated on §1.
- **Custom CA with `--cacert`** *(was F3.1)*, which *validates* instead of giving
  up on validation. It is the better answer to the problem `--insecure` papers
  over.
- **Basic and Digest auth** with automatic encoding. Four of five clients have it.
- **OAuth 2.0 / OIDC** with automatic token refresh. *(was F4)*
- **Secrets in the OS keychain** as an alternative `SecretProvider`. *(was F4)*
  The port already exists; this is an adapter plus a decision about which crate.
- **AWS Signature v4**, if anyone asks. Listed for completeness.

### 5. Interfaces

- **Finish the TUI** *(was F2.5)*: vim operators (`dd`, `yy`, `p`, `ciw`),
  undo and redo (`u`, `Ctrl-r`), search in the response pane, and creating
  requests rather than only editing them.
- **Desktop GUI with Tauri.** *(was F5)*
- **`crates/wasm`**, for a web UI with no backend. *(was F5)*

### 6. Ecosystem: http-files.org

Register HTTP Studio in the community registry. There is **no application and no
approval step** — it is a pull request against
[`http-files/http-files.org`](https://github.com/http-files/http-files.org):

1. An entry in `site/src/data/clients.yaml`.
2. A column in the `site/src/data/features.yaml` categories where we have
   something to report — and a value for **every** feature in those categories,
   because the validator enforces completeness. All eight categories is 78 cells.
3. `{% client-card id="hts" /%}` in `clients/overview.mdoc` and `clients/cli.mdoc`.
4. Tool counts updated in `clients/overview.mdoc`, `compare/overview.mdoc` and
   `index.mdoc`.
5. Separately, an issue asking for a **maintainer-owner** seat, which carries a
   vote on what the specification selects.

The registry is self-declared, so the entry is worth exactly what its accuracy is
worth. Declaring `true` where something half-works is worse than not appearing.

> **The core profile compliance badge does not exist yet.** The
> [standardization process](https://http-files.org/standardization/process/)
> puts it at step 3, and the project is on step 1 — documenting reality and
> seating maintainer-owners. Core v1 will be derived from the features currently
> marked `universal: true`; we satisfy all twelve. There is nothing to request
> until they publish the profile.

---

## Two roadmap entries that did not survive contact

Kept here because the reasoning is the useful part.

**`hts index rebuild`** — "rebuild from scratch, proving it is a cache". It
cannot be done: the history records *what happened*, and that is not derivable
from the workspace files, so there is nothing to rebuild it from. What does prove
it is a cache is being able to delete it and keep working, which is what
`hts index clear` gives.

**"Request the core profile compatibility badge"** — there is no badge to request
yet, and no application process. See §6.

## Non-goals

- **Cloud sync.** The workspace is a git repository; that is the sync, and it
  already works.
- **User accounts or telemetry.**
- **A scripting language of our own.** If arbitrary logic is needed, the path is
  calling `hts -o json` from a language you already use.
