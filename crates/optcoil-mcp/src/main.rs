mod server;

use std::path::PathBuf;

use rmcp::{ServiceExt, transport::stdio};
use server::{McpServer, ServerState};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args_os().skip(1);
    let mut workspace_dir: Option<PathBuf> = None;
    while let Some(arg) = args.next() {
        if arg == "--workspace-dir" {
            if workspace_dir.is_some() {
                return Err("--workspace-dir may only be specified once".into());
            }
            workspace_dir = Some(args.next().ok_or("--workspace-dir requires a path")?.into());
        } else if arg == "--help" || arg == "-h" {
            eprintln!(
                "optcoil-mcp --workspace-dir <directory>\n\nRun the local Converra MCP server over stdio. All persistent data stays under the explicit workspace directory."
            );
            return Ok(());
        } else {
            return Err(format!("unknown argument: {}", arg.to_string_lossy()).into());
        }
    }
    let workspace_dir = workspace_dir.ok_or("--workspace-dir is required")?;
    let state = ServerState::open(workspace_dir)?;
    let server = McpServer::new(state.clone()).serve(stdio()).await?;
    let result = server.waiting().await;
    state.shutdown();
    result?;
    Ok(())
}
