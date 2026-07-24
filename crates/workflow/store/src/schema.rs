use std::path::Path;
use std::time::{Duration, Instant};

use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::{Connection, Executor, Row, SqliteConnection, SqlitePool};

use crate::{Error, Result, is_retryable_sqlite_code};

pub(crate) const SCHEMA_VERSION: i64 = 1;
const BOOTSTRAP_TIMEOUT: Duration = Duration::from_secs(5);
const RETRY_BACKOFF: Duration = Duration::from_millis(25);

pub(crate) const TABLES: &[&str] = &[
    "runs",
    "run_heads",
    "objects",
    "role_sessions",
    "run_turns",
    "run_user_prompts",
    "agent_prompt_windows",
];

const DDL: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS runs (run_id TEXT PRIMARY KEY NOT NULL, data BLOB NOT NULL)",
    "CREATE TABLE IF NOT EXISTS run_heads (run_id TEXT PRIMARY KEY NOT NULL, data BLOB NOT NULL)",
    "CREATE TABLE IF NOT EXISTS objects (hash TEXT PRIMARY KEY NOT NULL, kind TEXT NOT NULL, data BLOB NOT NULL)",
    "CREATE TABLE IF NOT EXISTS role_sessions (run_id TEXT NOT NULL, role_id TEXT NOT NULL, data BLOB NOT NULL, PRIMARY KEY(run_id, role_id))",
    "CREATE TABLE IF NOT EXISTS run_turns (run_id TEXT NOT NULL, step_record_id TEXT NOT NULL, position INTEGER NOT NULL, object_hash TEXT NOT NULL, PRIMARY KEY(run_id, step_record_id, position))",
    "CREATE INDEX IF NOT EXISTS run_turns_object_hash ON run_turns(object_hash)",
    "CREATE TABLE IF NOT EXISTS run_user_prompts (run_id TEXT NOT NULL, sequence INTEGER NOT NULL, data BLOB NOT NULL, PRIMARY KEY(run_id, sequence))",
    "CREATE TABLE IF NOT EXISTS agent_prompt_windows (run_id TEXT PRIMARY KEY NOT NULL, window_id TEXT NOT NULL, data BLOB NOT NULL)",
];

pub(crate) async fn connect(path: &Path) -> Result<SqlitePool> {
    preflight_existing_file(path).await?;
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }

    let options = connect_options(path, true);
    let mut connection = SqliteConnection::connect_with(&options).await?;
    wait_at_test_bootstrap_barrier(path).await;
    bootstrap(&mut connection).await?;
    establish_wal(&mut connection).await?;
    connection.close().await?;

    Ok(SqlitePoolOptions::new()
        .min_connections(0)
        .max_connections(4)
        .acquire_timeout(Duration::from_secs(5))
        .connect_with(connect_options(path, false))
        .await?)
}

#[cfg(test)]
type BootstrapBarrier = (std::path::PathBuf, std::sync::Arc<tokio::sync::Barrier>);

#[cfg(test)]
fn bootstrap_barrier() -> &'static std::sync::Mutex<Option<BootstrapBarrier>> {
    static BARRIER: std::sync::OnceLock<std::sync::Mutex<Option<BootstrapBarrier>>> =
        std::sync::OnceLock::new();
    BARRIER.get_or_init(|| std::sync::Mutex::new(None))
}

#[cfg(test)]
async fn wait_at_test_bootstrap_barrier(path: &Path) {
    let barrier = bootstrap_barrier()
        .lock()
        .expect("bootstrap barrier lock poisoned")
        .as_ref()
        .filter(|(barrier_path, _)| barrier_path == path)
        .map(|(_, barrier)| barrier.clone());
    if let Some(barrier) = barrier {
        barrier.wait().await;
    }
}

#[cfg(not(test))]
async fn wait_at_test_bootstrap_barrier(_path: &Path) {}

fn connect_options(path: &Path, create: bool) -> SqliteConnectOptions {
    SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(create)
        .foreign_keys(true)
        .busy_timeout(Duration::ZERO)
}

async fn preflight_existing_file(path: &Path) -> Result<()> {
    let Ok(metadata) = tokio::fs::metadata(path).await else {
        return Ok(());
    };
    if metadata.len() == 0 {
        return Ok(());
    }
    let bytes = tokio::fs::read(path).await?;
    if bytes.get(..16) != Some(b"SQLite format 3\0") {
        return Err(Error::NonSqliteFile(path.display().to_string()));
    }

    let options = connect_options(path, false).read_only(true);
    let mut connection = SqliteConnection::connect_with(&options).await?;
    let version: i64 = sqlx::query_scalar("PRAGMA user_version")
        .fetch_one(&mut connection)
        .await?;
    connection.close().await?;
    reject_future(version)
}

async fn bootstrap(connection: &mut SqliteConnection) -> Result<()> {
    let started = Instant::now();
    loop {
        match connection.execute("BEGIN IMMEDIATE").await {
            Ok(_) => {
                let result = initialize_in_transaction(connection).await;
                match result {
                    Ok(()) => {
                        connection.execute("COMMIT").await?;
                        return Ok(());
                    }
                    Err(error) => {
                        let _ = connection.execute("ROLLBACK").await;
                        return Err(error);
                    }
                }
            }
            Err(error) if is_retryable_sqlite_error(&error) => {
                if started.elapsed() >= BOOTSTRAP_TIMEOUT {
                    return Err(Error::BootstrapTimeout);
                }
                tokio::time::sleep(RETRY_BACKOFF).await;
            }
            Err(error) => return Err(error.into()),
        }
    }
}

async fn initialize_in_transaction(connection: &mut SqliteConnection) -> Result<()> {
    let version: i64 = sqlx::query_scalar("PRAGMA user_version")
        .fetch_one(&mut *connection)
        .await?;
    reject_future(version)?;
    if version == 0 {
        for statement in DDL {
            connection.execute(*statement).await?;
        }
        connection.execute("PRAGMA user_version = 1").await?;
    }
    validate_tables(connection).await
}

async fn validate_tables(connection: &mut SqliteConnection) -> Result<()> {
    for table in TABLES {
        let row = sqlx::query("SELECT name FROM sqlite_master WHERE type = 'table' AND name = ?")
            .bind(table)
            .fetch_optional(&mut *connection)
            .await?;
        if row.is_none() {
            return Err(sqlx::Error::Protocol(format!(
                "workflow store schema is missing table {table}"
            ))
            .into());
        }
    }
    Ok(())
}

async fn establish_wal(connection: &mut SqliteConnection) -> Result<()> {
    let started = Instant::now();
    loop {
        match sqlx::query("PRAGMA journal_mode = WAL")
            .fetch_one(&mut *connection)
            .await
        {
            Ok(row) => {
                let mode: String = row.try_get(0)?;
                if !mode.eq_ignore_ascii_case("wal") {
                    return Err(sqlx::Error::Protocol(format!(
                        "failed to enable SQLite WAL mode (got {mode})"
                    ))
                    .into());
                }
                return Ok(());
            }
            Err(error) if is_retryable_sqlite_error(&error) => {
                if started.elapsed() >= BOOTSTRAP_TIMEOUT {
                    return Err(Error::BootstrapTimeout);
                }
                tokio::time::sleep(RETRY_BACKOFF).await;
            }
            Err(error) => return Err(error.into()),
        }
    }
}

fn reject_future(version: i64) -> Result<()> {
    if version > SCHEMA_VERSION {
        Err(Error::FutureSchema {
            found: version,
            supported: SCHEMA_VERSION,
        })
    } else {
        Ok(())
    }
}

fn is_retryable_sqlite_error(error: &sqlx::Error) -> bool {
    match error {
        sqlx::Error::Database(error) => is_retryable_sqlite_code(error.code().as_deref()),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::Row;

    #[tokio::test]
    async fn initializes_new_and_empty_files() {
        for empty in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join(if empty { "empty.db" } else { "new.db" });
            if empty {
                tokio::fs::write(&path, []).await.unwrap();
            }
            let store = crate::SqliteWorkflowStore::connect(&path).await.unwrap();
            assert_eq!(
                &tokio::fs::read(&path).await.unwrap()[..16],
                b"SQLite format 3\0"
            );
            let mut connection = store.pool().acquire().await.unwrap();
            let version: i64 = sqlx::query_scalar("PRAGMA user_version")
                .fetch_one(&mut *connection)
                .await
                .unwrap();
            let foreign_keys: i64 = sqlx::query_scalar("PRAGMA foreign_keys")
                .fetch_one(&mut *connection)
                .await
                .unwrap();
            let mode: String = sqlx::query("PRAGMA journal_mode")
                .fetch_one(&mut *connection)
                .await
                .unwrap()
                .get(0);
            assert_eq!(version, 1);
            assert_eq!(foreign_keys, 1);
            assert_eq!(mode.to_ascii_lowercase(), "wal");
            assert_eq!(store.pool().options().get_max_connections(), 4);
        }
        println!(
            "EVIDENCE schema-init header=SQLite_format_3 user_version=1 wal=true foreign_keys=true max_connections=4"
        );
    }

    #[tokio::test]
    async fn reopens_supported_schema_version() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("data.db");
        crate::SqliteWorkflowStore::connect(&path)
            .await
            .unwrap()
            .close()
            .await;
        let store = crate::SqliteWorkflowStore::connect(&path).await.unwrap();
        assert_eq!(
            sqlx::query_scalar::<_, i64>("PRAGMA user_version")
                .fetch_one(store.pool())
                .await
                .unwrap(),
            1
        );
        let table_count: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM sqlite_master WHERE type='table' AND name IN ('runs','run_heads','objects','role_sessions','run_turns','run_user_prompts','agent_prompt_windows')",
        )
        .fetch_one(store.pool())
        .await
        .unwrap();
        assert_eq!(table_count, 7);
        println!("EVIDENCE schema-reopen user_version=1 tables=7");
    }

    #[tokio::test]
    async fn rejects_future_schema_version_without_modifying_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("future.db");
        let store = crate::SqliteWorkflowStore::connect(&path).await.unwrap();
        sqlx::query("PRAGMA user_version = 2")
            .execute(store.pool())
            .await
            .unwrap();
        store.close().await;
        let before = tokio::fs::read(&path).await.unwrap();
        let error = crate::SqliteWorkflowStore::connect(&path)
            .await
            .unwrap_err();
        assert!(matches!(error, Error::FutureSchema { found: 2, .. }));
        assert_eq!(tokio::fs::read(&path).await.unwrap(), before);
        println!("EVIDENCE schema-future rejected=true bytes_unchanged=true");
    }

    #[tokio::test]
    async fn rejects_non_sqlite_file_without_modifying_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("legacy.redb");
        let before = b"legacy-redb-placeholder\n".to_vec();
        tokio::fs::write(&path, &before).await.unwrap();
        let error = crate::SqliteWorkflowStore::connect(&path)
            .await
            .unwrap_err();
        assert!(matches!(error, Error::NonSqliteFile(_)));
        assert_eq!(tokio::fs::read(&path).await.unwrap(), before);
        println!("EVIDENCE schema-non-sqlite rejected=true bytes_unchanged=true");
    }

    #[tokio::test]
    async fn concurrent_first_connects_initialize_one_schema() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("data.db");
        let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(2));
        *bootstrap_barrier()
            .lock()
            .expect("bootstrap barrier lock poisoned") = Some((path.clone(), barrier));
        let (a, b) = tokio::join!(
            crate::SqliteWorkflowStore::connect(&path),
            crate::SqliteWorkflowStore::connect(&path)
        );
        *bootstrap_barrier()
            .lock()
            .expect("bootstrap barrier lock poisoned") = None;
        let a = a.unwrap();
        let b = b.unwrap();
        for store in [&a, &b] {
            let count: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM sqlite_master WHERE type='table' AND name IN ('runs','run_heads','objects','role_sessions','run_turns','run_user_prompts','agent_prompt_windows')",
            )
            .fetch_one(store.pool())
            .await
            .unwrap();
            assert_eq!(count, 7);
        }
        println!("EVIDENCE schema-concurrent connects=2 user_version=1 tables=7");
    }
}
