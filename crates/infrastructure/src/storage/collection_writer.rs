//! Writing collections into the workspace: [`CollectionWriter`] for the file
//! system.
//!
//! It is where an import stops being a conversion and becomes a change to
//! files somebody else may have edited, so the rules are conservative:
//!
//! - **It merges, never replaces a file.** A `.http` holds requests nobody
//!   imported, plus comments and ordering that a person chose. Only the blocks
//!   the import names are touched.
//! - **A request already there is skipped by default.** Re-importing is how a
//!   specification gets followed as it grows; overwriting by default would make
//!   doing that something to be afraid of.
//! - **The result is parsed before it is written.** The same rule
//!   [`super::filesystem`]'s saving follows, for the same reason: leaving a
//!   workspace the engine cannot read back is worse than refusing to write.

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use http_studio_application::{
    ApplicationError, CollectionWriter, ImportOutcome, ImportPolicy, ImportReport, ImportedRequest,
};
use http_studio_domain::{Collection, RequestDefinition, RequestId};

use crate::storage::filesystem::FileSystemWorkspace;
use crate::storage::http_file::{self, ParsedRequest};
use crate::storage::http_writer;

#[async_trait]
impl CollectionWriter for FileSystemWorkspace {
    async fn write(
        &self,
        collection: &Collection,
        policy: ImportPolicy,
    ) -> Result<ImportReport, ApplicationError> {
        let collection = collection.clone();
        self.blocking(move |root| write_collection(root, &collection, policy))
            .await
    }
}

/// Writes one collection, synchronously.
fn write_collection(
    root: &Path,
    collection: &Collection,
    policy: ImportPolicy,
) -> Result<ImportReport, ApplicationError> {
    let relative = relative_file(&collection.name)?;
    let directory = destination_dir(root);
    let path = directory.join(&relative);

    let prefix = collection.name.trim_matches('/').to_owned();
    let (content, requests) = if path.is_file() {
        let existing = std::fs::read_to_string(&path).map_err(|error| {
            ApplicationError::repository(format!("{}: {error}", path.display()))
        })?;
        merge_into(&existing, collection, &prefix, policy)
            .map_err(|error| ApplicationError::repository(format!("{}: {error}", path.display())))?
    } else {
        (
            http_writer::render_collection(collection),
            collection
                .requests
                .iter()
                .map(|request| imported(&prefix, request, ImportOutcome::Created))
                .collect(),
        )
    };

    // Nothing is written that cannot be read back, which is also what catches
    // a body carrying the format's own separator.
    http_file::parse(&content, &prefix).map_err(|error| {
        ApplicationError::repository(format!(
            "{}: what was about to be written does not parse: {error}",
            path.display()
        ))
    })?;

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| {
            ApplicationError::repository(format!("{}: {error}", parent.display()))
        })?;
    }

    std::fs::write(&path, content)
        .map_err(|error| ApplicationError::repository(format!("{}: {error}", path.display())))?;

    Ok(ImportReport {
        collection: collection.name.clone(),
        location: display_path(&path, root),
        requests,
    })
}

/// Merges the collection into a file that already exists.
///
/// Returns the file's new contents and what became of each request.
fn merge_into(
    existing: &str,
    collection: &Collection,
    prefix: &str,
    policy: ImportPolicy,
) -> Result<(String, Vec<ImportedRequest>), http_file::ParseError> {
    let parsed = http_file::parse(existing, prefix)?;
    let mut lines: Vec<String> = existing.lines().map(str::to_owned).collect();

    let mut outcomes = Vec::with_capacity(collection.requests.len());
    let mut replacements: Vec<(usize, usize, String)> = Vec::new();
    let mut appended: Vec<&RequestDefinition> = Vec::new();

    for request in &collection.requests {
        match find_block(&parsed.requests, &request.name) {
            Some(block) if policy == ImportPolicy::Replace => {
                replacements.push((block.start, block.end, http_writer::render_request(request)));
                outcomes.push(imported(prefix, request, ImportOutcome::Replaced));
            }
            Some(_) => outcomes.push(imported(prefix, request, ImportOutcome::Skipped)),
            None => {
                appended.push(request);
                outcomes.push(imported(prefix, request, ImportOutcome::Created));
            }
        }
    }

    // Back to front: splicing a block changes the line numbers of everything
    // after it, and the ones before are the only ones still valid.
    replacements.sort_by_key(|(start, _, _)| std::cmp::Reverse(*start));
    for (start, end, block) in replacements {
        let replacement: Vec<String> = block.lines().map(str::to_owned).collect();
        lines.splice(start..end.min(lines.len()), replacement);
    }

    for request in appended {
        if lines.last().is_some_and(|line| !line.trim().is_empty()) {
            lines.push(String::new());
        }
        lines.extend(
            http_writer::render_request(request)
                .lines()
                .map(str::to_owned),
        );
    }

    // Variables the file does not declare yet go at the end of its preamble,
    // which is where the format expects them and where a reader looks first.
    let missing: Vec<String> = collection
        .variables
        .iter()
        .filter(|(name, _)| !parsed.variables.contains_key(name.as_str()))
        .map(|(name, value)| format!("@{name} = {value}"))
        .collect();

    if !missing.is_empty() {
        let at = preamble_end(&lines);
        let mut block = missing;
        if lines.get(at).is_some_and(|line| !line.trim().is_empty()) {
            block.push(String::new());
        }
        lines.splice(at..at, block);
    }

    let mut content = lines.join("\n");
    content.push('\n');
    Ok((content, outcomes))
}

/// The block in the file that holds the request named `name`, if any.
///
/// Matching is by slug and not by the name as written, so re-importing after
/// somebody retitled `Get pet` to `Get a pet` still finds it: both are the
/// same identifier, which is what `hts run` uses.
fn find_block<'a>(blocks: &'a [ParsedRequest], name: &str) -> Option<&'a ParsedRequest> {
    let wanted = http_file::slug(name);
    blocks
        .iter()
        .find(|block| http_file::slug(&block.definition.name) == wanted)
}

/// Where the preamble ends: the line the first `###` sits on.
fn preamble_end(lines: &[String]) -> usize {
    lines
        .iter()
        .position(|line| line.trim_start().starts_with("###"))
        .unwrap_or(lines.len())
}

/// The identifier and outcome of one imported request.
fn imported(prefix: &str, request: &RequestDefinition, outcome: ImportOutcome) -> ImportedRequest {
    ImportedRequest {
        // Rebuilt rather than taken from the definition: what identifies a
        // request is where it ended up, and only the writer knows that.
        id: RequestId::new(format!("{prefix}/{}", http_file::slug(&request.name))),
        outcome,
    }
}

/// Turns a collection's name into the relative `.http` path it is written to.
///
/// A name may be nested (`admin/users`), which is how the reader already maps
/// subdirectories to collections. What it may not do is leave the workspace:
/// a specification is a document from outside, and `../../.bashrc` as a tag
/// name must not become a path.
fn relative_file(name: &str) -> Result<PathBuf, ApplicationError> {
    let trimmed = name.trim().trim_matches('/');
    let refuse = |reason: &str| {
        ApplicationError::repository(format!(
            "`{name}` is not a usable collection name: {reason}"
        ))
    };

    if trimmed.is_empty() {
        return Err(refuse("it is empty"));
    }

    let mut path = PathBuf::new();
    for segment in trimmed.split('/') {
        let segment = segment.trim();
        if segment.is_empty() {
            return Err(refuse("it has an empty segment"));
        }
        if segment == ".." || segment == "." {
            return Err(refuse("it walks outside the workspace"));
        }
        if segment.contains('\\') || segment.contains(':') {
            return Err(refuse("it holds a path separator"));
        }
        path.push(segment);
    }

    Ok(path.with_extension("http"))
}

/// The directory a collection is written into.
///
/// It is the one the reader reads from, so an import lands where `hts ls`
/// looks. The exception is a root with no `collections/` **and** no `.http`
/// files of its own: there, `collections/` is created, because a workspace that
/// cannot be discovered afterwards is not a workspace.
fn destination_dir(root: &Path) -> PathBuf {
    let reader = FileSystemWorkspace::new(root).collections_root();
    if reader != root || has_http_files(root) {
        return reader;
    }

    root.join("collections")
}

/// `true` when the directory holds at least one `.http` or `.rest` file.
fn has_http_files(directory: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return false;
    };

    entries.flatten().any(|entry| {
        entry
            .path()
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extension == "http" || extension == "rest")
    })
}

/// The path as it is worth showing: relative to the workspace when it is
/// inside it, absolute when it is not.
fn display_path(path: &Path, root: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .components()
        .filter_map(|component| component.as_os_str().to_str())
        .collect::<Vec<_>>()
        .join("/")
}

#[cfg(test)]
mod tests {
    // In tests, `unwrap` documents the expectation and its panic IS the failure.
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use std::collections::BTreeMap;

    use http_studio_domain::{Body, HttpMethod, RequestOptions};

    use super::*;

    fn request(name: &str, url: &str) -> RequestDefinition {
        RequestDefinition {
            id: RequestId::new(format!("x/{name}")),
            name: name.to_owned(),
            method: HttpMethod::Get,
            url: url.to_owned(),
            headers: Vec::new(),
            query: Vec::new(),
            body: Body::Empty,
            variables: BTreeMap::new(),
            description: None,
            options: RequestOptions::default(),
        }
    }

    fn collection(name: &str, requests: Vec<RequestDefinition>) -> Collection {
        Collection {
            name: name.to_owned(),
            variables: BTreeMap::new(),
            requests,
        }
    }

    /// A workspace with a `collections/` directory, as `hts` creates them.
    fn workspace() -> tempfile::TempDir {
        let directory = tempfile::tempdir().unwrap();
        std::fs::create_dir(directory.path().join("collections")).unwrap();
        directory
    }

    #[test]
    fn a_new_collection_becomes_a_new_file() {
        let workspace = workspace();
        let report = write_collection(
            workspace.path(),
            &collection(
                "petstore",
                vec![request("list-pets", "https://api.test/pets")],
            ),
            ImportPolicy::Skip,
        )
        .unwrap();

        assert_eq!(report.location, "collections/petstore.http");
        assert_eq!(report.count(ImportOutcome::Created), 1);
        assert!(
            std::fs::read_to_string(workspace.path().join("collections/petstore.http"))
                .unwrap()
                .contains("### list-pets")
        );
    }

    #[test]
    fn a_nested_name_becomes_a_subdirectory() {
        let workspace = workspace();
        let report = write_collection(
            workspace.path(),
            &collection(
                "admin/users",
                vec![request("list", "https://api.test/users")],
            ),
            ImportPolicy::Skip,
        )
        .unwrap();

        assert_eq!(report.location, "collections/admin/users.http");
        assert!(
            workspace
                .path()
                .join("collections/admin/users.http")
                .is_file()
        );
    }

    #[test]
    fn a_name_that_walks_out_of_the_workspace_is_refused() {
        let workspace = workspace();
        let error = write_collection(
            workspace.path(),
            &collection("../../escape", vec![request("x", "https://api.test/x")]),
            ImportPolicy::Skip,
        )
        .unwrap_err();

        assert!(
            error.to_string().contains("outside the workspace"),
            "{error}"
        );
    }

    #[test]
    fn importing_twice_leaves_the_second_one_alone() {
        let workspace = workspace();
        let collection = collection(
            "petstore",
            vec![request("list-pets", "https://api.test/pets")],
        );
        write_collection(workspace.path(), &collection, ImportPolicy::Skip).unwrap();

        let report = write_collection(workspace.path(), &collection, ImportPolicy::Skip).unwrap();

        assert_eq!(report.count(ImportOutcome::Skipped), 1);
        let content =
            std::fs::read_to_string(workspace.path().join("collections/petstore.http")).unwrap();
        assert_eq!(
            content.matches("### list-pets").count(),
            1,
            "a skipped request must not be appended a second time"
        );
    }

    #[test]
    fn replace_rewrites_only_that_block() {
        let workspace = workspace();
        let path = workspace.path().join("collections/petstore.http");
        std::fs::write(
            &path,
            "### list-pets\nGET https://api.test/pets\n\n### hand-written\n# @description Mine.\nGET https://api.test/mine\n",
        )
        .unwrap();

        let report = write_collection(
            workspace.path(),
            &collection(
                "petstore",
                vec![request("list-pets", "https://api.test/v2/pets")],
            ),
            ImportPolicy::Replace,
        )
        .unwrap();

        assert_eq!(report.count(ImportOutcome::Replaced), 1);
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(
            content.contains("GET https://api.test/v2/pets"),
            "{content}"
        );
        assert!(
            content.contains("# @description Mine."),
            "a request nobody imported has to survive untouched:\n{content}"
        );
    }

    #[test]
    fn a_request_the_file_does_not_have_is_appended() {
        let workspace = workspace();
        let path = workspace.path().join("collections/petstore.http");
        std::fs::write(&path, "### list-pets\nGET https://api.test/pets\n").unwrap();

        let report = write_collection(
            workspace.path(),
            &collection(
                "petstore",
                vec![
                    request("list-pets", "https://api.test/pets"),
                    request("create-pet", "https://api.test/pets"),
                ],
            ),
            ImportPolicy::Skip,
        )
        .unwrap();

        assert_eq!(report.count(ImportOutcome::Skipped), 1);
        assert_eq!(report.count(ImportOutcome::Created), 1);

        let file = http_file::parse(&std::fs::read_to_string(&path).unwrap(), "petstore").unwrap();
        assert_eq!(file.requests.len(), 2);
    }

    #[test]
    fn several_blocks_can_be_replaced_at_once() {
        // Each splice moves the lines after it, so replacing more than one
        // block is where a naive implementation writes into the wrong place.
        let workspace = workspace();
        let path = workspace.path().join("collections/petstore.http");
        std::fs::write(
            &path,
            "### one\nGET https://api.test/1\n\n### two\nGET https://api.test/2\n\n### three\nGET https://api.test/3\n",
        )
        .unwrap();

        write_collection(
            workspace.path(),
            &collection(
                "petstore",
                vec![
                    request("one", "https://api.test/v2/1"),
                    request("three", "https://api.test/v2/3"),
                ],
            ),
            ImportPolicy::Replace,
        )
        .unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("GET https://api.test/v2/1"), "{content}");
        assert!(content.contains("GET https://api.test/2"), "{content}");
        assert!(content.contains("GET https://api.test/v2/3"), "{content}");
    }

    #[test]
    fn missing_file_variables_are_added_to_the_preamble() {
        let workspace = workspace();
        let path = workspace.path().join("collections/petstore.http");
        std::fs::write(
            &path,
            "@base_url = https://api.test\n\n### one\nGET {{base_url}}/1\n",
        )
        .unwrap();

        let mut incoming = collection("petstore", vec![request("two", "{{base_url}}/2")]);
        incoming.variables = BTreeMap::from([
            ("base_url".to_owned(), "https://other.test".to_owned()),
            ("api_version".to_owned(), "v2".to_owned()),
        ]);

        write_collection(workspace.path(), &incoming, ImportPolicy::Skip).unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        let file = http_file::parse(&content, "petstore").unwrap();
        assert_eq!(
            file.variables.get("base_url").map(String::as_str),
            Some("https://api.test"),
            "a variable already declared is the workspace's, not the document's:\n{content}"
        );
        assert_eq!(
            file.variables.get("api_version").map(String::as_str),
            Some("v2")
        );
    }

    #[test]
    fn a_workspace_with_loose_files_keeps_them_at_the_root() {
        // Reading prefers `collections/` when it exists, so creating it here
        // would quietly stop the files already there from being read.
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(
            directory.path().join("api.http"),
            "### x\nGET https://api.test/x\n",
        )
        .unwrap();

        let report = write_collection(
            directory.path(),
            &collection("petstore", vec![request("list", "https://api.test/pets")]),
            ImportPolicy::Skip,
        )
        .unwrap();

        assert_eq!(report.location, "petstore.http");
    }

    #[test]
    fn an_empty_workspace_gets_a_collections_directory() {
        let directory = tempfile::tempdir().unwrap();

        let report = write_collection(
            directory.path(),
            &collection("petstore", vec![request("list", "https://api.test/pets")]),
            ImportPolicy::Skip,
        )
        .unwrap();

        assert_eq!(
            report.location, "collections/petstore.http",
            "an import nobody can discover afterwards is not worth writing"
        );
    }
}
