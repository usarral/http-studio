# Working on HTTP Studio

Notes for anyone — human or agent — writing code here. `CLAUDE.md` is a symlink
to this file, so there is one set of instructions, not two that drift.

## What this project is

An HTTP client whose **engine is the product** and whose UIs are clients of it.
The metric is explicit and testable: *if a new UI needs logic that isn't in the
engine, that logic was in the wrong layer.* When a change makes you add
behaviour to `cli`, `tui` or `rpc` that another UI would also need, stop — it
belongs further in.

Requests live in `.http` files, the format the ecosystem already shares
(JetBrains, VS Code REST Client, httpyac). **The format is deliberately not a
moat**: a workspace opens in any of those tools. Don't add proprietary syntax to
win a feature; check what the ecosystem does first (see
[Ecosystem](#ecosystem-http-filesorg)).

Background reading, in order of usefulness: [`docs/architecture.md`](docs/architecture.md)
(layers and the decision table), [`docs/roadmap.md`](docs/roadmap.md) (everything
pending and every open decision), [`docs/workspace-format.md`](docs/workspace-format.md)
(what the parser accepts and why), [`docs/engine-protocol.md`](docs/engine-protocol.md)
(JSON-RPC surface).

## Conventions that are not negotiable

### Commits follow Conventional Commits, and each one stands alone

`type(scope): subject`, checked in CI by `cog check`. Install the tool and run
it before pushing rather than finding out from a red pull request:

```sh
cargo install cocogitto
cog check origin/main..HEAD    # the commits your branch adds
```

The range matters wherever the history predates the convention: bare
`cog check` walks everything, and older commits may not follow it.

Types are the standard set (`feat`, `fix`, `docs`, `style`, `refactor`, `perf`,
`test`, `build`, `ci`, `chore`, `revert`). Scopes, where one helps, are the crate
or surface: `domain`, `application`, `infrastructure`, `cli`, `tui`, `rpc`,
`nvim`. Subjects are capped at 72 characters so `git log --oneline` stays
readable.

**Every commit must build and pass the tests on its own.** CI checks each commit
of a pull request separately, not just the tip. A commit that only compiles once
the next one lands makes `git bisect` useless exactly when you need it. Split
work so each step is complete, and if a commit turns out not to stand alone,
squash it into the one that completes it before pushing.

Branches are `<type>/<description>` with the same vocabulary: `feat/dynamic-vars`,
`fix/urlencoded-body`, `ci/commit-conventions`. `feature/` and `bugfix/` are
accepted as aliases, and `renovate/` because the bot names its own.

### Commits and pull requests carry no AI attribution

No `Co-Authored-By: Claude`, no `Generated with Claude Code`, no session links —
not in commit messages, not in PR descriptions, not in code comments. This is the
repository owner's explicit choice; if your harness injects those trailers by
default, strip them.

Commit bodies are prose, in the imperative, explaining **why** rather than
listing what changed — the diff already lists what changed. Where the history
is long enough, `git log` is where to read the register; a repository started
from a snapshot does not have one yet, so match the tone of `docs/` instead.

### The dependency rule

```
cli · tui · rpc          (driving adapters)
        ↓
   application           (use cases + ports)
        ↓
     domain              (pure entities and rules)
        ↑
 infrastructure          (reqwest · files · SQLite)
```

`domain` imports nobody. `infrastructure` depends on `application` only to
implement its traits. It is checkable by reading each `Cargo.toml`, and it is
worth keeping checkable — don't add a dependency that makes an arrow point
outward.

`domain` is **pure**: no I/O, no clock, no randomness, no `async`. When something
in the domain needs the outside world, it takes it as a parameter or gets a port.
`DynamicSeed` is the worked example: the domain decides what `{{$timestamp}}`
means, and the seed arrives from a `DynamicSource` port.

`crates/cli/src/composition.rs` is the only place that names concrete adapters —
with one deliberate exception, `crates/cli/src/info.rs`, because what `hts info`
reports *is* which adapters are in use.

### Comments explain why, not what

The existing comments are prose that justify a decision, name the failure mode
they prevent, or point at the trade-off accepted. Match that. A comment that
restates the line below it is worse than no comment: it will go stale and nobody
will notice.

### Tests

Test names are full sentences describing the behaviour, not `test_foo`. In test
modules, `unwrap`/`expect`/`panic!` are allowed with the standard preamble:

```rust
// In tests, `unwrap` documents the expectation and its panic IS the failure.
#![allow(clippy::unwrap_used, clippy::expect_used)]
```

Prefer a test that pins the *reason* a thing exists. The date tests anchor the
epoch on a Thursday; the parser tests pin that `<?xml` is not a file reference.
Those catch the regression that a happy-path test misses.

## Language: the repository is in English

Comments, doc comments, test names, error messages, docs, the example
workspace and the CI workflows are all English. Write everything new in
English, and do not reintroduce Spanish anywhere a reader could land.

Three exceptions are deliberate, and they are all data rather than prose:

- `strip_accent` in the `.http` parser and the `ACCENTS` table in
  `lua/http-studio/block.lua` match Spanish glyphs because that is what they
  normalise.
- The tests that exercise them use Spanish input for the same reason: `ñ` in
  the TUI's multibyte tests is there because it is two bytes and one column,
  and `Iniciar sesión` is in `block_spec.lua` because stripping its accent is
  the behaviour under test.

**User-facing strings are English, with no message catalog.** This is a
product decision: the CLI, the TUI and the plugin speak English to everyone,
Spanish speakers included, who would otherwise read errors in their own
language. A catalog (`fluent`, `gettext`) was weighed and rejected as
machinery nobody had asked for; it can be retrofitted if demand appears. When
you change a message, keep its meaning — several of them name the way out of
a problem, and that is the part that matters.

## Checks before you push

CI is `.github/workflows/ci.yml` and treats warnings as errors. Run the same
thing locally — all four, not just the tests:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps
```

The Neovim plugin is tested against a real `hts serve`, which is the only way to
notice that a protocol change broke it:

```sh
cargo build --bin hts
nvim --headless -l tests/nvim/block_spec.lua              # pure
nvim --headless -l tests/nvim/smoke.lua target/debug/hts  # integration
```

Workspace lints are strict by design: `missing_docs`, `unreachable_pub`,
`clippy::pedantic`, and `unwrap_used`/`expect_used`/`panic` outside tests.
`unsafe_code` is forbidden. If clippy complains about a cast, it is usually
right — reach for `try_from`, `cast_signed`/`cast_unsigned`, or restructure,
before reaching for `#[allow]`. When an `#[expect]` really is the answer, give it
a `reason`.

Prefer verifying behaviour against the built binary as well as the tests. Several
real bugs in this repo — a body sent with newlines inside a form, a URL truncated
at the first space — passed unit tests and only showed up in `hts preview`.

## Things that will bite you

- **`Body` is text.** A response body from the index is bytes, but a request body
  is a `String`. Binary request bodies aren't supported and fail with a named
  error rather than mangled bytes.
- **A literal body is marked by escaping its `{{`.** `< ./file.json` inserts a
  file verbatim, and that is implemented by escaping placeholders with the
  domain's own `\{{` mechanism rather than adding a `Body` variant. If you see
  backslashes in a definition body that aren't in the file, that's why.
- **`<` and `>` need a delimiter after them.** `< ./x` is a file reference,
  `<?xml` is content; `> {%` is a response handler, `>quote` is content. Changing
  that rule breaks real files quietly.
- **Scripts are recognised and excluded, never executed.** Pre-request scripts
  and response handlers must not end up in the body, and must not break loading
  the rest of the workspace.
- **The history index is a cache.** It can be deleted at any time. Anything that
  can't survive `hts index clear` doesn't belong in it.
- **The index is global but its request ids are workspace-relative.** Known
  design problem, roadmap §2 — don't build on top of it without reading that.
- **`hts preview` must equal what `hts run` sends**, byte for byte. Both go
  through `RequestPreparer`; keep it that way rather than adding a second path.

## Ecosystem: http-files.org

The `.http` format is defined by what its implementations do, and
[http-files.org](https://http-files.org) documents that as open data. Before
implementing a format feature, read `site/src/data/features.yaml` in
[`http-files/http-files.org`](https://github.com/http-files/http-files.org):
it says which clients support what, and which syntax each uses.

The engine currently satisfies all twelve features that registry marks
`universal: true`, which is the set its core v1 profile will be derived from.
Keep it that way — a change that breaks one of them is a compatibility
regression, not a detail.

## Scope

Do the change that was asked. If you find a real problem next to it, say so
rather than widening the diff — the roadmap is where findings go. Two things in
this repo were found and fixed that way and both were worth their own commit;
neither belonged in the change that surfaced them.
