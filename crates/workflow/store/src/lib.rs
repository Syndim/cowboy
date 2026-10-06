//! SQLx SQLite-backed workflow store.

mod error;
mod hash;
mod native_ownership;
mod schema;
mod sqlite_store;

#[cfg(test)]
mod contract;

pub use error::{Error, Result};
pub use hash::{canonical_object_bytes, object_hash};
pub use native_ownership::{
    NativeProcessExit, NativeShutdownEvidence, NativeWriterExit, NativeWriterStart,
};
#[cfg(test)]
pub use sqlite_store::RestartFailurePoint;
pub use sqlite_store::{
    RestartCreationOutcome, RestartSeed, SqliteWorkflowStore, StoreWaitCancellation,
    StoreWaitObserver, is_retryable_sqlite_code,
};
