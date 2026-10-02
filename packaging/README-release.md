# Converra 0.3.0

Design and cost optimization for high-temperature superconducting magnets.
All calculations use the same local Rust engine. No Rust installation is needed
to use these binaries.

## Desktop

Launch `Converra.exe` on Windows, `Converra` on Linux, or `Converra.app` on macOS.
The macOS CLI/MCP archive is a separate download. Linux desktop use requires a
graphical session and graphics drivers.

The workbench starts with a small measured-conductor study and illustrative
prices. Review **Applicability and work estimate**, run, then use **Engineering
study** to name alternatives, inspect diagnoses, compare decisions and save a
workspace. See [the workspace guide](docs/ENGINEERING_WORKSPACE.md).

Signing is optional in the release workflow. Unsigned builds can trigger macOS
Gatekeeper or Windows SmartScreen; the macOS headless archive is unsigned.

## CLI

From the extracted archive directory, run:

```bash
./optcoil --version
./optcoil preflight benchmarks/coupled/first-study.json
./optcoil coupled-search benchmarks/coupled/first-study.json --output study.json
./optcoil verify study.json benchmarks/coupled/first-study.json
./optcoil report study.json --output study.html
```

On Windows use `./optcoil.exe`. Searches can take several minutes. Output files
are protected against replacement; choose fresh names for repeated runs.
The example's prices are invented teaching inputs, not supplier quotes.

## AI agents

Configure your MCP client to launch the extracted `optcoil-mcp` binary with a
dedicated absolute study-directory path:

```json
{
  "mcpServers": {
    "converra": {
      "command": "/absolute/path/to/optcoil-mcp",
      "args": ["--workspace-dir", "/absolute/path/to/my-converra-study"]
    }
  }
}
```

On Windows use `optcoil-mcp.exe`. Run one server per study directory.
See [the MCP guide](crates/optcoil-mcp/README.md) for tools, resources, jobs,
cancellation and portable evidence. The server communicates over stdio.

## Results, attribution and downloads

`PASS`, `FAIL`, `INCONCLUSIVE` and `NOT_EVALUATED` remain distinct. A screening
pass does not establish engineering acceptance. Offline verification checks
artifact integrity and arithmetic; it does not validate the underlying physics.

Converra uses the [MIT license](LICENSE). Embedded conductor data retains its own
licenses and attribution in `data/materials/`; see also
[supported domains](docs/SUPPORTED_DOMAINS.md) and [the changelog](CHANGELOG.md).

Download `SHA256SUMS` beside your archive from
[the release](https://github.com/AvilaLabs/Converra/releases/tag/v0.3.0).
Compare its digest with `sha256sum <archive>` on Linux,
`shasum -a 256 <archive>` on macOS, or `Get-FileHash <archive> -Algorithm SHA256`
in Windows PowerShell.

[Browser workbench](https://converra.avilalabs.org) ·
[Source and full documentation](https://github.com/AvilaLabs/Converra/tree/v0.3.0) ·
[Report a problem](https://github.com/AvilaLabs/Converra/issues)
