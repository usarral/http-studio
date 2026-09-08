//! The main loop: terminal, keyboard and the bridge to the engine.
//!
//! It is the only part of the TUI that does I/O, and that is why it is
//! deliberately thin. Its job is translating in both directions:
//!
//! - terminal keys → [`App::on_key`]
//! - the [`Command`] the app returns → a call to the engine → an `on_*` method
//!
//! The keyboard is read on a separate thread because `crossterm::event::read`
//! blocks: leaving it in an async task would freeze the executor and with it
//! the events of the in-flight request, which is exactly what one wants to
//! watch moving meanwhile.

use std::io::{Stdout, Write};

use crossterm::event::{
    Event, KeyEventKind, KeyboardEnhancementFlags, PopKeyboardEnhancementFlags,
    PushKeyboardEnhancementFlags,
};
use crossterm::terminal::{EnterAlternateScreen, LeaveAlternateScreen};
use futures_util::StreamExt;
use http_studio_application::{Engine, SendRequestInput};
use http_studio_domain::{Collection, Environment, ExecutionEvent, RequestId, ResolvedRequest};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use tokio::sync::mpsc::{self, UnboundedSender};
use tokio::task::JoinHandle;

use crate::app::{App, Command};
use crate::ui;

/// A failure that stops the interface from running.
#[derive(Debug, thiserror::Error)]
pub enum TuiError {
    /// The terminal is not responding.
    #[error("terminal error: {0}")]
    Terminal(#[from] std::io::Error),
}

/// The result of a command, on its way back to the application.
enum Outcome {
    /// The workspace, loaded.
    Workspace(Box<(Vec<Collection>, Vec<Environment>)>),
    /// A request's source text.
    Source(RequestId, String),
    /// A successful save.
    Saved,
    /// A preview, with the variables and their origin.
    Preview(Box<ResolvedRequest>, Vec<(String, String, String)>),
    /// One event of the execution in flight.
    Execution(Box<ExecutionEvent>),
    /// Something failed.
    Failed(String),
}

/// Starts the interface and does not return until the user quits.
///
/// # Errors
///
/// [`TuiError::Terminal`] if the terminal cannot be prepared or restored.
/// Engine errors do **not** abort: they are shown in the status line, because
/// a workspace with a broken file must be fixable without quitting.
pub async fn run(engine: Engine) -> Result<(), TuiError> {
    let (mut terminal, enhanced) = setup()?;
    let result = event_loop(engine, &mut terminal).await;
    restore(&mut terminal, enhanced)?;
    result
}

/// Prepares the terminal in raw mode and on the alternate screen.
///
/// It also reports whether the enhanced keyboard protocol was enabled, so that
/// exactly what was enabled can be disabled: asking the terminal again on the
/// way out could give a different answer and leave it half configured.
fn setup() -> Result<(Terminal<CrosstermBackend<Stdout>>, bool), TuiError> {
    crossterm::terminal::enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    crossterm::execute!(stdout, EnterAlternateScreen)?;

    // Without this, a classic ANSI terminal sends the same byte for `Enter`
    // and for `Ctrl-Enter`, and the run shortcut would be indistinguishable
    // from splitting the line. It is an optional improvement: where it is not
    // supported the interface still works and `Ctrl-Enter` behaves as
    // `Enter`.
    let enhanced = crossterm::terminal::supports_keyboard_enhancement().unwrap_or(false);
    if enhanced {
        crossterm::execute!(
            stdout,
            PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
        )?;
    }

    Ok((Terminal::new(CrosstermBackend::new(stdout))?, enhanced))
}

/// Puts the terminal back the way it was.
///
/// It is always called, including when the loop ended in error: leaving the
/// terminal in raw mode forces the user to type `reset` blind.
fn restore(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    enhanced: bool,
) -> Result<(), TuiError> {
    if enhanced {
        // The error is discarded on purpose: if the terminal refuses the
        // `pop`, carrying on and giving raw mode back matters more than
        // learning that an optional improvement could not be undone.
        let _ = crossterm::execute!(terminal.backend_mut(), PopKeyboardEnhancementFlags);
    }

    crossterm::terminal::disable_raw_mode()?;
    crossterm::execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    std::io::stdout().flush()?;
    Ok(())
}

/// The event loop.
async fn event_loop(
    engine: Engine,
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
) -> Result<(), TuiError> {
    let (keys_tx, mut keys) = mpsc::unbounded_channel::<Event>();
    let (out_tx, mut outcomes) = mpsc::unbounded_channel::<Outcome>();

    spawn_input_reader(keys_tx);

    let mut app = App::new();
    let mut running: Option<JoinHandle<()>> = None;

    dispatch(&engine, &out_tx, Command::Reload, &mut running, &app);
    terminal.draw(|frame| ui::draw(frame, &mut app))?;

    loop {
        tokio::select! {
            Some(event) = keys.recv() => {
                let command = match event {
                    // `Release` arrives on some terminals and would double
                    // every keystroke if it were not filtered out.
                    Event::Key(key) if key.kind != KeyEventKind::Release => app.on_key(key),
                    // A `Resize` changes no state: the redraw at the end of
                    // every turn of the loop is enough.
                    _ => None,
                };

                if let Some(command) = command {
                    dispatch(&engine, &out_tx, command, &mut running, &app);
                }
            }

            Some(outcome) = outcomes.recv() => apply_outcome(&mut app, outcome),

            else => break,
        }

        if app.should_quit {
            break;
        }

        terminal.draw(|frame| ui::draw(frame, &mut app))?;
    }

    if let Some(handle) = running {
        handle.abort();
    }

    Ok(())
}

/// Reads the keyboard on a dedicated thread and forwards it to the loop.
fn spawn_input_reader(sender: UnboundedSender<Event>) {
    std::thread::spawn(move || {
        // The thread dies on its own when the receiver closes, that is, when
        // the loop ends: no stop signal is needed.
        while let Ok(event) = crossterm::event::read() {
            if sender.send(event).is_err() {
                break;
            }
        }
    });
}

/// Translates a command's result into an application transition.
fn apply_outcome(app: &mut App, outcome: Outcome) {
    match outcome {
        Outcome::Workspace(payload) => {
            let (collections, environments) = *payload;
            app.on_workspace(collections, environments);
        }
        Outcome::Source(id, source) => app.on_source(id, &source),
        Outcome::Saved => app.on_saved(),
        Outcome::Preview(request, variables) => app.on_preview(&request, variables),
        Outcome::Execution(event) => app.on_execution(&event),
        Outcome::Failed(message) => app.error(message),
    }
}

/// Runs a command against the engine without blocking the loop.
fn dispatch(
    engine: &Engine,
    out: &UnboundedSender<Outcome>,
    command: Command,
    running: &mut Option<JoinHandle<()>>,
    app: &App,
) {
    let engine = engine.clone();
    let out = out.clone();
    let environment = app.environment.clone();

    match command {
        Command::Cancel => {
            if let Some(handle) = running.take() {
                handle.abort();
                let _ = out.send(Outcome::Execution(Box::new(ExecutionEvent::Failed {
                    message: "execution cancelled".to_owned(),
                })));
            }
        }

        Command::Reload => {
            tokio::spawn(async move {
                let listing = engine.list_requests();
                let outcome = match (listing.collections().await, listing.environments().await) {
                    (Ok(collections), Ok(environments)) => {
                        Outcome::Workspace(Box::new((collections, environments)))
                    }
                    (Err(error), _) | (_, Err(error)) => Outcome::Failed(error.to_string()),
                };
                let _ = out.send(outcome);
            });
        }

        Command::OpenSource(id) => {
            tokio::spawn(async move {
                let outcome = match engine.read_source(&id).await {
                    Ok(Some(source)) => Outcome::Source(id, source),
                    Ok(None) => Outcome::Failed(format!("the source of `{id}` was not found")),
                    Err(error) => Outcome::Failed(error.to_string()),
                };
                let _ = out.send(outcome);
            });
        }

        Command::Save(id, source) => {
            tokio::spawn(async move {
                let outcome = match engine.save_source(&id, &source).await {
                    Ok(()) => Outcome::Saved,
                    Err(error) => Outcome::Failed(error.to_string()),
                };
                let _ = out.send(outcome);
            });
        }

        Command::Preview(id) => {
            tokio::spawn(async move {
                let input = SendRequestInput::new(id).with_environment(environment);
                let outcome = match engine.preview_request().execute(input).await {
                    Ok(prepared) => {
                        // Each variable's origin is computed by the engine;
                        // the UI only draws it, so it cannot drift from what
                        // was actually used when resolving.
                        let variables = prepared
                            .context
                            .names()
                            .into_iter()
                            .filter_map(|name| {
                                let (origin, value) = prepared.context.lookup(name)?;
                                Some((name.to_owned(), value.to_owned(), origin.to_owned()))
                            })
                            .collect();
                        Outcome::Preview(Box::new(prepared.request), variables)
                    }
                    Err(error) => Outcome::Failed(error.to_string()),
                };
                let _ = out.send(outcome);
            });
        }

        Command::Run(id) => {
            // Only one execution at a time is allowed: two streams drawing on
            // the same pane would produce a mixed, unreadable response.
            if let Some(previous) = running.take() {
                previous.abort();
            }

            *running = Some(tokio::spawn(async move {
                let input = SendRequestInput::new(id).with_environment(environment);
                match engine.send_request().execute(input).await {
                    Ok(mut events) => {
                        while let Some(event) = events.next().await {
                            if out.send(Outcome::Execution(Box::new(event))).is_err() {
                                break;
                            }
                        }
                    }
                    Err(error) => {
                        let _ = out.send(Outcome::Failed(error.to_string()));
                    }
                }
            }));
        }
    }
}
