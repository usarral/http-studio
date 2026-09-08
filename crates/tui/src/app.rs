//! The application state and its transitions.
//!
//! `App` does not talk to the engine: when an action needs the network or the
//! disk, [`App::apply`] returns a [`Command`] and it is the `runner` that
//! carries it out, handing the result back through the `on_*` methods.
//!
//! That cut is what makes the TUI checkable: this module's tests move cursors,
//! change mode and run commands with `assert_eq!`, with no terminal, no tokio
//! and no workspace on disk.

use std::collections::BTreeSet;

use http_studio_domain::{
    Collection, Environment, ExecutionEvent, HttpMethod, RequestId, ResolvedRequest,
};

use crate::keymap::{Action, Keymap, Mode, Motion, Pane};

/// A command the `runner` must carry out against the engine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// Reload collections and environments.
    Reload,
    /// Load a request's source text into the editor.
    OpenSource(RequestId),
    /// Run the request.
    Run(RequestId),
    /// Resolve it without sending it.
    Preview(RequestId),
    /// Save the editor's text.
    Save(RequestId, String),
    /// Abort the execution in flight.
    Cancel,
}

/// The severity of a status-line message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Severity {
    /// Informational.
    #[default]
    Info,
    /// A successful operation.
    Success,
    /// A warning.
    Warning,
    /// An error.
    Error,
}

/// The class of a line in the response pane, so it can be coloured.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LineKind {
    /// Plain text.
    #[default]
    Plain,
    /// Dimmed: headers, metrics.
    Dim,
    /// A 2xx status.
    Success,
    /// An error status or a transport failure.
    Error,
    /// A warning.
    Warning,
    /// A JSON body.
    Json,
}

/// One line of the response pane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResponseLine {
    /// The text to draw.
    pub text: String,
    /// How to colour it.
    pub kind: LineKind,
}

impl ResponseLine {
    /// Builds a line.
    #[must_use]
    pub fn new(text: impl Into<String>, kind: LineKind) -> Self {
        Self {
            text: text.into(),
            kind,
        }
    }
}

/// A visible row of the collection tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TreeRow {
    /// A collection header, with its folded state.
    Collection {
        /// The collection's name.
        name: String,
        /// `true` when it is folded.
        folded: bool,
    },
    /// A request.
    Request {
        /// The identifier.
        id: RequestId,
        /// The HTTP method.
        method: HttpMethod,
        /// A short name to display.
        label: String,
    },
}

/// The editor's buffer.
#[derive(Debug, Clone, Default)]
pub struct Editor {
    /// The block's lines.
    pub lines: Vec<String>,
    /// The cursor's line, starting at 0.
    pub line: usize,
    /// The cursor's column, starting at 0.
    pub col: usize,
    /// The line where the visual selection started.
    pub visual_from: Option<usize>,
    /// `true` when there are unsaved changes.
    pub dirty: bool,
}

impl Editor {
    /// The buffer's whole text.
    #[must_use]
    pub fn text(&self) -> String {
        self.lines.join("\n")
    }

    /// The length of the cursor's line.
    fn line_len(&self) -> usize {
        self.lines.get(self.line).map_or(0, String::len)
    }

    /// The range of lines selected in visual mode, if any.
    #[must_use]
    pub fn visual_range(&self) -> Option<(usize, usize)> {
        let from = self.visual_from?;
        Some((from.min(self.line), from.max(self.line)))
    }
}

/// The response pane's state.
#[derive(Debug, Clone, Default)]
pub struct Response {
    /// The lines accumulated so far.
    pub lines: Vec<ResponseLine>,
    /// The cursor's line.
    pub cursor: usize,
    /// The summary drawn in the pane's title.
    pub summary: String,
    /// `true` while an execution is in flight.
    pub running: bool,
}

/// The application's whole state.
pub struct App {
    /// The active mode.
    pub mode: Mode,
    /// The focused pane.
    pub focus: Pane,
    /// The keyboard's state between keystrokes.
    pub keymap: Keymap,
    /// The collections loaded.
    pub collections: Vec<Collection>,
    /// The environments available.
    pub environments: Vec<Environment>,
    /// The selected environment.
    pub environment: Option<String>,
    /// The names of the folded collections.
    pub folded: BTreeSet<String>,
    /// The selected row of the tree.
    pub tree_cursor: usize,
    /// The request open in the editor.
    pub current: Option<RequestId>,
    /// The editor's buffer.
    pub editor: Editor,
    /// The response pane.
    pub response: Response,
    /// The contents of the command line or of the search.
    pub input: String,
    /// The last pattern searched for.
    pub search: String,
    /// The status line's message.
    pub message: String,
    /// The message's severity.
    pub severity: Severity,
    /// `true` when the variables pane is visible.
    pub show_vars: bool,
    /// The resolved variables: name, value and originating scope.
    pub variables: Vec<(String, String, String)>,
    /// `true` when the pane with the resolved request is visible.
    pub show_request: bool,
    /// The last resolved request, exactly as it was sent or previewed.
    ///
    /// It is the request *after* interpolating variables, which is the one
    /// worth seeing: the editor already shows the one that is written.
    pub request: Option<Box<ResolvedRequest>>,
    /// `true` when it is time to quit.
    pub should_quit: bool,
    /// A pane's usable height, for `Ctrl-d` and `Ctrl-u`.
    pub viewport: usize,
}

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}

impl App {
    /// Creates the empty application, before the workspace is loaded.
    #[must_use]
    pub fn new() -> Self {
        Self {
            mode: Mode::Normal,
            focus: Pane::Tree,
            keymap: Keymap::new(),
            collections: Vec::new(),
            environments: Vec::new(),
            environment: None,
            folded: BTreeSet::new(),
            tree_cursor: 0,
            current: None,
            editor: Editor::default(),
            response: Response::default(),
            input: String::new(),
            search: String::new(),
            message: "press ? or :help for help".to_owned(),
            severity: Severity::Info,
            show_vars: false,
            variables: Vec::new(),
            show_request: false,
            request: None,
            should_quit: false,
            viewport: 20,
        }
    }

    // ── Queries ────────────────────────────────────────────────────────

    /// The tree's visible rows, honouring the folds.
    #[must_use]
    pub fn tree_rows(&self) -> Vec<TreeRow> {
        let mut rows = Vec::new();

        for collection in &self.collections {
            let folded = self.folded.contains(&collection.name);
            rows.push(TreeRow::Collection {
                name: collection.name.clone(),
                folded,
            });

            if folded {
                continue;
            }

            for request in &collection.requests {
                rows.push(TreeRow::Request {
                    id: request.id.clone(),
                    method: request.method,
                    label: request
                        .id
                        .as_str()
                        .rsplit('/')
                        .next()
                        .unwrap_or(request.name.as_str())
                        .to_owned(),
                });
            }
        }

        rows
    }

    /// The number of lines in the focused pane.
    fn focused_len(&self) -> usize {
        match self.focus {
            Pane::Tree => self.tree_rows().len(),
            Pane::Editor => self.editor.lines.len(),
            Pane::Response => self.response.lines.len(),
        }
    }

    /// The focused pane's cursor.
    fn focused_cursor(&self) -> usize {
        match self.focus {
            Pane::Tree => self.tree_cursor,
            Pane::Editor => self.editor.line,
            Pane::Response => self.response.cursor,
        }
    }

    /// Places the focused pane's cursor, clamping it.
    fn set_cursor(&mut self, value: usize) {
        let last = self.focused_len().saturating_sub(1);
        let value = value.min(last);

        match self.focus {
            Pane::Tree => self.tree_cursor = value,
            Pane::Editor => {
                self.editor.line = value;
                // On changing line the cursor cannot stay past its end,
                // just as in vim.
                self.editor.col = self.editor.col.min(self.editor.line_len());
            }
            Pane::Response => self.response.cursor = value,
        }
    }

    // ── Input ──────────────────────────────────────────────────────────

    /// Translates a keystroke and applies the resulting action.
    pub fn on_key(&mut self, key: crossterm::event::KeyEvent) -> Option<Command> {
        let mut keymap = std::mem::take(&mut self.keymap);
        let action = keymap.feed(self.mode, key);
        self.keymap = keymap;
        self.apply(action)
    }

    /// Applies an already translated action.
    #[expect(
        clippy::too_many_lines,
        reason = "it is the action dispatcher: splitting it forces jumping \
                  between functions to follow a single keystroke"
    )]
    pub fn apply(&mut self, action: Action) -> Option<Command> {
        match action {
            Action::Nothing => None,

            Action::Motion(motion, count) => {
                self.motion(motion, count);
                None
            }

            Action::CyclePane(forward) => {
                self.focus = match (self.focus, forward) {
                    (Pane::Tree, true) | (Pane::Response, false) => Pane::Editor,
                    (Pane::Editor, true) | (Pane::Tree, false) => Pane::Response,
                    (Pane::Response, true) | (Pane::Editor, false) => Pane::Tree,
                };
                None
            }

            Action::Focus(pane) => {
                self.focus = pane;
                None
            }

            Action::SetMode(mode) => {
                match mode {
                    Mode::Insert if self.focus != Pane::Editor => {
                        self.warn("editing only happens in the request pane");
                        return None;
                    }
                    Mode::Visual if self.focus == Pane::Editor => {
                        self.editor.visual_from = Some(self.editor.line);
                    }
                    Mode::Command | Mode::Search => self.input.clear(),
                    _ => {}
                }
                self.mode = mode;
                None
            }

            Action::InsertAfter => {
                if self.focus == Pane::Editor {
                    self.editor.col = (self.editor.col + 1).min(self.editor.line_len());
                    self.mode = Mode::Insert;
                }
                None
            }

            Action::OpenLineBelow => {
                if self.focus == Pane::Editor {
                    let at = (self.editor.line + 1).min(self.editor.lines.len());
                    self.editor.lines.insert(at, String::new());
                    self.editor.line = at;
                    self.editor.col = 0;
                    self.editor.dirty = true;
                    self.mode = Mode::Insert;
                }
                None
            }

            Action::Confirm => self.confirm(),
            Action::Run => self.current.clone().map(Command::Run),
            Action::Preview => self.current.clone().map(Command::Preview),
            Action::Cancel => {
                if self.response.running {
                    Some(Command::Cancel)
                } else {
                    self.info("there is no execution in flight");
                    None
                }
            }
            Action::Save => self.save(),

            Action::ToggleVars => {
                self.show_vars = !self.show_vars;
                if self.show_vars && self.variables.is_empty() {
                    // The variables are only known once resolved, so asking
                    // for a preview is how the pane gets populated.
                    return self.current.clone().map(Command::Preview);
                }
                None
            }

            Action::ToggleRequest => {
                self.show_request = !self.show_request;
                if self.show_request && self.request.is_none() {
                    // As with the variables: the resolved request does not
                    // exist until someone resolves it.
                    return self.current.clone().map(Command::Preview);
                }
                None
            }

            Action::CycleEnv => {
                self.cycle_environment();
                None
            }

            Action::SearchAgain(forward) => {
                self.search_again(forward);
                None
            }

            Action::Char(c) => {
                match self.mode {
                    Mode::Command | Mode::Search => self.input.push(c),
                    Mode::Insert => self.insert_char(c),
                    _ => {}
                }
                None
            }

            Action::Backspace => {
                match self.mode {
                    Mode::Command | Mode::Search => {
                        if self.input.pop().is_none() {
                            self.mode = Mode::Normal;
                        }
                    }
                    Mode::Insert => self.backspace(),
                    _ => {}
                }
                None
            }

            Action::Newline => {
                if self.mode == Mode::Insert {
                    self.split_line();
                }
                None
            }

            Action::Submit => self.submit(),

            Action::Escape => {
                if self.mode == Mode::Insert {
                    self.editor.col = self.editor.col.saturating_sub(1);
                }
                self.mode = Mode::Normal;
                self.editor.visual_from = None;
                self.input.clear();
                self.keymap.reset();
                None
            }

            Action::Help => {
                self.show_help();
                None
            }

            Action::Quit => self.quit(),
        }
    }

    // ── Movement ───────────────────────────────────────────────────────

    /// Applies a motion `count` times.
    fn motion(&mut self, motion: Motion, count: usize) {
        let cursor = self.focused_cursor();

        match motion {
            Motion::Down => self.set_cursor(cursor.saturating_add(count)),
            Motion::Up => self.set_cursor(cursor.saturating_sub(count)),
            Motion::HalfPageDown => self.set_cursor(cursor.saturating_add(self.viewport / 2)),
            Motion::HalfPageUp => self.set_cursor(cursor.saturating_sub(self.viewport / 2)),
            Motion::FirstLine => self.set_cursor(0),
            Motion::LastLine => self.set_cursor(usize::MAX),
            // The user counts lines from 1; the buffer from 0.
            Motion::ToLine(n) => self.set_cursor(n.saturating_sub(1)),

            Motion::Left => {
                if self.focus == Pane::Editor {
                    self.editor.col = self.editor.col.saturating_sub(count);
                } else {
                    self.apply(Action::CyclePane(false));
                }
            }
            Motion::Right => {
                if self.focus == Pane::Editor {
                    let limit = self.editor.line_len().saturating_sub(1);
                    self.editor.col = self.editor.col.saturating_add(count).min(limit);
                } else {
                    self.apply(Action::CyclePane(true));
                }
            }
            Motion::LineStart => self.editor.col = 0,
            Motion::LineEnd => self.editor.col = self.editor.line_len().saturating_sub(1),
            Motion::WordForward => self.word(count, true),
            Motion::WordBack => self.word(count, false),
        }
    }

    /// Moves the cursor by words within the current line.
    fn word(&mut self, count: usize, forward: bool) {
        if self.focus != Pane::Editor {
            return;
        }

        let Some(line) = self.editor.lines.get(self.editor.line) else {
            return;
        };

        // The start of each word on the line, as byte indices.
        let starts: Vec<usize> = line
            .char_indices()
            .filter(|(index, c)| {
                !c.is_whitespace()
                    && (*index == 0
                        || line[..*index]
                            .chars()
                            .next_back()
                            .is_some_and(char::is_whitespace))
            })
            .map(|(index, _)| index)
            .collect();

        for _ in 0..count {
            let col = self.editor.col;
            self.editor.col = if forward {
                starts
                    .iter()
                    .copied()
                    .find(|start| *start > col)
                    .unwrap_or_else(|| line.len().saturating_sub(1))
            } else {
                starts
                    .iter()
                    .copied()
                    .rev()
                    .find(|start| *start < col)
                    .unwrap_or(0)
            };
        }
    }

    // ── Editing ────────────────────────────────────────────────────────

    /// Inserts a character at the cursor.
    fn insert_char(&mut self, c: char) {
        let col = self.editor.col.min(self.editor.line_len());
        if let Some(line) = self.editor.lines.get_mut(self.editor.line) {
            line.insert(col, c);
            self.editor.col = col + c.len_utf8();
            self.editor.dirty = true;
        }
    }

    /// Deletes the preceding character, joining lines when at the start.
    fn backspace(&mut self) {
        if self.editor.col == 0 {
            if self.editor.line == 0 {
                return;
            }
            let removed = self.editor.lines.remove(self.editor.line);
            self.editor.line -= 1;
            self.editor.col = self.editor.line_len();
            if let Some(line) = self.editor.lines.get_mut(self.editor.line) {
                line.push_str(&removed);
            }
            self.editor.dirty = true;
            return;
        }

        // It steps back to the previous character boundary so a multibyte
        // character is not split in half.
        if let Some(line) = self.editor.lines.get_mut(self.editor.line) {
            let mut at = self.editor.col.min(line.len());
            while at > 0 && !line.is_char_boundary(at - 1) {
                at -= 1;
            }
            let start = at.saturating_sub(1);
            if line.is_char_boundary(start) && start < line.len() {
                line.remove(start);
                self.editor.col = start;
                self.editor.dirty = true;
            }
        }
    }

    /// Splits the current line at the cursor.
    fn split_line(&mut self) {
        let col = self.editor.col.min(self.editor.line_len());
        if let Some(line) = self.editor.lines.get_mut(self.editor.line) {
            let rest = line.split_off(col);
            self.editor.lines.insert(self.editor.line + 1, rest);
            self.editor.line += 1;
            self.editor.col = 0;
            self.editor.dirty = true;
        }
    }

    // ── Compound actions ───────────────────────────────────────────────

    /// `Enter`: opens in the tree, runs in the editor.
    fn confirm(&mut self) -> Option<Command> {
        match self.focus {
            Pane::Tree => match self.tree_rows().get(self.tree_cursor)? {
                TreeRow::Collection { name, .. } => {
                    if !self.folded.remove(name) {
                        self.folded.insert(name.clone());
                    }
                    None
                }
                TreeRow::Request { id, .. } => Some(Command::OpenSource(id.clone())),
            },
            Pane::Editor => self.current.clone().map(Command::Run),
            Pane::Response => None,
        }
    }

    /// Saves the buffer if there is anything to save.
    fn save(&mut self) -> Option<Command> {
        let id = self.current.clone()?;
        if !self.editor.dirty {
            self.info("there are no changes to save");
            return None;
        }
        Some(Command::Save(id, self.editor.text()))
    }

    /// Quits, warning when unsaved changes remain.
    fn quit(&mut self) -> Option<Command> {
        if self.editor.dirty {
            self.warn("there are unsaved changes: use :w, or :q! to discard them");
            return None;
        }
        self.should_quit = true;
        None
    }

    /// Moves to the next environment in the list.
    fn cycle_environment(&mut self) {
        if self.environments.is_empty() {
            self.warn("the workspace defines no environments");
            return;
        }

        let position = self
            .environment
            .as_ref()
            .and_then(|current| {
                self.environments
                    .iter()
                    .position(|env| &env.name == current)
            })
            .map_or(0, |index| (index + 1) % self.environments.len());

        if let Some(env) = self.environments.get(position) {
            self.environment = Some(env.name.clone());
            self.variables.clear();
            self.request = None;
            self.success(format!("environment → {}", env.name));
        }
    }

    /// Runs the command line or the search.
    fn submit(&mut self) -> Option<Command> {
        let input = std::mem::take(&mut self.input);
        let mode = self.mode;
        self.mode = Mode::Normal;

        if mode == Mode::Search {
            self.search = input;
            self.search_again(true);
            return None;
        }

        self.command(&input)
    }

    /// Interprets a `:` command.
    fn command(&mut self, input: &str) -> Option<Command> {
        let mut parts = input.split_whitespace();
        let name = parts.next().unwrap_or_default();
        let argument = parts.collect::<Vec<_>>().join(" ");

        match name {
            "" => None,
            "run" | "r" => self.current.clone().map(Command::Run),
            "preview" | "p" => self.current.clone().map(Command::Preview),
            "cancel" => Some(Command::Cancel),
            "reload" | "e" => Some(Command::Reload),
            "w" | "write" => self.save(),
            "q" | "quit" => self.quit(),
            "q!" | "quit!" => {
                self.should_quit = true;
                None
            }
            "wq" => {
                let command = self.save();
                self.should_quit = command.is_none();
                command
            }
            "vars" | "v" => self.apply(Action::ToggleVars),
            "request" | "req" => self.apply(Action::ToggleRequest),
            "help" | "h" => {
                self.show_help();
                None
            }
            "env" => {
                if argument.is_empty() {
                    let names: Vec<&str> = self
                        .environments
                        .iter()
                        .map(|env| env.name.as_str())
                        .collect();
                    self.info(format!("environments: {}", names.join(", ")));
                } else if self.environments.iter().any(|env| env.name == argument) {
                    self.environment = Some(argument.clone());
                    self.variables.clear();
                    self.request = None;
                    self.success(format!("environment → {argument}"));
                } else {
                    self.error(format!("there is no environment `{argument}`"));
                }
                None
            }
            other => {
                self.error(format!("unknown command: `{other}`"));
                None
            }
        }
    }

    /// Searches for the current pattern from the editor's cursor.
    fn search_again(&mut self, forward: bool) {
        if self.search.is_empty() {
            self.warn("there is no search pattern");
            return;
        }

        let needle = self.search.to_lowercase();
        let total = self.editor.lines.len();
        if total == 0 {
            return;
        }

        for step in 1..=total {
            let offset = if forward { step } else { total - step };
            let index = (self.editor.line + offset) % total;

            if let Some(line) = self.editor.lines.get(index)
                && let Some(column) = line.to_lowercase().find(&needle)
            {
                self.focus = Pane::Editor;
                self.editor.line = index;
                self.editor.col = column;
                self.info(format!("/{}", self.search));
                return;
            }
        }

        self.error(format!("pattern not found: {}", self.search));
    }

    /// Dumps the help into the response pane.
    fn show_help(&mut self) {
        const HELP: &[(&str, &str)] = &[
            ("MOVEMENT", ""),
            ("h j k l", "move the cursor"),
            ("3j", "count + motion"),
            ("gg / G / {n}G", "first / last / line n"),
            ("w / b / 0 / $", "word, and start and end of line"),
            ("Ctrl-d / Ctrl-u", "half a page"),
            ("", ""),
            ("PANES", ""),
            ("Tab / Ctrl-w h l", "change pane"),
            ("Enter", "open a request, or fold the collection"),
            ("", ""),
            ("EDITING", ""),
            ("i / a / o", "insert"),
            ("v", "visual by lines"),
            ("Esc", "back to normal"),
            ("Ctrl-Enter", "run without leaving insert mode"),
            ("", ""),
            ("SEARCH", ""),
            ("/pattern, n, N", "search and repeat"),
            ("", ""),
            ("LEADER (space)", ""),
            ("<Space>r / p", "run / preview"),
            ("<Space>v / e", "variables / environment"),
            ("<Space>i", "see the resolved request"),
            ("<Space>w / c", "save / cancel"),
            ("", ""),
            ("COMMANDS", ""),
            (":run :preview :cancel", ""),
            (":env <name>", ""),
            (":vars :request :reload", ""),
            (":help", ""),
            (":w :q :q! :wq", ""),
        ];

        self.response.lines = HELP
            .iter()
            .map(|(key, description)| {
                if description.is_empty() {
                    ResponseLine::new(*key, LineKind::Warning)
                } else {
                    ResponseLine::new(format!("  {key:<22} {description}"), LineKind::Plain)
                }
            })
            .collect();

        "help".clone_into(&mut self.response.summary);
        self.response.cursor = 0;
        self.focus = Pane::Response;
    }

    // ── The results the runner hands back ──────────────────────────────

    /// Receives the freshly loaded workspace.
    pub fn on_workspace(&mut self, collections: Vec<Collection>, environments: Vec<Environment>) {
        self.collections = collections;
        self.environments = environments;

        if self.environment.is_none() {
            self.environment = self.environments.first().map(|env| env.name.clone());
        }

        let requests: usize = self.collections.iter().map(|c| c.requests.len()).sum();
        let collections = self.collections.len();
        self.info(format!(
            "{requests} {} in {collections} {}",
            plural(requests, "request", "requests"),
            plural(collections, "collection", "collections")
        ));
    }

    /// Receives the source text of the request being opened.
    pub fn on_source(&mut self, id: RequestId, source: &str) {
        // `str::lines()` discards the trailing empty line, and on saving that
        // would eat the blank separation between the file's blocks. `split`
        // preserves the exact structure, which is what an editor demands.
        self.editor.lines = source
            .split('\n')
            .map(|line| line.strip_suffix('\r').unwrap_or(line).to_owned())
            .collect();
        if self.editor.lines.is_empty() {
            self.editor.lines.push(String::new());
        }

        // The cursor lands on the request line, which is where one wants to be.
        self.editor.line = self
            .editor
            .lines
            .iter()
            .position(|line| {
                let trimmed = line.trim_start();
                !trimmed.is_empty()
                    && !trimmed.starts_with('#')
                    && !trimmed.starts_with("//")
                    && !trimmed.starts_with('@')
            })
            .unwrap_or(0);

        self.editor.col = 0;
        self.editor.dirty = false;
        self.editor.visual_from = None;
        self.current = Some(id);
        self.focus = Pane::Editor;
        self.request = None;
        self.response.lines.clear();
        self.response.summary.clear();
    }

    /// Confirms that it was saved.
    pub fn on_saved(&mut self) {
        self.editor.dirty = false;
        self.success("saved");
    }

    /// Receives a preview.
    pub fn on_preview(
        &mut self,
        request: &ResolvedRequest,
        variables: Vec<(String, String, String)>,
    ) {
        self.variables = variables;
        self.request = Some(Box::new(request.clone()));
        self.response.lines = vec![
            ResponseLine::new("preview · nothing has been sent", LineKind::Warning),
            ResponseLine::new(String::new(), LineKind::Plain),
            ResponseLine::new(
                format!("{} {}", request.method, request.url),
                LineKind::Plain,
            ),
        ];

        for header in &request.headers {
            self.response.lines.push(ResponseLine::new(
                format!("{}: {}", header.name, header.value),
                LineKind::Dim,
            ));
        }

        if let Some(body) = body_text(&request.body) {
            self.response
                .lines
                .push(ResponseLine::new(String::new(), LineKind::Plain));
            for line in body.lines() {
                self.response
                    .lines
                    .push(ResponseLine::new(line, LineKind::Json));
            }
        }

        "preview".clone_into(&mut self.response.summary);
        self.response.cursor = 0;
        self.focus = Pane::Response;
    }

    /// Receives an execution event from the engine.
    ///
    /// It is the same sequence the CLI consumes; here it is drawn incrementally
    /// instead of waiting for the end, which is precisely why the engine emits
    /// a stream and not a result.
    pub fn on_execution(&mut self, event: &ExecutionEvent) {
        match event {
            ExecutionEvent::Resolved { request } => {
                self.request = Some(request.clone());
                self.response.running = true;
                self.response.cursor = 0;
                "in flight".clone_into(&mut self.response.summary);
                self.response.lines = vec![
                    ResponseLine::new(
                        format!("{} {}", request.method, request.url),
                        LineKind::Plain,
                    ),
                    ResponseLine::new(String::new(), LineKind::Plain),
                    ResponseLine::new("connecting…", LineKind::Dim),
                ];
            }

            ExecutionEvent::Head { head } => {
                self.response.lines.pop();
                let kind = if head.is_success() {
                    LineKind::Success
                } else {
                    LineKind::Error
                };
                self.response.lines.push(ResponseLine::new(
                    format!("{} {}", head.status, head.version),
                    kind,
                ));
                for header in &head.headers {
                    self.response.lines.push(ResponseLine::new(
                        format!("{}: {}", header.name, header.value),
                        LineKind::Dim,
                    ));
                }
                self.response
                    .lines
                    .push(ResponseLine::new(String::new(), LineKind::Plain));
                self.response.summary = format!("{}", head.status);
            }

            ExecutionEvent::BodyChunk { len } => {
                self.response.summary = format!("{len} B…");
            }

            ExecutionEvent::Completed { exchange } => {
                for line in exchange.body.as_text().lines() {
                    self.response
                        .lines
                        .push(ResponseLine::new(line, LineKind::Json));
                }
                self.response
                    .lines
                    .push(ResponseLine::new(String::new(), LineKind::Plain));
                self.response.lines.push(ResponseLine::new(
                    format!(
                        "{} ms · {} bytes",
                        exchange.timings.total.as_millis(),
                        exchange.body.len()
                    ),
                    LineKind::Dim,
                ));

                self.response.running = false;
                self.response.summary = format!(
                    "{} · {} ms",
                    exchange.head.status,
                    exchange.timings.total.as_millis()
                );

                if exchange.head.is_success() {
                    self.success("completed");
                } else {
                    self.error(format!("response {}", exchange.head.status));
                }
                self.focus = Pane::Response;
            }

            ExecutionEvent::Failed { message } => {
                self.response.running = false;
                self.response
                    .lines
                    .push(ResponseLine::new(message.clone(), LineKind::Error));
                "failed".clone_into(&mut self.response.summary);
                self.error(message.clone());
            }
        }
    }

    // ── Messages ───────────────────────────────────────────────────────

    /// An informational message.
    pub fn info(&mut self, text: impl Into<String>) {
        self.message = text.into();
        self.severity = Severity::Info;
    }

    /// A success message.
    pub fn success(&mut self, text: impl Into<String>) {
        self.message = text.into();
        self.severity = Severity::Success;
    }

    /// A warning.
    pub fn warn(&mut self, text: impl Into<String>) {
        self.message = text.into();
        self.severity = Severity::Warning;
    }

    /// An error.
    pub fn error(&mut self, text: impl Into<String>) {
        self.message = text.into();
        self.severity = Severity::Error;
    }
}

/// Picks the singular or the plural according to the count.
fn plural<'a>(count: usize, one: &'a str, many: &'a str) -> &'a str {
    if count == 1 { one } else { many }
}

/// The body text of a resolved request, when it has one.
pub(crate) fn body_text(body: &http_studio_domain::Body) -> Option<String> {
    use http_studio_domain::Body;

    match body {
        Body::Empty => None,
        Body::Text { content } | Body::Json { content } => Some(content.clone()),
        Body::Form { fields } => Some(
            fields
                .iter()
                .map(|(key, value)| format!("{key}={value}"))
                .collect::<Vec<_>>()
                .join("&"),
        ),
    }
}

#[cfg(test)]
mod tests {
    // In tests, `unwrap` documents the expectation and its panic IS the failure.
    #![allow(clippy::unwrap_used)]

    use super::*;
    use http_studio_domain::{Body, Header, RequestDefinition, RequestOptions};
    use std::collections::BTreeMap;

    fn request(id: &str, method: HttpMethod) -> RequestDefinition {
        RequestDefinition {
            id: RequestId::new(id),
            name: id.to_owned(),
            method,
            url: "{{base_url}}/x".to_owned(),
            headers: vec![Header::new("Accept", "application/json")],
            query: vec![],
            body: Body::Empty,
            variables: BTreeMap::new(),
            description: None,
            options: RequestOptions::default(),
        }
    }

    fn app() -> App {
        let mut app = App::new();
        app.on_workspace(
            vec![
                Collection {
                    name: "auth".to_owned(),
                    variables: BTreeMap::new(),
                    requests: vec![
                        request("auth/login", HttpMethod::Post),
                        request("auth/refresh", HttpMethod::Post),
                    ],
                },
                Collection {
                    name: "echo".to_owned(),
                    variables: BTreeMap::new(),
                    requests: vec![request("echo/get", HttpMethod::Get)],
                },
            ],
            vec![
                Environment {
                    name: "dev".to_owned(),
                    variables: BTreeMap::new(),
                },
                Environment {
                    name: "prod".to_owned(),
                    variables: BTreeMap::new(),
                },
            ],
        );
        app
    }

    #[test]
    fn the_tree_interleaves_collections_and_requests() {
        let rows = app().tree_rows();

        assert_eq!(rows.len(), 5);
        assert!(matches!(rows[0], TreeRow::Collection { .. }));
        assert!(matches!(rows[1], TreeRow::Request { .. }));
        assert!(matches!(rows[3], TreeRow::Collection { .. }));
    }

    #[test]
    fn folding_a_collection_hides_its_requests() {
        let mut app = app();
        app.apply(Action::Confirm);

        assert_eq!(app.tree_rows().len(), 3);
    }

    #[test]
    fn unfolding_shows_them_again() {
        let mut app = app();
        app.apply(Action::Confirm);
        app.apply(Action::Confirm);

        assert_eq!(app.tree_rows().len(), 5);
    }

    #[test]
    fn the_trees_cursor_does_not_run_off_the_bottom() {
        let mut app = app();
        app.apply(Action::Motion(Motion::Down, 99));

        assert_eq!(app.tree_cursor, 4);
    }

    #[test]
    fn the_cursor_does_not_run_off_the_top() {
        let mut app = app();
        app.apply(Action::Motion(Motion::Up, 99));

        assert_eq!(app.tree_cursor, 0);
    }

    #[test]
    fn the_count_moves_several_rows() {
        let mut app = app();
        app.apply(Action::Motion(Motion::Down, 3));

        assert_eq!(app.tree_cursor, 3);
    }

    #[test]
    fn capital_g_goes_to_the_end() {
        let mut app = app();
        app.apply(Action::Motion(Motion::LastLine, 1));

        assert_eq!(app.tree_cursor, 4);
    }

    #[test]
    fn opening_a_request_asks_for_its_source() {
        let mut app = app();
        app.apply(Action::Motion(Motion::Down, 1));

        assert_eq!(
            app.apply(Action::Confirm),
            Some(Command::OpenSource(RequestId::new("auth/login")))
        );
    }

    #[test]
    fn on_loading_the_source_the_cursor_lands_on_the_request_line() {
        let mut app = app();
        app.on_source(
            RequestId::new("auth/login"),
            "### Login\n# @name login\n@email = a@b.com\nPOST {{base_url}}/login\nAccept: */*",
        );

        assert_eq!(app.editor.line, 3);
        assert_eq!(app.focus, Pane::Editor);
        assert!(!app.editor.dirty);
    }

    #[test]
    fn the_round_trip_preserves_the_trailing_blank_line() {
        // It is the one separating one `###` block from the next: losing it
        // on save would glue two requests together in the file.
        let mut app = app();
        let source = "### X\nGET https://a.test\n";
        app.on_source(RequestId::new("echo/get"), source);

        assert_eq!(app.editor.text(), source);
    }

    #[test]
    fn the_round_trip_preserves_windows_line_endings() {
        let mut app = app();
        app.on_source(RequestId::new("echo/get"), "### X\r\nGET https://a.test");

        assert_eq!(app.editor.lines, vec!["### X", "GET https://a.test"]);
    }

    #[test]
    fn the_load_message_agrees_in_number() {
        let mut app = App::new();
        app.on_workspace(
            vec![Collection {
                name: "only".to_owned(),
                variables: BTreeMap::new(),
                requests: vec![request("only/one", HttpMethod::Get)],
            }],
            vec![],
        );

        assert_eq!(app.message, "1 request in 1 collection");
    }

    #[test]
    fn tab_cycles_through_the_three_panes() {
        let mut app = app();

        app.apply(Action::CyclePane(true));
        assert_eq!(app.focus, Pane::Editor);
        app.apply(Action::CyclePane(true));
        assert_eq!(app.focus, Pane::Response);
        app.apply(Action::CyclePane(true));
        assert_eq!(app.focus, Pane::Tree);
    }

    #[test]
    fn h_and_l_change_pane_outside_the_editor() {
        let mut app = app();
        app.apply(Action::Motion(Motion::Right, 1));

        assert_eq!(app.focus, Pane::Editor);
    }

    #[test]
    fn h_and_l_move_the_cursor_inside_the_editor() {
        let mut app = app();
        app.on_source(RequestId::new("echo/get"), "GET https://a.test");
        app.apply(Action::Motion(Motion::Right, 4));

        assert_eq!(app.focus, Pane::Editor);
        assert_eq!(app.editor.col, 4);
    }

    #[test]
    fn dollar_goes_to_the_end_of_the_line() {
        let mut app = app();
        app.on_source(RequestId::new("echo/get"), "GET https://a.test");
        app.apply(Action::Motion(Motion::LineEnd, 1));

        assert_eq!(app.editor.col, "GET https://a.test".len() - 1);
    }

    #[test]
    fn w_jumps_to_the_next_word() {
        let mut app = app();
        app.on_source(RequestId::new("echo/get"), "GET https://a.test");
        app.apply(Action::Motion(Motion::WordForward, 1));

        assert_eq!(app.editor.col, 4);
    }

    #[test]
    fn b_goes_back_to_the_previous_word() {
        let mut app = app();
        app.on_source(RequestId::new("echo/get"), "GET https://a.test");
        app.apply(Action::Motion(Motion::LineEnd, 1));
        app.apply(Action::Motion(Motion::WordBack, 1));

        assert_eq!(app.editor.col, 4);
    }

    #[test]
    fn inserting_text_marks_the_buffer_dirty() {
        let mut app = app();
        app.on_source(RequestId::new("echo/get"), "GET https://a.test");
        app.apply(Action::SetMode(Mode::Insert));
        app.apply(Action::Char('X'));

        assert!(app.editor.dirty);
        assert!(app.editor.lines[0].starts_with('X'));
    }

    #[test]
    fn there_is_no_editing_outside_the_request_pane() {
        let mut app = app();
        app.apply(Action::SetMode(Mode::Insert));

        assert_eq!(app.mode, Mode::Normal);
        assert_eq!(app.severity, Severity::Warning);
    }

    #[test]
    fn o_opens_a_line_below_and_enters_insert_mode() {
        let mut app = app();
        app.on_source(RequestId::new("echo/get"), "GET https://a.test");
        app.apply(Action::OpenLineBelow);

        assert_eq!(app.mode, Mode::Insert);
        assert_eq!(app.editor.lines.len(), 2);
        assert_eq!(app.editor.line, 1);
    }

    #[test]
    fn enter_in_insert_mode_splits_the_line() {
        let mut app = app();
        app.on_source(RequestId::new("echo/get"), "GETX");
        app.apply(Action::SetMode(Mode::Insert));
        app.apply(Action::Motion(Motion::Right, 3));
        app.apply(Action::Newline);

        assert_eq!(app.editor.lines, vec!["GET".to_owned(), "X".to_owned()]);
    }

    #[test]
    fn deleting_at_the_start_joins_the_previous_line() {
        let mut app = app();
        app.on_source(RequestId::new("echo/get"), "GET\nX");
        app.apply(Action::Motion(Motion::Down, 1));
        app.apply(Action::SetMode(Mode::Insert));
        app.apply(Action::Backspace);

        assert_eq!(app.editor.lines, vec!["GETX".to_owned()]);
    }

    #[test]
    fn deleting_respects_multibyte_characters() {
        let mut app = app();
        app.on_source(RequestId::new("echo/get"), "ñ");
        app.apply(Action::SetMode(Mode::Insert));
        app.editor.col = "ñ".len();
        app.apply(Action::Backspace);

        assert_eq!(app.editor.lines, vec![String::new()]);
    }

    #[test]
    fn escape_leaves_visual_mode_and_clears_the_selection() {
        let mut app = app();
        app.on_source(RequestId::new("echo/get"), "a\nb\nc");
        app.apply(Action::SetMode(Mode::Visual));
        app.apply(Action::Motion(Motion::Down, 2));

        assert_eq!(app.editor.visual_range(), Some((0, 2)));

        app.apply(Action::Escape);
        assert_eq!(app.mode, Mode::Normal);
        assert_eq!(app.editor.visual_range(), None);
    }

    #[test]
    fn saving_without_changes_emits_no_command() {
        let mut app = app();
        app.on_source(RequestId::new("echo/get"), "GET https://a.test");

        assert_eq!(app.apply(Action::Save), None);
    }

    #[test]
    fn saving_with_changes_emits_the_whole_text() {
        let mut app = app();
        app.on_source(RequestId::new("echo/get"), "GET https://a.test");
        app.apply(Action::SetMode(Mode::Insert));
        app.apply(Action::Char('X'));
        app.apply(Action::Escape);

        assert_eq!(
            app.apply(Action::Save),
            Some(Command::Save(
                RequestId::new("echo/get"),
                "XGET https://a.test".to_owned()
            ))
        );
    }

    #[test]
    fn quitting_with_unsaved_changes_warns_instead_of_quitting() {
        let mut app = app();
        app.on_source(RequestId::new("echo/get"), "GET https://a.test");
        app.apply(Action::SetMode(Mode::Insert));
        app.apply(Action::Char('X'));
        app.apply(Action::Escape);
        app.apply(Action::Quit);

        assert!(!app.should_quit);
        assert_eq!(app.severity, Severity::Warning);
    }

    #[test]
    fn a_forced_quit_leaves_even_with_changes() {
        let mut app = app();
        app.on_source(RequestId::new("echo/get"), "GET https://a.test");
        app.editor.dirty = true;
        app.command("q!");

        assert!(app.should_quit);
    }

    #[test]
    fn the_leader_cycles_the_environment() {
        let mut app = app();
        assert_eq!(app.environment.as_deref(), Some("dev"));

        app.apply(Action::CycleEnv);
        assert_eq!(app.environment.as_deref(), Some("prod"));

        app.apply(Action::CycleEnv);
        assert_eq!(app.environment.as_deref(), Some("dev"));
    }

    #[test]
    fn the_env_command_selects_by_name() {
        let mut app = app();
        app.command("env prod");

        assert_eq!(app.environment.as_deref(), Some("prod"));
        assert_eq!(app.severity, Severity::Success);
    }

    #[test]
    fn the_env_command_rejects_a_name_that_does_not_exist() {
        let mut app = app();
        app.command("env staging");

        assert_eq!(app.environment.as_deref(), Some("dev"));
        assert_eq!(app.severity, Severity::Error);
    }

    #[test]
    fn an_unknown_command_warns() {
        let mut app = app();
        app.command("made-up");

        assert_eq!(app.severity, Severity::Error);
    }

    #[test]
    fn the_run_command_emits_the_command_for_the_open_request() {
        let mut app = app();
        app.on_source(RequestId::new("echo/get"), "GET https://a.test");

        assert_eq!(
            app.command("run"),
            Some(Command::Run(RequestId::new("echo/get")))
        );
    }

    #[test]
    fn with_no_request_open_run_emits_nothing() {
        let mut app = app();
        assert_eq!(app.command("run"), None);
    }

    #[test]
    fn the_search_moves_the_cursor_to_the_match() {
        let mut app = app();
        app.on_source(
            RequestId::new("echo/get"),
            "### X\nGET https://a.test\nAccept: */*",
        );
        app.mode = Mode::Search;
        app.input = "accept".to_owned();
        app.apply(Action::Submit);

        assert_eq!(app.editor.line, 2);
        assert_eq!(app.editor.col, 0);
        assert_eq!(app.focus, Pane::Editor);
    }

    #[test]
    fn the_search_wraps_around_at_the_end() {
        let mut app = app();
        app.on_source(RequestId::new("echo/get"), "one\ntwo\none");
        app.mode = Mode::Search;
        app.input = "one".to_owned();
        app.apply(Action::Submit);
        assert_eq!(app.editor.line, 2);

        app.apply(Action::SearchAgain(true));
        assert_eq!(app.editor.line, 0);
    }

    #[test]
    fn a_search_with_no_results_warns() {
        let mut app = app();
        app.on_source(RequestId::new("echo/get"), "GET https://a.test");
        app.mode = Mode::Search;
        app.input = "does-not-exist".to_owned();
        app.apply(Action::Submit);

        assert_eq!(app.severity, Severity::Error);
    }

    #[test]
    fn deleting_the_whole_command_line_leaves_the_mode() {
        let mut app = app();
        app.apply(Action::SetMode(Mode::Command));
        app.apply(Action::Backspace);

        assert_eq!(app.mode, Mode::Normal);
    }

    #[test]
    fn the_help_is_drawn_in_the_response_pane() {
        let mut app = app();
        app.apply(Action::Help);

        assert_eq!(app.focus, Pane::Response);
        assert!(app.response.lines.len() > 10);
    }

    #[test]
    fn the_execution_events_draw_the_response_as_they_arrive() {
        use http_studio_domain::{Exchange, ResponseBody, ResponseHead, Timings};

        let mut app = app();
        let resolved = ResolvedRequest {
            id: RequestId::new("echo/get"),
            method: HttpMethod::Get,
            url: "https://a.test/x".parse().unwrap(),
            headers: vec![],
            body: Body::Empty,
            options: RequestOptions::default(),
        };

        app.on_execution(&ExecutionEvent::Resolved {
            request: Box::new(resolved.clone()),
        });
        assert!(app.response.running);
        assert_eq!(app.response.summary, "in flight");

        let head = ResponseHead {
            status: 200,
            version: "HTTP/2.0".to_owned(),
            headers: vec![Header::new("content-type", "application/json")],
        };
        app.on_execution(&ExecutionEvent::Head {
            head: Box::new(head.clone()),
        });
        assert_eq!(app.response.summary, "200");

        app.on_execution(&ExecutionEvent::Completed {
            exchange: Box::new(Exchange {
                request: resolved,
                head,
                body: ResponseBody {
                    bytes: b"{\"ok\":true}".to_vec(),
                },
                timings: Timings::default(),
            }),
        });

        assert!(!app.response.running);
        assert_eq!(app.severity, Severity::Success);
        assert!(app.response.lines.iter().any(|l| l.text.contains("\"ok\"")));
    }

    #[test]
    fn a_transport_failure_is_marked_as_an_error() {
        let mut app = app();
        app.on_execution(&ExecutionEvent::Failed {
            message: "could not connect".to_owned(),
        });

        assert!(!app.response.running);
        assert_eq!(app.severity, Severity::Error);
    }

    #[test]
    fn cancelling_with_no_execution_emits_no_command() {
        let mut app = app();
        assert_eq!(app.apply(Action::Cancel), None);
    }

    #[test]
    fn cancelling_with_an_execution_in_flight_emits_the_command() {
        let mut app = app();
        app.response.running = true;

        assert_eq!(app.apply(Action::Cancel), Some(Command::Cancel));
    }

    #[test]
    fn opening_the_variables_with_no_data_asks_for_a_preview() {
        let mut app = app();
        app.on_source(RequestId::new("echo/get"), "GET https://a.test");

        assert_eq!(
            app.apply(Action::ToggleVars),
            Some(Command::Preview(RequestId::new("echo/get")))
        );
    }

    #[test]
    fn opening_the_unresolved_request_asks_for_a_preview() {
        let mut app = app();
        app.on_source(RequestId::new("echo/get"), "GET https://a.test");

        assert_eq!(
            app.apply(Action::ToggleRequest),
            Some(Command::Preview(RequestId::new("echo/get")))
        );
        assert!(app.show_request);
    }

    #[test]
    fn closing_the_request_asks_for_nothing() {
        let mut app = app();
        app.show_request = true;

        assert_eq!(app.apply(Action::ToggleRequest), None);
        assert!(!app.show_request);
    }

    #[test]
    fn the_request_command_toggles_the_pane() {
        let mut app = app();
        let _ = app.apply(Action::SetMode(Mode::Command));
        app.input = "request".to_owned();
        let _ = app.apply(Action::Submit);

        assert!(app.show_request);
    }

    #[test]
    fn running_keeps_the_resolved_request_for_the_pane() {
        use http_studio_domain::{Body, RequestOptions};

        let mut app = app();
        app.on_execution(&ExecutionEvent::Resolved {
            request: Box::new(ResolvedRequest {
                id: RequestId::new("echo/get"),
                method: HttpMethod::Get,
                url: "https://a.test/x".parse().unwrap(),
                headers: vec![],
                body: Body::Empty,
                options: RequestOptions::default(),
            }),
        });

        assert_eq!(
            app.request.as_deref().map(|request| request.url.as_str()),
            Some("https://a.test/x")
        );
    }

    #[test]
    fn opening_another_request_discards_the_previously_resolved_one() {
        // Otherwise the pane would show the earlier request as if it were this one.
        let mut app = app();
        app.on_source(RequestId::new("echo/get"), "GET https://a.test");
        app.on_preview(&resolved(), Vec::new());
        assert!(app.request.is_some());

        app.on_source(RequestId::new("echo/post"), "POST https://a.test");
        assert!(app.request.is_none());
    }

    /// A minimal resolved request for the pane's tests.
    fn resolved() -> ResolvedRequest {
        use http_studio_domain::{Body, RequestOptions};

        ResolvedRequest {
            id: RequestId::new("echo/get"),
            method: HttpMethod::Get,
            url: "https://a.test/x".parse().unwrap(),
            headers: vec![],
            body: Body::Empty,
            options: RequestOptions::default(),
        }
    }
}
