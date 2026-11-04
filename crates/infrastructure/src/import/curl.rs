//! Reading a cURL command as a request.
//!
//! It is the shortest path from "somebody pasted me a command" — an API's
//! documentation, a browser's *Copy as cURL*, a colleague's message — to a
//! request that lives in the workspace and can be run again.
//!
//! # What it is faithful to
//!
//! What curl would have put on the wire, not what the author probably meant.
//! `-d` with no `Content-Type` becomes an explicit
//! `Content-Type: application/x-www-form-urlencoded` header, because that is
//! what curl sends; writing nothing there and letting the engine guess would
//! make the imported request differ from the one that was working.
//!
//! # What it refuses
//!
//! A flag it does not know is an error, and so is one it knows but cannot
//! express — `-F` (multipart), `--cert`, `-d @file`. Dropping flags silently
//! would hand back a request that looks right and behaves differently, which
//! is the one outcome worth avoiding in a tool whose job is reproducing a
//! call.

use std::collections::BTreeMap;

use http_studio_application::{ApplicationError, CollectionImporter};
use http_studio_domain::{
    Body, Collection, Header, HttpMethod, RequestDefinition, RequestId, RequestOptions,
};

use crate::storage::http_file::slug;

/// The name of the collection an imported command lands in by default.
pub const DEFAULT_COLLECTION: &str = "imported";

/// The format's name, as it appears in error messages.
const FORMAT: &str = "curl";

/// Reads cURL commands as requests.
#[derive(Debug, Clone)]
pub struct CurlImporter {
    collection: String,
    name: Option<String>,
}

impl Default for CurlImporter {
    fn default() -> Self {
        Self::new()
    }
}

impl CurlImporter {
    /// An importer writing into [`DEFAULT_COLLECTION`], naming the request
    /// after its URL.
    #[must_use]
    pub fn new() -> Self {
        Self {
            collection: DEFAULT_COLLECTION.to_owned(),
            name: None,
        }
    }

    /// Sets the collection the command lands in, builder style.
    #[must_use]
    pub fn with_collection(mut self, collection: impl Into<String>) -> Self {
        self.collection = collection.into();
        self
    }

    /// Sets the request's name instead of deriving it from the URL.
    #[must_use]
    pub fn with_name(mut self, name: Option<String>) -> Self {
        self.name = name;
        self
    }
}

impl CollectionImporter for CurlImporter {
    /// `origin` says nothing here: a command carries its own URL, so where it
    /// was read from has no bearing on what it calls.
    fn import(
        &self,
        document: &str,
        _origin: Option<&str>,
    ) -> Result<Vec<Collection>, ApplicationError> {
        let command =
            parse(document).map_err(|message| ApplicationError::import(FORMAT, message))?;

        let name = self
            .name
            .clone()
            .unwrap_or_else(|| derive_name(command.method, &command.url));

        Ok(vec![Collection {
            name: self.collection.clone(),
            variables: BTreeMap::new(),
            requests: vec![RequestDefinition {
                id: RequestId::new(format!("{}/{}", self.collection, slug(&name))),
                name,
                method: command.method,
                url: command.url,
                headers: command.headers,
                query: Vec::new(),
                body: command.body,
                variables: BTreeMap::new(),
                description: None,
                options: RequestOptions {
                    insecure_tls: command.insecure,
                },
            }],
        }])
    }
}

/// A cURL command, once read.
#[derive(Debug, PartialEq, Eq)]
struct Command {
    method: HttpMethod,
    url: String,
    headers: Vec<Header>,
    body: Body,
    insecure: bool,
}

/// What the command line said, before it is turned into a request.
#[derive(Default)]
struct Parsed {
    method: Option<String>,
    /// The method a flag implied: `-I` is a `HEAD` without saying so.
    ///
    /// Kept apart from `method` because an explicit `-X` wins over it, which
    /// is curl's own precedence.
    implied_method: Option<HttpMethod>,
    url: Option<String>,
    headers: Vec<Header>,
    data: Vec<String>,
    json: bool,
    query_only: bool,
    insecure: bool,
    user: Option<String>,
}

/// Reads a cURL command line into a request.
fn parse(command: &str) -> Result<Command, String> {
    let tokens = tokenize(command)?;
    let mut parsed = Parsed::default();
    let mut index = skip_program(&tokens);

    while index < tokens.len() {
        let token = &tokens[index];
        index += 1;

        if let Some(long) = token.strip_prefix("--") {
            let (flag, inline) = match long.split_once('=') {
                Some((flag, value)) => (flag, Some(value.to_owned())),
                None => (long, None),
            };
            index = apply(&mut parsed, flag, inline, &tokens, index)?;
            continue;
        }

        // A lone `-` is curl's stdin; as a bare word it is not a URL either.
        if token.len() > 1 && token.starts_with('-') {
            index = apply_short(&mut parsed, &token[1..], &tokens, index)?;
            continue;
        }

        set_url(&mut parsed, token.clone())?;
    }

    build(parsed)
}

/// Skips the `curl` program name, and a shell prompt in front of it.
///
/// Commands get pasted exactly as they were read, `$ ` included, and refusing
/// those would be refusing the common case for no reason.
fn skip_program(tokens: &[String]) -> usize {
    let mut index = 0;
    if tokens
        .first()
        .is_some_and(|token| token == "$" || token == "#")
    {
        index += 1;
    }

    if tokens.get(index).is_some_and(|token| {
        let lowercase = token.to_ascii_lowercase();
        lowercase == "curl" || lowercase.ends_with("/curl") || lowercase.ends_with("curl.exe")
    }) {
        index += 1;
    }

    index
}

/// Records the URL, refusing a second one.
fn set_url(parsed: &mut Parsed, url: String) -> Result<(), String> {
    if let Some(first) = &parsed.url {
        return Err(format!(
            "the command calls more than one URL (`{first}` and `{url}`): import them one at a time"
        ));
    }

    parsed.url = Some(url);
    Ok(())
}

/// Applies one long flag, returning the index to carry on from.
fn apply(
    parsed: &mut Parsed,
    flag: &str,
    inline: Option<String>,
    tokens: &[String],
    index: usize,
) -> Result<usize, String> {
    let mut cursor = index;
    let mut inline = inline;
    // `--header x` and `--header=x` are the same flag; which form was used is
    // the caller's business, not this function's.
    let mut value = |flag: &str| -> Result<String, String> {
        if let Some(value) = inline.take() {
            return Ok(value);
        }
        let value = tokens
            .get(cursor)
            .ok_or_else(|| format!("`--{flag}` needs a value"))?
            .clone();
        cursor += 1;
        Ok(value)
    };

    match flag {
        "request" => parsed.method = Some(value(flag)?),
        "header" => parsed.headers.push(header(&value(flag)?)?),
        "data" | "data-raw" | "data-ascii" | "data-binary" => {
            parsed.data.push(data(&value(flag)?)?);
        }
        "data-urlencode" => parsed.data.push(urlencoded(&value(flag)?)?),
        "json" => {
            parsed.data.push(data(&value(flag)?)?);
            parsed.json = true;
        }
        "get" => parsed.query_only = true,
        "head" => parsed.implied_method = Some(HttpMethod::Head),
        "insecure" => parsed.insecure = true,
        "url" => set_url(parsed, value(flag)?)?,
        "user" => parsed.user = Some(value(flag)?),
        "cookie" => parsed.headers.push(cookie(&value(flag)?)?),
        "user-agent" => parsed.headers.push(Header::new("User-Agent", value(flag)?)),
        "referer" => parsed.headers.push(Header::new("Referer", value(flag)?)),
        _ if IGNORED.contains(&flag) => {}
        _ if IGNORED_WITH_VALUE.contains(&flag) => {
            value(flag)?;
        }
        _ => return Err(unsupported(&format!("--{flag}"))),
    }

    Ok(cursor)
}

/// Applies a group of short flags (`-sSL`, `-XPOST`).
///
/// Bundling is curl's own syntax and browsers emit it, so the group is walked
/// character by character: the first one that takes a value swallows the rest
/// of the token, or the next one if the token ends there.
fn apply_short(
    parsed: &mut Parsed,
    group: &str,
    tokens: &[String],
    index: usize,
) -> Result<usize, String> {
    let mut cursor = index;

    for (position, flag) in group.char_indices() {
        let long = match flag {
            'X' => "request",
            'H' => "header",
            'd' => "data",
            'u' => "user",
            'b' => "cookie",
            'A' => "user-agent",
            'e' => "referer",
            'k' => "insecure",
            'I' => "head",
            'G' => "get",
            'F' => return Err(unsupported("-F")),
            'T' => return Err(unsupported("-T")),
            _ if IGNORED_SHORT.contains(&flag) => continue,
            _ if IGNORED_SHORT_WITH_VALUE.contains(&flag) => "",
            _ => return Err(unsupported(&format!("-{flag}"))),
        };

        if !TAKES_VALUE.contains(&long) {
            apply(parsed, long, None, tokens, cursor)?;
            continue;
        }

        // The value is the rest of the token when there is one: `-XPOST`.
        let attached = &group[position + flag.len_utf8()..];
        if attached.is_empty() {
            let value = tokens
                .get(cursor)
                .ok_or_else(|| format!("`-{flag}` needs a value"))?
                .clone();
            cursor += 1;
            if !long.is_empty() {
                apply(parsed, long, Some(value), tokens, cursor)?;
            }
        } else {
            if !long.is_empty() {
                apply(parsed, long, Some(attached.to_owned()), tokens, cursor)?;
            }
            return Ok(cursor);
        }

        return Ok(cursor);
    }

    Ok(cursor)
}

/// Turns what was read into a request.
fn build(parsed: Parsed) -> Result<Command, String> {
    let url = parsed
        .url
        .ok_or_else(|| "the command names no URL".to_owned())?;

    let mut headers = parsed.headers;
    if let Some(user) = parsed.user {
        // `-u` is credentials, and Basic auth is base64 of them. Leaving the
        // pair for the user to encode by hand would be handing back a request
        // that does not work.
        headers.push(Header::new(
            "Authorization",
            format!("Basic {}", base64(user.as_bytes())),
        ));
    }

    let joined = parsed.data.join("&");
    let (mut url, body) = if parsed.query_only {
        // `-G` moves the data into the query, which is why a command can carry
        // `-d` and still be a GET.
        (append_query(&url, &joined), Body::Empty)
    } else {
        (url, body_from(&joined, &mut headers, parsed.json))
    };
    url = url.trim().to_owned();

    let method = match parsed.method {
        Some(method) => method_from(&method)?,
        None => match parsed.implied_method {
            Some(method) => method,
            None if parsed.query_only || body.is_empty() => HttpMethod::Get,
            // curl's own rule: data with no `-X` is a POST.
            None => HttpMethod::Post,
        },
    };

    Ok(Command {
        method,
        url,
        headers,
        body,
        insecure: parsed.insecure,
    })
}

/// Classifies the body and adds the `Content-Type` curl would have sent.
fn body_from(data: &str, headers: &mut Vec<Header>, json: bool) -> Body {
    if data.is_empty() {
        return Body::Empty;
    }

    let declared = headers
        .iter()
        .find(|header| header.name.eq_ignore_ascii_case("content-type"))
        .map(|header| header.value.clone());

    match declared {
        Some(content_type) if content_type.contains("json") => Body::Json {
            content: data.to_owned(),
        },
        Some(_) => Body::Text {
            content: data.to_owned(),
        },
        None if json => {
            // `--json` is shorthand for the two headers plus the body, and
            // curl sends both.
            headers.push(Header::new("Content-Type", "application/json"));
            headers.push(Header::new("Accept", "application/json"));
            Body::Json {
                content: data.to_owned(),
            }
        }
        None => {
            // curl's default for `-d`, written down rather than left implicit:
            // a request that only works because of a default is a request that
            // breaks when the default changes.
            headers.push(Header::new(
                "Content-Type",
                "application/x-www-form-urlencoded",
            ));
            Body::Text {
                content: data.to_owned(),
            }
        }
    }
}

/// Appends `data` to the URL's query, the way `-G` does.
fn append_query(url: &str, data: &str) -> String {
    if data.is_empty() {
        return url.to_owned();
    }

    let separator = if url.contains('?') { '&' } else { '?' };
    format!("{url}{separator}{data}")
}

/// Reads `Name: value`, curl's `-H` syntax.
fn header(raw: &str) -> Result<Header, String> {
    // `-H 'Name;'` tells curl to send the header empty; `-H 'Name:'` removes
    // it. Both are about curl's defaults rather than about this request, and
    // neither has anything to put in a file.
    if let Some(name) = raw.trim().strip_suffix(';') {
        return Ok(Header::new(name.trim(), ""));
    }

    let (name, value) = raw
        .split_once(':')
        .ok_or_else(|| format!("`{raw}` is not a `Name: value` header"))?;

    let name = name.trim();
    if name.is_empty() {
        return Err(format!("`{raw}` has no header name"));
    }

    Ok(Header::new(name, value.trim()))
}

/// Reads `-b`, which is a cookie string or a file to read them from.
fn cookie(raw: &str) -> Result<Header, String> {
    if !raw.contains('=') {
        return Err(format!(
            "`-b {raw}` reads cookies from a file, which is not part of a request: \
             pass them as `-b 'name=value'`"
        ));
    }

    Ok(Header::new("Cookie", raw.trim()))
}

/// Reads one `-d`, refusing the forms that mean "read a file".
fn data(raw: &str) -> Result<String, String> {
    if let Some(path) = raw.strip_prefix('@') {
        return Err(format!(
            "`-d @{path}` reads the body from a file, which a request cannot carry: \
             paste the body inline, or put it in the `.http` with `< ./{path}`"
        ));
    }

    Ok(raw.to_owned())
}

/// Reads one `--data-urlencode`, encoding what curl would encode.
fn urlencoded(raw: &str) -> Result<String, String> {
    if raw.contains('@') && !raw.contains('=') {
        return Err(format!(
            "`--data-urlencode {raw}` reads a file, which a request cannot carry"
        ));
    }

    let encode = |value: &str| -> String {
        url::form_urlencoded::byte_serialize(value.as_bytes()).collect()
    };

    Ok(match raw.split_once('=') {
        // `=content` encodes the content with no name in front of it.
        Some(("", content)) => encode(content),
        Some((name, content)) => format!("{name}={}", encode(content)),
        // Bare content is encoded whole, name included if it has one.
        None => encode(raw),
    })
}

/// Translates the method, uppercasing it the way the wire wants.
fn method_from(raw: &str) -> Result<HttpMethod, String> {
    match raw.trim().to_ascii_uppercase().as_str() {
        "GET" => Ok(HttpMethod::Get),
        "POST" => Ok(HttpMethod::Post),
        "PUT" => Ok(HttpMethod::Put),
        "PATCH" => Ok(HttpMethod::Patch),
        "DELETE" => Ok(HttpMethod::Delete),
        "HEAD" => Ok(HttpMethod::Head),
        "OPTIONS" => Ok(HttpMethod::Options),
        other => Err(format!("`{other}` is not a method the engine can send")),
    }
}

/// The request's name, when the invocation did not give one.
///
/// Method and path, which is what tells two requests to the same API apart in
/// a listing. The host is the fallback for a URL with no path, where there is
/// nothing else to say.
fn derive_name(method: HttpMethod, url: &str) -> String {
    let without_scheme = url
        .split_once("://")
        .map_or(url, |(_, rest)| rest)
        .trim_start_matches('/');

    let (authority, path) = without_scheme
        .split_once('/')
        .unwrap_or((without_scheme, ""));

    let path = path.split(['?', '#']).next().unwrap_or("");
    let subject = if path.trim_matches('/').is_empty() {
        authority
    } else {
        path
    };

    slug(&format!("{method} {subject}"))
}

/// Flags that change how curl behaves locally and say nothing about the
/// request on the wire.
const IGNORED: &[&str] = &[
    "compressed",
    "location",
    "location-trusted",
    "silent",
    "no-silent",
    "show-error",
    "no-show-error",
    "fail",
    "fail-with-body",
    "include",
    "no-include",
    "verbose",
    "no-verbose",
    "progress-bar",
    "no-progress-meter",
    "no-buffer",
    "globoff",
    "remote-name",
    "create-dirs",
    "ipv4",
    "ipv6",
    "http1.1",
    "http2",
    "http3",
    "tlsv1.2",
    "tlsv1.3",
    "no-keepalive",
    "path-as-is",
    "raw",
    "styled-output",
    "no-styled-output",
];

/// The same, for the ones that swallow a value.
const IGNORED_WITH_VALUE: &[&str] = &[
    "output",
    "max-time",
    "connect-timeout",
    "retry",
    "retry-delay",
    "retry-max-time",
    "write-out",
    "limit-rate",
    "dump-header",
    "trace",
    "trace-ascii",
    "cookie-jar",
    "max-redirs",
];

/// Short flags with no value that only affect curl's own behaviour.
const IGNORED_SHORT: &[char] = &['s', 'S', 'f', 'i', 'v', 'L', 'j', 'O', 'N', '4', '6', '#'];

/// Short flags that swallow a value and say nothing about the request.
const IGNORED_SHORT_WITH_VALUE: &[char] = &['o', 'm', 'D', 'w', 'c'];

/// Long flags that take a value, for resolving a bundled short one.
const TAKES_VALUE: &[&str] = &[
    "request",
    "header",
    "data",
    "user",
    "cookie",
    "user-agent",
    "referer",
    "url",
    "",
];

/// The message for a flag that cannot become part of a request.
fn unsupported(flag: &str) -> String {
    match flag {
        "-F" | "--form" => {
            "`-F` sends a multipart form, which the engine cannot build yet".to_owned()
        }
        "-T" | "--upload-file" => {
            "`-T` uploads a file, which a request in a `.http` cannot carry".to_owned()
        }
        "--cert" | "--key" | "--cacert" | "-E" => format!(
            "`{flag}` configures TLS certificates, which belong to an environment \
             rather than to a request"
        ),
        "--proxy" | "-x" => format!("`{flag}` sets a proxy, which is not part of the request"),
        _ => format!("`{flag}` is not a flag this importer knows"),
    }
}

/// Encodes bytes as base64, for `-u`'s Basic credentials.
///
/// Hand-rolled rather than pulling in a dependency for twenty lines: the
/// alphabet is fixed, the padding rule is fixed, and the test pins both
/// against the vectors in RFC 4648.
fn base64(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);

    for chunk in bytes.chunks(3) {
        let first = chunk.first().copied().unwrap_or(0);
        let second = chunk.get(1).copied().unwrap_or(0);
        let third = chunk.get(2).copied().unwrap_or(0);

        out.push(symbol(first >> 2));
        out.push(symbol(((first & 0b11) << 4) | (second >> 4)));

        // The padding is what says how many of the three bytes were real.
        if chunk.len() > 1 {
            out.push(symbol(((second & 0b1111) << 2) | (third >> 6)));
        } else {
            out.push('=');
        }

        if chunk.len() > 2 {
            out.push(symbol(third & 0b11_1111));
        } else {
            out.push('=');
        }
    }

    out
}

/// The base64 character for one six-bit group.
fn symbol(six_bits: u8) -> char {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

    ALPHABET
        .get(usize::from(six_bits))
        .map_or('=', |byte| char::from(*byte))
}

/// Splits a command line into tokens the way a shell would.
///
/// It handles what commands actually arrive wrapped in: line continuations,
/// single and double quotes, and the `$'…'` form browsers use whenever a value
/// carries a newline or a non-ASCII character.
fn tokenize(command: &str) -> Result<Vec<String>, String> {
    let characters: Vec<char> = command.chars().collect();
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut started = false;
    let mut index = 0;

    while index < characters.len() {
        let character = characters[index];

        match character {
            '\\' | '^' if is_continuation(&characters, index) => {
                // A trailing `\` or `^` joins the next line: `^` because that
                // is what Windows' *Copy as cURL (cmd)* emits.
                index += 2;
            }
            '\\' => {
                index += 1;
                if let Some(escaped) = characters.get(index) {
                    current.push(*escaped);
                    index += 1;
                }
                started = true;
            }
            '$' if characters.get(index + 1) == Some(&'\'') => {
                let (value, next) = ansi_c(&characters, index + 2)?;
                current.push_str(&value);
                index = next;
                started = true;
            }
            '\'' => {
                let (value, next) = until(&characters, index + 1, '\'')?;
                current.push_str(&value);
                index = next;
                started = true;
            }
            '"' => {
                let (value, next) = double_quoted(&characters, index + 1)?;
                current.push_str(&value);
                index = next;
                started = true;
            }
            _ if character.is_whitespace() => {
                if started {
                    tokens.push(std::mem::take(&mut current));
                    started = false;
                }
                index += 1;
            }
            _ => {
                current.push(character);
                started = true;
                index += 1;
            }
        }
    }

    if started {
        tokens.push(current);
    }

    if tokens.is_empty() {
        return Err("the command is empty".to_owned());
    }

    Ok(tokens)
}

/// `true` when the character at `index` escapes the end of the line.
fn is_continuation(characters: &[char], index: usize) -> bool {
    matches!(characters.get(index + 1), Some('\n' | '\r'))
}

/// Reads up to the closing `delimiter`, keeping everything literal.
fn until(characters: &[char], from: usize, delimiter: char) -> Result<(String, usize), String> {
    let mut value = String::new();
    let mut index = from;

    while index < characters.len() {
        if characters[index] == delimiter {
            return Ok((value, index + 1));
        }
        value.push(characters[index]);
        index += 1;
    }

    Err(format!("the command has an unclosed `{delimiter}`"))
}

/// Reads a double-quoted run, honouring the escapes a shell honours there.
fn double_quoted(characters: &[char], from: usize) -> Result<(String, usize), String> {
    let mut value = String::new();
    let mut index = from;

    while index < characters.len() {
        match characters[index] {
            '"' => return Ok((value, index + 1)),
            '\\' => {
                let escaped = characters.get(index + 1).copied();
                index += 2;
                match escaped {
                    // Inside double quotes a backslash only escapes these; in
                    // front of anything else it is a literal backslash, which
                    // is what makes a Windows path survive.
                    Some(next @ ('"' | '\\' | '$' | '`')) => value.push(next),
                    Some('\n') => {}
                    Some(next) => {
                        value.push('\\');
                        value.push(next);
                    }
                    None => return Err("the command has an unclosed `\"`".to_owned()),
                }
            }
            character => {
                value.push(character);
                index += 1;
            }
        }
    }

    Err("the command has an unclosed `\"`".to_owned())
}

/// Reads a `$'…'` run, where the escapes are C's.
///
/// Browsers reach for this form whenever a header or a body holds a newline or
/// a non-ASCII character, so reading `\n` as a line break rather than as two
/// characters is the difference between an importable command and a mangled
/// body.
fn ansi_c(characters: &[char], from: usize) -> Result<(String, usize), String> {
    let mut value = String::new();
    let mut index = from;

    while index < characters.len() {
        match characters[index] {
            '\'' => return Ok((value, index + 1)),
            '\\' => {
                let escaped = characters.get(index + 1).copied();
                index += 2;
                match escaped {
                    Some('n') => value.push('\n'),
                    Some('t') => value.push('\t'),
                    Some('r') => value.push('\r'),
                    Some('0') => value.push('\0'),
                    Some('a') => value.push('\u{7}'),
                    Some('b') => value.push('\u{8}'),
                    Some('f') => value.push('\u{c}'),
                    Some('v') => value.push('\u{b}'),
                    Some('e') => value.push('\u{1b}'),
                    Some('x') => {
                        let (character, next) = hex(characters, index, 2)?;
                        value.push(character);
                        index = next;
                    }
                    Some('u') => {
                        let (character, next) = hex(characters, index, 4)?;
                        value.push(character);
                        index = next;
                    }
                    Some(next) => value.push(next),
                    None => return Err("the command has an unclosed `$'`".to_owned()),
                }
            }
            character => {
                value.push(character);
                index += 1;
            }
        }
    }

    Err("the command has an unclosed `$'`".to_owned())
}

/// Reads up to `digits` hexadecimal digits as one character.
fn hex(characters: &[char], from: usize, digits: usize) -> Result<(char, usize), String> {
    let mut code = String::new();
    let mut index = from;

    while code.len() < digits && characters.get(index).is_some_and(char::is_ascii_hexdigit) {
        code.push(characters[index]);
        index += 1;
    }

    let point = u32::from_str_radix(&code, 16)
        .map_err(|_| "an escape in the command has no hexadecimal digits".to_owned())?;

    let character = char::from_u32(point)
        .ok_or_else(|| format!("`\\x{code}` is not a character the body can carry"))?;

    Ok((character, index))
}

#[cfg(test)]
mod tests {
    // In tests, `unwrap` documents the expectation and its panic IS the failure.
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    /// Imports a command and returns the single request it produced.
    fn request(command: &str) -> RequestDefinition {
        let collections = CurlImporter::new()
            .import(command, None)
            .unwrap_or_else(|error| panic!("`{command}` did not import: {error}"));

        assert_eq!(collections.len(), 1);
        collections.into_iter().next().unwrap().requests.remove(0)
    }

    /// The error message a command fails with.
    fn failure(command: &str) -> String {
        CurlImporter::new().import(command, None).err().map_or_else(
            || panic!("`{command}` was expected to fail"),
            |error| error.to_string(),
        )
    }

    fn header_value(request: &RequestDefinition, name: &str) -> Option<String> {
        request
            .headers
            .iter()
            .find(|header| header.name.eq_ignore_ascii_case(name))
            .map(|header| header.value.clone())
    }

    #[test]
    fn a_bare_url_is_a_get() {
        let request = request("curl https://api.example.com/pets");

        assert_eq!(request.method, HttpMethod::Get);
        assert_eq!(request.url, "https://api.example.com/pets");
        assert_eq!(request.body, Body::Empty);
    }

    #[test]
    fn the_program_name_is_optional() {
        assert_eq!(
            request("https://api.example.com/pets").url,
            "https://api.example.com/pets"
        );
    }

    #[test]
    fn a_pasted_shell_prompt_is_skipped() {
        // Commands arrive copied out of documentation, `$ ` and all.
        assert_eq!(
            request("$ curl https://api.example.com/pets").url,
            "https://api.example.com/pets"
        );
    }

    #[test]
    fn data_with_no_method_is_a_post() {
        // curl's own rule, and the one that makes most pasted commands work.
        let request = request("curl https://api.example.com/pets -d 'name=lila'");

        assert_eq!(request.method, HttpMethod::Post);
        assert_eq!(
            request.body,
            Body::Text {
                content: "name=lila".to_owned()
            }
        );
    }

    #[test]
    fn data_with_no_content_type_declares_the_one_curl_sends() {
        let request = request("curl https://api.example.com/pets -d 'name=lila'");

        assert_eq!(
            header_value(&request, "content-type").as_deref(),
            Some("application/x-www-form-urlencoded"),
            "the imported request has to send what the original sent"
        );
    }

    #[test]
    fn a_json_content_type_makes_a_json_body() {
        let request = request(
            "curl -X POST https://api.example.com/pets -H 'Content-Type: application/json' -d '{\"name\":\"lila\"}'",
        );

        assert_eq!(
            request.body,
            Body::Json {
                content: "{\"name\":\"lila\"}".to_owned()
            }
        );
        assert_eq!(request.headers.len(), 1, "no header is invented");
    }

    #[test]
    fn the_json_flag_brings_both_headers_with_it() {
        let request = request("curl --json '{\"a\":1}' https://api.example.com/pets");

        assert_eq!(
            header_value(&request, "accept").as_deref(),
            Some("application/json")
        );
        assert!(matches!(request.body, Body::Json { .. }));
    }

    #[test]
    fn several_data_flags_are_joined_the_way_curl_joins_them() {
        let request = request("curl https://api.example.com/x -d one=1 -d two=2");

        assert_eq!(
            request.body,
            Body::Text {
                content: "one=1&two=2".to_owned()
            }
        );
    }

    #[test]
    fn data_urlencode_encodes_only_the_value() {
        let request = request("curl https://api.example.com/x --data-urlencode 'q=a b&c'");

        assert_eq!(
            request.body,
            Body::Text {
                content: "q=a+b%26c".to_owned()
            }
        );
    }

    #[test]
    fn get_moves_the_data_into_the_query() {
        let request = request("curl -G https://api.example.com/search -d q=rust");

        assert_eq!(request.method, HttpMethod::Get);
        assert_eq!(request.url, "https://api.example.com/search?q=rust");
        assert_eq!(request.body, Body::Empty);
    }

    #[test]
    fn get_appends_to_a_url_that_already_has_a_query() {
        let request = request("curl -G 'https://api.example.com/search?lang=es' -d q=rust");

        assert_eq!(request.url, "https://api.example.com/search?lang=es&q=rust");
    }

    #[test]
    fn head_is_recognised() {
        assert_eq!(
            request("curl -I https://api.example.com/x").method,
            HttpMethod::Head
        );
    }

    #[test]
    fn insecure_becomes_the_request_option() {
        // The one curl flag that is part of the request rather than of curl.
        assert!(
            request("curl -k https://api.example.com/x")
                .options
                .insecure_tls
        );
    }

    #[test]
    fn credentials_are_encoded_as_basic_auth() {
        let request = request("curl -u demo:secret https://api.example.com/x");

        assert_eq!(
            header_value(&request, "authorization").as_deref(),
            Some("Basic ZGVtbzpzZWNyZXQ=")
        );
    }

    #[test]
    fn base64_matches_the_rfc_vectors() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foob"), "Zm9vYg==");
        assert_eq!(base64(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn bundled_short_flags_are_split() {
        let request = request("curl -sSL -XPOST https://api.example.com/x");

        assert_eq!(request.method, HttpMethod::Post);
    }

    #[test]
    fn a_line_continuation_joins_the_command() {
        let request = request(
            "curl 'https://api.example.com/pets' \\\n  -H 'Accept: application/json' \\\n  -d 'name=lila'",
        );

        assert_eq!(request.method, HttpMethod::Post);
        assert_eq!(
            header_value(&request, "accept").as_deref(),
            Some("application/json")
        );
    }

    #[test]
    fn a_windows_caret_continuation_joins_too() {
        let request = request("curl https://api.example.com/x ^\n  -H \"Accept: text/plain\"");

        assert_eq!(
            header_value(&request, "accept").as_deref(),
            Some("text/plain")
        );
    }

    #[test]
    fn an_ansi_c_quoted_body_keeps_its_line_breaks() {
        // This is the form every browser uses once a body holds a newline, so
        // reading `\n` as two characters would mangle exactly the commands
        // people paste most.
        let request = request(
            "curl https://api.example.com/x -H 'Content-Type: application/json' --data-raw $'{\\n  \"a\": 1\\n}'",
        );

        assert_eq!(
            request.body,
            Body::Json {
                content: "{\n  \"a\": 1\n}".to_owned()
            }
        );
    }

    #[test]
    fn an_ansi_c_escape_can_be_hexadecimal() {
        let request = request("curl https://api.example.com/x -H $'X-Tag: caf\\xe9'");

        assert_eq!(header_value(&request, "x-tag").as_deref(), Some("café"));
    }

    #[test]
    fn a_double_quoted_value_keeps_a_lone_backslash() {
        let request = request("curl https://api.example.com/x -H \"X-Path: C:\\temp\"");

        assert_eq!(
            header_value(&request, "x-path").as_deref(),
            Some("C:\\temp")
        );
    }

    #[test]
    fn a_header_removal_is_read_as_an_empty_one() {
        let request = request("curl https://api.example.com/x -H 'X-Trace;'");

        assert_eq!(header_value(&request, "x-trace").as_deref(), Some(""));
    }

    #[test]
    fn the_name_comes_from_the_method_and_the_path() {
        assert_eq!(
            request("curl -X POST https://api.example.com/v1/pets").name,
            "post-v1-pets"
        );
    }

    #[test]
    fn a_url_with_no_path_is_named_after_its_host() {
        assert_eq!(
            request("curl https://api.example.com").name,
            "get-api-example-com"
        );
    }

    #[test]
    fn the_query_does_not_end_up_in_the_name() {
        assert_eq!(
            request("curl 'https://api.example.com/search?q=a+very+long+thing'").name,
            "get-search"
        );
    }

    #[test]
    fn a_given_name_wins() {
        let collections = CurlImporter::new()
            .with_name(Some("sign in".to_owned()))
            .import("curl https://api.example.com/login", None)
            .unwrap();

        assert_eq!(collections[0].requests[0].name, "sign in");
        assert_eq!(collections[0].requests[0].id.as_str(), "imported/sign-in");
    }

    #[test]
    fn the_collection_can_be_chosen() {
        let collections = CurlImporter::new()
            .with_collection("petstore")
            .import("curl https://api.example.com/pets", None)
            .unwrap();

        assert_eq!(collections[0].name, "petstore");
    }

    #[test]
    fn flags_that_only_change_curl_are_ignored() {
        let request = request(
            "curl --compressed --retry 3 -o out.json --connect-timeout 5 https://api.example.com/x",
        );

        assert_eq!(request.url, "https://api.example.com/x");
        assert!(request.headers.is_empty());
    }

    #[test]
    fn a_multipart_form_says_what_it_cannot_do() {
        let error = failure("curl -F file=@photo.png https://api.example.com/x");

        assert!(error.contains("multipart"), "{error}");
    }

    #[test]
    fn a_body_from_a_file_points_at_the_way_out() {
        // The `.http` format can do this with `<`, and saying so is more use
        // than refusing.
        let error = failure("curl -d @body.json https://api.example.com/x");

        assert!(error.contains("< ./body.json"), "{error}");
    }

    #[test]
    fn an_unknown_flag_is_an_error_rather_than_a_silent_drop() {
        let error = failure("curl --resolve example.com:443:127.0.0.1 https://api.example.com/x");

        assert!(error.contains("--resolve"), "{error}");
    }

    #[test]
    fn a_command_with_no_url_says_so() {
        assert!(failure("curl -X POST -d x=1").contains("no URL"));
    }

    #[test]
    fn two_urls_are_refused_rather_than_half_imported() {
        let error = failure("curl https://api.example.com/a https://api.example.com/b");

        assert!(error.contains("one at a time"), "{error}");
    }

    #[test]
    fn an_unclosed_quote_is_an_error() {
        assert!(failure("curl 'https://api.example.com/x").contains("unclosed"));
    }

    #[test]
    fn a_chrome_copy_as_curl_imports_whole() {
        // Shaped exactly as Chrome emits it, which is the single most common
        // thing anyone will paste into this importer.
        let request = request(
            "curl 'https://api.example.com/v1/pets?limit=10' \\\n  -H 'accept: application/json' \\\n  -H 'authorization: Bearer abc.def' \\\n  -H 'content-type: application/json' \\\n  --data-raw '{\"name\":\"lila\"}' \\\n  --compressed",
        );

        assert_eq!(request.method, HttpMethod::Post);
        assert_eq!(request.url, "https://api.example.com/v1/pets?limit=10");
        assert_eq!(request.headers.len(), 3);
        assert_eq!(
            header_value(&request, "authorization").as_deref(),
            Some("Bearer abc.def")
        );
        assert!(matches!(request.body, Body::Json { .. }));
    }
}
