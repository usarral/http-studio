//! Turning `hts import` arguments into one use-case call.
//!
//! Every format goes through the same three steps — pick the importer, say
//! where the document is, report what happened — so what is left here is the
//! part that is genuinely the console's: reading a command from a file or from
//! standard input, because that is where a pasted one ends up.

use std::io::{Read, Write};
use std::path::PathBuf;

use anyhow::{Context, Result};
use http_studio_application::{ImportInput, ImportSource};

use crate::cli::ImportFormat;
use crate::composition::{build_import_engine, curl_importer, openapi_importer};
use crate::render::Renderer;

/// Runs the import the arguments describe.
///
/// # Errors
///
/// If the document cannot be read, the workspace cannot be written, or the
/// document is not the format it was taken for.
pub(crate) async fn run(
    workspace: Option<PathBuf>,
    format: ImportFormat,
    renderer: &Renderer,
    out: &mut impl Write,
) -> Result<()> {
    let mut insecure = false;
    let (importer, common, source) = match format {
        ImportFormat::Curl(args) => {
            // A cURL command is the document: there is nothing to fetch, which
            // is why it goes in as text and not as a location.
            let document = read_argument(&args.command)?;
            (
                curl_importer(args.name),
                args.common,
                ImportSource::Text(document),
            )
        }
        ImportFormat::OpenApi(args) => {
            insecure = args.insecure;
            (
                openapi_importer(args.tags),
                args.common,
                // A path or a URL: telling them apart is the engine's job, so
                // every interface does it the same way.
                ImportSource::Location(args.location),
            )
        }
    };

    // A dry run writes nothing, so it has no business demanding a workspace:
    // converting a command is useful from anywhere.
    let engine = build_import_engine(workspace, !common.dry_run)?;

    let input = ImportInput::new(source)
        .with_into(common.to.clone())
        .with_policy(common.policy())
        .with_dry_run(common.dry_run)
        .with_insecure_tls(insecure);

    let output = engine.import_collections(importer).execute(input).await?;
    renderer.imported(&output, out)?;
    out.flush()?;

    Ok(())
}

/// Reads an argument that may be a document, `@file`, or `@-` for stdin.
///
/// It is the same convention `--data` already uses, and it matters most here:
/// a browser's *Copy as cURL* is several lines long, and pasting it as a
/// single quoted argument is the part people get wrong.
fn read_argument(raw: &str) -> Result<String> {
    let Some(source) = raw.strip_prefix('@') else {
        return Ok(raw.to_owned());
    };

    if source == "-" {
        let mut buffer = String::new();
        std::io::stdin()
            .read_to_string(&mut buffer)
            .context("the command could not be read from standard input")?;
        return Ok(buffer);
    }

    std::fs::read_to_string(source).with_context(|| format!("`{source}` could not be read"))
}

#[cfg(test)]
mod tests {
    // In tests, `unwrap` documents the expectation and its panic IS the failure.
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn an_argument_that_is_not_a_reference_is_the_document() {
        assert_eq!(
            read_argument("curl https://api.example.com/x").unwrap(),
            "curl https://api.example.com/x"
        );
    }

    #[test]
    fn an_at_prefixed_argument_is_read_from_disk() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("command.sh");
        std::fs::write(&path, "curl https://api.example.com/x\n").unwrap();

        let document = read_argument(&format!("@{}", path.display())).unwrap();

        assert_eq!(document, "curl https://api.example.com/x\n");
    }

    #[test]
    fn a_missing_file_names_itself() {
        let error = read_argument("@./nowhere/command.sh").unwrap_err();

        assert!(
            error.to_string().contains("./nowhere/command.sh"),
            "{error}"
        );
    }
}
