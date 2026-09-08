//! The HTTP Studio terminal interface.
//!
//! The project's third *driving adapter*, alongside the CLI and the JSON-RPC
//! server. It links [`http_studio_application::Engine`] in process, so it
//! reimplements nothing of the engine: it only decides what to draw and how it
//! is typed.
//!
//! # Layout
//!
//! The module is split so the interesting part can be tested without a
//! terminal:
//!
//! - [`keymap`]: the vim grammar (modes, counts, operators). Pure.
//! - [`app`]: state and transitions. Pure, apart from the commands it emits.
//! - [`wrap`]: laying text out into rows. Pure.
//! - [`ui`]: drawing with ratatui. No logic.
//! - [`runner`]: the loop, the terminal and the bridge to the engine.
//!
//! `app` executes nothing: when an action needs to talk to the engine it
//! returns an [`app::Command`] the `runner` carries out. That keeps the state
//! checkable with `assert_eq!` instead of with a simulated terminal.
//!
//! # Vim out of the box
//!
//! `hjkl`, counts (`3j`), `gg`/`G`/`{n}G`, `w`/`b`/`0`/`$`, `Ctrl-d`/`Ctrl-u`,
//! `Ctrl-w h/l` and `Tab` between panes, `i`/`a`/`o`/`v`/`Esc`, `/` with
//! `n`/`N`, `:` with commands, and `<Space>` as the leader.

pub mod app;
pub mod keymap;
pub mod runner;
pub mod ui;
pub mod wrap;

pub use runner::run;
