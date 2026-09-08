//! Environments in `http-client.env.json`, the `.http` ecosystem's file.
//!
//! It is the format JetBrains HTTP Client and httpyac already use, so an
//! existing workspace works untouched:
//!
//! ```json
//! {
//!   "dev":  { "base_url": "https://dev.api.example.com" },
//!   "prod": { "base_url": "https://api.example.com" }
//! }
//! ```
//!
//! Beside it there may be an `http-client.private.env.json` with the same
//! shape. That second file is **not versioned** and its values override the
//! public one's for the same environment: it is where the tokens go, following
//! the convention anyone coming from JetBrains already expects.

use std::collections::BTreeMap;
use std::path::Path;

use http_studio_application::ApplicationError;
use http_studio_domain::Environment;

/// The versionable environments file.
pub(crate) const PUBLIC_ENV_FILE: &str = "http-client.env.json";
/// The private environments file, which must not be versioned.
pub(crate) const PRIVATE_ENV_FILE: &str = "http-client.private.env.json";

/// The file's shape: environment name → variables.
type EnvironmentDocument = BTreeMap<String, BTreeMap<String, String>>;

/// Loads a workspace's environments, merging the public and private files.
///
/// A workspace with neither file is valid: everything can be declared with
/// `@variables` inside the `.http` files themselves.
///
/// # Errors
///
/// [`ApplicationError::Repository`] when one of the files exists but cannot be
/// read, or is not valid JSON.
pub(crate) fn load_environments(root: &Path) -> Result<Vec<Environment>, ApplicationError> {
    let mut merged = read_document(&root.join(PUBLIC_ENV_FILE))?;

    for (name, private) in read_document(&root.join(PRIVATE_ENV_FILE))? {
        // The private file wins per variable, not per environment: that way it
        // is enough to list the secrets, without repeating the public config.
        merged.entry(name).or_default().extend(private);
    }

    Ok(merged
        .into_iter()
        .map(|(name, variables)| Environment { name, variables })
        .collect())
}

/// Reads an environments file, treating its absence as an empty document.
fn read_document(path: &Path) -> Result<EnvironmentDocument, ApplicationError> {
    if !path.is_file() {
        return Ok(EnvironmentDocument::new());
    }

    let content = std::fs::read_to_string(path)
        .map_err(|error| ApplicationError::repository(format!("{}: {error}", path.display())))?;

    serde_json::from_str(&content)
        .map_err(|error| ApplicationError::repository(format!("{}: {error}", path.display())))
}
