//! Tests for the SQLite index: what it stores, what it returns and what can be
//! deleted.
//!
//! They run against a real database in a temporary directory rather than
//! against a double. Half of what needs checking here — that the schema
//! migrates, that `VACUUM` leaves the file small, that a large body is
//! truncated — only exists inside SQLite, so a double would prove nothing.

// In tests, `unwrap`/`expect` document the expectation and their panic IS the failure.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;
use std::time::Duration;

use http_studio_application::HistoryMaintenance;
use http_studio_application::{ExchangeRecorder, HistoryQuery, HistoryReader, PruneScope};
use http_studio_domain::{
    Exchange, Header, HttpMethod, RequestId, RequestOptions, ResolvedRequest, ResponseBody,
    ResponseHead, Timings,
};
use http_studio_infrastructure::{MAX_STORED_BODY, SqliteHistory};

/// An exchange with the given body, ready to record.
fn exchange(id: &str, status: u16, body: Vec<u8>) -> Exchange {
    Exchange {
        request: ResolvedRequest {
            id: RequestId::new(id),
            method: HttpMethod::Get,
            url: "https://api.test/ping".parse().unwrap(),
            headers: vec![Header::new("Accept", "application/json")],
            body: http_studio_domain::Body::Empty,
            options: RequestOptions::default(),
        },
        head: ResponseHead {
            status,
            version: "HTTP/1.1".to_owned(),
            headers: vec![Header::new("content-type", "application/json")],
        },
        body: ResponseBody { bytes: body },
        timings: Timings {
            time_to_first_byte: Duration::from_millis(5),
            total: Duration::from_millis(12),
        },
    }
}

/// Opens a fresh database inside `dir`.
fn open(dir: &Path) -> SqliteHistory {
    SqliteHistory::open(&dir.join("history.db")).expect("the database must open")
}

#[tokio::test]
async fn stores_and_returns_the_response_body() {
    let dir = tempfile::tempdir().unwrap();
    let history = open(dir.path());
    history
        .record(&exchange("demo/ping", 200, b"{\"pong\":true}".to_vec()))
        .await
        .unwrap();

    let id = history.query(&HistoryQuery::default()).await.unwrap()[0].id;
    let detail = history.detail(id).await.unwrap().expect("the row exists");

    assert_eq!(detail.body.as_text(), "{\"pong\":true}");
    assert!(!detail.body_truncated);
    assert_eq!(detail.request.id.as_str(), "demo/ping");
    assert_eq!(detail.head.status, 200);
}

#[tokio::test]
async fn a_body_too_large_is_truncated_and_says_so() {
    let dir = tempfile::tempdir().unwrap();
    let history = open(dir.path());
    let huge = vec![b'x'; MAX_STORED_BODY + 1_000];
    history
        .record(&exchange("demo/download", 200, huge.clone()))
        .await
        .unwrap();

    let entry = history
        .query(&HistoryQuery::default())
        .await
        .unwrap()
        .remove(0);
    let detail = history.detail(entry.id).await.unwrap().unwrap();

    assert!(detail.body_truncated);
    assert_eq!(detail.body.len(), MAX_STORED_BODY);
    // The real size survives even when the body does not: it is what gets
    // compared between runs to see whether a response changed weight.
    assert_eq!(entry.response_bytes, u64::try_from(huge.len()).unwrap());
}

#[tokio::test]
async fn an_execution_that_does_not_exist_is_not_an_error() {
    let dir = tempfile::tempdir().unwrap();
    assert!(open(dir.path()).detail(9_999).await.unwrap().is_none());
}

#[tokio::test]
async fn the_stats_count_executions_and_distinct_requests() {
    let dir = tempfile::tempdir().unwrap();
    let history = open(dir.path());
    for id in ["demo/a", "demo/a", "demo/b"] {
        history
            .record(&exchange(id, 200, b"hola".to_vec()))
            .await
            .unwrap();
    }

    let stats = history.stats().await.unwrap();

    assert_eq!(stats.executions, 3);
    assert_eq!(stats.requests, 2);
    assert_eq!(stats.stored_body_bytes, 12);
    assert!(stats.oldest.is_some() && stats.newest.is_some());
}

#[tokio::test]
async fn the_stats_of_an_empty_index_are_zeroes() {
    let dir = tempfile::tempdir().unwrap();
    let stats = open(dir.path()).stats().await.unwrap();

    assert_eq!(stats.executions, 0);
    assert_eq!(stats.oldest, None);
}

#[tokio::test]
async fn pruning_by_keeping_the_last_leaves_only_those() {
    let dir = tempfile::tempdir().unwrap();
    let history = open(dir.path());
    for status in [200, 201, 202, 203] {
        history
            .record(&exchange("demo/a", status, b"x".to_vec()))
            .await
            .unwrap();
    }

    let deleted = history
        .prune(PruneScope::KeepLast { entries: 2 })
        .await
        .unwrap();

    assert_eq!(deleted, 2);
    let remaining = history.query(&HistoryQuery::default()).await.unwrap();
    assert_eq!(remaining.len(), 2);
    // The most recent are kept, which are the ones being looked at.
    assert_eq!(remaining[0].status, 203);
    assert_eq!(remaining[1].status, 202);
}

#[tokio::test]
async fn pruning_by_age_leaves_todays_rows_alone() {
    let dir = tempfile::tempdir().unwrap();
    let history = open(dir.path());
    history
        .record(&exchange("demo/a", 200, b"x".to_vec()))
        .await
        .unwrap();

    let deleted = history
        .prune(PruneScope::OlderThan { days: 7 })
        .await
        .unwrap();

    assert_eq!(deleted, 0);
    assert_eq!(history.stats().await.unwrap().executions, 1);
}

#[tokio::test]
async fn emptying_the_index_leaves_it_usable() {
    // This is the hybrid model's promise: the index can be deleted whole and
    // the program keeps working, because the files are the source of truth.
    let dir = tempfile::tempdir().unwrap();
    let history = open(dir.path());
    history
        .record(&exchange("demo/a", 200, b"x".to_vec()))
        .await
        .unwrap();

    assert_eq!(history.prune(PruneScope::All).await.unwrap(), 1);
    assert_eq!(history.stats().await.unwrap().executions, 0);

    history
        .record(&exchange("demo/a", 200, b"y".to_vec()))
        .await
        .unwrap();
    assert_eq!(history.stats().await.unwrap().executions, 1);
}

#[tokio::test]
async fn a_database_on_the_old_schema_migrates_when_opened() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("history.db");

    // The schema as it was before response bodies were stored.
    let legacy = rusqlite::Connection::open(&path).unwrap();
    legacy
        .execute_batch(
            "CREATE TABLE exchanges (
                 id             INTEGER PRIMARY KEY AUTOINCREMENT,
                 request_id     TEXT    NOT NULL,
                 method         TEXT    NOT NULL,
                 url            TEXT    NOT NULL,
                 status         INTEGER NOT NULL,
                 duration_ms    INTEGER NOT NULL,
                 response_bytes INTEGER NOT NULL,
                 executed_at    TEXT    NOT NULL DEFAULT (datetime('now')),
                 request_json   TEXT    NOT NULL,
                 response_json  TEXT    NOT NULL
             );",
        )
        .unwrap();
    drop(legacy);

    let history = SqliteHistory::open(&path).expect("it must migrate rather than fail");
    history
        .record(&exchange("demo/a", 200, b"hola".to_vec()))
        .await
        .unwrap();

    let id = history.query(&HistoryQuery::default()).await.unwrap()[0].id;
    assert_eq!(
        history.detail(id).await.unwrap().unwrap().body.as_text(),
        "hola"
    );
}
