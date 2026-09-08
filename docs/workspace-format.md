# The workspace format

Requests live in **`.http`** files, the plain-text format JetBrains HTTP Client,
VS Code REST Client and httpyac already use, and which is being documented as a
standard at <https://http-files.org>.

Choosing it over a format of our own has a deliberate consequence: **the format
is not a moat**. An HTTP Studio workspace opens in JetBrains with nothing to
convert, and a JetBrains one opens here. What sets this project apart is the
engine and the interfaces built on it, not the lock-in.

## Layout

```
my-api/
├── http-client.env.json           # environments (versioned)
├── http-client.private.env.json   # per-environment secrets (NOT versioned)
└── collections/
    ├── auth.http
    └── admin/
        └── users.http
```

The workspace root is the first directory, walking up from the current one, that
contains `collections/` or an `http-client.env.json` — the same way `git` looks
for `.git`. It can be forced with `--workspace` or `HTS_WORKSPACE`.

If there is no `collections/`, the root itself is scanned: a bare folder of
`.http` files works without ceremony.

## How it maps to the model

| On disk | In the engine |
|---|---|
| One `.http` file | A collection |
| A `###` block inside the file | A request |
| `@variables` before the first `###` | Collection variables |
| `@variables` inside a block | Request variables |
| `http-client.env.json` | The environments |

It is the format's natural mapping: a `.http` already groups related requests,
which is exactly what a collection was.

A request's **identifier** is `<file path without extension>/<name>`:
`collections/auth.http` with `# @name login` produces `auth/login`. That is what
you type in the CLI.

Both the `.http` and `.rest` extensions are accepted.

## Syntax

```http
# File variables: they apply to every request in this collection.
@api_version = v1

### Sign in
# @name login
# @description Obtains a session token.
@email = demo@example.com
POST {{base_url}}/{{api_version}}/auth/login HTTP/1.1
Accept: application/json
Content-Type: application/json

{"email": "{{email}}", "password": "{{password}}"}

### List users
# @name users
GET {{base_url}}/{{api_version}}/users
  ?limit=10
  &page=1
Accept: application/json
```

The core profile rules that are implemented:

- `###` separates requests; the text after it is the title.
- The **method may be omitted**: it defaults to `GET`.
- A protocol version at the end of the line is accepted and ignored — the
  transport negotiates it, not the file.
- Headers as `Name: value`, one per line.
- A **blank line** separates headers from the body. From there on the content is
  literal: a body line starting with `#` is data.
- Line comments with `#` and with `//`.
- Metadata as comments with a directive: `# @name`, `# @description`,
  `# @insecure`.
- URL continuation on indented lines starting with `?` or `&`.
- Variable declarations with `@name = value`. httpyac's lazy form
  (`@name := value`) is also accepted and treated as eager: deferred evaluation
  only matters with scripting, which we do not support yet.

### A request's name

Decided in this order: the `# @name` directive, failing that the title after
`###` (normalised: `Sign in` → `sign-in`), and failing that its position in the
file (`request-1`).

### Certificates that don't validate

`# @insecure` turns off TLS certificate verification **for that request**:

```http
### Internal environment health
# @name health
# @insecure
GET https://internal.local/health
```

It is what you need with a private CA or a self-signed certificate, where the
engine otherwise returns `UnknownIssuer`. The `# @no-reject-unauthorized` alias
is accepted — that is httpyac's name for it — along with an explicit value
(`# @insecure false`) to switch it off without deleting the line.

Being **per request** and written into the file is deliberate: it shows up in a
pull request review, and no sibling request inherits it. From the CLI,
`--insecure` (or `-k`) does the same for a single invocation, and can only relax
what the file says, never tighten it.

### The body

The content is sent **literally**. The `Content-Type` comes from the header you
write: none is derived here. If the `Content-Type` mentions `json`, the engine
marks the body as JSON so UIs know to highlight and format it.

### A body from a file

A long body does not have to live inside the `.http`:

```http
### Create an order
# @name create
POST {{base_url}}/orders
Content-Type: application/json

< ./order.json
```

The path is **relative to the `.http` that writes it**, the same as in
JetBrains, VS Code REST Client and httpyac, so a collection can be moved to
another folder without rewriting a single reference. Absolute paths are allowed,
but leaving the workspace is not: the body ends up going out over the network,
and a `.http` someone hands you should not be able to send
`< ../../.ssh/id_rsa`.

There are two forms, and the difference matters:

| Form | What it does |
|---|---|
| `< ./order.json` | inserts the file **literally**: its `{{braces}}` are sent as they are |
| `<@ ./order.json` | runs it through the variable interpolator first |

The first is what lets you send a template — Mustache, Handlebars — without the
engine trying to resolve it and dying because the variable does not exist. An
encoding attached to the at sign is also accepted (`<@utf8 ./order.json`); we
only know how to read UTF-8, and naming another is an error rather than a
silently wrong read.

A `<` only opens a reference when a space or an `@` follows it. That is what
tells the directive apart from a body starting with `<?xml` or `<html>`.

The reference is the whole body: it cannot carry more text after it. The file is
read when the collection loads, so a broken reference shows up in `hts ls`
rather than only when the request is sent.

### Multi-line forms

An `application/x-www-form-urlencoded` body can be written one field per line,
with `&` in front of the ones that follow:

```http
POST {{base_url}}/login
Content-Type: application/x-www-form-urlencoded

name=foo
&password=bar
&scope=all
```

That is sent as `name=foo&password=bar&scope=all`, on a single line. It is what
the five clients in the registry do, and it only applies when the
`Content-Type` announces it: in any other body a line starting with `&` is
content and is left alone. It does not apply to a body pulled in with `<`
either: those are a file's bytes, and no client rewrites them.

### Scripts: recognised, never executed

The format allows pre-request scripts (`< {% … %}`) and response handlers
(`> {% … %}` and `> ./handler.js`). **HTTP Studio does not execute them**, but it
does recognise them and keep them out of the request:

- a pre-request script is skipped, and the rest of the file — and of the
  workspace — keeps loading;
- a response handler closes the body instead of travelling attached to it.

That is the difference between "we don't support this" and "this gets sent wrong
in silence". A JetBrains file with scripts opens here and its requests work;
whatever the script was going to compute does not.

### Out of scope for now

Executing those scripts, and httpyac's assertions (`?? status == 200`). They are
on the [roadmap](roadmap.md), not in the minimal parser.

## Environments

`http-client.env.json`, the ecosystem's file:

```json
{
  "dev":  { "base_url": "https://dev.api.example.com" },
  "prod": { "base_url": "https://api.example.com" }
}
```

Selected with `--env prod`. This file **is versioned**, so it should only hold
values you would be happy to show in a pull request.

## Secrets

There are two ways in, and neither puts a token inside a `.http`.

### `http-client.private.env.json` (local)

Same format, one value per environment, and **not versioned** (it is in
`.gitignore`). Its values override the public file's **per variable**, so it is
enough to list the secrets without repeating anything else:

```json
{
  "dev":  { "api_token": "abc" },
  "prod": { "api_token": "xyz" }
}
```

It is the convention anyone coming from JetBrains already expects.

### `HTS_SECRET_*` (CI)

An environment variable prefixed with `HTS_SECRET_` becomes available in
lowercase:

```
HTS_SECRET_API_TOKEN=xyz   →   {{api_token}}
```

Meant for CI, where there are no local files to copy around.

## Variables

The syntax is `{{name}}`, with optional spaces (`{{ name }}`). Values may
themselves contain placeholders and are expanded in cascade, with cycle
detection. `\{{` produces a literal `{{`.

### Dynamic variables

A name starting with `$` is not looked up in any file: it is generated when the
request is resolved.

| Placeholder | Value |
|---|---|
| `{{$timestamp}}` | seconds since the Unix epoch |
| `{{$randomInt}}` | an integer from 0 to 1000, like JetBrains |
| `{{$randomInt 1 100}}` | an integer in that range, like VS Code |
| `{{$uuid}}` / `{{$guid}}` | a v4 UUID (both names, like httpyac) |
| `{{$isoTimestamp}}` | `2026-09-05T11:15:12Z` |
| `{{$datetime iso8601}}` | the same |
| `{{$datetime rfc1123}}` | `Sat, 05 Sep 2026 11:15:12 GMT` |

A name that is not in the table is an error with that name in it, not an empty
string sent without warning. The same goes for a `$datetime` format we cannot
produce: inventing one would send a date the file did not ask for.

**The value is one per execution, not one per occurrence.** Two `{{$uuid}}` in
the same request give the same identifier, and two executions give different
ones. That is deliberate: the case that actually comes up is a correlation id
repeated in a header and in the body, and two different values there would be a
bug.

They are not secrets and are no use for anything that depends on being
unpredictable: the generator is fast, not cryptographic.

### Precedence

From lowest to highest priority — **the last one wins**:

| # | Source | Declared in |
|---|---|---|
| 1 | Collection | `@var` before the first `###` |
| 2 | Request | `@var` inside the block |
| 3 | Environment | `http-client.env.json` plus its private file, with `--env` |
| 4 | Secrets | `HTS_SECRET_*` environment variables |
| 5 | Override | `--var key=value` |

The logic lives in `http_studio_application::context::build_variable_context`,
and its tests document each level. `hts preview` shows the result, and the
`request/preview` JSON-RPC method additionally returns the scope each value came
from.
