#[cfg(feature = "sdk")]
mod sdk;

#[cfg(feature = "sdk")]
pub use sdk::CopilotSdkBackendConnector;
