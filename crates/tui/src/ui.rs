//! Drawing the interface with ratatui.
//!
//! It does nothing but translate [`App`]'s state into terminal cells. It makes
//! no decisions: if an `if` over business rules were needed here, the decision
//! would be in the wrong layer.

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};

use http_studio_domain::{HttpMethod, ResolvedRequest};

use crate::app::{App, LineKind, Severity, TreeRow, body_text};
use crate::keymap::{Mode, Pane};
use crate::wrap;

/// The palette, grouped so literals are not repeated all over the module.
mod palette {
    use ratatui::style::Color;

    /// The main text.
    pub(super) const FG: Color = Color::Rgb(201, 209, 217);
    /// Secondary text.
    pub(super) const DIM: Color = Color::Rgb(107, 116, 136);
    /// Borders and separators.
    pub(super) const BORDER: Color = Color::Rgb(35, 40, 56);
    /// The focused pane's border.
    pub(super) const BORDER_ON: Color = Color::Rgb(49, 64, 92);
    /// The main accent.
    pub(super) const BLUE: Color = Color::Rgb(97, 175, 239);
    /// Success.
    pub(super) const GREEN: Color = Color::Rgb(126, 198, 153);
    /// A warning, and the POST method.
    pub(super) const YELLOW: Color = Color::Rgb(229, 192, 123);
    /// An error.
    pub(super) const RED: Color = Color::Rgb(224, 108, 117);
    /// Visual mode.
    pub(super) const MAGENTA: Color = Color::Rgb(198, 120, 221);
    /// Variables and values.
    pub(super) const ORANGE: Color = Color::Rgb(209, 154, 102);
    /// Files and keys.
    pub(super) const CYAN: Color = Color::Rgb(86, 182, 194);
    /// The selected row's background.
    pub(super) const SEL: Color = Color::Rgb(42, 49, 69);
    /// The selected row's background in the focused pane.
    pub(super) const SEL_ON: Color = Color::Rgb(49, 64, 92);
    /// The visual selection's background.
    pub(super) const VISUAL: Color = Color::Rgb(44, 36, 64);
}

/// Draws the whole application.
pub fn draw(frame: &mut Frame<'_>, app: &mut App) {
    let root = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(3), Constraint::Length(1)])
        .split(frame.area());

    let panes = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(28),
            Constraint::Percentage(40),
            Constraint::Percentage(40),
        ])
        .split(root[0]);

    // The viewport feeds Ctrl-d and Ctrl-u, which must move half a real
    // screen and not some invented constant.
    app.viewport = usize::from(root[0].height).saturating_sub(2).max(1);

    draw_tree(frame, app, panes[0]);
    draw_editor(frame, app, panes[1]);
    draw_response(frame, app, panes[2]);
    draw_status(frame, app, root[1]);

    // Each floating pane has its own so both can be open at once: comparing
    // the resolved request with the variables that produced it is exactly what
    // one does when something goes out to the wrong environment.
    if app.show_request {
        draw_request(frame, app, panes[1]);
    }

    if app.show_vars {
        draw_variables(frame, app, panes[2]);
    }
}

/// A pane's frame, highlighted when it has the focus.
fn pane_block(title: &str, focused: bool, extra: &str) -> Block<'static> {
    let color = if focused { palette::BLUE } else { palette::DIM };

    let mut spans = vec![
        Span::raw(" "),
        Span::styled(title.to_owned(), Style::default().fg(color)),
        Span::raw(" "),
    ];

    if !extra.is_empty() {
        spans.push(Span::styled(
            format!("{extra} "),
            Style::default().fg(palette::DIM),
        ));
    }

    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(if focused {
            palette::BORDER_ON
        } else {
            palette::BORDER
        }))
        .title(Line::from(spans))
}

/// The selected row's style.
fn selected_style(focused: bool) -> Style {
    Style::default().bg(if focused {
        palette::SEL_ON
    } else {
        palette::SEL
    })
}

/// The colour associated with an HTTP method.
fn method_color(method: HttpMethod) -> Color {
    match method {
        HttpMethod::Get => palette::GREEN,
        HttpMethod::Post => palette::YELLOW,
        HttpMethod::Delete => palette::RED,
        _ => palette::BLUE,
    }
}

/// Crops a list around the cursor so it fits in `height`.
///
/// The cursor is centred rather than cropping from the top: going down a long
/// list, always seeing context below avoids the feeling of "pushing" the view
/// line by line from the edge.
fn window(cursor: usize, total: usize, height: usize) -> usize {
    if total <= height {
        return 0;
    }
    cursor
        .saturating_sub(height / 2)
        .min(total.saturating_sub(height))
}

/// The collection tree.
fn draw_tree(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let focused = app.focus == Pane::Tree;
    let rows = app.tree_rows();
    let height = usize::from(area.height).saturating_sub(2);
    let start = window(app.tree_cursor, rows.len(), height);

    let lines: Vec<Line<'_>> = rows
        .iter()
        .enumerate()
        .skip(start)
        .take(height)
        .map(|(index, row)| {
            let mut line = match row {
                TreeRow::Collection { name, folded } => Line::from(vec![
                    Span::styled(
                        if *folded { "▸ " } else { "▾ " },
                        Style::default().fg(palette::DIM),
                    ),
                    Span::styled(name.clone(), Style::default().fg(palette::CYAN)),
                ]),
                TreeRow::Request { method, label, id } => {
                    let current = app.current.as_ref() == Some(id);
                    Line::from(vec![
                        Span::raw("  "),
                        Span::styled(
                            format!("{:<6}", method.as_str()),
                            Style::default().fg(method_color(*method)),
                        ),
                        Span::styled(
                            label.clone(),
                            if current {
                                Style::default()
                                    .fg(palette::FG)
                                    .add_modifier(Modifier::BOLD)
                            } else {
                                Style::default().fg(palette::FG)
                            },
                        ),
                    ])
                }
            };

            if index == app.tree_cursor {
                line = line.style(selected_style(focused));
            }
            line
        })
        .collect();

    let count: usize = app.collections.iter().map(|c| c.requests.len()).sum();
    frame.render_widget(
        Paragraph::new(lines).block(pane_block("collections", focused, &format!("{count}"))),
        area,
    );
}

/// The width of the editor's line-number gutter.
const GUTTER: usize = 4;

/// The request editor, with `.http` highlighting.
fn draw_editor(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let focused = app.focus == Pane::Editor;
    let height = usize::from(area.height).saturating_sub(2);
    let text_width = usize::from(area.width).saturating_sub(2 + GUTTER).max(1);

    // The text is laid out into rows before the window is cropped: with line
    // wrapping, the cursor's row no longer matches its logical line.
    let rows = wrap::rows(&app.editor.lines, text_width);
    let cursor_row = wrap::row_of(&rows, app.editor.line, app.editor.col);
    let start = window(cursor_row, rows.len(), height);
    let visual = app.editor.visual_range();

    let lines: Vec<Line<'_>> = rows
        .iter()
        .skip(start)
        .take(height)
        .map(|row| {
            let text = app.editor.lines[row.line].as_str();

            // The number is only drawn on each line's first row, so how many
            // lines there really are stays visible at a glance.
            let label = if row.start == 0 {
                format!("{:>3} ", row.line + 1)
            } else {
                " ".repeat(GUTTER)
            };

            let mut spans = vec![Span::styled(label, Style::default().fg(palette::DIM))];
            spans.extend(slice_spans(&highlight_http(text), row.start, row.end));

            let mut line = Line::from(spans);
            if row.line == app.editor.line {
                line = line.style(selected_style(focused));
            } else if visual.is_some_and(|(lo, hi)| row.line >= lo && row.line <= hi) {
                line = line.style(Style::default().bg(palette::VISUAL));
            }
            line
        })
        .collect();

    let title = app
        .current
        .as_ref()
        .map_or_else(String::new, std::string::ToString::to_string);
    let mark = if app.editor.dirty { "[+]" } else { "" };

    frame.render_widget(
        Paragraph::new(lines).block(pane_block("request", focused, &format!("{title} {mark}"))),
        area,
    );

    // The terminal's real cursor is only placed in the editor: it is the only
    // pane where a column matters.
    // With an empty editor there is no row, and no cursor to place either: it
    // happens right at startup, before the first request is opened.
    if focused
        && app.mode != Mode::Command
        && app.mode != Mode::Search
        && let Some(row) = rows.get(cursor_row)
    {
        let text = app.editor.lines[row.line].as_str();
        let column = wrap::width_of(&text[row.start..app.editor.col.clamp(row.start, text.len())]);

        // Clamping to the pane's last cell is what keeps the cursor from
        // wandering into the neighbouring pane when a line ends right at the
        // edge.
        let x = (area.x + 1)
            .saturating_add(u16::try_from(GUTTER + column).unwrap_or(u16::MAX))
            .min(area.x + area.width.saturating_sub(2));
        let y = area.y + 1 + u16::try_from(cursor_row.saturating_sub(start)).unwrap_or(0);
        frame.set_cursor_position((x, y));
    }
}

/// Crops a list of highlighted spans to the byte range `[start, end)`.
///
/// It is needed because highlighting works on the whole line — it has to see
/// the method or the header's name to pick the colour — and wrapping cuts
/// afterwards. Cropping the already coloured spans keeps the colours on the
/// continuation rows; re-highlighting the loose fragment does not.
fn slice_spans(spans: &[Span<'static>], start: usize, end: usize) -> Vec<Span<'static>> {
    let mut out = Vec::new();
    let mut at = 0;

    for span in spans {
        let span_end = at + span.content.len();
        let from = start.max(at);
        let to = end.min(span_end);

        if from < to {
            out.push(Span::styled(
                span.content[from - at..to - at].to_owned(),
                span.style,
            ));
        }

        at = span_end;
        if at >= end {
            break;
        }
    }

    out
}

/// Highlights one `.http` line.
fn highlight_http(text: &str) -> Vec<Span<'static>> {
    let trimmed = text.trim_start();

    if trimmed.starts_with("###") {
        return vec![Span::styled(
            text.to_owned(),
            Style::default()
                .fg(palette::MAGENTA)
                .add_modifier(Modifier::BOLD),
        )];
    }

    if trimmed.starts_with('#') || trimmed.starts_with("//") {
        let color = if trimmed.contains('@') {
            palette::CYAN
        } else {
            palette::DIM
        };
        return vec![Span::styled(text.to_owned(), Style::default().fg(color))];
    }

    if trimmed.starts_with('@') {
        return vec![Span::styled(
            text.to_owned(),
            Style::default().fg(palette::CYAN),
        )];
    }

    // The method is located by position and not with `split_whitespace`, so
    // that concatenating the spans still yields the original text: an indented
    // line would lose its leading characters when cropped.
    if let Some(method) = trimmed.split_whitespace().next()
        && let Some(color) = http_method(method)
    {
        let at = text.len() - trimmed.len();
        let mut spans = Vec::new();
        if at > 0 {
            spans.push(Span::styled(
                text[..at].to_owned(),
                Style::default().fg(palette::FG),
            ));
        }
        spans.push(Span::styled(
            method.to_owned(),
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        ));
        spans.extend(highlight_variables(&text[at + method.len()..]));
        return spans;
    }

    if let Some((name, value)) = text.split_once(':')
        && !name.is_empty()
        && name.chars().all(|c| c.is_alphanumeric() || c == '-')
    {
        let mut spans = vec![
            Span::styled(name.to_owned(), Style::default().fg(palette::BLUE)),
            Span::raw(":"),
        ];
        spans.extend(highlight_variables(value));
        return spans;
    }

    highlight_variables(text)
}

/// A token's colour when it is a known HTTP method.
fn http_method(token: &str) -> Option<Color> {
    match token {
        "GET" => Some(palette::GREEN),
        "POST" => Some(palette::YELLOW),
        "PUT" | "PATCH" => Some(palette::BLUE),
        "DELETE" => Some(palette::RED),
        "HEAD" | "OPTIONS" => Some(palette::CYAN),
        _ => None,
    }
}

/// Marks the `{{placeholders}}` inside a fragment.
fn highlight_variables(text: &str) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    let mut rest = text;

    while let Some(open) = rest.find("{{") {
        if let Some(close) = rest[open..].find("}}") {
            spans.push(Span::styled(
                rest[..open].to_owned(),
                Style::default().fg(palette::FG),
            ));
            spans.push(Span::styled(
                rest[open..open + close + 2].to_owned(),
                Style::default().fg(palette::ORANGE),
            ));
            rest = &rest[open + close + 2..];
        } else {
            break;
        }
    }

    spans.push(Span::styled(
        rest.to_owned(),
        Style::default().fg(palette::FG),
    ));
    spans
}

/// The response pane.
fn draw_response(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let focused = app.focus == Pane::Response;
    let height = usize::from(area.height).saturating_sub(2);
    let width = usize::from(area.width).saturating_sub(2).max(1);

    // An unformatted JSON body is one enormously long line: without wrapping,
    // the pane would show only as much of the response as fits across.
    let texts: Vec<&str> = app
        .response
        .lines
        .iter()
        .map(|entry| entry.text.as_str())
        .collect();
    let rows = wrap::rows(&texts, width);
    let start = window(
        wrap::row_of(&rows, app.response.cursor, 0),
        rows.len(),
        height,
    );

    let lines: Vec<Line<'_>> = rows
        .iter()
        .skip(start)
        .take(height)
        .map(|row| {
            let entry = &app.response.lines[row.line];
            let style = Style::default().fg(match entry.kind {
                LineKind::Plain => palette::FG,
                LineKind::Dim => palette::DIM,
                LineKind::Success => palette::GREEN,
                LineKind::Error => palette::RED,
                LineKind::Warning => palette::YELLOW,
                LineKind::Json => palette::CYAN,
            });

            let mut line = Line::from(Span::styled(
                entry.text[row.start..row.end].to_owned(),
                style,
            ));
            if focused && row.line == app.response.cursor {
                line = line.style(selected_style(true));
            }
            line
        })
        .collect();

    frame.render_widget(
        Paragraph::new(lines).block(pane_block("response", focused, &app.response.summary)),
        area,
    );
}

/// The floating pane with the already resolved request.
///
/// It shows what **goes out on the wire**, which is not what is in the editor:
/// the variables are interpolated, the implicit `Content-Type` is already set
/// and the query is merged into the URL. It answers "but what did I send?".
fn draw_request(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let content = match app.request.as_deref() {
        None => vec![vec![Span::styled(
            "unresolved: run or preview it with <Space>r / p",
            Style::default().fg(palette::DIM),
        )]],
        Some(request) => request_lines(request),
    };

    // An `Authorization` carrying a JWT, or a URL with a query, will not fit
    // in one go: they wrap as in the background panes, they are not cropped.
    let lines = wrapped(&content, usize::from(area.width).saturating_sub(4));

    let rect = floating(area, lines.len());
    frame.render_widget(Clear, rect);
    frame.render_widget(
        Paragraph::new(lines).block(pane_block("resolved request", true, "")),
        rect,
    );
}

/// Lays already highlighted lines out into rows of `width` columns.
fn wrapped(content: &[Vec<Span<'static>>], width: usize) -> Vec<Line<'static>> {
    let texts: Vec<String> = content
        .iter()
        .map(|spans| spans.iter().map(|span| span.content.as_ref()).collect())
        .collect();

    wrap::rows(&texts, width)
        .iter()
        .map(|row| Line::from(slice_spans(&content[row.line], row.start, row.end)))
        .collect()
}

/// Translates the resolved request into the lines that get drawn.
fn request_lines(request: &ResolvedRequest) -> Vec<Vec<Span<'static>>> {
    let mut lines = vec![vec![
        Span::styled(
            format!("{} ", request.method),
            Style::default()
                .fg(method_color(request.method))
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(request.url.to_string(), Style::default().fg(palette::FG)),
    ]];

    // The TLS option is not a header, but it is part of what happens when
    // sending, so it is shown next to the request and not hidden away.
    if request.options.insecure_tls {
        lines.push(vec![Span::styled(
            "! without verifying the TLS certificate",
            Style::default().fg(palette::YELLOW),
        )]);
    }

    for header in &request.headers {
        lines.push(vec![
            Span::styled(
                format!("{}: ", header.name),
                Style::default().fg(palette::BLUE),
            ),
            Span::styled(header.value.clone(), Style::default().fg(palette::FG)),
        ]);
    }

    if let Some(body) = body_text(&request.body) {
        lines.push(vec![Span::raw("")]);
        for line in body.lines() {
            lines.push(vec![Span::styled(
                line.to_owned(),
                Style::default().fg(palette::CYAN),
            )]);
        }
    }

    lines
}

/// The rectangle of a floating pane anchored at the bottom, with room for `rows` rows.
fn floating(area: Rect, rows: usize) -> Rect {
    let height = u16::try_from(rows + 2)
        .unwrap_or(u16::MAX)
        .min(area.height.saturating_sub(2))
        .max(3);

    Rect {
        x: area.x + 1,
        y: area.y + area.height.saturating_sub(height + 1),
        width: area.width.saturating_sub(2),
        height,
    }
}

/// The floating pane with the variables and their originating scope.
fn draw_variables(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let rect = floating(area, app.variables.len());

    let lines: Vec<Line<'_>> = if app.variables.is_empty() {
        vec![Line::from(Span::styled(
            "no variables: run a preview with <Space>p",
            Style::default().fg(palette::DIM),
        ))]
    } else {
        app.variables
            .iter()
            .map(|(name, value, origin)| {
                Line::from(vec![
                    Span::styled(
                        format!("{:<14}", truncate(name, 14)),
                        Style::default().fg(palette::CYAN),
                    ),
                    Span::styled(
                        format!("{:<28}", truncate(value, 28)),
                        Style::default().fg(palette::GREEN),
                    ),
                    Span::styled(truncate(origin, 22), Style::default().fg(palette::DIM)),
                ])
            })
            .collect()
    };

    frame.render_widget(Clear, rect);
    frame.render_widget(
        Paragraph::new(lines).block(pane_block("variables · the last scope wins", true, "")),
        rect,
    );
}

/// Crops a text to `width` columns, marking the cut with a `…`.
///
/// It counts characters and not bytes: an accented token takes more bytes than
/// columns, and cutting it by bytes would misalign the table, besides being
/// able to split a character in half.
fn truncate(text: &str, width: usize) -> String {
    if text.chars().count() <= width {
        return text.to_owned();
    }

    let mut out: String = text.chars().take(width.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// The status line, in vim's style.
fn draw_status(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let (mode_bg, mode_text) = match app.mode {
        Mode::Normal => (palette::BLUE, app.mode.label()),
        Mode::Insert => (palette::GREEN, app.mode.label()),
        Mode::Visual => (palette::MAGENTA, app.mode.label()),
        Mode::Command => (palette::YELLOW, app.mode.label()),
        Mode::Search => (palette::ORANGE, app.mode.label()),
    };

    let mut spans = vec![
        Span::styled(
            format!(" {mode_text} "),
            Style::default()
                .bg(mode_bg)
                .fg(Color::Rgb(11, 13, 18))
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!(
                " {} ",
                app.environment.as_deref().unwrap_or("no environment")
            ),
            Style::default().fg(palette::GREEN),
        ),
    ];

    // In command or search mode, what is being typed takes over the line.
    if matches!(app.mode, Mode::Command | Mode::Search) {
        let prefix = if app.mode == Mode::Command { ':' } else { '/' };
        spans.push(Span::styled(
            format!("{prefix}{}", app.input),
            Style::default().fg(palette::FG),
        ));
        spans.push(Span::styled("▏", Style::default().fg(palette::FG)));
    } else {
        spans.push(Span::styled(
            app.message.clone(),
            Style::default().fg(match app.severity {
                Severity::Info => palette::DIM,
                Severity::Success => palette::GREEN,
                Severity::Warning => palette::YELLOW,
                Severity::Error => palette::RED,
            }),
        ));
    }

    let hint = app.keymap.hint();
    if !hint.is_empty() {
        spans.push(Span::styled(
            format!("  {hint}"),
            Style::default().fg(palette::YELLOW),
        ));
    }

    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

#[cfg(test)]
mod tests {
    // In tests, `unwrap` documents the expectation and its panic IS the failure.
    #![allow(clippy::unwrap_used)]

    use super::*;
    use http_studio_domain::RequestOptions;

    /// Each line's plain text, which is what is worth checking.
    fn plain(content: &[Vec<Span<'static>>]) -> Vec<String> {
        content
            .iter()
            .map(|spans| spans.iter().map(|span| span.content.as_ref()).collect())
            .collect()
    }

    #[test]
    fn the_window_does_not_scroll_when_everything_fits() {
        assert_eq!(window(3, 5, 10), 0);
    }

    #[test]
    fn the_window_centres_the_cursor() {
        assert_eq!(window(50, 100, 10), 45);
    }

    #[test]
    fn the_window_stops_at_the_end_of_the_list() {
        assert_eq!(window(99, 100, 10), 90);
    }

    #[test]
    fn the_window_does_not_go_past_the_beginning() {
        assert_eq!(window(1, 100, 10), 0);
    }

    #[test]
    fn highlights_the_method_on_the_request_line() {
        let spans = highlight_http("GET https://a.test");
        assert_eq!(spans[0].content, "GET");
    }

    #[test]
    fn the_highlighting_preserves_the_original_text() {
        // `slice_spans` crops by bytes over the already coloured spans, so
        // concatenating them has to be exactly the line it started from.
        for text in [
            "GET https://a.test",
            "    POST {{base_url}}/x HTTP/1.1",
            "Authorization: Bearer {{token}}",
            "# @insecure",
            "{\"email\": \"a@b.test\"}",
        ] {
            let joined: String = highlight_http(text)
                .iter()
                .map(|span| span.content.as_ref())
                .collect();
            assert_eq!(joined, text);
        }
    }

    #[test]
    fn the_resolved_request_shows_url_headers_and_body() {
        use http_studio_domain::{Body, Header, RequestId};

        let request = ResolvedRequest {
            id: RequestId::new("demo"),
            method: HttpMethod::Post,
            url: "https://api.example.com/users".parse().unwrap(),
            headers: vec![Header::new("Accept", "application/json")],
            body: Body::Json {
                content: "{\"a\": 1}".to_owned(),
            },
            options: RequestOptions::default(),
        };

        let texts = plain(&request_lines(&request));
        assert!(texts[0].contains("POST"));
        assert!(texts[0].contains("https://api.example.com/users"));
        assert!(texts.iter().any(|line| line == "Accept: application/json"));
        assert!(texts.iter().any(|line| line.contains("\"a\": 1")));
    }

    #[test]
    fn the_resolved_request_warns_that_tls_is_not_verified() {
        use http_studio_domain::{Body, RequestId};

        let request = ResolvedRequest {
            id: RequestId::new("demo"),
            method: HttpMethod::Get,
            url: "https://internal.local/health".parse().unwrap(),
            headers: vec![],
            body: Body::Empty,
            options: RequestOptions { insecure_tls: true },
        };

        assert!(
            plain(&request_lines(&request))
                .iter()
                .any(|line| line.contains("without verifying the TLS certificate"))
        );
    }

    #[test]
    fn the_request_pane_wraps_the_long_lines() {
        let content = vec![vec![Span::raw("Authorization: Bearer abcdefghij")]];
        let lines = wrapped(&content, 12);

        assert!(lines.len() > 1);
        let joined: String = lines
            .iter()
            .flat_map(|line| line.spans.iter())
            .map(|span| span.content.as_ref())
            .collect();
        assert_eq!(joined, "Authorization: Bearer abcdefghij");
    }

    #[test]
    fn crops_the_spans_to_the_range_asked_for() {
        let spans = highlight_http("GET https://a.test");
        let sliced: String = slice_spans(&spans, 4, 12)
            .iter()
            .map(|span| span.content.as_ref())
            .collect();

        assert_eq!(sliced, "https://");
    }

    #[test]
    fn a_range_falling_inside_a_single_span_does_not_lose_it() {
        let spans = highlight_http("GET https://a.test");
        let sliced: String = slice_spans(&spans, 0, 3)
            .iter()
            .map(|span| span.content.as_ref())
            .collect();

        assert_eq!(sliced, "GET");
    }

    #[test]
    fn highlights_the_placeholders_as_their_own_span() {
        let spans = highlight_variables("https://{{host}}/x");
        let contents: Vec<&str> = spans.iter().map(|s| s.content.as_ref()).collect();

        assert!(contents.contains(&"{{host}}"));
    }

    #[test]
    fn an_unclosed_brace_does_not_break_the_highlighting() {
        let spans = highlight_variables("https://{{host");
        assert!(!spans.is_empty());
    }

    #[test]
    fn does_not_crop_what_already_fits() {
        assert_eq!(truncate("short", 10), "short");
    }

    #[test]
    fn crops_marking_the_cut() {
        assert_eq!(truncate("abcdefghij", 5), "abcd…");
    }

    #[test]
    fn crops_counting_characters_and_not_bytes() {
        // Five characters, eight bytes: it must be neither cropped nor split.
        assert_eq!(truncate("ñañón", 5), "ñañón");
    }

    #[test]
    fn tells_a_separator_from_a_comment_and_a_variable() {
        assert_eq!(highlight_http("### Login").len(), 1);
        assert_eq!(highlight_http("# @name login").len(), 1);
        assert_eq!(highlight_http("@email = a@b.com").len(), 1);
    }
}
