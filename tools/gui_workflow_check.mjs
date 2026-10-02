// Deterministic GUI regression runner for Converra's egui workbench.
// Uses Chrome DevTools Protocol directly; no browser automation package.
// Run against an isolated Chromium started with --remote-debugging-port=9227
// and a local Trunk server. A plan contains real canvas coordinates captured
// from the current build; the app's read-only ?verify=1 status bridge supplies
// identity and state assertions, never action hooks.
import { mkdir, readFile, readdir, stat, writeFile } from 'node:fs/promises';
import { dirname, resolve } from 'node:path';
import process from 'node:process';

const [planPath, artifactDirArg] = process.argv.slice(2);
if (!planPath || !artifactDirArg) {
  throw new Error('Usage: node tools/gui_workflow_check.mjs <workflow-plan.json> <artifact-directory>');
}
const plan = JSON.parse(await readFile(resolve(planPath), 'utf8'));
if (process.env.CONVERRA_GUI_URL) plan.url = process.env.CONVERRA_GUI_URL;
if (process.env.CONVERRA_GUI_DOWNLOADS) plan.downloadDirectory = process.env.CONVERRA_GUI_DOWNLOADS;
const artifactDir = resolve(artifactDirArg);
await mkdir(artifactDir, { recursive: true });
const endpoint = process.env.CONVERRA_GUI_CDP ?? 'http://127.0.0.1:9227';
const tabs = await (await fetch(`${endpoint}/json/list`)).json();
const tab = tabs.find(entry => entry.type === 'page');
if (!tab) throw new Error('No page in the isolated verification browser');
const socket = new WebSocket(tab.webSocketDebuggerUrl);
await new Promise((ok, fail) => { socket.onopen = ok; socket.onerror = fail; });
let nextId = 0;
const pending = new Map();
const events = [];
const expectedReloadInterventions = new Set();
socket.onmessage = ({ data }) => {
  const message = JSON.parse(data);
  if (message.id) {
    const request = pending.get(message.id);
    pending.delete(message.id);
    if (message.error) request.reject(new Error(JSON.stringify(message.error)));
    else request.resolve(message.result);
  } else if (message.method === 'Page.javascriptDialogOpening') {
    if (message.params.type === 'beforeunload') {
      events.push(message);
      call('Page.handleJavaScriptDialog', { accept: true }).catch(error => events.push({ method: 'Harness.dialogError', error: String(error) }));
    } else {
      events.push({ method: 'Harness.unexpectedDialog', params: message.params });
    }
  } else if (['Runtime.exceptionThrown', 'Log.entryAdded', 'Browser.downloadWillBegin', 'Browser.downloadProgress', 'Network.loadingFailed', 'Page.loadEventFired', 'Page.frameNavigated'].includes(message.method)) {
    events.push(message);
  }
};
function call(method, params = {}) {
  return new Promise((ok, fail) => {
    const id = ++nextId;
    pending.set(id, { resolve: ok, reject: fail });
    socket.send(JSON.stringify({ id, method, params }));
  });
}
const pause = ms => new Promise(ok => setTimeout(ok, ms));
async function evaluate(expression) {
  const result = await call('Runtime.evaluate', { expression, returnByValue: true, awaitPromise: true });
  if (result.exceptionDetails) throw new Error(JSON.stringify(result.exceptionDetails));
  return result.result.value;
}
function pathValue(object, path) {
  return path.split('.').reduce((value, part) => value?.[part], object);
}
function assertExpectations(actual, expected, label) {
  for (const [path, value] of Object.entries(expected ?? {})) {
    const found = pathValue(actual, path);
    if (JSON.stringify(found) !== JSON.stringify(value)) {
      throw new Error(`${label}: ${path} expected ${JSON.stringify(value)}, got ${JSON.stringify(found)}`);
    }
  }
}
function assertContains(actual, expected, label) {
  for (const [path, fragment] of Object.entries(expected ?? {})) {
    const value = pathValue(actual, path);
    if (typeof value !== 'string' || !value.toLowerCase().includes(String(fragment).toLowerCase())) {
      throw new Error(`${label}: expected ${path} to contain ${JSON.stringify(fragment)}, got ${JSON.stringify(value)}`);
    }
  }
}
function assertIdentityRelations(current, previous, { changed = [], unchanged = [] } = {}, label) {
  for (const path of changed) {
    if (JSON.stringify(pathValue(current, path)) === JSON.stringify(pathValue(previous, path))) {
      throw new Error(`${label}: expected ${path} to change`);
    }
  }
  for (const path of unchanged) {
    if (JSON.stringify(pathValue(current, path)) !== JSON.stringify(pathValue(previous, path))) {
      throw new Error(`${label}: expected ${path} to stay unchanged`);
    }
  }
}
async function status() {
  const value = await evaluate('window.__converraStatus ?? null');
  if (!value) throw new Error('Missing read-only window.__converraStatus; open the app with ?verify=1');
  for (const field of ['route', 'inputSha256', 'resultCaseSha256', 'resultCurrent', 'jobState', 'dirty']) {
    if (!(field in value)) throw new Error(`Status bridge is missing ${field}`);
  }
  return value;
}
async function screenshot(name) {
  const png = await call('Page.captureScreenshot', { format: 'png', captureBeyondViewport: false });
  await writeFile(resolve(artifactDir, `${name}.png`), Buffer.from(png.data, 'base64'));
  const ax = await call('Accessibility.getFullAXTree');
  const state = await evaluate(`({url:location.href,title:document.title,width:innerWidth,height:innerHeight,canvas:[...document.querySelectorAll('canvas')].map(c=>({width:c.width,height:c.height}))})`);
  await writeFile(resolve(artifactDir, `${name}.json`), JSON.stringify({ state, status: await status(), events, accessibility: ax }, null, 2));
}
async function dispatch(action) {
  switch (action.type) {
    case 'click': {
      const x = Number(action.x), y = Number(action.y);
      await call('Input.dispatchMouseEvent', { type: 'mouseMoved', x, y });
      await call('Input.dispatchMouseEvent', { type: 'mousePressed', x, y, button: 'left', clickCount: 1 });
      await call('Input.dispatchMouseEvent', { type: 'mouseReleased', x, y, button: 'left', clickCount: 1 });
      break;
    }
    case 'key': {
      const key = action.key;
      const codes = { Enter: 13, Escape: 27, Tab: 9, Backspace: 8, Delete: 46, ArrowDown: 40, ArrowUp: 38 };
      const code = codes[key] ?? key.toUpperCase().charCodeAt(0);
      const modifiers = Number(action.modifiers ?? 0);
      await call('Input.dispatchKeyEvent', { type: 'rawKeyDown', key, modifiers, windowsVirtualKeyCode: code });
      await call('Input.dispatchKeyEvent', { type: 'keyUp', key, modifiers, windowsVirtualKeyCode: code });
      break;
    }
    case 'text':
      await call('Input.insertText', { text: String(action.text) });
      break;
    case 'upload': {
      const { root } = await call('DOM.getDocument');
      const { nodeIds } = await call('DOM.querySelectorAll', { nodeId: root.nodeId, selector: 'input[type=file]' });
      if (!nodeIds.length) throw new Error('No browser file input; open the genuine import/file dialog before upload');
      const index = action.inputIndex == null ? nodeIds.length - 1 : Number(action.inputIndex);
      let file;
      if (action.latestDownload) {
        const directory = resolve(dirname(resolve(planPath)), plan.downloadDirectory);
        const candidates = (await readdir(directory)).filter(name => !name.endsWith('.crdownload'));
        const matching = [];
        for (const name of candidates) {
          if (action.name && name !== action.name) continue;
          if (action.suffix && !name.endsWith(action.suffix)) continue;
          matching.push({ name, modified: (await stat(resolve(directory, name))).mtimeMs });
        }
        matching.sort((a, b) => b.modified - a.modified);
        if (!matching.length) throw new Error(`No completed browser download${action.name ? ` named ${action.name}` : action.suffix ? ` ending in ${action.suffix}` : ''}`);
        file = resolve(directory, matching[0].name);
      } else {
        file = resolve(dirname(resolve(planPath)), action.file);
      }
      await call('DOM.setFileInputFiles', { nodeId: nodeIds[index], files: [file] });
      break;
    }
    case 'reload': {
      const eventStart = events.length;
      await call('Page.reload', { ignoreCache: true });
      const deadline = Date.now() + Number(action.timeoutMs ?? 25000);
      while (Date.now() < deadline) {
        try {
          const navigated = events.slice(eventStart).some(event => event.method === 'Page.frameNavigated' && event.params.type === 'Navigation');
          const loaded = events.slice(eventStart).some(event => event.method === 'Page.loadEventFired');
          if (navigated && loaded && await evaluate('document.querySelector("canvas")?.width > 0 && !document.getElementById("converra-loading") && !!window.__converraStatus')) break;
        } catch { /* navigation has not committed yet */ }
        await pause(100);
      }
      const reloadEvents = events.slice(eventStart);
      const navigated = reloadEvents.some(event => event.method === 'Page.frameNavigated' && event.params.type === 'Navigation');
      const loaded = reloadEvents.some(event => event.method === 'Page.loadEventFired');
      const ready = await evaluate('document.querySelector("canvas")?.width > 0 && !document.getElementById("converra-loading") && !!window.__converraStatus').catch(() => false);
      if (!(navigated && loaded && ready)) {
        const diagnostic = await evaluate('({url:location.href,readyState:document.readyState,canvas:[...document.querySelectorAll("canvas")].map(c=>({width:c.width,height:c.height})),loading:!!document.getElementById("converra-loading"),status:window.__converraStatus??null})').catch(error => ({ evaluationError: String(error) }));
        const png = await call('Page.captureScreenshot', { format: 'png', captureBeyondViewport: false }).catch(() => null);
        if (png) await writeFile(resolve(artifactDir, 'reload-failure.png'), Buffer.from(png.data, 'base64'));
        await writeFile(resolve(artifactDir, 'reload-failure.json'), JSON.stringify({ diagnostic, reloadEvents, allEvents: events }, null, 2));
        throw new Error(`Workbench did not finish reloading: ${JSON.stringify(diagnostic)}`);
      }
      // Headless Chromium may log this intervention when a CDP-triggered
      // reload has no browser-recognized user activation. It did not display
      // a dialog; accept only this exact message after this reload navigated,
      // loaded, and restored a live canvas/status bridge.
      for (const event of reloadEvents) {
        if (event.method === 'Log.entryAdded' && event.params.entry?.level === 'error' &&
            event.params.entry.text?.startsWith("Blocked attempt to show a 'beforeunload' confirmation panel for a frame that never had a user gesture since its load.")) {
          expectedReloadInterventions.add(event);
        }
      }
      break;
    }
    case 'scroll': {
      const x = Number(action.x), y = Number(action.y);
      await call('Input.dispatchMouseEvent', { type: 'mouseMoved', x, y });
      for (let i = 0; i < Number(action.repeat ?? 1); i++) {
        await call('Input.dispatchMouseEvent', { type: 'mouseWheel', x, y, deltaX: Number(action.deltaX ?? 0), deltaY: Number(action.deltaY) });
      }
      break;
    }
    case 'drag': {
      const fromX = Number(action.fromX), fromY = Number(action.fromY);
      const toX = Number(action.toX), toY = Number(action.toY);
      const steps = Math.max(1, Number(action.steps ?? 12));
      await call('Input.dispatchMouseEvent', { type: 'mouseMoved', x: fromX, y: fromY });
      await call('Input.dispatchMouseEvent', { type: 'mousePressed', x: fromX, y: fromY, button: 'left', clickCount: 1 });
      for (let i = 1; i <= steps; i++) {
        const progress = i / steps;
        await call('Input.dispatchMouseEvent', {
          type: 'mouseMoved',
          x: fromX + (toX - fromX) * progress,
          y: fromY + (toY - fromY) * progress,
          buttons: 1,
        });
      }
      await call('Input.dispatchMouseEvent', { type: 'mouseReleased', x: toX, y: toY, button: 'left', clickCount: 1 });
      break;
    }
    case 'waitDownload': {
      const directory = resolve(dirname(resolve(planPath)), plan.downloadDirectory);
      const deadline = Date.now() + Number(action.timeoutMs ?? 10000);
      while (Date.now() < deadline) {
        try {
          const names = await readdir(directory);
          const found = names.find(name => !name.endsWith('.crdownload') && (!action.name || name === action.name) && (!action.suffix || name.endsWith(action.suffix)));
          if (found) {
            const info = await stat(resolve(directory, found));
            if (info.size > 0) break;
          }
        } catch { /* browser has not downloaded the file yet */ }
        await pause(100);
      }
      const names = await readdir(directory).catch(() => []);
      if (!names.some(name => !name.endsWith('.crdownload') && (!action.name || name === action.name) && (!action.suffix || name.endsWith(action.suffix)))) {
        throw new Error(`Timed out waiting for browser download${action.name ? ` named ${action.name}` : action.suffix ? ` ending in ${action.suffix}` : ''}`);
      }
      break;
    }
    case 'assertStudyDownload': {
      const directory = resolve(dirname(resolve(planPath)), plan.downloadDirectory);
      const path = resolve(directory, action.name);
      const workspace = JSON.parse(await readFile(path, 'utf8'));
      if (workspace.schema !== action.schema) throw new Error(`Downloaded study schema expected ${action.schema}, got ${workspace.schema}`);
      const variants = workspace.variants ?? [];
      const variantIds = variants.map(variant => variant.id);
      if (JSON.stringify(variantIds) !== JSON.stringify(action.variantIds)) {
        throw new Error(`Downloaded study variants expected ${JSON.stringify(action.variantIds)}, got ${JSON.stringify(variantIds)}`);
      }
      if (action.variantNames) {
        const variantNames = variants.map(variant => variant.name);
        if (JSON.stringify(variantNames) !== JSON.stringify(action.variantNames)) {
          throw new Error(`Downloaded study variant names expected ${JSON.stringify(action.variantNames)}, got ${JSON.stringify(variantNames)}`);
        }
      }
      const resultHashes = [];
      for (const variant of variants) {
        if (!variant.dataset_bundles?.some(bundle => bundle.includes(action.datasetId))) {
          throw new Error(`Downloaded study variant ${variant.id} is missing the imported dataset bundle ${action.datasetId}`);
        }
        if ((variant.results ?? []).length !== 1) throw new Error(`Downloaded study variant ${variant.id} should contain one completed result`);
        resultHashes.push(variant.results[0].case_sha256);
      }
      if (JSON.stringify(resultHashes) !== JSON.stringify(action.resultHashes)) {
        throw new Error(`Downloaded study result identities expected ${JSON.stringify(action.resultHashes)}, got ${JSON.stringify(resultHashes)}`);
      }
      break;
    }
    case 'wait':
      break;
    default:
      throw new Error(`Unsupported action type: ${action.type}`);
  }
}
async function waitUntil(expect, timeoutMs = 10000) {
  const deadline = Date.now() + timeoutMs;
  let last;
  while (Date.now() < deadline) {
    last = await status();
    let matched = true;
    for (const [path, value] of Object.entries(expect ?? {})) {
      if (JSON.stringify(pathValue(last, path)) !== JSON.stringify(value)) matched = false;
    }
    if (matched) return last;
    await pause(80);
  }
  throw new Error(`Timed out waiting for status ${JSON.stringify(expect)}; last=${JSON.stringify(last)}`);
}

try {
  await call('Runtime.enable');
  await call('Log.enable');
  await call('Page.enable');
  await call('Network.enable');
  await call('Network.setCacheDisabled', { cacheDisabled: true });
  await call('DOM.enable');
  await call('Accessibility.enable');
  await call('Page.setInterceptFileChooserDialog', { enabled: true });
  if (plan.downloadDirectory) {
    await call('Browser.setDownloadBehavior', { behavior: 'allow', downloadPath: resolve(dirname(resolve(planPath)), plan.downloadDirectory), eventsEnabled: true });
  }
  const viewport = plan.viewport ?? { width: 1440, height: 1000 };
  await call('Emulation.setDeviceMetricsOverride', { ...viewport, deviceScaleFactor: 1, mobile: false });
  if (plan.url) {
    const url = new URL(plan.url);
    const loopback = url.protocol === 'http:' && ['127.0.0.1', 'localhost'].includes(url.hostname) && !!url.port;
    if (!loopback) throw new Error('Workflow regression accepts only a local loopback workbench URL');
    await call('Page.navigate', { url: `${url.href}${url.search ? '&' : '?'}verify=1` });
    const deadline = Date.now() + 25000;
    while (Date.now() < deadline && !await evaluate('document.querySelector("canvas")?.width > 0 && !document.getElementById("converra-loading")')) await pause(200);
    if (!await evaluate('document.querySelector("canvas")?.width > 0 && !document.getElementById("converra-loading")')) throw new Error('Workbench did not finish loading');
  }
  await pause(250);
  let previous = await status();
  await screenshot('00-start');
  const completedPhases = new Set();
  for (const [index, step] of plan.steps.entries()) {
    if (!step.phase || !step.name || !step.action) throw new Error(`Plan step ${index} needs phase, name, and action`);
    const started = Date.now();
    if (step.action.type !== 'wait') await dispatch(step.action);
    if (step.waitFor) await waitUntil(step.waitFor, step.timeoutMs);
    if (step.waitDownload) await dispatch({ type: 'waitDownload', ...step.waitDownload });
    else if (step.settleMs !== 0) await pause(Number(step.settleMs ?? 250));
    let current = await status();
    if (step.expect) assertExpectations(current, step.expect, step.name);
    if (step.expectContains) assertContains(current, step.expectContains, step.name);
    if (step.identity) assertIdentityRelations(current, previous, step.identity, step.name);
    if (step.responsivenessMs != null) {
      // Two real animation frames must advance while the worker is active;
      // this catches a synchronous solve that blocks input/navigation.
      const before = Date.now();
      const frameStatus = await evaluate(`new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(() => resolve(window.__converraStatus?.jobState))))`);
      const elapsed = Date.now() - before;
      if (elapsed > step.responsivenessMs) throw new Error(`${step.name}: canvas stopped responding for ${elapsed} ms`);
      if (frameStatus !== step.expectJobState) throw new Error(`${step.name}: expected jobState ${step.expectJobState} during responsive frames, got ${frameStatus}`);
    }
    const name = `${String(index + 1).padStart(2, '0')}-${step.phase}-${step.name.replace(/[^a-z0-9]+/gi, '-').toLowerCase()}`;
    await screenshot(name);
    completedPhases.add(step.phase);
    console.log(JSON.stringify({ step: step.name, phase: step.phase, elapsedMs: Date.now() - started, status: current }));
    previous = current;
  }
  const required = plan.requiredPhases ?? ['import', 'run', 'cancel', 'revise', 'compare', 'save', 'reopen', 'recovery'];
  const missing = required.filter(phase => !completedPhases.has(phase));
  if (missing.length) throw new Error(`Workflow plan lacks required phases: ${missing.join(', ')}`);
  const errors = events.filter(event => !expectedReloadInterventions.has(event) && (event.method === 'Runtime.exceptionThrown' || event.method === 'Harness.unexpectedDialog' || event.method === 'Harness.dialogError' || event.method === 'Log.entryAdded' && event.params.entry?.level === 'error' && !/api\.avilalabs\.org/i.test(`${event.params.entry?.url ?? ''} ${event.params.entry?.text ?? ''}`) && !(/favicon\.ico/i.test(event.params.entry?.url ?? '') && /404/.test(event.params.entry?.text ?? ''))));
  if (errors.length) throw new Error(`Browser reported ${errors.length} runtime or console error(s); see trace JSON`);
  await writeFile(resolve(artifactDir, 'summary.json'), JSON.stringify({ passed: true, completedPhases: [...completedPhases], steps: plan.steps.length, finalStatus: previous, events }, null, 2));
  console.log(JSON.stringify({ passed: true, steps: plan.steps.length, completedPhases: [...completedPhases], artifacts: artifactDir }));
} finally {
  socket.close();
}
