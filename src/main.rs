#[tokio::main]
async fn main() -> std::process::ExitCode {
    mcp_editor_lsp_bridge::cli::run().await
}
