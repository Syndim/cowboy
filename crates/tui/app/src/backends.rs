//! Composition-root backend registry.
//!
//! Cowboy's binaries decide which agent backends exist. The workflow engine
//! itself only routes backend-neutral launch specs through a registry, so this
//! module is where concrete protocols (ACP today, pi RPC and others later) get
//! installed.

use cowboy_agent_client::BackendRegistry;

/// Registry with every backend kind compiled into this build.
pub fn production_backend_registry() -> BackendRegistry {
    BackendRegistry::new(vec![std::sync::Arc::new(
        cowboy_agent_acp::AcpBackendConnector,
    )])
}
