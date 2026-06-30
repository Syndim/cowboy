set shell := ['nu', '-c']
set dotenv-load := true

[private]
default:
    just -l

# Run the cowboy TUI
start:
    cargo run

# Run live ACP integration tests against a backend (copilot or omp).
# Requires the agent CLI authenticated on PATH, e.g. `copilot --acp` or `omp acp`.
acp-test backend:
    cargo test -p cowboy-agent-acp --test {{backend}}_acp -- --ignored --nocapture

# Build workflow test apps into target/debug/test-apps
test-apps:
    mkdir target/debug/test-apps
    cargo build -p cowboy-agent-acp --bin acp-chat -p cowboy-workflow-lua --bin workflow-chart -p cowboy-workflow-store --bin store-cli -p cowboy-workflow-agent --bin execute-agent -p cowboy-workflow-catalog --bin catalog-cli -p cowboy-workflow-engine --bin engine-cli
    rm -f target/debug/test-apps/acp-chat target/debug/test-apps/workflow-chart target/debug/test-apps/store-cli target/debug/test-apps/execute-agent target/debug/test-apps/catalog-cli target/debug/test-apps/engine-cli
    mv target/debug/acp-chat target/debug/test-apps/acp-chat
    mv target/debug/workflow-chart target/debug/test-apps/workflow-chart
    mv target/debug/store-cli target/debug/test-apps/store-cli
    mv target/debug/execute-agent target/debug/test-apps/execute-agent
    mv target/debug/catalog-cli target/debug/test-apps/catalog-cli
    mv target/debug/engine-cli target/debug/test-apps/engine-cli
    rm -rf target/debug/test-apps/test_files
    cp -r crates/workflow/lua/test_files target/debug/test-apps/test_files
    rm -rf target/debug/test-apps/catalog-workflows
    cp -r crates/workflow/catalog/test_files target/debug/test-apps/catalog-workflows
    rm -rf target/debug/test-apps/engine-workflows
    cp -r crates/workflow/engine/test_files target/debug/test-apps/engine-workflows