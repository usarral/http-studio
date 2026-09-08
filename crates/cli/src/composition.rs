//! The composition root: the only place where concrete adapters are chosen.
//!
//! That every infrastructure `new(...)` lives here is what makes the layers'
//! independence real: writing a TUI or a JSON-RPC server takes nothing but
//! replicating this module with the same pieces, without duplicating a single
//! line of logic.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use http_studio_application::{
    CollectionRepository, Engine, EnginePorts, EnvironmentRepository, ExchangeRecorder,
    HistoryMaintenance, HistoryReader, RequestSource,
};
use http_studio_domain::RequestDefinition;
use http_studio_infrastructure::{
    EnvSecretProvider, FileSystemWorkspace, InlineWorkspace, NullHistory, ReqwestTransport,
    SqliteHistory, SystemDynamicSource,
};

/// The total timeout per request.
///
/// An interactive client must give up before its user does. It will become
/// configurable per workspace once `http-studio.yaml` exists.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Builds the engine, choosing the concrete adapters.
///
/// When `workspace` is `None` the search goes upwards from the current
/// directory, just as `git` does with `.git`.
///
/// # Errors
///
/// If no workspace is found, or the HTTP client cannot be built.
pub(crate) fn build_engine(workspace: Option<PathBuf>, history_enabled: bool) -> Result<Engine> {
    let root = resolve_root(workspace)?.with_context(|| {
        "no workspace was found: a `collections/` directory or an \
         `http-client.env.json` is needed"
    })?;

    let filesystem = Arc::new(FileSystemWorkspace::new(root));
    assemble(
        filesystem.clone(),
        filesystem.clone(),
        filesystem,
        history_enabled,
    )
}

/// Builds the engine for an ad-hoc request, demanding no workspace.
///
/// The environments are still read from the workspace **when there is one**:
/// that allows `hts run '{{base_url}}/health' --env prod` from inside a
/// project, and lets the same command work outside it with a full URL.
///
/// # Errors
///
/// If the HTTP client cannot be built.
pub(crate) fn build_inline_engine(
    workspace: Option<PathBuf>,
    definition: RequestDefinition,
    history_enabled: bool,
) -> Result<Engine> {
    let inline = Arc::new(InlineWorkspace::new(definition));

    // With no workspace there are no environments, but there is no need to
    // abort either: it points at the current directory and the read comes back
    // with an empty list.
    let root = resolve_root(workspace)?
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    let filesystem = Arc::new(FileSystemWorkspace::new(root));

    assemble(inline.clone(), filesystem, inline, history_enabled)
}

/// Builds the engine without demanding a workspace.
///
/// The execution index lives in the user's data directory and not inside the
/// workspace, so `hts history` and `hts index` have to work from any
/// directory. Demanding a root they will not read was borrowing a requirement
/// from the command next door.
///
/// # Errors
///
/// If the current directory cannot be read, or the HTTP client cannot be
/// built.
pub(crate) fn build_index_engine(workspace: Option<PathBuf>) -> Result<Engine> {
    let root = resolve_root(workspace)?
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));

    let filesystem = Arc::new(FileSystemWorkspace::new(root));
    assemble(filesystem.clone(), filesystem.clone(), filesystem, true)
}

/// Locates the workspace root, or `None` when there is none.
pub(crate) fn resolve_root(workspace: Option<PathBuf>) -> Result<Option<PathBuf>> {
    if let Some(path) = workspace {
        return Ok(Some(path));
    }

    let current = std::env::current_dir().context("the current directory could not be read")?;
    Ok(FileSystemWorkspace::discover(&current))
}

/// Assembles the engine out of the adapters already chosen.
fn assemble(
    collections: Arc<dyn CollectionRepository>,
    environments: Arc<dyn EnvironmentRepository>,
    source: Arc<dyn RequestSource>,
    history_enabled: bool,
) -> Result<Engine> {
    let transport = Arc::new(
        ReqwestTransport::new(REQUEST_TIMEOUT).context("the HTTP client could not be built")?,
    );
    let (recorder, history, maintenance) = build_history(history_enabled);

    Ok(Engine::new(EnginePorts {
        collections,
        environments,
        secrets: Arc::new(EnvSecretProvider::new()),
        dynamic: Arc::new(SystemDynamicSource::new()),
        transport,
        recorder,
        history,
        maintenance,
        source,
    }))
}

/// Chooses the history adapter, which acts as both recorder and reader.
///
/// Failing to open the database is **not** a reason to abort: the history is
/// an extra, so it degrades silently to [`NullHistory`] rather than stopping
/// the user from running their request.
fn build_history(
    enabled: bool,
) -> (
    Arc<dyn ExchangeRecorder>,
    Arc<dyn HistoryReader>,
    Arc<dyn HistoryMaintenance>,
) {
    if enabled
        && let Some(path) = SqliteHistory::default_path()
        && let Ok(history) = SqliteHistory::open(&path)
    {
        let history = Arc::new(history);
        return (history.clone(), history.clone(), history);
    }

    let null = Arc::new(NullHistory);
    (null.clone(), null.clone(), null)
}
