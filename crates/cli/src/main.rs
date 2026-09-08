//! The `hts` binary: the first interface over the HTTP Studio engine.
//!
//! It is a *driving adapter*: it translates console arguments into use-case
//! inputs, and engine events into text. It holds no business logic, and that
//! is precisely the criterion the architecture will be judged by once the TUI
//! and the Neovim plugin arrive: if either needs to reimplement something that
//! lives here, that something was in the wrong layer.
//!
//! # Exit codes
//!
//! | Code | Meaning |
//! |------|---------|
//! | `0` | The request was sent and answered with a 2xx status. |
//! | `1` | A usage, workspace or network error. |
//! | `2` | The request was sent but answered with a non-2xx status. |
//!
//! Telling 1 from 2 makes `hts run` usable as a smoke test in CI without
//! confusing "the API is down" with "the command is misspelt".

mod adhoc;
mod cli;
mod composition;
mod extract;
mod info;
mod render;

use std::io::Write;
use std::process::ExitCode;

use anyhow::Result;
use clap::Parser;
use futures_util::StreamExt;
use http_studio_application::{Engine, HistoryQuery, PruneScope, SendRequestInput};
use http_studio_domain::{ExecutionEvent, RequestId};
use http_studio_infrastructure::{InlineWorkspace, SqliteHistory};

use crate::cli::{Cli, Command, IndexAction, RunArgs};
use crate::composition::{build_engine, build_index_engine, build_inline_engine};
use crate::render::Renderer;

/// The exit code when the HTTP response is not 2xx.
const EXIT_HTTP_NOT_SUCCESS: u8 = 2;

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();

    match run(cli).await {
        Ok(code) => code,
        Err(error) => {
            // `{error:#}` unfolds anyhow's context chain, which is where the
            // real cause lives (which file, which variable).
            eprintln!("error: {error:#}");
            ExitCode::FAILURE
        }
    }
}

/// Dispatches the subcommand and returns the exit code.
async fn run(cli: Cli) -> Result<ExitCode> {
    let renderer = Renderer::new(cli.output);

    match cli.command {
        Command::Run(args) => {
            let (engine, id) = prepare(cli.workspace, &args, !args.no_history)?;
            execute(&engine, &renderer, &args, id).await
        }
        Command::Preview(args) => {
            let (engine, id) = prepare(cli.workspace, &args, false)?;
            preview(&engine, &renderer, &args, id).await
        }
        Command::Ls => {
            let engine = build_engine(cli.workspace, false)?;
            let collections = engine.list_requests().collections().await?;
            let mut stdout = std::io::stdout().lock();
            renderer.collections(&collections, &mut stdout)?;
            Ok(ExitCode::SUCCESS)
        }
        Command::Env => {
            let engine = build_engine(cli.workspace, false)?;
            let environments = engine.list_requests().environments().await?;
            let mut stdout = std::io::stdout().lock();
            renderer.environments(&environments, &mut stdout)?;
            Ok(ExitCode::SUCCESS)
        }
        Command::History(args) => {
            let engine = build_index_engine(cli.workspace)?;
            let mut stdout = std::io::stdout().lock();

            // `--show` replaces the listing: whoever asks for one specific
            // execution does not also want the table in front of it.
            if let Some(id) = args.show {
                let Some(detail) = engine.history_detail(id).await? else {
                    anyhow::bail!("there is no execution with the identifier {id}");
                };
                renderer.history_detail(&detail, &mut stdout)?;
                return Ok(ExitCode::SUCCESS);
            }

            let entries = engine
                .history(&HistoryQuery {
                    request_id: args.request.as_deref().map(RequestId::new),
                    status: args.status,
                    limit: args.limit,
                })
                .await?;
            renderer.history(&entries, &mut stdout)?;
            Ok(ExitCode::SUCCESS)
        }
        Command::Index { action } => {
            let engine = build_index_engine(cli.workspace)?;
            let mut stdout = std::io::stdout().lock();

            match action.unwrap_or(IndexAction::Stats) {
                IndexAction::Stats => {
                    let stats = engine.index_stats().await?;
                    let size = SqliteHistory::default_path()
                        .map(|path| info::FileState::of(path).size.unwrap_or_default());
                    renderer.index_stats(&stats, size, &mut stdout)?;
                }
                IndexAction::Prune { older_than, keep } => {
                    let scope = match (older_than, keep) {
                        (Some(days), _) => PruneScope::OlderThan { days },
                        (None, Some(entries)) => PruneScope::KeepLast { entries },
                        (None, None) => {
                            anyhow::bail!("say what to keep: `--older-than DAYS` or `--keep N`")
                        }
                    };
                    let deleted = engine.prune_index(scope).await?;
                    renderer.pruned(deleted, &mut stdout)?;
                }
                IndexAction::Clear => {
                    let deleted = engine.prune_index(PruneScope::All).await?;
                    renderer.pruned(deleted, &mut stdout)?;
                }
            }

            Ok(ExitCode::SUCCESS)
        }
        Command::Info => {
            let info = info::Info::gather(cli.workspace).await?;
            let mut stdout = std::io::stdout().lock();
            renderer.info(&info, &mut stdout)?;
            Ok(ExitCode::SUCCESS)
        }
        Command::Tui => {
            let engine = build_engine(cli.workspace, true)?;
            http_studio_tui::run(engine).await?;
            Ok(ExitCode::SUCCESS)
        }
        Command::Serve => {
            let engine = build_engine(cli.workspace, true)?;
            // The server speaks over stdout, so nothing else may write there:
            // any trace would have to go to stderr.
            http_studio_rpc::serve(engine, tokio::io::stdin(), tokio::io::stdout()).await?;
            Ok(ExitCode::SUCCESS)
        }
        Command::Completions { shell } => {
            let mut command = <Cli as clap::CommandFactory>::command();
            clap_complete::generate(shell, &mut command, "hts", &mut std::io::stdout());
            Ok(ExitCode::SUCCESS)
        }
    }
}

/// Picks the engine and the identifier depending on whether the target is a
/// URL or the workspace.
///
/// It is where `hts run https://…` and `hts run auth/login` part ways, and the
/// only place: from here on the rest of the program does not know which it
/// was.
fn prepare(
    workspace: Option<std::path::PathBuf>,
    args: &RunArgs,
    history: bool,
) -> Result<(Engine, RequestId)> {
    if args.is_inline() {
        let definition = adhoc::build(args)?;
        let engine = build_inline_engine(workspace, definition, history)?;
        return Ok((engine, InlineWorkspace::request_id()));
    }

    let engine = build_engine(workspace, history)?;
    Ok((engine, RequestId::new(args.request.clone())))
}

/// Runs a request, consuming the engine's stream of events.
async fn execute(
    engine: &Engine,
    renderer: &Renderer,
    args: &RunArgs,
    id: RequestId,
) -> Result<ExitCode> {
    let input = SendRequestInput::new(id)
        .with_environment(args.environment.clone())
        .with_overrides(args.overrides())
        .with_insecure_tls(args.insecure);

    let mut events = engine.send_request().execute(input).await?;
    let mut stdout = std::io::stdout().lock();
    let mut outcome = ExitCode::SUCCESS;
    let mut body = None;

    while let Some(event) = events.next().await {
        // The exit code is decided by the terminal events; the rest are only
        // drawn.
        match &event {
            ExecutionEvent::Completed { exchange } => {
                if !exchange.head.is_success() {
                    outcome = ExitCode::from(EXIT_HTTP_NOT_SUCCESS);
                }
                if args.extract.is_some() {
                    body = Some(exchange.body.as_text().into_owned());
                }
            }
            ExecutionEvent::Failed { .. } => outcome = ExitCode::FAILURE,
            _ => {}
        }

        // `--extract` replaces the normal output: whoever asks for one value
        // does not also want the whole response in front of it.
        if args.extract.is_none() {
            renderer.event(&event, &mut stdout)?;
        }
    }

    if let Some(path) = &args.extract
        && let Some(body) = body
    {
        writeln!(stdout, "{}", extract::extract(&body, path)?)?;
    }

    stdout.flush()?;
    Ok(outcome)
}

/// Resolves a request and shows it without sending it.
async fn preview(
    engine: &Engine,
    renderer: &Renderer,
    args: &RunArgs,
    id: RequestId,
) -> Result<ExitCode> {
    let input = SendRequestInput::new(id)
        .with_environment(args.environment.clone())
        .with_overrides(args.overrides())
        .with_insecure_tls(args.insecure);

    let prepared = engine.preview_request().execute(input).await?;
    let mut stdout = std::io::stdout().lock();
    renderer.preview(&prepared.request, &mut stdout)?;
    stdout.flush()?;

    Ok(ExitCode::SUCCESS)
}
