//! The execution history, backed by SQLite.
//!
//! This is the "index" half of the hybrid model. It records **what happened**,
//! not **what gets sent**: that is why it can be deleted at any moment without
//! breaking the workspace, and why it lives outside the git repository, in the
//! user's data directory, rather than next to the `.http` files.
//!
//! `rusqlite` has a synchronous API, so every write goes through
//! `spawn_blocking` and the connection is guarded by a `Mutex`. For the volume
//! of an interactive HTTP client that is more than enough; if more is ever
//! needed, the [`ExchangeRecorder`] port allows swapping the adapter without
//! touching anything else.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use http_studio_application::{
    ApplicationError, ExchangeRecorder, HistoryMaintenance, HistoryQuery, HistoryReader, PruneScope,
};
use http_studio_domain::{
    Exchange, HistoryDetail, HistoryEntry, HistoryStats, RequestId, ResolvedRequest, ResponseBody,
    ResponseHead,
};
use rusqlite::Connection;

/// The schema creation statements, applied idempotently.
const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS exchanges (
    id             INTEGER PRIMARY KEY AUTOINCREMENT,
    request_id     TEXT    NOT NULL,
    method         TEXT    NOT NULL,
    url            TEXT    NOT NULL,
    status         INTEGER NOT NULL,
    duration_ms    INTEGER NOT NULL,
    response_bytes INTEGER NOT NULL,
    executed_at    TEXT    NOT NULL DEFAULT (datetime('now')),
    request_json   TEXT    NOT NULL,
    response_json  TEXT    NOT NULL,
    response_body  BLOB,
    body_truncated INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS idx_exchanges_request_id ON exchanges (request_id);
CREATE INDEX IF NOT EXISTS idx_exchanges_executed_at ON exchanges (executed_at DESC);
";

/// Columns added after the schema's first version.
///
/// `CREATE TABLE IF NOT EXISTS` leaves an existing table alone, so without this
/// a database created by the previous version would be missing the new columns.
/// They are applied one at a time and the "already exists" error is ignored:
/// the index is a cache, but wiping it to add a column would be throwing away
/// someone's history for our own convenience.
const MIGRATIONS: &[&str] = &[
    "ALTER TABLE exchanges ADD COLUMN response_body BLOB",
    "ALTER TABLE exchanges ADD COLUMN body_truncated INTEGER NOT NULL DEFAULT 0",
];

/// The most response body that gets stored, in bytes.
///
/// The index exists to answer "what did this reply last time?", not to archive
/// downloads: 256 KiB covers any API response and keeps the database
/// manageable even after months of accumulation. What does not fit is truncated
/// and marked as truncated, rather than quietly stored half-complete.
pub const MAX_STORED_BODY: usize = 256 * 1024;

/// A history recorder over a local SQLite database.
#[derive(Clone)]
pub struct SqliteHistory {
    connection: Arc<Mutex<Connection>>,
}

impl SqliteHistory {
    /// Opens — or creates — the database at `path` and applies the schema.
    ///
    /// # Errors
    ///
    /// [`ApplicationError::Repository`] when the directory cannot be created,
    /// the database cannot be opened, or the schema cannot be applied.
    pub fn open(path: &Path) -> Result<Self, ApplicationError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|error| {
                ApplicationError::repository(format!("{}: {error}", parent.display()))
            })?;
        }

        let connection = Connection::open(path).map_err(|error| {
            ApplicationError::repository(format!("{}: {error}", path.display()))
        })?;

        connection
            .execute_batch(SCHEMA)
            .map_err(|error| ApplicationError::repository(format!("schema: {error}")))?;

        for migration in MIGRATIONS {
            // The only expected error is "duplicate column name", which here
            // means "it was already there"; any other surfaces on open.
            drop(connection.execute(migration, []));
        }

        Ok(Self {
            connection: Arc::new(Mutex::new(connection)),
        })
    }

    /// The history's default path, in the user's data directory.
    ///
    /// Returns `None` when the system exposes no standard directories, in which
    /// case the caller decides — by using [`NullHistory`], for instance.
    #[must_use]
    pub fn default_path() -> Option<PathBuf> {
        directories::ProjectDirs::from("es", "usarral", "http-studio")
            .map(|dirs| dirs.data_dir().join("history.db"))
    }

    /// Inserts the exchange synchronously.
    fn insert(connection: &Connection, exchange: &Exchange) -> Result<(), ApplicationError> {
        let request_json = serde_json::to_string(&exchange.request).map_err(|error| {
            ApplicationError::repository(format!("serializing the request: {error}"))
        })?;
        let response_json = serde_json::to_string(&exchange.head).map_err(|error| {
            ApplicationError::repository(format!("serializing the response: {error}"))
        })?;

        let stored = &exchange.body.bytes[..exchange.body.len().min(MAX_STORED_BODY)];
        let truncated = i64::from(exchange.body.len() > MAX_STORED_BODY);

        connection
            .execute(
                "INSERT INTO exchanges (
                     request_id, method, url, status, duration_ms,
                     response_bytes, request_json, response_json,
                     response_body, body_truncated
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                rusqlite::params![
                    exchange.request.id.as_str(),
                    exchange.request.method.as_str(),
                    exchange.request.url.as_str(),
                    exchange.head.status,
                    i64::try_from(exchange.timings.total.as_millis()).unwrap_or(i64::MAX),
                    // `response_bytes` is the response's REAL size, not the
                    // stored slice's: it is what gets compared between runs.
                    i64::try_from(exchange.body.len()).unwrap_or(i64::MAX),
                    request_json,
                    response_json,
                    stored,
                    truncated,
                ],
            )
            .map_err(|error| {
                ApplicationError::repository(format!("inserting into the history: {error}"))
            })?;

        Ok(())
    }
}

#[async_trait]
impl ExchangeRecorder for SqliteHistory {
    async fn record(&self, exchange: &Exchange) -> Result<(), ApplicationError> {
        let connection = Arc::clone(&self.connection);
        let exchange = exchange.clone();

        tokio::task::spawn_blocking(move || {
            let guard = connection.lock().map_err(|_| {
                ApplicationError::repository("the history mutex was poisoned by a panic")
            })?;
            SqliteHistory::insert(&guard, &exchange)
        })
        .await
        .map_err(|error| ApplicationError::repository(format!("task interrupted: {error}")))?
    }
}

/// Builds the query statement and its parameters from the filter.
///
/// They are assembled together so they cannot fall out of step: every `AND`
/// added to the clause pushes its value onto the parameter list in the same
/// breath.
fn build_query(query: &HistoryQuery) -> (String, Vec<Box<dyn rusqlite::ToSql>>) {
    let mut conditions = Vec::new();
    let mut params: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();

    if let Some(request_id) = &query.request_id {
        conditions.push("request_id = ?");
        params.push(Box::new(request_id.as_str().to_owned()));
    }

    if let Some(status) = query.status {
        conditions.push("status = ?");
        params.push(Box::new(i64::from(status)));
    }

    let where_clause = if conditions.is_empty() {
        String::new()
    } else {
        format!(" WHERE {}", conditions.join(" AND "))
    };

    params.push(Box::new(i64::from(query.limit)));

    (
        format!(
            "SELECT id, request_id, method, url, status, duration_ms, response_bytes, executed_at
             FROM exchanges{where_clause}
             ORDER BY id DESC
             LIMIT ?"
        ),
        params,
    )
}

/// Translates a row of the index into a domain entry.
fn row_to_entry(row: &rusqlite::Row<'_>) -> rusqlite::Result<HistoryEntry> {
    Ok(HistoryEntry {
        id: row.get(0)?,
        request_id: RequestId::new(row.get::<_, String>(1)?),
        method: row.get(2)?,
        url: row.get(3)?,
        // SQLite only has signed integers; these fields are non-negative by
        // construction, so a negative value would be corruption and saturates
        // to zero rather than failing a whole query over one old row.
        status: u16::try_from(row.get::<_, i64>(4)?).unwrap_or_default(),
        duration_ms: u64::try_from(row.get::<_, i64>(5)?).unwrap_or_default(),
        response_bytes: u64::try_from(row.get::<_, i64>(6)?).unwrap_or_default(),
        executed_at: row.get(7)?,
    })
}

#[async_trait]
impl HistoryReader for SqliteHistory {
    async fn query(&self, query: &HistoryQuery) -> Result<Vec<HistoryEntry>, ApplicationError> {
        let connection = Arc::clone(&self.connection);
        let query = query.clone();

        tokio::task::spawn_blocking(move || {
            let guard = connection.lock().map_err(|_| {
                ApplicationError::repository("the history mutex was poisoned by a panic")
            })?;

            let (sql, params) = build_query(&query);
            let mut statement = guard
                .prepare(&sql)
                .map_err(|error| ApplicationError::repository(format!("query: {error}")))?;

            let rows = statement
                .query_map(rusqlite::params_from_iter(params.iter()), row_to_entry)
                .map_err(|error| ApplicationError::repository(format!("query: {error}")))?;

            rows.collect::<rusqlite::Result<Vec<_>>>()
                .map_err(|error| ApplicationError::repository(format!("query: {error}")))
        })
        .await
        .map_err(|error| ApplicationError::repository(format!("task interrupted: {error}")))?
    }

    async fn detail(&self, id: i64) -> Result<Option<HistoryDetail>, ApplicationError> {
        let connection = Arc::clone(&self.connection);

        tokio::task::spawn_blocking(move || {
            let guard = connection.lock().map_err(|_| {
                ApplicationError::repository("the history mutex was poisoned by a panic")
            })?;
            read_detail(&guard, id)
        })
        .await
        .map_err(|error| ApplicationError::repository(format!("task interrupted: {error}")))?
    }
}

/// Reads a row's detail and rebuilds it as a domain entity.
///
/// The request and the headers travel as JSON in their columns, so a change to
/// the model can meet old rows that no longer parse. Those count as one row
/// being corrupt rather than the index failing: an error naming the row is more
/// use than losing the query.
fn read_detail(
    connection: &Connection,
    id: i64,
) -> Result<Option<HistoryDetail>, ApplicationError> {
    let mut statement = connection
        .prepare(
            "SELECT id, request_id, method, url, status, duration_ms, response_bytes,
                    executed_at, request_json, response_json, response_body, body_truncated
             FROM exchanges WHERE id = ?1",
        )
        .map_err(|error| ApplicationError::repository(format!("query: {error}")))?;

    let mut rows = statement
        .query([id])
        .map_err(|error| ApplicationError::repository(format!("query: {error}")))?;

    let Some(row) = rows
        .next()
        .map_err(|error| ApplicationError::repository(format!("query: {error}")))?
    else {
        return Ok(None);
    };

    let entry = row_to_entry(row)
        .map_err(|error| ApplicationError::repository(format!("row {id}: {error}")))?;

    let column = |index: usize| -> Result<String, ApplicationError> {
        row.get::<_, String>(index)
            .map_err(|error| ApplicationError::repository(format!("row {id}: {error}")))
    };

    let request: ResolvedRequest = serde_json::from_str(&column(8)?)
        .map_err(|error| ApplicationError::repository(format!("row {id}: request: {error}")))?;
    let head: ResponseHead = serde_json::from_str(&column(9)?)
        .map_err(|error| ApplicationError::repository(format!("row {id}: response: {error}")))?;

    // A row older than the body column carries `NULL` there: the detail comes
    // back anyway, with an empty body, instead of pretending it does not exist.
    let bytes = row
        .get::<_, Option<Vec<u8>>>(10)
        .map_err(|error| ApplicationError::repository(format!("row {id}: {error}")))?
        .unwrap_or_default();
    let body_truncated = row.get::<_, i64>(11).unwrap_or_default() != 0;

    Ok(Some(HistoryDetail {
        entry,
        request,
        head,
        body: ResponseBody { bytes },
        body_truncated,
    }))
}

/// Translates the prune scope into its SQL condition and parameters.
///
/// `KeepLast` cannot be expressed as a `WHERE` over columns, so it is resolved
/// with a subquery over the identifiers: they increase, so "the N most recent"
/// is "the N largest".
fn prune_statement(scope: PruneScope) -> (String, Vec<Box<dyn rusqlite::ToSql>>) {
    match scope {
        PruneScope::All => ("DELETE FROM exchanges".to_owned(), Vec::new()),
        PruneScope::OlderThan { days } => (
            "DELETE FROM exchanges WHERE executed_at < datetime('now', ?1)".to_owned(),
            vec![Box::new(format!("-{days} days"))],
        ),
        PruneScope::KeepLast { entries } => (
            "DELETE FROM exchanges WHERE id NOT IN \
             (SELECT id FROM exchanges ORDER BY id DESC LIMIT ?1)"
                .to_owned(),
            vec![Box::new(i64::from(entries))],
        ),
    }
}

#[async_trait]
impl HistoryMaintenance for SqliteHistory {
    async fn stats(&self) -> Result<HistoryStats, ApplicationError> {
        let connection = Arc::clone(&self.connection);

        tokio::task::spawn_blocking(move || {
            let guard = connection.lock().map_err(|_| {
                ApplicationError::repository("the history mutex was poisoned by a panic")
            })?;

            guard
                .query_row(
                    "SELECT COUNT(*), COUNT(DISTINCT request_id), MIN(executed_at),
                            MAX(executed_at), COALESCE(SUM(LENGTH(response_body)), 0)
                     FROM exchanges",
                    [],
                    |row| {
                        Ok(HistoryStats {
                            executions: u64::try_from(row.get::<_, i64>(0)?).unwrap_or_default(),
                            requests: u64::try_from(row.get::<_, i64>(1)?).unwrap_or_default(),
                            oldest: row.get(2)?,
                            newest: row.get(3)?,
                            stored_body_bytes: u64::try_from(row.get::<_, i64>(4)?)
                                .unwrap_or_default(),
                        })
                    },
                )
                .map_err(|error| ApplicationError::repository(format!("query: {error}")))
        })
        .await
        .map_err(|error| ApplicationError::repository(format!("task interrupted: {error}")))?
    }

    async fn prune(&self, scope: PruneScope) -> Result<u64, ApplicationError> {
        let connection = Arc::clone(&self.connection);

        tokio::task::spawn_blocking(move || {
            let guard = connection.lock().map_err(|_| {
                ApplicationError::repository("the history mutex was poisoned by a panic")
            })?;

            let (sql, params) = prune_statement(scope);
            let deleted = guard
                .execute(&sql, rusqlite::params_from_iter(params.iter()))
                .map_err(|error| ApplicationError::repository(format!("prune: {error}")))?;

            // `VACUUM` after emptying: otherwise the file keeps its size and
            // `hts index` would go on reporting the megabytes it just claimed
            // to have freed, which is precisely what was asked not to happen.
            if deleted > 0 {
                drop(guard.execute("VACUUM", []));
            }

            Ok(u64::try_from(deleted).unwrap_or_default())
        })
        .await
        .map_err(|error| ApplicationError::repository(format!("task interrupted: {error}")))?
    }
}

/// A recorder that discards everything.
///
/// Used when the user asks for `--no-history`, when there is no data directory
/// available, and in tests that would rather not touch the disk.
#[derive(Debug, Clone, Copy, Default)]
pub struct NullHistory;

#[async_trait]
impl ExchangeRecorder for NullHistory {
    async fn record(&self, _exchange: &Exchange) -> Result<(), ApplicationError> {
        Ok(())
    }
}

#[async_trait]
impl HistoryReader for NullHistory {
    async fn query(&self, _query: &HistoryQuery) -> Result<Vec<HistoryEntry>, ApplicationError> {
        Ok(Vec::new())
    }

    async fn detail(&self, _id: i64) -> Result<Option<HistoryDetail>, ApplicationError> {
        Ok(None)
    }
}

#[async_trait]
impl HistoryMaintenance for NullHistory {
    async fn stats(&self) -> Result<HistoryStats, ApplicationError> {
        Ok(HistoryStats::default())
    }

    async fn prune(&self, _scope: PruneScope) -> Result<u64, ApplicationError> {
        Ok(0)
    }
}
