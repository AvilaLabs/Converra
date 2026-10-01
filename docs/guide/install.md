# Install and choose a workflow

## Browser

Open [converra.avilalabs.org](https://converra.avilalabs.org). You can edit cases, import CSV/TSV/XLSX material tables, run searches and scenario studies, and download reports and review packages. Calculations run on your device in a Web Worker.

Draft recovery is specific to this browser on this device. Export a study workspace to keep a portable copy. Folder libraries, queues, directory comparisons and watch mode require desktop.

## Desktop and CLI

Download an archive from [GitHub Releases](https://github.com/AvilaLabs/Converra/releases/latest).

| Platform | Download |
| --- | --- |
| Windows x64 | [Desktop, CLI and MCP ZIP](https://github.com/AvilaLabs/Converra/releases/latest/download/converra-windows-x64.zip) |
| Linux x64 | [Desktop, CLI and MCP archive](https://github.com/AvilaLabs/Converra/releases/latest/download/converra-linux-x64.tar.gz) |
| macOS Apple Silicon | [App](https://github.com/AvilaLabs/Converra/releases/latest/download/converra-macos-arm64.zip) · [CLI and MCP](https://github.com/AvilaLabs/Converra/releases/latest/download/converra-macos-arm64-cli.tar.gz) |
| macOS Intel | [App](https://github.com/AvilaLabs/Converra/releases/latest/download/converra-macos-x64.zip) · [CLI and MCP](https://github.com/AvilaLabs/Converra/releases/latest/download/converra-macos-x64-cli.tar.gz) |

Extract and launch `Converra` or `Converra.app`. The CLI is `optcoil`; the local agent server is `optcoil-mcp`. Windows adds `.exe`. No Rust installation is needed for these binaries. Verify the archives against the release's `SHA256SUMS`. Linux desktop needs a graphical session and working graphics drivers.

The v0.2.0 archives predate several workflows documented here. The hosted workbench and current source contain the newer features.

## Build current source

Install [Rust](https://rustup.rs/). The repository pins Rust 1.95.0.

```bash
git clone https://github.com/AvilaLabs/Converra.git
cd Converra
cargo run --release -p optcoil-app
```

For headless use, `cargo run --release -- --help` builds the default CLI package. The internal crate and executable names retain `optcoil`.

To serve a local browser build, install Trunk 0.21.14 and the `wasm32-unknown-unknown` Rust target, then run `trunk serve` in `crates/optcoil-app`.

Keep private input files in ignored `customer-data/` and generated calculations in ignored `runs/`.
