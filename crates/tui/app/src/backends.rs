//! Composition-root backend registry.
//!
//! Cowboy's binaries decide which agent backends exist. The workflow engine
//! itself only routes backend-neutral launch specs through a registry, so this
//! module is where concrete protocols (ACP today, pi RPC and others later) get
//! installed.

use cowboy_agent_client::BackendRegistry;

/// Registry with every backend kind compiled into this build.
pub fn production_backend_registry() -> BackendRegistry {
    BackendRegistry::new(vec![
        std::sync::Arc::new(cowboy_agent_acp::AcpBackendConnector),
        std::sync::Arc::new(cowboy_agent_claude::ClaudeCodeBackendConnector),
        std::sync::Arc::new(cowboy_agent_pi::PiRpcBackendConnector),
        std::sync::Arc::new(cowboy_agent_codex::CodexBackendConnector),
        #[cfg(feature = "copilot-sdk")]
        std::sync::Arc::new(cowboy_agent_copilot_sdk::CopilotSdkBackendConnector::default()),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registers_compiled_backends() {
        let registry = production_backend_registry();
        assert!(registry.connector("acp").is_some());
        assert!(registry.connector("claude-code").is_some());
        assert!(registry.connector("pi-rpc").is_some());
        assert!(registry.connector("codex-app-server").is_some());
        assert_eq!(
            registry.connector("copilot-sdk").is_some(),
            cfg!(feature = "copilot-sdk")
        );
    }
}
