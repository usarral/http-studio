//! The command-line interface's definition.
//!
//! This module only describes *what* the binary accepts. It holds no business
//! logic: it translates arguments into the use cases' input types.

use std::collections::BTreeMap;
use std::path::PathBuf;

use clap::{Parser, Subcommand, ValueEnum};

/// An HTTP client whose engine is decoupled from its interface.
#[derive(Debug, Parser)]
#[command(name = "hts", version, about, long_about = None)]
pub(crate) struct Cli {
    /// The workspace root. By default the first ancestor containing
    /// `collections/` is looked for upwards from the current directory.
    #[arg(long, short = 'w', global = true, env = "HTS_WORKSPACE")]
    pub(crate) workspace: Option<PathBuf>,

    /// The output format.
    #[arg(long, short = 'o', global = true, value_enum, default_value_t = OutputFormat::Pretty)]
    pub(crate) output: OutputFormat,

    /// The command to run.
    #[command(subcommand)]
    pub(crate) command: Command,
}

/// The format results are emitted in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum OutputFormat {
    /// Human-readable, with colour when the output is a terminal.
    Pretty,
    /// One JSON line per event, meant for scripts and for other UIs.
    Json,
    /// The response body alone, verbatim. Meant for pipes.
    Body,
    /// The status code alone.
    Status,
    /// Status and headers, no body.
    Headers,
    /// Nothing. The result is read from the exit code.
    Silent,
}

/// The subcommands available.
#[derive(Debug, Subcommand)]
pub(crate) enum Command {
    /// Sends a request from the workspace.
    Run(RunArgs),
    /// Resolves a request and shows what would be sent, without sending it.
    Preview(RunArgs),
    /// Lists the collections and their requests.
    Ls,
    /// Lists the environments available.
    Env,
    /// Opens the interactive terminal interface, with vim motions.
    Tui,
    /// Queries the local history of executions.
    History(HistoryArgs),
    /// Shows which workspace and which paths `hts` is using.
    Info,
    /// Inspects or cleans the local index of executions.
    Index {
        /// What to do with the index. With nothing, it summarises it.
        #[command(subcommand)]
        action: Option<IndexAction>,
    },
    /// Starts the JSON-RPC server over stdio, for editors and other UIs.
    ///
    /// It is not interactive: it speaks the protocol described in
    /// `docs/engine-protocol.md` and ends when the client closes the input.
    Serve,
    /// Prints your shell's completion script.
    ///
    /// For example: `hts completions zsh > ~/.zfunc/_hts`.
    Completions {
        /// The shell to generate the script for.
        #[arg(value_enum)]
        shell: clap_complete::Shell,
    },
}

/// The index's maintenance operations.
///
/// They exist so that "the index is a cache" is checkable and not a promise:
/// what it takes up can be inspected and emptied without the workspace ever
/// noticing.
#[derive(Debug, Subcommand)]
pub(crate) enum IndexAction {
    /// Summarises what is stored: executions, requests and size.
    Stats,
    /// Deletes old executions, keeping the recent ones.
    Prune {
        /// Deletes anything older than this many days.
        #[arg(long, value_name = "DAYS", conflicts_with = "keep")]
        older_than: Option<u32>,

        /// Keeps only the N most recent executions.
        #[arg(long, value_name = "N")]
        keep: Option<u32>,
    },
    /// Empties the whole index.
    ///
    /// Nothing but history is lost: the requests live in the `.http` files,
    /// which is what makes the index a cache.
    Clear,
}

/// The arguments of `hts history`.
#[derive(Debug, clap::Args)]
pub(crate) struct HistoryArgs {
    /// Shows the detail of one execution, by its identifier.
    ///
    /// The identifier is the number `hts -o json history` gives.
    #[arg(long = "show", value_name = "ID")]
    pub(crate) show: Option<i64>,

    /// Narrows to one request, e.g. `auth/login`.
    #[arg(long, short = 'r')]
    pub(crate) request: Option<String>,

    /// Narrows to an exact status code.
    #[arg(long, short = 's')]
    pub(crate) status: Option<u16>,

    /// The maximum number of executions to show.
    #[arg(long, short = 'n', default_value_t = 20)]
    pub(crate) limit: u32,
}

/// The arguments shared by `run` and `preview`.
///
/// They are the same on purpose: previewing must accept exactly the same
/// invocation as running, or the preview would stop being trustworthy.
#[derive(Debug, clap::Args)]
pub(crate) struct RunArgs {
    /// The request's identifier (`auth/login`), or a URL directly.
    ///
    /// When the argument contains `://` it is treated as an ad-hoc request and
    /// no workspace need exist.
    #[arg(value_name = "REQUEST|URL")]
    pub(crate) request: String,

    /// An ad-hoc request's HTTP method.
    #[arg(long = "method", short = 'X', value_name = "METHOD")]
    pub(crate) method: Option<String>,

    /// An ad-hoc request's header. Repeatable: `-H 'Accept: text/plain'`.
    #[arg(long = "header", short = 'H', value_parser = parse_header, value_name = "NAME: VALUE")]
    pub(crate) headers: Vec<(String, String)>,

    /// An ad-hoc request's body. With `@file` it is read from disk, and with
    /// `@-` from standard input.
    #[arg(long = "data", short = 'd', value_name = "BODY")]
    pub(crate) data: Option<String>,

    /// Like `--data`, but it also declares `Content-Type: application/json`.
    #[arg(long = "json", value_name = "JSON", conflicts_with = "data")]
    pub(crate) json: Option<String>,

    /// The environment to apply.
    #[arg(long = "env", short = 'e', env = "HTS_ENV")]
    pub(crate) environment: Option<String>,

    /// Forces a variable's value. Repeatable: `--var host=x --var id=7`.
    #[arg(long = "var", value_parser = parse_key_value, value_name = "KEY=VALUE")]
    pub(crate) variables: Vec<(String, String)>,

    /// Prints one value of the JSON body, e.g. `--extract data.token`.
    ///
    /// It accepts array indices: `items.0.id`.
    #[arg(long, value_name = "PATH")]
    pub(crate) extract: Option<String>,

    /// Does not record the execution in the local history.
    #[arg(long)]
    pub(crate) no_history: bool,

    /// Does not verify the server's TLS certificate, like `curl -k`.
    ///
    /// To leave it written in the request instead of repeating it on every
    /// invocation, add `# @insecure` to its block in the `.http` file.
    #[arg(long = "insecure", short = 'k')]
    pub(crate) insecure: bool,
}

impl RunArgs {
    /// Turns the repeated `--var`s into the map the use case expects.
    ///
    /// When a key repeats, the last one wins: it is what anyone re-editing a
    /// command in their shell history expects.
    #[must_use]
    pub(crate) fn overrides(&self) -> BTreeMap<String, String> {
        self.variables.iter().cloned().collect()
    }

    /// `true` when the argument is a URL and not a workspace identifier.
    ///
    /// Two signals, neither ambiguous: an identifier is a logical path
    /// (`auth/login`) that never carries a scheme nor starts with `{{`.
    ///
    /// The second is needed because in `'{{base_url}}/health'` the scheme
    /// lives inside the variable and there is no `://` to see until after
    /// resolving.
    #[must_use]
    pub(crate) fn is_inline(&self) -> bool {
        self.request.contains("://") || self.request.trim_start().starts_with("{{")
    }
}

/// Parses `Name: value`, in the style of `curl -H`.
fn parse_header(raw: &str) -> Result<(String, String), String> {
    raw.split_once(':')
        .map(|(name, value)| (name.trim().to_owned(), value.trim().to_owned()))
        .filter(|(name, _)| !name.is_empty())
        .ok_or_else(|| format!("expected `Name: value`, got `{raw}`"))
}

/// Parses `key=value`, allowing `=` inside the value.
fn parse_key_value(raw: &str) -> Result<(String, String), String> {
    raw.split_once('=')
        .map(|(key, value)| (key.trim().to_owned(), value.to_owned()))
        .filter(|(key, _)| !key.is_empty())
        .ok_or_else(|| format!("expected `key=value`, got `{raw}`"))
}

#[cfg(test)]
mod tests {
    // In tests, `unwrap` documents the expectation and its panic IS the failure.
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn parses_key_value_pairs() {
        assert_eq!(
            parse_key_value("host=api.example.com").unwrap(),
            ("host".to_owned(), "api.example.com".to_owned())
        );
    }

    #[test]
    fn accepts_an_equals_sign_inside_the_value() {
        assert_eq!(
            parse_key_value("q=a=b").unwrap(),
            ("q".to_owned(), "a=b".to_owned())
        );
    }

    #[test]
    fn rejects_values_without_an_equals_sign() {
        assert!(parse_key_value("host").is_err());
    }

    #[test]
    fn rejects_an_empty_key() {
        assert!(parse_key_value("=value").is_err());
    }

    #[test]
    fn the_last_repeated_var_wins() {
        let mut args = target("x");
        args.variables = vec![
            ("host".to_owned(), "one".to_owned()),
            ("host".to_owned(), "two".to_owned()),
        ];
        assert_eq!(
            args.overrides().get("host").map(String::as_str),
            Some("two")
        );
    }

    /// The minimal arguments with the given target.
    fn target(request: &str) -> RunArgs {
        RunArgs {
            request: request.to_owned(),
            method: None,
            headers: Vec::new(),
            data: None,
            json: None,
            environment: None,
            variables: Vec::new(),
            extract: None,
            no_history: false,
            insecure: false,
        }
    }

    #[test]
    fn a_complete_url_is_ad_hoc() {
        assert!(target("https://api.example.com/x").is_inline());
        assert!(target("http://localhost:8080").is_inline());
    }

    #[test]
    fn a_url_starting_with_a_variable_is_ad_hoc() {
        // The scheme lives inside the variable, so there is no `://` yet.
        assert!(target("{{base_url}}/health").is_inline());
        assert!(target("  {{base_url}}/health").is_inline());
    }

    #[test]
    fn a_workspace_identifier_is_not_ad_hoc() {
        assert!(!target("auth/login").is_inline());
        assert!(!target("admin/users/list").is_inline());
        assert!(!target("health").is_inline());
    }

    #[test]
    fn the_cli_definition_is_valid() {
        use clap::CommandFactory;
        Cli::command().debug_assert();
    }
}
