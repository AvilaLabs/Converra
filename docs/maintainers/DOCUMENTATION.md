# Maintain the Converra Handbook

The current user documentation lives in `docs/guide/`. `book.toml` builds it with **mdBook 0.5.4**, the Rust theme and optional Navy dark theme. Its public URL is <https://converra.avilalabs.org/docs/>.

Keep `SUMMARY.md` task-oriented and keep dated OC records, investigations and release procedures outside the public book source. Link relevant evidence with its configuration and verdict intact. mdBook copies unlisted source assets, so the chapter/link checker also rejects unlisted HTML output.

## Build and check

```bash
mdbook build
python3 scripts/check_handbook.py
npm ci --prefix docs/checks --ignore-scripts
docs/checks/node_modules/.bin/playwright install chromium
python3 -m http.server 8080 --directory dist --bind 127.0.0.1
# In a second terminal:
node scripts/docs_smoke.mjs
```

The browser check validates chapter navigation, search, Rust/Navy themes and a 390 px mobile layout. `HANDBOOK_URL` can select the live site or a combined bundle. Run local builds and executable checks under the resource limits required for the workstation, with temporary files on disk.

The standalone **Build handbook** workflow supplies quick documentation checks and a `converra-handbook` artifact. The main **ci** browser-workflow job builds the real app, executes the full existing GUI workflow, and runs `deploy/web/sync.sh dist/workbench`. That script builds/checks the book, replaces the entire public destination and includes `/docs/` in the asset manifest. The resulting **converra-web** artifact contains the app and handbook together.

## Publish a checked revision

Push only the reviewed changes to `main` after all gates in `AGENTS.md` pass. Wait for CI and the handbook workflow on the exact pushed SHA to finish successfully.

Download the `converra-web` artifact from that SHA's successful **ci** run into a new directory, then check and deploy it:

```bash
gh run download CI_RUN_ID --repo AvilaLabs/Converra \
  --name converra-web --dir dist/verified-web
python3 scripts/check_handbook.py dist/verified-web/docs
npx wrangler@4 deploy --config deploy/web/wrangler.jsonc \
  --assets dist/verified-web --dry-run
npx wrangler@4 deploy --config deploy/web/wrangler.jsonc \
  --assets dist/verified-web
```

Use the complete web artifact. Publishing a handbook-only directory would replace the workbench. The existing Worker is `converra-workbench`; its custom domain does not need a separate DNS record for `/docs/`. Confirm the Cloudflare account with `wrangler whoami` when setting up access.

After deployment, open the workbench, its Help → Handbook link, the handbook, benchmark chapter and mobile/search interactions. Deployment remains explicit; CI builds a verified artifact without deploying to Cloudflare.

## Keep content current

Check commands against `optcoil-cli`, Python examples against `optcoil-py`, and MCP tools against `optcoil-mcp`. Check geometry and material limits against model/physics declarations rather than old OC prose. Distinguish source and hosted additions from the v0.2.0 archives (later additions ship from v0.3.0), and keep the overview version synchronized with the workspace.

Document benchmark metric, baseline, inputs and verdict together. Shared physics acceptance is not independent physical validation; cache reuse is not a fresh-solve speed comparison. Never turn a failed or inconclusive historical result into a current recommendation.
