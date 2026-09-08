# The Neovim plugin

Sends the `.http` request under the cursor to the HTTP Studio engine.

```
┌ auth.http ──────────────────┬ http-studio://response ─────────┐
│ ### Sign in                 │ POST https://api.example.com/…  │
│ # @name login               │                                 │
│ POST {{base_url}}/auth/login│ 200 HTTP/2.0                    │
│ Content-Type: application/j…│ content-type: application/json  │
│                             │                                 │
│ {"email": "{{email}}"}      │ { "token": "eyJhbGciOi…" }      │
│                             │                                 │
│                             │ 142 ms · 241 bytes              │
└─────────────────────────────┴─────────────────────────────────┘
```

## What it does, and what it doesn't

It does **not** resolve variables, decide precedence or format responses. It
speaks JSON-RPC 2.0 with `hts serve` and draws whatever arrives. That is why the
CLI, the TUI and this plugin give exactly the same result: one engine, three
ways of looking at it.

The only logic it duplicates is working out which block the cursor is in, and
that is deliberately fenced in: the name it infers is checked against the
server's real identifiers, so a divergence shows up as "I can't find that
request" and never as sending a different one.

## Installation

It needs `hts` on the PATH and Neovim 0.10 or newer.

`lua/`, `plugin/` and `doc/` hang off the repository root, which is where plugin
managers look for them: the same repository is both the Rust workspace and the
plugin, and there is no `runtimepath` to fiddle with.

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

To work against a local checkout and a freshly built binary, without waiting for
`cargo install`:

```lua
{
  dir = "~/work/http-studio",
  ft = "http",
  config = function()
    require("http-studio").setup({
      cmd = vim.fn.expand("~/work/http-studio/target/debug/hts"),
    })
  end,
}
```

## Usage

| Command | What it does |
|---|---|
| `:HtsRun` | Sends the request under the cursor |
| `:HtsPreview` | Resolves it without sending, showing where each variable came from |
| `:HtsCancel` | Aborts the execution in flight |
| `:HtsPick` | A picker with every request in the workspace |
| `:HtsEnv [name]` | Switches environment; with no argument it asks |
| `:HtsHistory [n]` | Recent executions from the local index |
| `:HtsReload` | Drops the cache after adding or renaming requests |
| `:HtsStop` | Stops the server |

Default mappings in `http` buffers: `<leader>hr` to send, `<leader>hp` to
preview, `<leader>he` for the environment, `<leader>hf` to pick. With
`keymaps = false` they are off and the `<Plug>(hts-run)` family is left for you
to map as you like.

`:help http-studio` has the full detail.

## Configuration

```lua
require("http-studio").setup({
  cmd = "hts",        -- path to the binary
  workspace = nil,    -- root; when nil, `hts` discovers it
  environment = nil,  -- the active environment
  keymaps = true,     -- mappings under <leader>h
})
```

## Tests

```console
$ nvim --headless -l tests/nvim/block_spec.lua      # pure, no network
$ cargo build
$ nvim --headless -l tests/nvim/smoke.lua target/debug/hts
```

The first covers block detection and naming. The second starts a real
`hts serve`, opens the example workspace and checks the whole path through to
the response. Both run in CI.
