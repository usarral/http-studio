# Importing

Two ways in, one mechanism behind them: a document in somebody else's format
becomes `.http` files in your workspace.

```console
$ hts import curl 'curl https://api.example.com/pets -H "accept: application/json"'
$ hts import openapi ./openapi.yaml
$ hts import openapi https://api.example.com/openapi.json --tag pets
```

Both write into `collections/`, both skip what is already there, and both can
show their work first:

| Flag | What it does |
|---|---|
| `--dry-run` | Prints the `.http` text instead of writing it |
| `--to COLLECTION` | Puts everything in one collection, whatever the format suggested |
| `--replace` | Overwrites requests that already exist, instead of keeping them |

An import **merges**. A `.http` file holds requests nobody imported, plus the
comments and the ordering a person chose, and only the blocks named by the
import are touched. Re-running one is how a description gets followed as it
grows, which is why the default is to keep what is there and say so:

```console
$ hts import openapi ./openapi.yaml
pets → collections/pets.http
  kept     pets/list-pets
  new      pets/adopt-pet
1 already there and kept; pass --replace to overwrite
```

Nothing is written that cannot be read back: everything goes through the `.http`
parser before it reaches the disk, so an import can never leave a workspace the
engine refuses to load.

## From a cURL command

The fastest way in, because a command is how an API usually reaches you — in its
documentation, in a browser's *Copy as cURL*, in a colleague's message.

```console
$ hts import curl "$(pbpaste)" --to petstore          # quoted, as one argument
$ hts import curl @command.sh                          # from a file
$ pbpaste | hts import curl @- --to petstore           # from standard input
```

`@file` and `@-` are worth knowing: a *Copy as cURL* is several lines long, and
quoting it by hand is the part that goes wrong.

What arrives is handled: `\` and `^` line continuations, single and double
quotes, and the `$'…'` form every browser switches to as soon as a header or a
body holds a newline or an accent.

The import is faithful to **what curl puts on the wire**, not to what the author
probably meant. `-d` with no `Content-Type` of its own gets an explicit
`Content-Type: application/x-www-form-urlencoded`, because that is what curl
sends; a request that works only thanks to a default is one that breaks when the
default moves.

| Flag | Becomes |
|---|---|
| `-X`, `--request` | The method. Data with no `-X` is a `POST`, as in curl |
| `-H`, `--header` | A header. `-H 'X-Trace;'` becomes an empty one |
| `-d`, `--data`, `--data-raw`, `--data-binary`, `--data-ascii` | The body, several of them joined with `&` |
| `--data-urlencode` | The body, with the value encoded |
| `--json` | A JSON body plus the two headers curl sends with it |
| `-G`, `--get` | The data moved into the query |
| `-I`, `--head` | `HEAD` |
| `-u`, `--user` | `Authorization: Basic …`, already encoded |
| `-b`, `--cookie` | A `Cookie` header |
| `-A`, `-e` | `User-Agent`, `Referer` |
| `-k`, `--insecure` | `# @insecure` on the request |
| `--compressed`, `-L`, `-s`, `-o`, `--retry`, `--max-time`… | Nothing: they are about curl, not about the request |

A flag it cannot express is an **error**, not a silent drop — `-F` (multipart),
`--cert`, `-d @file`, `--proxy`. Each says what it cannot do and, where there is
one, names the way round it: a body from a file is something the `.http` format
does with `< ./body.json`, so the message says that. Handing back a request that
looks right and behaves differently is the one outcome worth avoiding in a tool
whose job is reproducing a call.

Without `--name`, the request is named after its method and path: `curl -X POST
https://api.example.com/v1/pets` becomes `post-v1-pets`.

## From an OpenAPI description

OpenAPI 3.0 and 3.1, and Swagger 2.0, in JSON or in YAML, from a path or from a
URL:

```console
$ hts import openapi ./openapi.yaml
$ hts import openapi https://petstore3.swagger.io/api/v3/openapi.json
$ hts import openapi https://internal.local/openapi.json -k    # own CA
```

A remote description is fetched through the engine's own transport, which is why
`-k` works here at all — and it applies to the **download** only. What the
imported requests do about TLS is written in the requests.

One operation becomes one request. Each tag becomes a collection, because that
is the grouping the document already chose for itself; operations with no tag go
to a collection named after the document's title. `--tag pets --tag store`
narrows the import, which matters on a public description with three hundred
operations.

```http
@base_url = https://petstore3.swagger.io/api/v3

### add-pet
# @description Add a new pet to the store.
POST {{base_url}}/pet
Content-Type: application/json
api_key: {{api_key}}

{
  "name": "doggie",
  "photoUrls": [
    "https://example.com"
  ],
  "status": "available"
}
```

What that example shows, point by point:

- **`@base_url` is a collection variable**, so an environment overrides it:
  a description imported from a public URL can be pointed at staging with
  `--env` and no editing. A relative server URL — the real Swagger Petstore
  declares `/api/v3` — is resolved against the address the document was
  downloaded from, which is what OpenAPI says it means.
- **Path parameters become placeholders with a declaration.** `/pet/{petId}`
  becomes `{{base_url}}/pet/{{petId}}` with `@petId = 0` above it, so the
  request runs as it stands and `--var petId=7` changes it.
- **Only required parameters travel.** There is no way to write a parameter
  that is present but switched off, and an optional filter nobody asked for
  quietly changing a response is worse than adding it by hand.
- **The body is built from the schema**, with `$ref`s followed inside the
  document, every property present, `readOnly` ones left out — an `id` in a
  creation body is the usual way to earn a `400` — and the first member of an
  `enum` chosen. A `format: uuid` or `date-time` becomes `{{$uuid}}` or
  `{{$isoTimestamp}}` rather than a frozen value, so the body is still right
  the second time it is sent.
- **Authentication arrives as a placeholder and nothing else.** `{{token}}`,
  `{{api_key}}` or `{{basic_auth}}`, with no declaration anywhere, so the engine
  says `undefined variable: api_key` until you put the value in
  `http-client.private.env.json` or `HTS_SECRET_api_key`. A secret in the
  collection is a secret in a pull request.

What it leaves out: `trace` operations, which the engine has no method for, and
external `$ref`s, which live in a document this importer will not go and fetch.
Everything else is imported — refusing a whole description over one operation
nobody wanted would be the wrong trade.

## Where it goes

The destination is the directory the reader already reads from: `collections/`
when it exists, the workspace root when the root holds loose `.http` files, and
a freshly created `collections/` in an empty workspace — an import nobody can
discover afterwards is not worth writing.

A `--dry-run` needs no workspace at all. Converting a command is useful from any
directory, and demanding a `collections/` directory in order to look at one
would be borrowing a requirement from the operation next door.

## Adding a format

The engine's shape is the point: `CollectionImporter` turns a document into
collections, and everything after that — fetching it, merging it, reporting what
happened — is already written. A Postman or Insomnia importer is one more
implementation of that trait plus a line in the composition root; no use case,
no interface and no file-format code needs to change. That is the test the
roadmap applies to the next one.
