//! Presentation: turning engine events into text for the terminal.
//!
//! It is deliberately the only part of the CLI that knows about colours,
//! widths and formatting. A different client (the TUI, the Neovim plugin)
//! replaces this whole module and reuses everything else unchanged.

use std::io::{IsTerminal, Write};

use http_studio_domain::{
    Collection, Environment, ExecutionEvent, HistoryDetail, HistoryEntry, HistoryStats,
    ResolvedRequest, ResponseHead,
};

use crate::cli::OutputFormat;
use crate::info::{self, Info, Workspace};

/// The ANSI codes in use, or empty strings when there is no colour.
struct Palette {
    bold: &'static str,
    dim: &'static str,
    green: &'static str,
    yellow: &'static str,
    red: &'static str,
    reset: &'static str,
}

impl Palette {
    /// The palette with colour.
    const COLOR: Self = Self {
        bold: "\u{1b}[1m",
        dim: "\u{1b}[2m",
        green: "\u{1b}[32m",
        yellow: "\u{1b}[33m",
        red: "\u{1b}[31m",
        reset: "\u{1b}[0m",
    };

    /// The neutral palette, for pipes and for `NO_COLOR`.
    const PLAIN: Self = Self {
        bold: "",
        dim: "",
        green: "",
        yellow: "",
        red: "",
        reset: "",
    };

    /// Decides whether colouring is in order.
    ///
    /// The `NO_COLOR` convention is honoured, and colour is switched off when
    /// the output is not a terminal, so that redirecting to a file does not
    /// litter the result with escape sequences.
    fn detect() -> &'static Self {
        if std::env::var_os("NO_COLOR").is_some() || !std::io::stdout().is_terminal() {
            &Self::PLAIN
        } else {
            &Self::COLOR
        }
    }

    /// The colour associated with an HTTP status code.
    fn for_status(&self, status: u16) -> &'static str {
        match status {
            200..=299 => self.green,
            300..=399 => self.dim,
            400..=599 => self.red,
            _ => self.yellow,
        }
    }
}

/// Writes the output in the chosen format.
pub(crate) struct Renderer {
    format: OutputFormat,
    palette: &'static Palette,
}

impl Renderer {
    /// Creates the renderer for a format.
    #[must_use]
    pub(crate) fn new(format: OutputFormat) -> Self {
        Self {
            format,
            palette: Palette::detect(),
        }
    }

    /// Processes an engine event.
    ///
    /// In `json` mode each event is emitted as an independent JSON line (JSON
    /// Lines), which is what makes the stream consumable from another process
    /// without waiting for the end.
    ///
    /// # Errors
    ///
    /// If writing to standard output fails.
    pub(crate) fn event(
        &self,
        event: &ExecutionEvent,
        out: &mut impl Write,
    ) -> std::io::Result<()> {
        match self.format {
            OutputFormat::Json => {
                let line = serde_json::to_string(event)
                    .unwrap_or_else(|error| format!(r#"{{"event":"failed","message":"{error}"}}"#));
                writeln!(out, "{line}")
            }
            OutputFormat::Pretty => self.pretty_event(event, out),
            // The machine modes react only to the terminal event: a script
            // capturing the output does not want to see progress.
            OutputFormat::Body | OutputFormat::Status | OutputFormat::Headers => {
                self.machine_event(event, out)
            }
            OutputFormat::Silent => Ok(()),
        }
    }

    /// `true` when the listings should come out as JSON.
    ///
    /// `ls`, `env` and `history` have only two presentations that make sense:
    /// readable or JSON. Every machine-oriented format falls into the second,
    /// except `silent`, which prints nothing.
    fn wants_json(&self) -> bool {
        !matches!(self.format, OutputFormat::Pretty | OutputFormat::Silent)
    }

    /// Emits only what a machine mode asks for, once finished.
    ///
    /// Failures go to standard error so as not to contaminate what the script
    /// is capturing: `$(hts ...)` must come back empty when something failed,
    /// not with the error message inside it.
    fn machine_event(&self, event: &ExecutionEvent, out: &mut impl Write) -> std::io::Result<()> {
        match event {
            ExecutionEvent::Completed { exchange } => match self.format {
                OutputFormat::Status => writeln!(out, "{}", exchange.head.status),
                OutputFormat::Headers => self.response_head(&exchange.head, out),
                _ => {
                    let body = exchange.body.as_text();
                    // It is written exactly as it arrived, without
                    // reformatting or adding a newline the body did not have.
                    out.write_all(body.as_bytes())?;
                    if !body.is_empty() && !body.ends_with('\n') {
                        writeln!(out)?;
                    }
                    Ok(())
                }
            },
            ExecutionEvent::Failed { message } => {
                eprintln!("error: {message}");
                Ok(())
            }
            _ => Ok(()),
        }
    }

    /// Renders an event in readable mode.
    ///
    /// The intermediate progress events (`BodyChunk`) are ignored on purpose:
    /// in a CLI they add nothing, whereas a TUI will use them. The engine
    /// emits the same for everyone; each UI chooses what to look at.
    fn pretty_event(&self, event: &ExecutionEvent, out: &mut impl Write) -> std::io::Result<()> {
        let p = self.palette;

        match event {
            ExecutionEvent::Resolved { request } => self.request_line(request, out),
            ExecutionEvent::Head { head } => self.response_head(head, out),
            ExecutionEvent::BodyChunk { .. } => Ok(()),
            ExecutionEvent::Completed { exchange } => {
                let timings = exchange.timings;
                writeln!(
                    out,
                    "{}{} ms · {} bytes{}",
                    p.dim,
                    timings.total.as_millis(),
                    exchange.body.len(),
                    p.reset
                )?;
                writeln!(out)?;
                write_body(&exchange.head, exchange.body.as_text().as_ref(), out)
            }
            ExecutionEvent::Failed { message } => {
                writeln!(out, "{}error:{} {message}", p.red, p.reset)
            }
        }
    }

    /// Prints the request line (`GET https://…`).
    fn request_line(&self, request: &ResolvedRequest, out: &mut impl Write) -> std::io::Result<()> {
        let p = self.palette;
        writeln!(
            out,
            "{}{}{} {}",
            p.bold, request.method, p.reset, request.url
        )
    }

    /// Prints the response's status and headers.
    fn response_head(&self, head: &ResponseHead, out: &mut impl Write) -> std::io::Result<()> {
        let p = self.palette;
        writeln!(
            out,
            "{}{}{} {}{}{}",
            p.for_status(head.status),
            head.status,
            p.reset,
            p.dim,
            head.version,
            p.reset
        )?;

        for header in &head.headers {
            writeln!(out, "{}{}:{} {}", p.dim, header.name, p.reset, header.value)?;
        }

        Ok(())
    }

    /// Shows a resolved request without running it, headers and body included.
    ///
    /// # Errors
    ///
    /// If writing to standard output fails.
    pub(crate) fn preview(
        &self,
        request: &ResolvedRequest,
        out: &mut impl Write,
    ) -> std::io::Result<()> {
        if self.wants_json() {
            let line = serde_json::to_string(request)
                .unwrap_or_else(|error| format!(r#"{{"error":"{error}"}}"#));
            return writeln!(out, "{line}");
        }

        let p = self.palette;
        self.request_line(request, out)?;
        for header in &request.headers {
            writeln!(out, "{}{}:{} {}", p.dim, header.name, p.reset, header.value)?;
        }

        // A preview that did not say TLS verification is off would be an
        // unfaithful preview: it is part of what sending will do.
        if request.options.insecure_tls {
            writeln!(
                out,
                "{}! without verifying the TLS certificate{}",
                p.yellow, p.reset
            )?;
        }

        match &request.body {
            http_studio_domain::Body::Empty => Ok(()),
            body => {
                writeln!(out)?;
                writeln!(out, "{}", body_preview(body))
            }
        }
    }

    /// Lists collections and requests.
    ///
    /// # Errors
    ///
    /// If writing to standard output fails.
    pub(crate) fn collections(
        &self,
        collections: &[Collection],
        out: &mut impl Write,
    ) -> std::io::Result<()> {
        if self.wants_json() {
            let line = serde_json::to_string(collections)
                .unwrap_or_else(|error| format!(r#"{{"error":"{error}"}}"#));
            return writeln!(out, "{line}");
        }

        let p = self.palette;
        for collection in collections {
            writeln!(out, "{}{}{}", p.bold, collection.name, p.reset)?;
            for request in &collection.requests {
                writeln!(
                    out,
                    "  {:<7} {}{}{}  {}{}{}",
                    request.method, p.bold, request.id, p.reset, p.dim, request.name, p.reset
                )?;
            }
        }
        Ok(())
    }

    /// Lists environments with the number of variables each contributes.
    ///
    /// # Errors
    ///
    /// If writing to standard output fails.
    pub(crate) fn environments(
        &self,
        environments: &[Environment],
        out: &mut impl Write,
    ) -> std::io::Result<()> {
        if self.wants_json() {
            let line = serde_json::to_string(environments)
                .unwrap_or_else(|error| format!(r#"{{"error":"{error}"}}"#));
            return writeln!(out, "{line}");
        }

        let p = self.palette;
        for environment in environments {
            writeln!(
                out,
                "{}{}{}  {}{} variables{}",
                p.bold,
                environment.name,
                p.reset,
                p.dim,
                environment.variables.len(),
                p.reset
            )?;
        }
        Ok(())
    }
}

impl Renderer {
    /// Lists the executions recorded in the index.
    ///
    /// # Errors
    ///
    /// If writing to standard output fails.
    pub(crate) fn history(
        &self,
        entries: &[HistoryEntry],
        out: &mut impl Write,
    ) -> std::io::Result<()> {
        if self.wants_json() {
            let line = serde_json::to_string(entries)
                .unwrap_or_else(|error| format!(r#"{{"error":"{error}"}}"#));
            return writeln!(out, "{line}");
        }

        let p = self.palette;
        // The column width is computed from the real data instead of being
        // set by eye: with long identifiers, a constant misaligns the whole
        // table.
        let width = entries
            .iter()
            .map(|entry| entry.request_id.as_str().len())
            .max()
            .unwrap_or(0);

        for entry in entries {
            writeln!(
                out,
                "{}{}{} {}{}{} {:<width$} {}{:>6} ms  {}{}",
                p.for_status(entry.status),
                entry.status,
                p.reset,
                p.dim,
                entry.executed_at,
                p.reset,
                entry.request_id,
                p.dim,
                entry.duration_ms,
                entry.url,
                p.reset,
            )?;
        }
        Ok(())
    }
}

impl Renderer {
    /// Shows the state of the runtime environment.
    ///
    /// # Errors
    ///
    /// If writing to standard output fails.
    pub(crate) fn info(&self, info: &Info, out: &mut impl Write) -> std::io::Result<()> {
        if self.wants_json() {
            return writeln!(out, "{}", info_json(info));
        }

        let p = self.palette;
        writeln!(out, "{}hts{} {}", p.bold, p.reset, info.version)?;

        match &info.workspace {
            Some(workspace) => self.workspace(workspace, out)?,
            None => writeln!(
                out,
                "{}no workspace:{} there is no `collections/` nor \
                 `http-client.env.json` in this directory or above it",
                p.yellow, p.reset
            )?,
        }

        // Only the count of the `HTS_SECRET_*`: a token printed here would
        // end up pasted into the issue where someone shows their `hts info`.
        let private_env = info.workspace.as_ref().map_or_else(String::new, |ws| {
            format!("{}  ·  ", presence(&ws.private_env))
        });
        field(
            "secrets",
            &format!("{private_env}{} HTS_SECRET_*", info.secrets),
            out,
        )?;

        match &info.history {
            Some(history) if history.exists() => field(
                "index",
                &format!(
                    "{}  {}{}{}",
                    info::shorten(&history.path),
                    p.dim,
                    info::human_size(history.size.unwrap_or_default()),
                    p.reset
                ),
                out,
            )?,
            Some(history) => field(
                "index",
                &format!(
                    "{}  {}not created yet{}",
                    info::shorten(&history.path),
                    p.dim,
                    p.reset
                ),
                out,
            )?,
            None => field("index", "not available on this system", out)?,
        }

        Ok(())
    }

    /// Prints the block for the workspace located.
    fn workspace(&self, workspace: &Workspace, out: &mut impl Write) -> std::io::Result<()> {
        let p = self.palette;
        let origin = if workspace.explicit {
            "given with --workspace or HTS_WORKSPACE"
        } else {
            "discovered from the current directory"
        };

        field(
            "workspace",
            &format!("{}  {}{origin}{}", workspace.root.display(), p.dim, p.reset),
            out,
        )?;
        field(
            "collections",
            &workspace.collections.display().to_string(),
            out,
        )?;

        let environments = match &workspace.environments {
            Some(names) if names.is_empty() => "none defined".to_owned(),
            Some(names) => names.join(", "),
            None => "could not be read".to_owned(),
        };
        field(
            "environments",
            &format!(
                "{environments}  {}{}{}",
                p.dim,
                presence(&workspace.public_env),
                p.reset
            ),
            out,
        )
    }
}

/// Writes a `label   value` line with the label aligned.
fn field(label: &str, value: &str, out: &mut impl Write) -> std::io::Result<()> {
    writeln!(out, "{:<13} {value}", format!("{label}:"))
}

/// Describes a file by its name and whether it is there or not.
fn presence(file: &info::FileState) -> String {
    let name = file.path.file_name().map_or_else(
        || file.path.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    );

    if file.exists() {
        name
    } else {
        format!("{name} (does not exist)")
    }
}

/// Serializes the state to JSON, for scripts.
fn info_json(info: &Info) -> String {
    let workspace = info.workspace.as_ref().map(|workspace| {
        serde_json::json!({
            "root": workspace.root,
            "explicit": workspace.explicit,
            "collections": workspace.collections,
            "environments": workspace.environments,
            "env_file": { "path": workspace.public_env.path, "exists": workspace.public_env.exists() },
            "private_env_file": {
                "path": workspace.private_env.path,
                "exists": workspace.private_env.exists(),
            },
        })
    });

    let history = info.history.as_ref().map(|history| {
        serde_json::json!({
            "path": history.path,
            "exists": history.exists(),
            "bytes": history.size,
        })
    });

    serde_json::json!({
        "version": info.version,
        "workspace": workspace,
        "history": history,
        "secrets": info.secrets,
    })
    .to_string()
}

impl Renderer {
    /// Shows a stored execution: what was sent and what answered.
    ///
    /// # Errors
    ///
    /// If writing to standard output fails.
    pub(crate) fn history_detail(
        &self,
        detail: &HistoryDetail,
        out: &mut impl Write,
    ) -> std::io::Result<()> {
        if self.wants_json() {
            let line = serde_json::to_string(detail)
                .unwrap_or_else(|error| format!(r#"{{"error":"{error}"}}"#));
            return writeln!(out, "{line}");
        }

        let p = self.palette;
        writeln!(
            out,
            "{}#{}{} {}{}{}",
            p.dim, detail.entry.id, p.reset, p.dim, detail.entry.executed_at, p.reset
        )?;
        self.request_line(&detail.request, out)?;
        for header in &detail.request.headers {
            writeln!(out, "{}{}:{} {}", p.dim, header.name, p.reset, header.value)?;
        }

        writeln!(out)?;
        self.response_head(&detail.head, out)?;
        writeln!(
            out,
            "{}{} ms · {} bytes{}",
            p.dim, detail.entry.duration_ms, detail.entry.response_bytes, p.reset
        )?;
        writeln!(out)?;

        write_body(&detail.head, detail.body.as_text().as_ref(), out)?;

        // The warning comes **after** the body, which is where it gets read:
        // a body ending mid-brace with no explanation is debugged twice.
        if detail.body_truncated {
            writeln!(
                out,
                "{}… the index stored only the beginning of this response{}",
                p.yellow, p.reset
            )?;
        }

        Ok(())
    }

    /// Summarises what the index holds.
    ///
    /// # Errors
    ///
    /// If writing to standard output fails.
    pub(crate) fn index_stats(
        &self,
        stats: &HistoryStats,
        file_bytes: Option<u64>,
        out: &mut impl Write,
    ) -> std::io::Result<()> {
        if self.wants_json() {
            let line = serde_json::json!({
                "executions": stats.executions,
                "requests": stats.requests,
                "oldest": stats.oldest,
                "newest": stats.newest,
                "stored_body_bytes": stats.stored_body_bytes,
                "file_bytes": file_bytes,
            });
            return writeln!(out, "{line}");
        }

        field("executions", &stats.executions.to_string(), out)?;
        field("requests", &stats.requests.to_string(), out)?;

        if let (Some(oldest), Some(newest)) = (&stats.oldest, &stats.newest) {
            field("from", oldest, out)?;
            field("to", newest, out)?;
        }

        field("bodies", &info::human_size(stats.stored_body_bytes), out)?;
        if let Some(bytes) = file_bytes {
            field("file", &info::human_size(bytes), out)?;
        }

        Ok(())
    }

    /// Reports how many executions were deleted.
    ///
    /// # Errors
    ///
    /// If writing to standard output fails.
    pub(crate) fn pruned(&self, deleted: u64, out: &mut impl Write) -> std::io::Result<()> {
        if self.wants_json() {
            return writeln!(out, "{}", serde_json::json!({ "deleted": deleted }));
        }

        let p = self.palette;
        let plural = if deleted == 1 {
            "execution deleted"
        } else {
            "executions deleted"
        };
        writeln!(
            out,
            "{}{deleted}{} {plural} from the index",
            p.bold, p.reset
        )
    }
}

/// Returns a request body's text for the preview.
fn body_preview(body: &http_studio_domain::Body) -> String {
    use http_studio_domain::Body;

    match body {
        Body::Empty => String::new(),
        Body::Text { content } | Body::Json { content } => content.clone(),
        Body::Form { fields } => fields
            .iter()
            .map(|(key, value)| format!("{key}={value}"))
            .collect::<Vec<_>>()
            .join("&"),
    }
}

/// Writes the response body, indenting the JSON when it is JSON.
///
/// It is only reformatted when the `Content-Type` announces it and the content
/// actually parses: a broken response must be seen exactly as it arrived,
/// because that is precisely what is being debugged.
fn write_body(head: &ResponseHead, body: &str, out: &mut impl Write) -> std::io::Result<()> {
    if body.is_empty() {
        return Ok(());
    }

    let is_json = head.headers.iter().any(|header| {
        header.name.eq_ignore_ascii_case("content-type") && header.value.contains("json")
    });

    if is_json
        && let Ok(value) = serde_json::from_str::<serde_json::Value>(body)
        && let Ok(pretty) = serde_json::to_string_pretty(&value)
    {
        return writeln!(out, "{pretty}");
    }

    writeln!(out, "{body}")
}
