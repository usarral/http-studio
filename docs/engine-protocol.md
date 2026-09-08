# The engine protocol, for clients that are not Rust

A client written in Rust (TUI, Tauri) links `http-studio-application` directly.
A client in Lua, TypeScript or Python needs a contract over a channel. The shape
is the one LSP uses, because the problem is identical and the editor ecosystem
already knows how to speak it.

`hts serve` is implemented and covered by 20 full-dialogue tests. The Neovim
plugin in this repository is its reference client.

## Transport

`hts serve` speaks **JSON-RPC 2.0 over stdio**, with LSP-style headers:

```
Content-Length: 92\r\n
\r\n
{"jsonrpc":"2.0","id":1,"method":"workspace/collections","params":{}}
```

stdio was chosen over a socket because the editor already knows how to launch
and supervise child processes (`jobstart` in Neovim), there are no ports or
authentication to manage, and the process dies with the editor.

## Methods

| Method | Kind | Returns |
|---|---|---|
| `workspace/collections` | request | Collections and their requests |
| `workspace/environments` | request | Available environments |
| `request/preview` | request | A `ResolvedRequest`, unsent |
| `request/send` | request | `{ "executionId": "..." }` |
| `request/cancel` | request | Cancels an execution in flight |
| `execution/event` | notification | One `ExecutionEvent` with its `executionId` |
| `history/query` | request | Recent exchanges from the SQLite index |
| `history/detail` | request | One stored execution, body included |

`request/send` replies with an identifier and then emits `execution/event`
notifications. It is the direct translation of the `ExecutionEvent` stream onto
an untyped channel: the client sees exactly the same sequence as a Rust client.

`request/preview` and `request/send` take the same parameters on purpose:
`requestId`, and optionally `environment`, `variables` and `insecureTls`. The
last is the equivalent of the CLI's `--insecure`: it can only relax what the
file declares, so a `false` does not switch off an `# @insecure` written into
the request.

Resolution — reading the workspace, interpolating variables — happens **before**
the reply: if it fails, the client gets an immediate error rather than an
`executionId` that would only serve to deliver a failure event a moment later.

Each request is served on its own task, so replies can arrive out of order —
they are matched by `id`, as JSON-RPC requires — and a `request/cancel` is
handled while the execution it aborts is still in flight.

`history/detail` takes `{ "id": 1 }` — the identifier `history/query` returns —
and replies `{ "detail": … }` with the request that was sent, the headers that
came back and the stored body. If that execution has already been pruned from
the index, `detail` comes back as `null`: that is a result, not an error.

The body the index stores is capped, so the detail carries `body_truncated` and
a client can say so instead of showing a cut-off response as if it were whole.

It is here and not only in the CLI for the usual reason: if a plugin written in
Lua had to open the SQLite database itself to show an old response, that logic
would be in the wrong layer.

### Errors

| Code | When |
|---|---|
| `-32700` | The JSON could not be parsed; the reply carries `id: null` |
| `-32600` | `jsonrpc` is not `"2.0"` |
| `-32601` | Unknown method |
| `-32602` | Missing or malformed parameters |
| `-32603` | Internal failure while serializing |
| `-32000` | The engine refused the operation (no such request or environment, an undefined variable) |

Separating `-32000` from the rest lets a client tell "you called me wrong" apart
from "your workspace has a problem", which are fixed in different ways.

An invalid message does **not** bring the session down: the error is returned and
the loop carries on.

## Serialization

The domain types already derive `Serialize`/`Deserialize` with
`#[serde(tag = "event", rename_all = "snake_case")]`, so the representation on
the wire **is** the engine's, not a parallel translation that could drift:

```json
{"event":"head","head":{"status":200,"version":"HTTP/2.0","headers":[…]}}
{"event":"body_chunk","len":241}
{"event":"completed","exchange":{…}}
```

## The simpler alternative: JSON Lines

`hts -o json run <id>` writes one event per line to stdout, with the same schema
as above. It needs no server and no framing, which makes it the right tool for a
script:

```lua
vim.system({ "hts", "-o", "json", "run", "auth/login", "--env", "dev" }, {
  stdout = function(_, data)
    for line in (data or ""):gmatch("[^\n]+") do
      local event = vim.json.decode(line)
      if event.event == "completed" then
        -- draw it into a buffer
      end
    end
  end,
})
```

Moving from this to `hts serve` is a change of transport: the shape of the
events is the same.

## The CLI's exit codes

Useful for CI, and for a client that would rather invoke the binary:

| Code | Meaning |
|---|---|
| `0` | Sent, 2xx response |
| `1` | Usage, workspace or network error |
| `2` | Sent, non-2xx response |

Separating `1` from `2` is what lets `hts run` work as a smoke test without
confusing "the API is down" with "the command is wrong".
