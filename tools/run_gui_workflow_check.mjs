// Build the actual WASM workbench, serve it on loopback, start an isolated
// headless Chromium profile, and execute the checked-in CDP action plan.
// Requires Node 22+, Trunk, wasm32-unknown-unknown, and Chromium/Chrome.
import { mkdtemp, mkdir, readFile, rm, writeFile } from 'node:fs/promises';
import { createHash } from 'node:crypto';
import { tmpdir } from 'node:os';
import { extname, resolve, sep } from 'node:path';
import { spawn, spawnSync } from 'node:child_process';
import { createServer } from 'node:http';

const repo = resolve(import.meta.dirname, '..');
const planPath = resolve(repo, process.argv[2] ?? 'tools/gui_workflow_plan.json');
const artifactDir = resolve(process.env.CONVERRA_GUI_ARTIFACTS ?? `${repo}/runs/gui-workflow`);
const browser = process.env.CHROME_BIN ?? ['chromium', 'chromium-browser', 'google-chrome', 'google-chrome-stable']
  .find(candidate => spawnSync('sh', ['-lc', `command -v ${candidate}`], { encoding: 'utf8' }).status === 0);
if (!browser) throw new Error('Chrome/Chromium not found; set CHROME_BIN to its executable path');
if (!process.env.CONVERRA_GUI_PREBUILT_DIST && spawnSync('trunk', ['--version'], { encoding: 'utf8' }).status !== 0) {
  throw new Error('Trunk is required to build the browser workbench');
}

const tempDir = await mkdtemp(resolve(tmpdir(), 'converra-gui-'));
const distDir = process.env.CONVERRA_GUI_PREBUILT_DIST
  ? resolve(process.env.CONVERRA_GUI_PREBUILT_DIST)
  : resolve(tempDir, 'dist');
const profileDir = resolve(tempDir, 'chrome-profile');
const downloadDir = resolve(artifactDir, 'downloads');
await mkdir(artifactDir, { recursive: true });
await mkdir(downloadDir, { recursive: true });
const children = [];
let server;
function launch(command, args, options = {}) {
  const child = spawn(command, args, { stdio: ['ignore', 'pipe', 'pipe'], ...options });
  children.push(child);
  child.stdout.on('data', chunk => process.stdout.write(chunk));
  child.stderr.on('data', chunk => process.stderr.write(chunk));
  return child;
}
async function waitFor(predicate, timeoutMs, description) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    if (await predicate()) return;
    await new Promise(resolvePause => setTimeout(resolvePause, 100));
  }
  throw new Error(`Timed out waiting for ${description}`);
}

try {
  if (!process.env.CONVERRA_GUI_PREBUILT_DIST) {
    const buildEnv = { ...process.env, NO_COLOR: 'true' };
    const trunk = launch('trunk', ['build', '--release', '--dist', distDir], { cwd: resolve(repo, 'crates/optcoil-app'), env: buildEnv });
    const exitCode = await new Promise((ok, fail) => {
      trunk.once('error', fail);
      trunk.once('exit', code => ok(code));
    });
    if (exitCode !== 0) throw new Error(`Trunk build failed with exit code ${exitCode}`);
  } else {
    await readFile(resolve(distDir, 'index.html'));
  }

  server = createServer(async (request, response) => {
    try {
      const urlPath = decodeURIComponent(new URL(request.url, 'http://localhost').pathname);
      const relative = urlPath === '/' ? 'index.html' : urlPath.slice(1);
      const path = resolve(distDir, relative);
      if (path !== distDir && !path.startsWith(`${distDir}${sep}`)) {
        response.writeHead(403).end('Forbidden');
        return;
      }
      const body = await readFile(path);
      const types = {
        '.html': 'text/html; charset=utf-8', '.js': 'text/javascript; charset=utf-8',
        '.wasm': 'application/wasm', '.css': 'text/css; charset=utf-8', '.png': 'image/png',
        '.json': 'application/json; charset=utf-8', '.svg': 'image/svg+xml',
      };
      response.writeHead(200, { 'Content-Type': types[extname(path)] ?? 'application/octet-stream', 'Cache-Control': 'no-store' });
      response.end(body);
    } catch {
      response.writeHead(404).end('Not found');
    }
  });
  await new Promise((ok, fail) => server.listen(0, '127.0.0.1', ok).once('error', fail));
  const { port } = server.address();
  const url = `http://127.0.0.1:${port}/?verify=1`;
  const browserProcess = launch(browser, [
    '--headless=new', '--no-sandbox', '--use-angle=swiftshader-webgl', '--enable-unsafe-swiftshader', '--disable-dev-shm-usage',
    '--no-first-run', '--no-default-browser-check', '--disable-background-networking',
    '--remote-debugging-port=0', `--user-data-dir=${profileDir}`, 'about:blank',
  ]);
  const activePortFile = resolve(profileDir, 'DevToolsActivePort');
  let cdpPort;
  await waitFor(async () => {
    try {
      cdpPort = Number((await readFile(activePortFile, 'utf8')).split(/\s+/)[0]);
      return Number.isInteger(cdpPort) && cdpPort > 0;
    } catch { return false; }
  }, 15000, 'isolated Chromium DevTools endpoint');
  await waitFor(async () => {
    try { return (await fetch(`http://127.0.0.1:${port}/`)).ok; } catch { return false; }
  }, 5000, 'local workbench server');

  const env = {
    ...process.env,
    CONVERRA_GUI_CDP: `http://127.0.0.1:${cdpPort}`,
    CONVERRA_GUI_URL: url,
    CONVERRA_GUI_DOWNLOADS: downloadDir,
  };
  const harness = launch(process.execPath, [resolve(repo, 'tools/gui_workflow_check.mjs'), planPath, artifactDir], { cwd: repo, env });
  const harnessCode = await new Promise((ok, fail) => {
    harness.once('error', fail);
    harness.once('exit', code => ok(code));
  });
  if (harnessCode !== 0) throw new Error(`CDP GUI workflow failed with exit code ${harnessCode}`);
  const bundleFiles = ['index.html', 'optcoil-app.js', 'optcoil-app_bg.wasm', 'worker.js'];
  const bundleSha256 = {};
  for (const name of bundleFiles) {
    bundleSha256[name] = createHash('sha256').update(await readFile(resolve(distDir, name))).digest('hex');
  }
  const sourceRevision = spawnSync('git', ['rev-parse', 'HEAD'], { cwd: repo, encoding: 'utf8' }).stdout.trim();
  const workingTreeDirty = spawnSync('git', ['status', '--short'], { cwd: repo, encoding: 'utf8' }).stdout.trim().length > 0;
  await writeFile(resolve(artifactDir, 'build-info.json'), JSON.stringify({ browser, url, sourceRevision, workingTreeDirty, bundleSha256, completedAt: new Date().toISOString() }, null, 2));
} finally {
  const running = children.filter(child => child.exitCode == null);
  const exits = running.map(child => new Promise(resolveExit => child.once('exit', resolveExit)));
  for (const child of running) child.kill('SIGTERM');
  if (exits.length) await Promise.race([Promise.all(exits), new Promise(resolvePause => setTimeout(resolvePause, 2000))]);
  for (const child of running) if (child.exitCode == null) child.kill('SIGKILL');
  if (running.some(child => child.exitCode == null)) await new Promise(resolvePause => setTimeout(resolvePause, 250));
  if (server) await new Promise(ok => server.close(() => ok()));
  await rm(tempDir, { recursive: true, force: true });
}
