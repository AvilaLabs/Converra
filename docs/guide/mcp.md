# Local MCP server

`optcoil-mcp` lets an MCP client operate a Converra study through typed local tools. It uses stdio and an explicit study directory. The GUI and server share the workspace format and engine operations.

Download the platform's CLI/MCP archive, or build current source:

```bash
cargo build --release -p optcoil-mcp
```

Configure your client with absolute executable and workspace paths:

```json
{
  "mcpServers": {
    "converra": {
      "command": "/absolute/path/to/optcoil-mcp",
      "args": ["--workspace-dir", "/absolute/path/to/study"]
    }
  }
}
```

Use `.exe` on Windows. Client configuration formats vary; the process arguments remain the same. Run one server per workspace directory.

## A study through tools

Discover capabilities and datasets, create/import a study, create a named variant and attach its material dependencies. Use `preflight_variant` and `diagnose_variant`, then `start_search`. Poll `get_job` until completed, cancelled or failed. A cancellation request alone does not establish completion.

Inspect the result, compare variants and export a review package. Resources expose full inputs and records when tool responses return compact summaries. Completed evidence persists with the workspace.

Source builds also offer `preview_robustness`, `start_robustness` and scenario history. A proposed follow-up is unevaluated until calculated. The live-session cache only reuses verified unchanged calculations; reopening a workspace preserves history without seeding that cache.

Setup, complete tool list and budgets: [MCP reference](https://github.com/AvilaLabs/Converra/blob/main/crates/optcoil-mcp/README.md).
