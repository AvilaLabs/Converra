# Welcome to Converra

Converra helps you design high-temperature superconducting magnets and compare winding choices against a fixed specification. Search geometry and REBCO conductor options, inspect operating limits, and compare the modeled cost of eligible designs.

This handbook is the starting point for using Converra. It follows a study from requirements and conductor data to a calculated decision and a review package.

**Converra 0.2.0** is the current workspace version. The handbook describes the current source and hosted workbench. Spreadsheet imports, draft recovery, diagnosis and scenario studies were added after the v0.2.0 desktop archives; build current source to use them on desktop. Python bindings currently require a source build.

## Choose where to begin

| Your task | Start here |
| --- | --- |
| Try the workbench without installing | [Open Converra](https://converra.avilalabs.org), then follow [your first study](quick-start.md) |
| Run a repeatable headless search | [Command-line workflow](cli.md) |
| Bring measured conductor data | [Conductor data and imports](materials.md) |
| Compare design alternatives | [Variants and scenarios](studies.md) |
| Understand a verdict or savings figure | [Read your results](results.md) and [benchmarks](benchmarks.md) |
| Integrate with code or an agent | [Python](python.md) or [local MCP](mcp.md) |

The desktop, browser, CLI, Python and MCP interfaces use the same Rust engine. Browser calculations run in a background worker; desktop and CLI also support local directories and batch workflows.

## What a calculated decision means

A passing screen means a declared model and sampling plan met the requirements it evaluated. Review material coverage, finer acceptance checks and unresolved engineering work before selecting a design. Converra retains `PASS`, `FAIL`, `INCONCLUSIVE` and `NOT_EVALUATED` separately so missing evidence stays visible.

Converra is developed by [Avila Labs](https://github.com/AvilaLabs) and released under the [MIT license](https://github.com/AvilaLabs/Converra/blob/main/LICENSE). Source, releases and issues are on [GitHub](https://github.com/AvilaLabs/Converra).
