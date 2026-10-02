# Packaging & releases

`Converra` (the egui workbench), `optcoil` (the headless CLI) and `optcoil-mcp`
(the local stdio MCP server) are built
and packaged by `.github/workflows/release.yml` on every `v*` tag:

```bash
git tag v0.3.0 && git push origin v0.3.0
```

Artifacts attached to the GitHub Release:

| Artifact | Contents |
|---|---|
| `converra-macos-arm64.zip` | `Converra.app` (Apple Silicon); separate `-cli.tar.gz` contains `optcoil` and `optcoil-mcp` |
| `converra-macos-x64.zip` | `Converra.app` (Intel); separate `-cli.tar.gz` contains `optcoil` and `optcoil-mcp` |
| `converra-windows-x64.zip` | `Converra.exe`, `optcoil.exe`, `optcoil-mcp.exe` |
| `converra-linux-x64.tar.gz` | `Converra`, `optcoil`, `optcoil-mcp` |

MCP is included from v0.2.0. Archives also include setup guides, example inputs,
the MIT license and dataset attribution. Each release provides `SHA256SUMS`.

The workflow verifies that the tag matches the workspace version and that
`ci.yml` passed on the exact source commit before building. It creates a draft
release with all six archives and checksums. Inspect the draft archives and their
headless executables before publishing the release. Tag the already tested commit;
do not move a published tag to repair a release.

Build and deploy the browser workbench separately:

```bash
deploy/web/sync.sh
cd deploy/web && npx wrangler deploy
```

Staging fails if Trunk fails. `release.json` records the workspace version, source
commit, dirty-tree state and SHA-256 of each staged asset. Deploy a clean, tested
commit, then compare the live manifest and asset hashes with the staged files.

The `.app` bundle is assembled by `packaging/make-app.sh` (Info.plist +
`icon.icns` generated from the bundled logo via `sips`/`iconutil`). The
Windows `.exe` icon is embedded at compile time by `embed-resource` via
`packaging/app.rc` → `packaging/icon.ico`.

## Signing

With no secrets configured, the workflow produces **unsigned** artifacts —
they run, but Windows SmartScreen warns and macOS Gatekeeper refuses
double-clicked `.app`s (right-click → Open works; not great for customers).
The secrets below enable Windows executable signing and macOS app signing and
notarization. On macOS these steps cover `Converra.app`; the separate headless
archive containing `optcoil` and `optcoil-mcp` remains unsigned and unnotarized.

### macOS (Apple Developer Program, $99/yr)

| Secret | Value |
|---|---|
| `MACOS_CERTIFICATE` | base64 of your **Developer ID Application** `.p12` |
| `MACOS_CERTIFICATE_PASSWORD` | the `.p12` export password |
| `MACOS_CODESIGN_IDENTITY` | e.g. `Developer ID Application: Avila Labs (ABCDE12345)` |
| `APPLE_ID` | Apple ID email for notarization |
| `APPLE_APP_PASSWORD` | app-specific password (`appleid.apple.com`) |
| `APPLE_TEAM_ID` | 10-char team ID |

Export the cert from Keychain Access → login keychain → "Developer ID
Application" → export as `.p12`, then `base64 -i cert.p12 | pbcopy`.

### Windows

| Secret | Value |
|---|---|
| `WINDOWS_CERTIFICATE` | base64 of a code-signing `.pfx` |
| `WINDOWS_CERTIFICATE_PASSWORD` | the `.pfx` password |

A standard OV certificate (DigiCert/Sectigo/etc.) works with the built-in
`signtool` step. Azure Trusted Signing is the cheaper modern alternative —
wire it by replacing the "Sign the binaries" step with the
`azure/trusted-signing-action` action instead.

## Local smoke builds

```bash
# macOS (on a Mac):
cargo build --release -p optcoil-app
packaging/make-app.sh target/release/optcoil-app 0.3.0
open Converra.app

# Windows: the .exe is portable — no packaging needed beyond zipping it.
cargo build --release -p optcoil-app   # produces target/release/optcoil-app.exe
```
