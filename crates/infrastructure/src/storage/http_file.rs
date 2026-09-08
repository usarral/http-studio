//! The `.http` format parser.
//!
//! It implements the core profile documented at <https://http-files.org>: the
//! intersection of what JetBrains HTTP Client, VS Code REST Client and httpyac
//! already do identically. That standard's rule is that the specification
//! follows the implementations, so only what is consensus is supported here,
//! and the divergences between clients are deliberately left out.
//!
//! # The grammar supported
//!
//! ```text
//! @base_url = https://api.example.com     ← file variables
//!
//! ### Sign in                              ← separator and title
//! # @name login                            ← metadata
//! @email = demo@example.com                ← request variables
//! POST {{base_url}}/auth/login HTTP/1.1    ← request line
//! Content-Type: application/json           ← headers
//!                                          ← blank line
//! {"email": "{{email}}"}                   ← body
//! ```
//!
//! - The method may be omitted; it defaults to `GET`.
//! - A protocol version at the end of the line is accepted and ignored: the
//!   transport negotiates it, not the file.
//! - Line comments with `#` and with `//`.
//! - URL continuation on indented lines starting with `?` or `&`.
//! - Transport directives: `# @insecure` (aliased as
//!   `# @no-reject-unauthorized`, which is how httpyac writes it) turns off TLS
//!   certificate verification **for that request**.
//! - A body from a file: `< ./body.json` inserts it literally and
//!   `<@ ./body.json` runs it through the variable interpolator first.
//!
//! The parser does not read that file: it is pure and never touches the disk.
//! It returns the reference in [`ParsedRequest::body_ref`], and whoever holds
//! the `.http`'s path — the file system adapter — resolves it.
//!
//! # Out of scope for now
//!
//! Executing pre-request scripts and response handlers. Those are divergences
//! between clients rather than consensus, and they belong on the roadmap, not
//! in the minimal parser.

use std::collections::BTreeMap;

use http_studio_domain::{Body, Header, HttpMethod, RequestDefinition, RequestId, RequestOptions};

/// The separator between requests inside a file.
const SEPARATOR: &str = "###";

/// The result of parsing a `.http` file.
#[derive(Debug, Clone, Default)]
pub struct HttpFile {
    /// Variables declared before the first request.
    ///
    /// These are the standard's "file" variables, and they act as collection
    /// variables: they apply to every request in the file.
    pub variables: BTreeMap<String, String>,
    /// The requests found, in the order they appear.
    pub requests: Vec<ParsedRequest>,
}

/// A request together with the span of file it came from.
///
/// The span is what allows rewriting **only** that block when saving from the
/// TUI, leaving everything else in the file untouched: sibling requests keep
/// their comments, order and formatting byte for byte. Without it, saving would
/// mean regenerating the whole file from the model, and whatever the parser
/// does not represent would be lost.
#[derive(Debug, Clone)]
pub struct ParsedRequest {
    /// The request, already converted to the domain.
    pub definition: RequestDefinition,
    /// Index of the block's first line, starting at 0.
    pub start: usize,
    /// Index of the line after the block's last.
    pub end: usize,
    /// The file the body comes from, when it was declared with `<`.
    ///
    /// When it is present, `definition.body` arrives empty: the parser does not
    /// read the disk, so whoever resolves the reference supplies the final
    /// body.
    pub body_ref: Option<BodyRef>,
}

/// A body taken from an external file (`< ./body.json`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BodyRef {
    /// The path exactly as written, relative to the `.http` holding it.
    pub path: String,
    /// `true` with `<@`: the content goes through the variable interpolator.
    ///
    /// The form without the at sign inserts the file **literally**, which is
    /// what VS Code REST Client and httpyac agree on doing. It is the
    /// difference between being able to send a Mustache template as it stands
    /// and the engine trying to resolve its braces.
    pub interpolate: bool,
    /// The directive's line, so it can be pointed at when the file is missing.
    pub line: usize,
}

/// A syntax error, carrying the line so an editor can point at it.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("line {line}: {message}")]
pub struct ParseError {
    /// The line number, starting at 1.
    pub line: usize,
    /// What was expected instead.
    pub message: String,
}

/// Parses the contents of a `.http` file.
///
/// `prefix` is the file's logical path without its extension (`auth`,
/// `admin/users`); it is prepended to each request's name to form its
/// identifier, so `auth.http` with `# @name login` produces `auth/login`.
///
/// # Errors
///
/// [`ParseError`] when a request has no request line, a header has no `:`, or
/// an `@` declaration is malformed.
pub fn parse(content: &str, prefix: &str) -> Result<HttpFile, ParseError> {
    let lines: Vec<Line<'_>> = content
        .lines()
        .enumerate()
        .map(|(index, text)| Line {
            number: index + 1,
            text,
        })
        .collect();

    let blocks = split_blocks(&lines);
    let mut file = HttpFile::default();

    for (position, block) in blocks.iter().enumerate() {
        if position == 0 && !block.has_request_line() {
            // The preamble before the first `###` only declares variables.
            file.variables = parse_declarations(&block.lines)?;
            continue;
        }

        if block.is_blank() {
            continue;
        }

        // The block runs to where the next one starts, or to the end.
        let end = blocks
            .get(position + 1)
            .map_or(lines.len(), |next| next.start);

        let (definition, body_ref) = parse_request(block, prefix, file.requests.len() + 1)?;
        file.requests.push(ParsedRequest {
            definition,
            start: block.start,
            end,
            body_ref,
        });
    }

    Ok(file)
}

/// One line with its number, so errors can be placed.
#[derive(Debug, Clone, Copy)]
struct Line<'a> {
    number: usize,
    text: &'a str,
}

/// A block separated by `###`, with its optional title.
#[derive(Debug, Default)]
struct Block<'a> {
    title: Option<String>,
    /// Index of the `###` line opening the block, starting at 0.
    start: usize,
    lines: Vec<Line<'a>>,
}

impl Block<'_> {
    /// `true` when the block holds nothing but comments and blanks.
    fn is_blank(&self) -> bool {
        self.lines.iter().all(|line| is_ignorable(line.text))
    }

    /// `true` when the block contains something that looks like a request line.
    ///
    /// It tells a variable preamble apart from a file that starts straight into
    /// a request with no `###` in front.
    fn has_request_line(&self) -> bool {
        self.lines.iter().any(|line| !is_ignorable(line.text))
    }
}

/// Splits the file at the `###` separators.
fn split_blocks<'a>(lines: &[Line<'a>]) -> Vec<Block<'a>> {
    let mut blocks = vec![Block::default()];

    for (index, line) in lines.iter().enumerate() {
        if let Some(rest) = line.text.trim_start().strip_prefix(SEPARATOR) {
            let title = rest.trim().trim_matches('#').trim();
            blocks.push(Block {
                title: (!title.is_empty()).then(|| title.to_owned()),
                start: index,
                lines: Vec::new(),
            });
            continue;
        }

        // `blocks` is never empty: it is initialised with one block.
        if let Some(current) = blocks.last_mut() {
            current.lines.push(*line);
        }
    }

    blocks
}

/// `true` when the line is a comment.
///
/// Metadata (`# @name`) counts as a comment to the standard, but matters here,
/// so it is read separately in [`parse_metadata`].
fn is_comment(text: &str) -> bool {
    let trimmed = text.trim();
    trimmed.starts_with('#') || trimmed.starts_with("//")
}

/// `true` when the line is empty or holds nothing but spaces.
fn is_blank(text: &str) -> bool {
    text.trim().is_empty()
}

/// `true` when the line adds nothing to the request: blank, a comment, or a
/// variable declaration.
fn is_ignorable(text: &str) -> bool {
    is_blank(text) || is_comment(text) || is_declaration(text)
}

/// `true` when the line declares a variable (`@name = value`).
fn is_declaration(text: &str) -> bool {
    text.trim_start().starts_with('@')
}

/// Extracts the `@name = value` declarations from a set of lines.
fn parse_declarations(lines: &[Line<'_>]) -> Result<BTreeMap<String, String>, ParseError> {
    let mut variables = BTreeMap::new();

    for line in lines {
        if !is_declaration(line.text) {
            continue;
        }

        let declaration = line.text.trim_start().trim_start_matches('@');
        let (name, value) = declaration.split_once('=').ok_or_else(|| ParseError {
            line: line.number,
            message: "expected `@name = value`".to_owned(),
        })?;

        // `@name := value` is httpyac's lazy variant. The syntax is accepted
        // and treated as eager: deferred evaluation only matters with
        // scripting, which we do not support yet.
        let name = name.trim().trim_end_matches(':').trim();
        if name.is_empty() {
            return Err(ParseError {
                line: line.number,
                message: "the variable name is empty".to_owned(),
            });
        }

        variables.insert(name.to_owned(), value.trim().to_owned());
    }

    Ok(variables)
}

/// Reads a block's `# @key value` metadata.
fn parse_metadata(lines: &[Line<'_>]) -> BTreeMap<String, String> {
    let mut metadata = BTreeMap::new();

    for line in lines {
        let trimmed = line.text.trim();
        let Some(comment) = trimmed
            .strip_prefix("//")
            .or_else(|| trimmed.strip_prefix('#'))
        else {
            continue;
        };

        let comment = comment.trim();
        let Some(directive) = comment.strip_prefix('@') else {
            continue;
        };

        let (key, value) = directive
            .split_once(char::is_whitespace)
            .unwrap_or((directive, ""));
        metadata.insert(key.trim().to_lowercase(), value.trim().to_owned());
    }

    metadata
}

/// Parses a whole block as a request.
///
/// It is walked in order rather than filtered up front: the blank line
/// separating headers from body is significant, and past it the content is
/// literal — a body line starting with `#` is data, not a comment.
fn parse_request(
    block: &Block<'_>,
    prefix: &str,
    position: usize,
) -> Result<(RequestDefinition, Option<BodyRef>), ParseError> {
    let metadata = parse_metadata(&block.lines);
    let variables = parse_declarations(&block.lines)?;
    let lines = &block.lines;

    // 1. Skip the preamble up to the request line.
    let mut cursor = 0;
    loop {
        if lines
            .get(cursor)
            .is_some_and(|line| is_ignorable(line.text))
        {
            cursor += 1;
            continue;
        }

        // A pre-request script (`< {% … %}`) sits right here in JetBrains
        // files. We do not run it, but skipping it is what lets the rest of the
        // file — and the rest of the workspace — keep loading.
        if let Some(after) = skip_script_block(lines, cursor, '<') {
            cursor = after;
            continue;
        }

        break;
    }

    let first = lines.get(cursor).ok_or_else(|| ParseError {
        line: lines.first().map_or(1, |line| line.number),
        message: "the block contains no request line".to_owned(),
    })?;
    let (method, mut url) = parse_request_line(*first)?;
    cursor += 1;

    // 2. URL continuations: indented lines starting with `?` or `&`.
    while let Some(line) = lines.get(cursor) {
        let trimmed = line.text.trim();
        if !(trimmed.starts_with('?') || trimmed.starts_with('&')) {
            break;
        }
        url.push_str(trimmed);
        cursor += 1;
    }

    // 3. Headers, up to the first blank line.
    let mut headers = Vec::new();
    while let Some(line) = lines.get(cursor) {
        if is_blank(line.text) {
            cursor += 1;
            break;
        }

        cursor += 1;
        if is_comment(line.text) || is_declaration(line.text) {
            continue;
        }

        let (name, value) = line.text.split_once(':').ok_or_else(|| ParseError {
            line: line.number,
            message: format!("expected `Header: value`, found `{}`", line.text.trim()),
        })?;
        headers.push(Header::new(name.trim(), value.trim()));
    }

    // 4. Body: the rest, literal, unless it is a file reference. The response
    //    handler, when there is one, closes the body: what follows is script,
    //    not data.
    let rest = &lines[cursor.min(lines.len())..];
    let body_lines = match rest
        .iter()
        .position(|line| opens_response_handler(line.text))
    {
        Some(end) => &rest[..end],
        None => rest,
    };

    let raw_body = body_lines
        .iter()
        .map(|line| line.text)
        .collect::<Vec<_>>()
        .join("\n");

    let (body, body_ref) = parse_body(body_lines, &raw_body, &headers)?;
    let name = request_name(&metadata, block.title.as_deref(), position);

    Ok((
        RequestDefinition {
            id: RequestId::new(format!("{prefix}/{}", slug(&name))),
            name,
            method,
            url,
            headers,
            query: Vec::new(),
            body,
            variables,
            description: metadata.get("description").cloned(),
            options: options_from(&metadata),
        },
        body_ref,
    ))
}

/// Skips a `{% … %}` block opened by `marker` (`<` or `>`).
///
/// Returns the index of the line after the close, or `None` when the line at
/// `at` does not open one. A block with no closing `%}` swallows the rest: that
/// is what a truncated file does, and the useful error is not the script's
/// syntax but "there is no request line here", which is what ends up being
/// said.
fn skip_script_block(lines: &[Line<'_>], at: usize, marker: char) -> Option<usize> {
    let first = lines.get(at)?;
    let rest = first.text.trim_start().strip_prefix(marker)?.trim_start();
    if !rest.starts_with("{%") {
        return None;
    }

    // The close can be on the same line: `< {% x %}` is valid.
    if rest.trim_start_matches("{%").contains("%}") {
        return Some(at + 1);
    }

    for (offset, line) in lines.iter().enumerate().skip(at + 1) {
        if line.text.contains("%}") {
            return Some(offset + 1);
        }
    }

    Some(lines.len())
}

/// `true` when the line opens a JetBrains response handler.
///
/// Those are `> {% … %}` and `> ./handler.js`. We do not run them, but they
/// have to stop counting as body: sending them down the wire attached to the
/// JSON — which is what used to happen — produces an invalid request without
/// saying why.
fn opens_response_handler(text: &str) -> bool {
    let Some(rest) = text.trim_start().strip_prefix('>') else {
        return false;
    };

    // The same rule as with `<`: a space or a brace has to follow, so it is not
    // confused with a body starting with `>` — a quote, a diff.
    rest.starts_with(char::is_whitespace) || rest.starts_with("{%")
}

/// Decides whether the body is written right there or comes from a file.
///
/// It can only be one of the two: mixing `< ./body.json` with loose text means
/// nothing in any client of the ecosystem, and accepting it silently would hide
/// a typo rather than point at it.
fn parse_body(
    body_lines: &[Line<'_>],
    raw_body: &str,
    headers: &[Header],
) -> Result<(Body, Option<BodyRef>), ParseError> {
    let Some(first) = body_lines.iter().find(|line| !is_blank(line.text)) else {
        return Ok((Body::Empty, None));
    };

    let Some(reference) = parse_body_ref(*first)? else {
        return Ok((inline_body_from(headers, raw_body.trim()), None));
    };

    if body_lines
        .iter()
        .any(|line| !is_blank(line.text) && line.number != first.number)
    {
        return Err(ParseError {
            line: first.number,
            message: "`<` takes the whole file as the body: nothing may follow it".to_owned(),
        });
    }

    Ok((Body::Empty, Some(reference)))
}

/// Recognises the `< ./body.json` line that takes the body from a file.
///
/// A `<` only opens a reference when a space or an `@` follows it. That is the
/// rule telling the directive apart from a body starting with `<?xml` or
/// `<html>`, and it is the one VS Code REST Client and httpyac already apply.
///
/// `<@` additionally asks for the content to go through the interpolator, and
/// accepts an encoding attached to the at sign (`<@utf8 ./body.json`). We only
/// know how to read UTF-8, so naming another is an explicit error: reading it as
/// if it were would send different bytes from the ones in the file.
fn parse_body_ref(line: Line<'_>) -> Result<Option<BodyRef>, ParseError> {
    // `trim_start` and not `trim`: the trailing space is significant. With it,
    // `< ` is a directive missing its path and gets flagged; without it, it
    // would be indistinguishable from a body whose first line is a lone `<`.
    let trimmed = line.text.trim_start();

    let (rest, interpolate) = if let Some(rest) = trimmed.strip_prefix("<@") {
        (rest, true)
    } else if let Some(rest) = trimmed.strip_prefix('<') {
        if !rest.starts_with(char::is_whitespace) {
            return Ok(None);
        }
        // `< {% … %}` is a pre-request script, not a path called `{%`.
        if rest.trim_start().starts_with("{%") {
            return Ok(None);
        }
        (rest, false)
    } else {
        return Ok(None);
    };

    let (encoding, path) = match rest.strip_prefix(char::is_whitespace) {
        Some(path) => ("", path),
        None => rest.split_once(char::is_whitespace).unwrap_or((rest, "")),
    };

    if !encoding.is_empty() && !matches!(encoding.to_ascii_lowercase().as_str(), "utf8" | "utf-8") {
        return Err(ParseError {
            line: line.number,
            message: format!("unsupported encoding `{encoding}`: only UTF-8 is read"),
        });
    }

    let path = path.trim();
    if path.is_empty() {
        return Err(ParseError {
            line: line.number,
            message: "the file path after `<` is missing".to_owned(),
        });
    }

    Ok(Some(BodyRef {
        path: path.to_owned(),
        interpolate,
        line: line.number,
    }))
}

/// Parses `METHOD URL [HTTP/x.y]`, with the method optional.
///
/// The URL is **everything** between the method and the protocol version,
/// spaces included. The line cannot be split on spaces: a placeholder with
/// arguments (`{{$randomInt 1 100}}`) carries its own, and splitting on the
/// first left the URL cut in half.
fn parse_request_line(line: Line<'_>) -> Result<(HttpMethod, String), ParseError> {
    let text = line.text.trim();
    if text.is_empty() {
        return Err(ParseError {
            line: line.number,
            message: "empty request line".to_owned(),
        });
    }

    let (method, rest) = match text.split_once(char::is_whitespace) {
        Some((first, rest)) => match method_from(first) {
            Some(method) => (method, rest.trim()),
            // With no explicit method, the whole line is the URL.
            None => (HttpMethod::Get, text),
        },
        None if method_from(text).is_some() => {
            return Err(ParseError {
                line: line.number,
                message: format!("the URL is missing after `{text}`"),
            });
        }
        None => (HttpMethod::Get, text),
    };

    if rest.is_empty() {
        return Err(ParseError {
            line: line.number,
            message: "the URL is missing".to_owned(),
        });
    }

    // The protocol version, when present, is the last token; the transport
    // negotiates it, not the file, so it is dropped.
    let url = match rest.rsplit_once(char::is_whitespace) {
        Some((head, tail)) if is_protocol_version(tail) => head.trim_end(),
        _ => rest,
    };

    Ok((method, url.to_owned()))
}

/// `true` when the token is a protocol version (`HTTP/1.1`, `HTTP/2`…).
fn is_protocol_version(token: &str) -> bool {
    token.len() > "HTTP/".len() && token[.."HTTP/".len()].eq_ignore_ascii_case("HTTP/")
}

/// Translates the method token, when it is a known one.
fn method_from(token: &str) -> Option<HttpMethod> {
    match token.to_ascii_uppercase().as_str() {
        "GET" => Some(HttpMethod::Get),
        "POST" => Some(HttpMethod::Post),
        "PUT" => Some(HttpMethod::Put),
        "PATCH" => Some(HttpMethod::Patch),
        "DELETE" => Some(HttpMethod::Delete),
        "HEAD" => Some(HttpMethod::Head),
        "OPTIONS" => Some(HttpMethod::Options),
        _ => None,
    }
}

/// Joins the lines of an `x-www-form-urlencoded` body split on `&`.
///
/// All five clients in the registry accept a form written like this:
///
/// ```text
/// Content-Type: application/x-www-form-urlencoded
///
/// name=foo
/// &password=bar
/// ```
///
/// and send `name=foo&password=bar`. Sending it with the newlines inside —
/// which is what used to happen — produces a form the server rejects without it
/// being visible why.
///
/// It only applies when the `Content-Type` announces it: in any other body, a
/// line starting with `&` is content and is left alone.
fn join_urlencoded(content: &str) -> String {
    let mut out = String::with_capacity(content.len());

    for line in content.lines() {
        if out.is_empty() {
            out.push_str(line);
        } else if line.trim_start().starts_with('&') {
            out.push_str(line.trim_start());
        } else {
            out.push('\n');
            out.push_str(line);
        }
    }

    out
}

/// `true` when the headers declare a `Content-Type` containing `needle`.
fn content_type_has(headers: &[Header], needle: &str) -> bool {
    headers.iter().any(|header| {
        header.name.eq_ignore_ascii_case("content-type") && header.value.contains(needle)
    })
}

/// Classifies the body according to the declared `Content-Type`.
///
/// The content is kept literal in every case; the type only exists so UIs know
/// how to highlight it. No header is derived: in `.http`, what is written is
/// what gets sent.
pub(crate) fn body_from(headers: &[Header], content: &str) -> Body {
    if content.is_empty() {
        return Body::Empty;
    }

    if content_type_has(headers, "json") {
        Body::Json {
            content: content.to_owned(),
        }
    } else {
        Body::Text {
            content: content.to_owned(),
        }
    }
}

/// Classifies a body **written inside the `.http`**.
///
/// Kept apart from [`body_from`] because joining the `&` lines is a rule of the
/// `.http` file, not of the content: a body pulled in with `< ./form.txt` is
/// that file's bytes, and no client in the ecosystem rewrites them.
fn inline_body_from(headers: &[Header], content: &str) -> Body {
    if content_type_has(headers, "x-www-form-urlencoded") {
        return body_from(headers, &join_urlencoded(content));
    }

    body_from(headers, content)
}

/// Translates the block's metadata into transport options.
///
/// The directives are boolean by presence: `# @insecure` is enough. An explicit
/// value (`# @insecure false`) is also accepted, so it can be switched off
/// without deleting the line — which in a versioned file is what you want while
/// checking whether the certificate validates yet.
fn options_from(metadata: &BTreeMap<String, String>) -> RequestOptions {
    // `no-reject-unauthorized` is httpyac's name; `insecure` is curl's, and the
    // one we document. Both are accepted so someone else's file does not have
    // to be rewritten.
    let insecure_tls = ["insecure", "no-reject-unauthorized"]
        .iter()
        .filter_map(|key| metadata.get(*key))
        .any(|value| is_enabled(value));

    RequestOptions { insecure_tls }
}

/// Reads a boolean directive's value.
///
/// With no value it is on: `# @insecure` is the normal way to write it.
fn is_enabled(value: &str) -> bool {
    !matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "false" | "no" | "0" | "off"
    )
}

/// Decides the request's name: `@name`, then the title, then its position.
fn request_name(
    metadata: &BTreeMap<String, String>,
    title: Option<&str>,
    position: usize,
) -> String {
    metadata
        .get("name")
        .filter(|name| !name.is_empty())
        .cloned()
        .or_else(|| title.map(str::to_owned))
        .unwrap_or_else(|| format!("request-{position}"))
}

/// Turns a readable name into a fragment of identifier.
///
/// It is normalised so the identifier can be typed into a CLI without quotes or
/// escapes, even when the title carries accents or punctuation.
fn slug(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut previous_dash = false;

    for character in name.chars() {
        if character.is_ascii_alphanumeric() {
            out.push(character.to_ascii_lowercase());
            previous_dash = false;
        } else if let Some(plain) = strip_accent(character) {
            out.push(plain);
            previous_dash = false;
        } else if !previous_dash && !out.is_empty() {
            out.push('-');
            previous_dash = true;
        }
    }

    let trimmed = out.trim_end_matches('-');
    if trimmed.is_empty() {
        "request".to_owned()
    } else {
        trimmed.to_owned()
    }
}

/// Reduces accented vowels and the eñe to ASCII.
///
/// `to_ascii_lowercase` leaves non-ASCII characters alone, so the accented
/// upper and lower cases are listed together.
fn strip_accent(character: char) -> Option<char> {
    match character {
        'á' | 'Á' | 'à' | 'À' | 'ä' | 'Ä' => Some('a'),
        'é' | 'É' | 'è' | 'È' | 'ë' | 'Ë' => Some('e'),
        'í' | 'Í' | 'ì' | 'Ì' | 'ï' | 'Ï' => Some('i'),
        'ó' | 'Ó' | 'ò' | 'Ò' | 'ö' | 'Ö' => Some('o'),
        'ú' | 'Ú' | 'ù' | 'Ù' | 'ü' | 'Ü' => Some('u'),
        'ñ' | 'Ñ' => Some('n'),
        'ç' | 'Ç' => Some('c'),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    // In tests, `unwrap` documents the expectation and its panic IS the failure.
    #![allow(clippy::unwrap_used)]

    use super::*;

    fn parse_ok(content: &str) -> HttpFile {
        parse(content, "demo").unwrap()
    }

    #[test]
    fn parses_a_minimal_request() {
        let file = parse_ok("GET https://api.example.com/health");

        assert_eq!(file.requests.len(), 1);
        assert_eq!(file.requests[0].definition.method, HttpMethod::Get);
        assert_eq!(
            file.requests[0].definition.url,
            "https://api.example.com/health"
        );
    }

    #[test]
    fn without_the_directive_a_request_verifies_the_certificate() {
        let file = parse_ok("GET https://api.example.com/health");

        assert!(!file.requests[0].definition.options.insecure_tls);
    }

    #[test]
    fn the_insecure_directive_turns_off_tls_verification() {
        let file = parse_ok("# @insecure\nGET https://interno.local/health");

        assert!(file.requests[0].definition.options.insecure_tls);
    }

    #[test]
    fn accepts_httpyacs_name_for_the_same_directive() {
        let file = parse_ok("# @no-reject-unauthorized\nGET https://interno.local/health");

        assert!(file.requests[0].definition.options.insecure_tls);
    }

    #[test]
    fn the_directive_can_be_switched_off_with_an_explicit_value() {
        let file = parse_ok("# @insecure false\nGET https://api.example.com/health");

        assert!(!file.requests[0].definition.options.insecure_tls);
    }

    #[test]
    fn the_directive_only_affects_its_own_block() {
        let file = parse_ok(
            "### Internal\n# @insecure\nGET https://interno.local/x\n\n\
             ### Public\nGET https://api.example.com/x\n",
        );

        assert!(file.requests[0].definition.options.insecure_tls);
        assert!(!file.requests[1].definition.options.insecure_tls);
    }

    #[test]
    fn the_default_method_is_get() {
        let file = parse_ok("https://api.example.com/health");

        assert_eq!(file.requests[0].definition.method, HttpMethod::Get);
        assert_eq!(
            file.requests[0].definition.url,
            "https://api.example.com/health"
        );
    }

    #[test]
    fn ignores_the_protocol_version() {
        let file = parse_ok("POST https://api.example.com/x HTTP/1.1");

        assert_eq!(file.requests[0].definition.method, HttpMethod::Post);
        assert_eq!(file.requests[0].definition.url, "https://api.example.com/x");
    }

    #[test]
    fn the_url_allows_spaces_inside_a_placeholder() {
        // `{{$randomInt 1 100}}` carries spaces: splitting the line on spaces
        // used to leave the URL cut off at the first one.
        let file = parse_ok("### X\nGET https://a.test/x?n={{$randomInt 1 100}}");

        assert_eq!(
            file.requests[0].definition.url,
            "https://a.test/x?n={{$randomInt 1 100}}"
        );
    }

    #[test]
    fn the_version_is_dropped_even_when_the_url_has_spaces() {
        let file = parse_ok("### X\nPOST https://a.test/{{$datetime rfc1123}} HTTP/1.1");

        assert_eq!(
            file.requests[0].definition.url,
            "https://a.test/{{$datetime rfc1123}}"
        );
        assert_eq!(file.requests[0].definition.method, HttpMethod::Post);
    }

    #[test]
    fn a_method_without_a_url_is_an_error() {
        let error = parse("### X\nPOST", "demo").unwrap_err();

        assert_eq!(error.line, 2);
        assert!(
            error.message.contains("the URL is missing"),
            "{}",
            error.message
        );
    }

    #[test]
    fn splits_several_requests_at_the_separator() {
        let file = parse_ok("### One\nGET https://a.test\n\n### Another\nPOST https://b.test\n");

        assert_eq!(file.requests.len(), 2);
        assert_eq!(file.requests[0].definition.name, "One");
        assert_eq!(file.requests[1].definition.name, "Another");
    }

    #[test]
    fn reads_the_file_variables() {
        let file =
            parse_ok("@host = api.example.com\n@version = v1\n\n### X\nGET https://{{host}}");

        assert_eq!(
            file.variables.get("host").map(String::as_str),
            Some("api.example.com")
        );
        assert_eq!(
            file.variables.get("version").map(String::as_str),
            Some("v1")
        );
    }

    #[test]
    fn a_requests_variables_are_its_own() {
        let file = parse_ok("@global = 1\n\n### X\n@local = 2\nGET https://a.test");

        assert_eq!(file.variables.get("local"), None);
        assert_eq!(
            file.requests[0]
                .definition
                .variables
                .get("local")
                .map(String::as_str),
            Some("2")
        );
    }

    #[test]
    fn accepts_the_lazy_declaration_form() {
        let file = parse_ok("@token := abc\n\n### X\nGET https://a.test");

        assert_eq!(file.variables.get("token").map(String::as_str), Some("abc"));
    }

    #[test]
    fn the_name_comes_from_the_name_directive() {
        let file = parse_ok("### Ignored title\n# @name login\nPOST https://a.test");

        assert_eq!(file.requests[0].definition.name, "login");
        assert_eq!(file.requests[0].definition.id.as_str(), "demo/login");
    }

    #[test]
    fn without_the_directive_the_name_comes_from_the_title() {
        let file = parse_ok("### Sign in\nPOST https://a.test");

        assert_eq!(file.requests[0].definition.name, "Sign in");
        assert_eq!(file.requests[0].definition.id.as_str(), "demo/sign-in");
    }

    #[test]
    fn with_no_title_or_directive_the_position_is_used() {
        let file = parse_ok("GET https://a.test\n\n### \nGET https://b.test");

        assert_eq!(file.requests[0].definition.id.as_str(), "demo/request-1");
        assert_eq!(file.requests[1].definition.id.as_str(), "demo/request-2");
    }

    #[test]
    fn reads_the_description_from_the_metadata() {
        let file = parse_ok("### X\n# @description Checks the health endpoint\nGET https://a.test");

        assert_eq!(
            file.requests[0].definition.description.as_deref(),
            Some("Checks the health endpoint")
        );
    }

    #[test]
    fn parses_headers() {
        let file = parse_ok(
            "### X\nGET https://a.test\nAccept: application/json\nAuthorization: Bearer {{token}}",
        );

        let headers = &file.requests[0].definition.headers;
        assert_eq!(headers.len(), 2);
        assert_eq!(headers[0].name, "Accept");
        assert_eq!(headers[1].value, "Bearer {{token}}");
    }

    #[test]
    fn the_body_starts_after_the_blank_line() {
        let file =
            parse_ok("### X\nPOST https://a.test\nContent-Type: application/json\n\n{\"a\": 1}");

        assert_eq!(
            file.requests[0].definition.body,
            Body::Json {
                content: "{\"a\": 1}".to_owned()
            }
        );
    }

    #[test]
    fn a_body_without_a_json_content_type_is_text() {
        let file = parse_ok("### X\nPOST https://a.test\n\nhola");

        assert_eq!(
            file.requests[0].definition.body,
            Body::Text {
                content: "hola".to_owned()
            }
        );
    }

    #[test]
    fn the_body_keeps_its_newlines() {
        let file = parse_ok("### X\nPOST https://a.test\n\nlinea1\nlinea2");

        assert_eq!(
            file.requests[0].definition.body,
            Body::Text {
                content: "linea1\nlinea2".to_owned()
            }
        );
    }

    #[test]
    fn a_hash_inside_the_body_is_not_a_comment() {
        let file = parse_ok(
            "### X\nPOST https://a.test\nContent-Type: application/json\n\n{\"color\": \"#fff\"}",
        );

        assert_eq!(
            file.requests[0].definition.body,
            Body::Json {
                content: "{\"color\": \"#fff\"}".to_owned()
            }
        );
    }

    #[test]
    fn with_no_body_the_body_is_empty() {
        let file = parse_ok("### X\nGET https://a.test\nAccept: */*\n");

        assert_eq!(file.requests[0].definition.body, Body::Empty);
    }

    #[test]
    fn the_body_can_come_from_a_file() {
        let file = parse_ok("### X\nPOST https://a.test\n\n< ./cuerpo.json");

        let reference = file.requests[0].body_ref.as_ref().unwrap();
        assert_eq!(reference.path, "./cuerpo.json");
        assert!(!reference.interpolate);
        // The parser never touches the disk: whoever resolves the path
        // supplies the body.
        assert_eq!(file.requests[0].definition.body, Body::Empty);
    }

    #[test]
    fn the_at_sign_asks_for_the_file_to_be_interpolated() {
        let file = parse_ok("### X\nPOST https://a.test\n\n<@ ./cuerpo.json");

        assert!(file.requests[0].body_ref.as_ref().unwrap().interpolate);
    }

    #[test]
    fn accepts_the_encoding_attached_to_the_at_sign() {
        let file = parse_ok("### X\nPOST https://a.test\n\n<@utf8 ./cuerpo.json");

        let reference = file.requests[0].body_ref.as_ref().unwrap();
        assert_eq!(reference.path, "./cuerpo.json");
        assert!(reference.interpolate);
    }

    #[test]
    fn an_encoding_we_cannot_read_is_an_error() {
        let error = parse(
            "### X\nPOST https://a.test\n\n<@latin1 ./cuerpo.json",
            "demo",
        )
        .unwrap_err();

        assert_eq!(error.line, 4);
    }

    #[test]
    fn an_xml_body_is_not_a_file_reference() {
        // `<?xml` starts with `<`, but with no space after it: it is content.
        let file = parse_ok("### X\nPOST https://a.test\n\n<?xml version=\"1.0\"?>\n<a/>");

        assert!(file.requests[0].body_ref.is_none());
        assert_eq!(
            file.requests[0].definition.body,
            Body::Text {
                content: "<?xml version=\"1.0\"?>\n<a/>".to_owned()
            }
        );
    }

    #[test]
    fn the_reference_allows_no_content_after_it() {
        let error = parse(
            "### X\nPOST https://a.test\n\n< ./cuerpo.json\nsobra",
            "demo",
        )
        .unwrap_err();

        assert_eq!(error.line, 4);
    }

    #[test]
    fn a_reference_without_a_path_is_an_error() {
        let error = parse("### X\nPOST https://a.test\n\n< ", "demo").unwrap_err();

        assert_eq!(error.line, 4);
    }

    #[test]
    fn a_lone_less_than_is_content() {
        // With no space after it, it is not a directive but literal body.
        let file = parse_ok("### X\nPOST https://a.test\n\n<");

        assert!(file.requests[0].body_ref.is_none());
        assert_eq!(
            file.requests[0].definition.body,
            Body::Text {
                content: "<".to_owned()
            }
        );
    }

    #[test]
    fn joins_the_lines_of_an_urlencoded_form() {
        let file = parse_ok(
            "### X\nPOST https://a.test/login\n             Content-Type: application/x-www-form-urlencoded\n\n             name=foo\n&password=bar\n&scope=all",
        );

        assert_eq!(
            file.requests[0].definition.body,
            Body::Text {
                content: "name=foo&password=bar&scope=all".to_owned()
            }
        );
    }

    #[test]
    fn the_ampersand_joining_only_applies_to_forms() {
        // In a text body, a line starting with `&` is content.
        let file = parse_ok("### X\nPOST https://a.test\nContent-Type: text/plain\n\na\n&b");

        assert_eq!(
            file.requests[0].definition.body,
            Body::Text {
                content: "a\n&b".to_owned()
            }
        );
    }

    #[test]
    fn the_response_handler_is_not_part_of_the_body() {
        let file = parse_ok(
            "### X\nPOST https://a.test\nContent-Type: application/json\n\n             {\"a\": 1}\n\n> {%\n  client.test(\"ok\", function() {});\n%}",
        );

        assert_eq!(
            file.requests[0].definition.body,
            Body::Json {
                content: "{\"a\": 1}".to_owned()
            }
        );
    }

    #[test]
    fn a_handler_in_an_external_file_is_not_body_either() {
        let file = parse_ok(
            "### X\nPOST https://a.test\nContent-Type: application/json\n\n             {\"a\": 1}\n\n> ./handler.js",
        );

        assert_eq!(
            file.requests[0].definition.body,
            Body::Json {
                content: "{\"a\": 1}".to_owned()
            }
        );
    }

    #[test]
    fn a_body_starting_with_greater_than_is_still_a_body() {
        // With no space or `{%` after it, it is not a handler: a quote, a diff.
        let file = parse_ok("### X\nPOST https://a.test\n\n>quote\n>another");

        assert_eq!(
            file.requests[0].definition.body,
            Body::Text {
                content: ">quote\n>another".to_owned()
            }
        );
    }

    #[test]
    fn a_pre_request_script_does_not_break_the_block() {
        let file = parse_ok(
            "### X\n# @name pre\n< {%\n  request.variables.set(\"x\", 1)\n%}\n             GET https://a.test/x",
        );

        assert_eq!(file.requests.len(), 1);
        assert_eq!(file.requests[0].definition.url, "https://a.test/x");
        assert!(file.requests[0].body_ref.is_none());
    }

    #[test]
    fn a_one_line_pre_request_script_is_skipped_too() {
        let file = parse_ok("### X\n< {% request.variables.set(\"x\", 1) %}\nGET https://a.test");

        assert_eq!(file.requests[0].definition.url, "https://a.test");
    }

    #[test]
    fn a_pre_request_script_does_not_break_sibling_requests() {
        // This is what actually mattered: a file using a feature we do not
        // support used to render the rest of the collection unusable.
        let file = parse_ok(
            "### With a script\n< {%\n  x\n%}\nGET https://a.test/1\n\n             ### Without one\nGET https://a.test/2\n",
        );

        assert_eq!(file.requests.len(), 2);
        assert_eq!(file.requests[1].definition.url, "https://a.test/2");
    }

    #[test]
    fn continues_the_url_on_indented_lines() {
        let file = parse_ok("### X\nGET https://a.test/users\n  ?limit=10\n  &page=2");

        assert_eq!(
            file.requests[0].definition.url,
            "https://a.test/users?limit=10&page=2"
        );
    }

    #[test]
    fn ignores_comments_of_both_styles() {
        let file = parse_ok("### X\n# a comment\n// another comment\nGET https://a.test");

        assert_eq!(file.requests.len(), 1);
        assert_eq!(file.requests[0].definition.url, "https://a.test");
    }

    #[test]
    fn an_empty_file_has_no_requests() {
        let file = parse_ok("");
        assert!(file.requests.is_empty());
    }

    #[test]
    fn a_file_of_only_comments_has_no_requests() {
        let file = parse_ok("# nothing to see\n\n### \n# nor here\n");
        assert!(file.requests.is_empty());
    }

    #[test]
    fn a_header_without_a_colon_is_an_error() {
        let error = parse("### X\nGET https://a.test\ninvalid-header", "demo").unwrap_err();
        assert_eq!(error.line, 3);
    }

    #[test]
    fn a_declaration_without_an_equals_is_an_error() {
        let error = parse("@broken\n\n### X\nGET https://a.test", "demo").unwrap_err();
        assert_eq!(error.line, 1);
    }

    #[test]
    fn the_identifier_carries_the_files_prefix() {
        let file = parse("### X\n# @name list\nGET https://a.test", "admin/users").unwrap();
        assert_eq!(file.requests[0].definition.id.as_str(), "admin/users/list");
    }

    #[test]
    fn the_slug_normalises_accents_and_punctuation() {
        assert_eq!(slug("Sign in"), "sign-in");
        assert_eq!(slug("Añadir  ítem!"), "anadir-item");
        assert_eq!(slug("¿?"), "request");
    }
}
