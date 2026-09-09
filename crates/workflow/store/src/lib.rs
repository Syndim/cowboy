//! SQLx SQLite-backed workflow store.

mod error;
mod hash;
mod schema;
mod sqlite_store;

#[cfg(test)]
mod contract;

pub use error::{Error, Result};
pub use hash::{canonical_object_bytes, object_hash};
#[cfg(test)]
pub use sqlite_store::RestartFailurePoint;
pub use sqlite_store::{
    RestartCreationOutcome, RestartSeed, SqliteWorkflowStore, StoreWaitCancellation,
    StoreWaitObserver, is_retryable_sqlite_code,
};
