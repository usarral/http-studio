//! The workspace repository, backed by the file system.
//!
//! Implements [`CollectionRepository`] and [`EnvironmentRepository`] by reading
//! `.http` and `http-client.env.json` files. It is the hybrid model's **source
//! of truth**: what lives here is what gets versioned, reviewed in a pull
//! request and shared with the team.
//!
//! # How it maps to the domain
//!
//! | On disk | In the domain |
//! |---|---|
//! | One `.http` file | A [`Collection`] |
//! | A `###` block inside the file | A [`RequestDefinition`] |
//! | `@variables` before the first `###` | The collection's variables |
//! | `http-client.env.json` | The [`Environment`]s |
//!
//! It is the format's natural mapping: a `.http` file already groups related
//! requests, which is exactly what a collection was.
//!
//! Reading is done with `std::fs` inside `spawn_blocking`: a workspace's tree
//! is small and the real cost is file system work, so moving it off the async
//! executor is more honest than pretending the I/O is async.

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use http_studio_application::{
    ApplicationError, CollectionRepository, EnvironmentRepository, LocatedRequest, RequestSource,
};
use http_studio_domain::{Body, Collection, Environment, Header, RequestId};

use crate::storage::env_file::{PRIVATE_ENV_FILE, PUBLIC_ENV_FILE, load_environments};
use crate::storage::http_file;

/// The subdirectory holding the collections.
const COLLECTIONS_DIR: &str = "collections";

/// A workspace read from a directory on disk.
#[derive(Debug, Clone)]
pub struct FileSystemWorkspace {
    root: PathBuf,
}

impl FileSystemWorkspace {
    /// Creates the repository pointing at the workspace root.
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// Finds the workspace root by walking up from `start`.
    ///
    /// The root is the first directory containing `collections/` or an
    /// `http-client.env.json`, the same way `git` locates `.git`. Accepting
    /// either signal is what allows both an organised workspace and a bare
    /// folder of `.http` files.
    #[must_use]
    pub fn discover(start: &Path) -> Option<PathBuf> {
        start
            .ancestors()
            .find(|candidate| {
                candidate.join(COLLECTIONS_DIR).is_dir()
                    || candidate.join(PUBLIC_ENV_FILE).is_file()
            })
            .map(Path::to_path_buf)
    }

    /// The workspace root.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The directory the `.http` files are read from, already decided.
    ///
    /// It is exposed because `hts info` has to be able to say where it is
    /// reading from: if the answer is "from the root" and you expected
    /// `collections/`, that is exactly the diagnosis you came for.
    #[must_use]
    pub fn collections_root(&self) -> PathBuf {
        Self::collections_dir(&self.root)
    }

    /// The environment files' paths: the public one first, then the private.
    ///
    /// Whether they exist or not: knowing where they were expected is half the
    /// answer when `--env prod` says that environment does not exist.
    #[must_use]
    pub fn environment_files(&self) -> (PathBuf, PathBuf) {
        (
            self.root.join(PUBLIC_ENV_FILE),
            self.root.join(PRIVATE_ENV_FILE),
        )
    }

    /// Where to look for the `.http` files.
    ///
    /// `collections/` is preferred when it exists; otherwise the root itself,
    /// so a folder of loose files works without ceremony.
    fn collections_dir(root: &Path) -> PathBuf {
        let nested = root.join(COLLECTIONS_DIR);
        if nested.is_dir() {
            nested
        } else {
            root.to_path_buf()
        }
    }

    /// Loads every collection synchronously.
    fn load_collections_blocking(root: &Path) -> Result<Vec<Collection>, ApplicationError> {
        if !root.is_dir() {
            return Err(ApplicationError::repository(format!(
                "the workspace {} does not exist",
                root.display()
            )));
        }

        let base = Self::collections_dir(root);
        let mut collections = Vec::new();
        collect_directory(&base, &base, root, &mut collections)?;
        collections.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(collections)
    }

    /// Runs a blocking load on Tokio's blocking task pool.
    async fn blocking<T, F>(&self, load: F) -> Result<T, ApplicationError>
    where
        T: Send + 'static,
        F: FnOnce(&Path) -> Result<T, ApplicationError> + Send + 'static,
    {
        let root = self.root.clone();
        tokio::task::spawn_blocking(move || load(&root))
            .await
            .map_err(|error| ApplicationError::repository(format!("task interrupted: {error}")))?
    }
}

#[async_trait]
impl CollectionRepository for FileSystemWorkspace {
    async fn list_collections(&self) -> Result<Vec<Collection>, ApplicationError> {
        self.blocking(Self::load_collections_blocking).await
    }

    async fn find_request(
        &self,
        id: &RequestId,
    ) -> Result<Option<LocatedRequest>, ApplicationError> {
        let collections = self.list_collections().await?;
        let wanted = id.clone();

        Ok(collections.into_iter().find_map(|collection| {
            let definition = collection
                .requests
                .iter()
                .find(|request| request.id == wanted)?
                .clone();

            Some(LocatedRequest {
                collection_name: collection.name,
                collection_variables: collection.variables,
                definition,
            })
        }))
    }
}

#[async_trait]
impl RequestSource for FileSystemWorkspace {
    async fn read(&self, id: &RequestId) -> Result<Option<String>, ApplicationError> {
        let wanted = id.clone();

        self.blocking(move |root| {
            let base = Self::collections_dir(root);
            let Some((path, start, end)) = locate_source(&base, &base, &wanted)? else {
                return Ok(None);
            };

            let content = std::fs::read_to_string(&path).map_err(|error| {
                ApplicationError::repository(format!("{}: {error}", path.display()))
            })?;

            let lines: Vec<&str> = content.lines().collect();
            Ok(Some(
                lines[start.min(lines.len())..end.min(lines.len())].join("\n"),
            ))
        })
        .await
    }

    async fn save(&self, id: &RequestId, source: &str) -> Result<(), ApplicationError> {
        let wanted = id.clone();
        let source = source.to_owned();

        self.blocking(move |root| {
            // Validated before the disk is touched: saving something the
            // engine could not read back would leave the workspace broken, with
            // no obvious way to recover it from inside the TUI.
            http_file::parse(&source, "validation").map_err(|error| {
                ApplicationError::repository(format!("the block no longer parses: {error}"))
            })?;

            let base = Self::collections_dir(root);
            let (path, start, end) = locate_source(&base, &base, &wanted)?.ok_or_else(|| {
                ApplicationError::RequestNotFound {
                    id: wanted.to_string(),
                }
            })?;

            splice_lines(&path, start, end, &source)
        })
        .await
    }
}

#[async_trait]
impl EnvironmentRepository for FileSystemWorkspace {
    async fn list_environments(&self) -> Result<Vec<Environment>, ApplicationError> {
        self.blocking(|root| {
            let mut environments = load_environments(root)?;
            environments.sort_by(|a, b| a.name.cmp(&b.name));
            Ok(environments)
        })
        .await
    }

    async fn find_environment(&self, name: &str) -> Result<Option<Environment>, ApplicationError> {
        let wanted = name.to_owned();
        Ok(self
            .list_environments()
            .await?
            .into_iter()
            .find(|environment| environment.name == wanted))
    }
}

/// Walks `dir` recursively, turning each `.http` into a collection.
fn collect_directory(
    dir: &Path,
    base: &Path,
    root: &Path,
    out: &mut Vec<Collection>,
) -> Result<(), ApplicationError> {
    for entry in read_dir_sorted(dir)? {
        if entry.is_dir() {
            collect_directory(&entry, base, root, out)?;
            continue;
        }

        if !is_http_file(&entry) {
            continue;
        }

        // A file with no requests — only variables or comments — does not add
        // an empty collection to the listing.
        let collection = load_collection(&entry, base, root)?;
        if !collection.requests.is_empty() {
            out.push(collection);
        }
    }

    Ok(())
}

/// Locates a request's file and its span of lines.
///
/// It walks the same tree the read does rather than keeping a separate index:
/// saving is a one-off operation triggered by a person, so the cost does not
/// matter, while the alternative — a cache to invalidate — would introduce a
/// way to be out of step.
fn locate_source(
    dir: &Path,
    base: &Path,
    id: &RequestId,
) -> Result<Option<(PathBuf, usize, usize)>, ApplicationError> {
    for entry in read_dir_sorted(dir)? {
        if entry.is_dir() {
            if let Some(found) = locate_source(&entry, base, id)? {
                return Ok(Some(found));
            }
            continue;
        }

        if !is_http_file(&entry) {
            continue;
        }

        let (content, name) = read_and_name(&entry, base)?;
        let file = http_file::parse(&content, &name).map_err(|error| {
            ApplicationError::repository(format!("{}: {error}", entry.display()))
        })?;

        if let Some(found) = file
            .requests
            .iter()
            .find(|parsed| &parsed.definition.id == id)
        {
            return Ok(Some((entry, found.start, found.end)));
        }
    }

    Ok(None)
}

/// Replaces the file's `[start, end)` span with `source`.
fn splice_lines(
    path: &Path,
    start: usize,
    end: usize,
    source: &str,
) -> Result<(), ApplicationError> {
    let content = std::fs::read_to_string(path)
        .map_err(|error| ApplicationError::repository(format!("{}: {error}", path.display())))?;

    let mut lines: Vec<&str> = content.lines().collect();
    let replacement: Vec<&str> = source.lines().collect();
    lines.splice(start..end.min(lines.len()), replacement);

    let mut out = lines.join("\n");
    // `lines()` drops the trailing newline; it is put back so saving does not
    // leave the file without one every time.
    if content.ends_with('\n') {
        out.push('\n');
    }

    std::fs::write(path, out)
        .map_err(|error| ApplicationError::repository(format!("{}: {error}", path.display())))
}

/// Reads a file and returns its contents along with its logical name.
fn read_and_name(path: &Path, base: &Path) -> Result<(String, String), ApplicationError> {
    let content = std::fs::read_to_string(path)
        .map_err(|error| ApplicationError::repository(format!("{}: {error}", path.display())))?;

    let name = relative_name(&path.with_extension(""), base).ok_or_else(|| {
        ApplicationError::repository(format!("unexpected path: {}", path.display()))
    })?;

    Ok((content, name))
}

/// Reads a `.http` file and turns it into a collection.
///
/// Bodies declared with `< ./body.json` are resolved here, at load time, not at
/// send time. It costs reading those files during an `hts ls` too, but in
/// exchange a [`Collection`] never lies about the body it carries, and a broken
/// reference shows up when listing rather than only when the request is sent —
/// exactly as a syntax error already does.
fn load_collection(path: &Path, base: &Path, root: &Path) -> Result<Collection, ApplicationError> {
    let content = std::fs::read_to_string(path)
        .map_err(|error| ApplicationError::repository(format!("{}: {error}", path.display())))?;

    let name = relative_name(&path.with_extension(""), base).ok_or_else(|| {
        ApplicationError::repository(format!("ruta inesperada: {}", path.display()))
    })?;

    // The file context is essential: a real workspace has dozens of `.http`
    // files, and a syntax error without a path is useless.
    let file = http_file::parse(&content, &name)
        .map_err(|error| ApplicationError::repository(format!("{}: {error}", path.display())))?;

    let mut requests = Vec::with_capacity(file.requests.len());
    for parsed in file.requests {
        let mut definition = parsed.definition;
        if let Some(reference) = parsed.body_ref {
            definition.body = load_body_ref(&reference, path, root, &definition.headers)?;
        }
        requests.push(definition);
    }

    Ok(Collection {
        name,
        variables: file.variables,
        requests,
    })
}

/// Reads the file a `< ./body.json` points at and turns it into a body.
///
/// The path is read relative to the `.http` that writes it, which is how
/// JetBrains, VS Code REST Client and httpyac resolve it: that way a collection
/// can be moved to another folder without rewriting a single reference.
fn load_body_ref(
    reference: &http_file::BodyRef,
    http_file_path: &Path,
    root: &Path,
    headers: &[Header],
) -> Result<Body, ApplicationError> {
    let declared = Path::new(&reference.path);
    let candidate = if declared.is_absolute() {
        declared.to_path_buf()
    } else {
        http_file_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(declared)
    };

    let fail = |detail: String| {
        ApplicationError::repository(format!(
            "{}: line {}: `{}`: {detail}",
            http_file_path.display(),
            reference.line,
            reference.path
        ))
    };

    let resolved = candidate
        .canonicalize()
        .map_err(|error| fail(error.to_string()))?;

    // The body ends up going out over the network, so the reference does not
    // leave the workspace: someone else's `.http` should not be able to take a
    // file from outside with `< ../../.ssh/id_rsa`. Inside it, you are in
    // charge.
    let boundary = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    if !resolved.starts_with(&boundary) {
        return Err(fail(format!(
            "is outside the workspace ({})",
            boundary.display()
        )));
    }

    let content = std::fs::read_to_string(&resolved).map_err(|error| {
        if error.kind() == std::io::ErrorKind::InvalidData {
            fail("not UTF-8: binary bodies are not supported yet".to_owned())
        } else {
            fail(error.to_string())
        }
    })?;

    // `<` inserts the file literally and `<@` runs it through the
    // interpolator. For the first, escaping its `{{` with the mechanism the
    // domain already defines is enough, rather than adding a `Body` variant
    // every layer would have to learn to tell apart.
    let content = if reference.interpolate {
        content
    } else {
        content.replace("{{", "\\{{")
    };

    Ok(http_file::body_from(headers, &content))
}

/// Lists a directory in a deterministic order.
///
/// Alphabetical order makes `hts ls` and the tests give the same result every
/// time, whatever the file system feels like.
fn read_dir_sorted(dir: &Path) -> Result<Vec<PathBuf>, ApplicationError> {
    let mut entries = std::fs::read_dir(dir)
        .map_err(|error| ApplicationError::repository(format!("{}: {error}", dir.display())))?
        .map(|entry| {
            entry.map(|entry| entry.path()).map_err(|error| {
                ApplicationError::repository(format!("{}: {error}", dir.display()))
            })
        })
        .collect::<Result<Vec<_>, _>>()?;

    entries.sort();
    Ok(entries)
}

/// `true` when the path has a `.http` or `.rest` extension.
///
/// `.rest` is the alternative extension VS Code REST Client accepts; it is
/// allowed so an existing workspace does not have to be renamed.
fn is_http_file(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension == "http" || extension == "rest")
}

/// `path` relative to `base`, with `/` as the separator.
///
/// Returns `None` when `path` does not hang off `base`, or when they are equal.
fn relative_name(path: &Path, base: &Path) -> Option<String> {
    let relative = path.strip_prefix(base).ok()?;
    if relative.as_os_str().is_empty() {
        return None;
    }

    Some(
        relative
            .components()
            .filter_map(|component| component.as_os_str().to_str())
            .collect::<Vec<_>>()
            .join("/"),
    )
}
