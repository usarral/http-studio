//! Importers: reading someone else's format as requests of our own.
//!
//! Each one is a [`http_studio_application::CollectionImporter`], and each one
//! is **pure**: text in, collections out, no disk and no network. Where the
//! document came from is the use case's business, which is what lets the same
//! importer serve a path, a URL and a string pasted on a command line.

pub mod curl;

pub use curl::CurlImporter;
