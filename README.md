# HTTP Studio

An HTTP client whose **engine is separate from its interface**, so a CLI, a TUI,
a Neovim plugin or a GUI can be built on top without reimplementing anything.

Today that means the engine, `hts` (the CLI), `hts tui` (a terminal interface
with vim motions) and `hts serve` (a JSON-RPC server over stdio, for editors).
The TUI did not add a single line of logic to the engine, which was the proof we
were after.

Requests live in **`.http`** files, the format the ecosystem already shares
(JetBrains, VS Code REST Client, httpyac). Your workspace opens in any of those
clients with nothing to convert.

```console
$ hts run auth/login --env prod
POST https://api.example.com/auth/login
200 HTTP/2.0
content-type: application/json; charset=utf-8
142 ms · 241 bytes

{
  "token": "eyJhbGciOi…"
}
```

## Why

Ordinary HTTP clients put the engine inside the UI. When you want a second
interface you reimplement variable resolution, environment precedence and error
handling; they drift apart, and the `curl` you copy out of the GUI stops being
the one the GUI sends.

Here the engine is the product. Requests are `.http` files you can version in
git and review in a pull request, and the history lives apart in a SQLite index
you can delete without losing anything.

## Installation

Prebuilt binaries are on [Releases](https://github.com/usarral/http-studio/releases)
for Linux x86_64, with a `SHA256SUMS` to check the download against. macOS and
Windows will follow.

From source, it needs stable Rust (2024 edition):

```console
$ git clone https://github.com/usarral/http-studio
$ cd http-studio
$ cargo install --path crates/cli
```

Publishing a new version is a matter of raising `version` under
`[workspace.package]` in `Cargo.toml` and merging to `main`: the release
workflow notices that `vX.Y.Z` does not exist yet, builds the matrix targets and
creates both the tag and the release. Pushes that don't change the version
publish nothing.

## Usage

```console
$ hts ls                                   # collections and requests
$ hts env                                  # available environments
$ hts preview auth/login --env dev         # what would be sent, without sending
$ hts run auth/login --env dev             # send it
$ hts run auth/login --var email=a@b.com   # force a variable
$ hts tui                                  # terminal interface
$ hts history -r auth/login -n 10          # recent executions
$ hts history --show 42                    # one execution in full, body included
$ hts index                                # what the history is taking up
$ hts index prune --keep 500               # keep only the last 500
$ hts info                                 # which workspace and paths are in use
$ hts serve                                # JSON-RPC server over stdio
```

When something doesn't go the way you expected, `hts info` is the first stop: it
says which workspace root it discovered, which directory the collections are
read from, which environments it can see and where the index lives. Nearly
everything that looks like a strange bug is really talking to a different
workspace than you thought.

### Long bodies, in a file of their own

A body does not have to live inside the `.http`:

```http
### Create an order
# @name create
POST {{base_url}}/orders
Content-Type: application/json

< ./order.json
```

The path is relative to the `.http` that writes it. `<` inserts the file
**literally** — its `{{braces}}` are sent as they are, which is what you need to
send a template — and `<@` runs it through the variable interpolator first. The
whole story is in
[the workspace format](docs/workspace-format.md#a-body-from-a-file).

### Values that change on every run

```http
POST {{base_url}}/orders
X-Request-Id: {{$uuid}}
X-Sent-At: {{$timestamp}}
Date: {{$datetime rfc1123}}
```

Also `{{$randomInt}}`, `{{$randomInt 1 100}}`, `{{$guid}}` and
`{{$isoTimestamp}}`. The value is **one per execution**: two `{{$uuid}}` in the
same request give the same identifier, which is what you want to correlate a
header with a body. A name we don't know is an error with that name in it, not
an empty string sent without warning.

### No workspace: just a URL

One-off calls need no files at all. If the argument is a URL, it is sent as it
stands:

```console
$ hts run https://api.example.com/health
$ hts run -X POST https://api.example.com/users --json '{"email":"a@b.com"}'
$ hts run https://api.example.com/x -H 'Authorization: Bearer xyz'
$ cat body.json | hts run -X POST https://api.example.com/x --json @-
```

And because it comes in through the same path as requests read from disk, it
inherits everything: from inside a workspace,
`hts run '{{base_url}}/health' --env prod` resolves the environment's variable.

### For scripts

```console
$ hts run auth/login -o body | jq .              # the body only
$ hts run auth/login -o status                   # the status code only
$ TOKEN=$(hts run auth/login --extract data.token)
$ hts run health -o silent || echo "the API is down"
$ hts completions zsh > ~/.zfunc/_hts
```

`--extract` walks the JSON with dotted paths and indices (`items.0.id`) and
returns strings **unquoted**, which is what you want to assign to a variable.
For anything more involved, `-o body | jq` is still the right answer.

There is an example workspace ready to try:

```console
$ hts -w examples/demo ls
$ hts -w examples/demo run echo/get --env dev --var page=2
```

### Certificates that don't validate

An internal environment with its own CA or a self-signed certificate returns
`UnknownIssuer` and leaves the request with nowhere to go. Verification can be
skipped in two ways, and only two:

```console
$ hts run https://internal.local/health --insecure   # or -k, like curl
```

```http
### Internal health
# @insecure
GET https://internal.local/health
```

The directive applies **to its block only**, so it stays written in the file and
shows up in a pull request review. The flag applies to that one invocation. No
global setting leaves it quietly switched on: being visible is exactly the
point. `hts preview` warns when a request is about to travel unverified.

`# @no-reject-unauthorized` is also accepted, which is how httpyac writes it, and
`# @insecure false` turns the directive off without deleting it.

### Exit codes

| Code | Meaning |
|---|---|
| `0` | Sent, 2xx response |
| `1` | Usage, workspace or network error |
| `2` | Sent, non-2xx response |

Separating `1` from `2` is what lets `hts run` work as a CI smoke test without
confusing "the API is down" with "the command is wrong".

In the machine output modes (`body`, `status`, `headers`, `silent`) errors go to
standard error, not standard output: a `$(hts ...)` comes back empty when
something failed, rather than capturing the error message.

## The workspace

```
my-api/
├── http-client.env.json           # environments (versioned)
├── http-client.private.env.json   # secrets (NOT versioned)
└── collections/
    └── auth.http
```

```http
# collections/auth.http

### Sign in
# @name login
@email = demo@example.com
POST {{base_url}}/auth/login
Accept: application/json
Content-Type: application/json

{"email": "{{email}}", "password": "{{password}}"}
```

A `.http` file is a collection; each `###` block is a request. The identifier
you type is `<file>/<name>`: `auth/login`.

The root is discovered by walking up from the current directory until
`collections/` or `http-client.env.json` turns up, the same way `git` finds
`.git`.

### Variables

`{{name}}`, with nesting and cycle detection. Precedence from lowest to highest:
**collection → request → environment (`--env`) → secrets → `--var`**.

Secrets never go in the `.http`. There are two ways in:

- `http-client.private.env.json`, one value per environment and ignored by git.
  It is the convention anyone coming from JetBrains already expects.
- `HTS_SECRET_API_TOKEN=xyz` as an environment variable, available as
  `{{api_token}}`. Meant for CI.

The full format is in [`docs/workspace-format.md`](docs/workspace-format.md).

## Architecture

Layers, with dependencies always pointing inward:

```
cli ──┐
      ├──► application ─────► domain ◄───── infrastructure
rpc ──┘    (use cases)        (pure)        (reqwest, .http, SQLite)
(driving)
```

| Crate | Responsibility |
|---|---|
| `crates/domain` | Entities, variable interpolation, resolution. No I/O, no `async`. |
| `crates/application` | Ports (traits), use cases and the `Engine` facade. |
| `crates/infrastructure` | Adapters: `reqwest`, the `.http` parser, SQLite, secrets. |
| `crates/rpc` | JSON-RPC 2.0 server over stdio. |
| `crates/tui` | Terminal interface with ratatui and vim motions. |
| `crates/cli` | The `hts` binary and the composition root. |

The piece that makes several UIs possible is that running a request returns a
**stream of events**, not a blocking result:

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
for `Completed`. Same engine, different presentation.

The full picture is in [`docs/architecture.md`](docs/architecture.md).

## The TUI

```console
$ hts tui
```

Three panes — collections, request, response — and **vim out of the box**:

| Keys | What they do |
|---|---|
| `h j k l`, `3j`, `gg`, `G`, `{n}G` | move, with counts |
| `w` `b` `0` `$`, `Ctrl-d` `Ctrl-u` | word, line, half page |
| `Tab`, `Ctrl-w h/l` | switch pane |
| `i` `a` `o` `v` `Esc` | edit the `.http` block |
| `Ctrl-Enter` | send without leaving insert mode |
| `/pattern`, `n`, `N` | search |
| `<Space>r` `p` `v` `e` `w` `c` | send, preview, variables, environment, save, cancel |
| `<Space>i` | see the resolved request |
| `:run :preview :env :vars :request :w :q` | commands |

`?` or `:help` list everything. Saving with `:w` rewrites **only** that block of
the file, so comments and formatting elsewhere survive byte for byte; before the
disk is touched, the block is checked to make sure it still parses.

Long lines — an `Authorization` carrying a JWT, a JSON body on one line — wrap to
the pane width instead of being cut off, breaking at spaces unless there are
none.

`Ctrl-Enter` needs a terminal that can tell that combination apart from plain
`Enter`, which takes the Kitty keyboard protocol (Kitty, Ghostty, WezTerm, Foot,
Alacritty ≥ 0.13). Where it isn't available the key behaves like `Enter`, which
in normal mode also sends. Inside tmux it needs:

```tmux
set -s extended-keys always
set -s extended-keys-format csi-u
```

The format matters: with the default `xterm` format tmux sends a sequence
`crossterm` cannot read, and the key is lost.

Two toggleable panes answer the two questions you ask when something didn't go
as expected, and both can be open at once:

- **`<Space>i`** (or `:request`) shows the **resolved** request: what goes over
  the wire, with variables interpolated, the implied `Content-Type` added, the
  query merged into the URL and a warning if the certificate won't be verified.
  It is not what the editor holds, which is the template.
- **`<Space>v`** (or `:vars`) shows each variable's effective value and **which
  scope it came from**, which is the question you ask when a request goes to the
  wrong environment.

Both fill themselves in: if nothing has been resolved yet, opening one runs a
preview.

## Integrating another UI

- **In Rust** (TUI, Tauri): link `http-studio-application` and copy
  `crates/cli/src/composition.rs`.
- **Outside Rust** (Neovim, VS Code): `hts serve` speaks JSON-RPC 2.0 over
  stdio, with LSP-style framing. The Neovim plugin living in this same
  repository is the reference implementation. For one-off scripts, `hts -o json`
  emits JSON Lines. The protocol is in
  [`docs/engine-protocol.md`](docs/engine-protocol.md).

## Neovim

The repository is both the Rust workspace and the plugin: `lua/`, `plugin/` and
`doc/` sit at the root, which is where plugin managers expect to find them. It
needs `hts` on the PATH and Neovim 0.10 or newer.

```lua
-- lazy.nvim
{
  "usarral/http-studio",
  ft = "http",
  config = function()
    require("http-studio").setup()
  end,
}
```

`:HtsRun` sends the request under the cursor; `:HtsPreview` resolves it without
sending and lists every variable with the scope it came from. Details in
[`docs/neovim.md`](docs/neovim.md) and in `:help http-studio`.

## Development

```console
$ cargo test --workspace
$ cargo clippy --workspace --all-targets -- -D warnings
$ cargo fmt --all --check
$ cargo doc --workspace --no-deps --open
$ nvim --headless -l tests/nvim/block_spec.lua
$ nvim --headless -l tests/nvim/smoke.lua target/debug/hts
```

The layer lints (`missing_docs`, `unreachable_pub`, `clippy::pedantic`) are on
across the whole workspace, and CI treats them as errors.

Conventions — commit format, branch names, and the rule that every commit stands
on its own — are in [`AGENTS.md`](AGENTS.md).

## Status and next steps

Everything pending lives in one place: [`docs/roadmap.md`](docs/roadmap.md),
which also explains the decisions still open and why.

Delivered: the engine and the CLI, the `.http` format, the history, `hts serve`,
the TUI, the Neovim plugin, and the twelve features the http-files.org registry
marks as universal.

Next, in order: per-workspace configuration and entering the http-files.org
registry.

## Licence

MIT.
