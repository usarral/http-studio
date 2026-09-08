//! `hts info`: which workspace and which paths this process is using.
//!
//! It exists to answer the question that precedes every other one when
//! something does not work: *am I talking about the workspace I think I am?*.
//! Nearly every failure of an HTTP client with automatic root discovery is
//! really that, and guessing it from outside costs more than printing it.
//!
//! It is the second place in the CLI that names concrete adapters, and on
//! purpose: what this command reports **is** which adapters there are and
//! which paths they read from. Even so it builds no
//! [`Engine`](http_studio_application::Engine), because an `hts info` has to
//! work precisely when the engine cannot be built.

use std::path::{Path, PathBuf};

use anyhow::Result;
use http_studio_application::EnvironmentRepository;
use http_studio_infrastructure::{EnvSecretProvider, FileSystemWorkspace, SqliteHistory};

/// The state of `hts`'s runtime environment.
#[derive(Debug)]
pub(crate) struct Info {
    /// The binary's version.
    pub(crate) version: &'static str,
    /// The workspace in use, or `None` when none was found.
    pub(crate) workspace: Option<Workspace>,
    /// The execution index's file.
    pub(crate) history: Option<FileState>,
    /// How many `HTS_SECRET_*` are defined. Never their values.
    pub(crate) secrets: usize,
}

/// The workspace located and the files that make it up.
#[derive(Debug)]
pub(crate) struct Workspace {
    /// The workspace root.
    pub(crate) root: PathBuf,
    /// `true` when it came from `--workspace` or from `HTS_WORKSPACE`.
    pub(crate) explicit: bool,
    /// The directory the `.http` files are read from.
    pub(crate) collections: PathBuf,
    /// The committable environments file.
    pub(crate) public_env: FileState,
    /// The private environments file.
    pub(crate) private_env: FileState,
    /// The names of the environments available, or `None` when unreadable.
    pub(crate) environments: Option<Vec<String>>,
}

/// A file whose path is worth knowing whether it exists or not.
#[derive(Debug)]
pub(crate) struct FileState {
    /// Where it is looked for.
    pub(crate) path: PathBuf,
    /// The size in bytes, or `None` when it does not exist.
    pub(crate) size: Option<u64>,
}

impl FileState {
    /// Checks `path`'s state without failing when it is absent.
    pub(crate) fn of(path: PathBuf) -> Self {
        let size = std::fs::metadata(&path)
            .ok()
            .filter(std::fs::Metadata::is_file)
            .map(|metadata| metadata.len());

        Self { path, size }
    }

    /// `true` when the file exists.
    pub(crate) fn exists(&self) -> bool {
        self.size.is_some()
    }
}

impl Info {
    /// Gathers the current state.
    ///
    /// It does not fail because something is missing: the absence of a
    /// workspace, of environments or of an index is precisely what one wants
    /// reported.
    ///
    /// # Errors
    ///
    /// If the current directory cannot be read to discover the root.
    pub(crate) async fn gather(workspace: Option<PathBuf>) -> Result<Self> {
        let explicit = workspace.is_some();
        let root = crate::composition::resolve_root(workspace)?;

        let workspace = match root {
            Some(root) => Some(Workspace::inspect(root, explicit).await),
            None => None,
        };

        Ok(Self {
            version: env!("CARGO_PKG_VERSION"),
            workspace,
            history: SqliteHistory::default_path().map(FileState::of),
            secrets: EnvSecretProvider::visible_count(),
        })
    }
}

impl Workspace {
    /// Looks at what is in the given root.
    async fn inspect(root: PathBuf, explicit: bool) -> Self {
        // Always absolute: `examples/demo` does not answer the question that
        // brings anyone here, which is exactly which directory is being
        // read.
        let root = root.canonicalize().unwrap_or(root);
        let adapter = FileSystemWorkspace::new(root.clone());
        let (public_env, private_env) = adapter.environment_files();

        // A broken `http-client.env.json` must not block the diagnosis: it
        // reports that they could not be read and the rest still comes out.
        let environments = adapter.list_environments().await.ok().map(|environments| {
            environments
                .into_iter()
                .map(|environment| environment.name)
                .collect()
        });

        Self {
            collections: adapter.collections_root(),
            root,
            explicit,
            public_env: FileState::of(public_env),
            private_env: FileState::of(private_env),
            environments,
        }
    }
}

/// Renders a path with `$HOME` shortened to `~`.
///
/// The index's path lives in the user's data directory and takes up half a
/// screen; abbreviating it leaves it readable without losing information.
pub(crate) fn shorten(path: &Path) -> String {
    let home = std::env::var_os("HOME").map(PathBuf::from);

    match home.and_then(|home| path.strip_prefix(home).ok().map(Path::to_path_buf)) {
        Some(relative) => format!("~/{}", relative.display()),
        None => path.display().to_string(),
    }
}

/// Formats a size in bytes readably.
#[expect(
    clippy::cast_precision_loss,
    reason = "it is a figure to read, not to compute with"
)]
pub(crate) fn human_size(bytes: u64) -> String {
    const KIB: f64 = 1024.0;
    const MIB: f64 = 1024.0 * KIB;

    let value = bytes as f64;

    if value < KIB {
        format!("{bytes} B")
    } else if value < MIB {
        format!("{:.1} KiB", value / KIB)
    } else {
        format!("{:.1} MiB", value / MIB)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_scale_to_the_readable_unit() {
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(2048), "2.0 KiB");
        assert_eq!(human_size(3 * 1024 * 1024), "3.0 MiB");
    }

    #[test]
    fn a_path_outside_the_home_is_left_alone() {
        assert_eq!(shorten(Path::new("/etc/hosts")), "/etc/hosts");
    }
}
