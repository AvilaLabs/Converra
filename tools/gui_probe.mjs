// Local GUI verification through Chrome DevTools Protocol. No package dependencies.
// Start an isolated headless Chromium with --remote-debugging-port=9227, then
// serve the Trunk build on localhost. Outputs are evidence, not customer validation.
import { mkdir, writeFile } from 'node:fs/promises';
import { dirname, resolve } from 'node:path';

const [command = 'snapshot', ...args] = process.argv.slice(2);
const endpoint = 'http://127.0.0.1:9227';
const tabs = await (await fetch(`${endpoint}/json/list`)).json();
const tab = tabs.find(t => t.type === 'page');
if (!tab) throw new Error('No page in the isolated verification browser');
const socket = new WebSocket(tab.webSocketDebuggerUrl);
await new Promise((ok, fail) => { socket.onopen = ok; socket.onerror = fail; });
let nextId = 0;
const pending = new Map();
const events = [];
socket.onmessage = ({ data }) => {
  const message = JSON.parse(data);
  if (message.id) {
    const request = pending.get(message.id);
    pending.delete(message.id);
    if (message.error) request.reject(new Error(JSON.stringify(message.error)));
    else request.resolve(message.result);
  } else if (['Runtime.exceptionThrown', 'Log.entryAdded', 'Browser.downloadWillBegin', 'Browser.downloadProgress'].includes(message.method)) {
    events.push(message);
  }
};
function call(method, params = {}) {
  return new Promise((resolve, reject) => {
    const id = ++nextId;
    pending.set(id, { resolve, reject });
    socket.send(JSON.stringify({ id, method, params }));
  });
}
const pause = ms => new Promise(ok => setTimeout(ok, ms));
async function evaluate(expression) {
  const r = await call('Runtime.evaluate', { expression, returnByValue: true, awaitPromise: true });
  if (r.exceptionDetails) throw new Error(JSON.stringify(r.exceptionDetails));
  return r.result.value;
}
async function snapshot(prefix) {
  prefix = resolve(prefix);
  await mkdir(dirname(prefix), { recursive: true });
  const png = await call('Page.captureScreenshot', { format: 'png', captureBeyondViewport: false });
  await writeFile(`${prefix}.png`, Buffer.from(png.data, 'base64'));
  const tree = await call('Accessibility.getFullAXTree');
  const dom = await evaluate('document.documentElement.outerHTML');
  const state = await evaluate('({url:location.href,title:document.title,width:innerWidth,height:innerHeight,canvas:[...document.querySelectorAll("canvas")].map(c=>({width:c.width,height:c.height})),fileInputs:document.querySelectorAll("input[type=file]").length})');
  await writeFile(`${prefix}.json`, JSON.stringify({ state, events, accessibility: tree }, null, 2));
  await writeFile(`${prefix}.html`, dom);
  console.log(JSON.stringify({ screenshot: `${prefix}.png`, ...state,
    runtimeExceptions: events.filter(e => e.method === 'Runtime.exceptionThrown').length,
    logErrors: events.filter(e => e.params.entry?.level === 'error').length }));
}
try {
  await call('Runtime.enable');
  await call('Log.enable');
  await call('Page.enable');
  await call('DOM.enable');
  await call('Page.setInterceptFileChooserDialog', { enabled: true });
  if (process.env.CONVERRA_GUI_DOWNLOADS) {
    await call('Browser.setDownloadBehavior', {
      behavior: 'allow', downloadPath: resolve(process.env.CONVERRA_GUI_DOWNLOADS), eventsEnabled: true,
    });
  }
  // CDP viewport overrides belong to this connection and are reset when it
  // closes. Apply the requested size on every invocation before interacting.
  await call('Emulation.setDeviceMetricsOverride', {
    width: Number(process.env.CONVERRA_GUI_WIDTH ?? 1440),
    height: Number(process.env.CONVERRA_GUI_HEIGHT ?? 1000),
    deviceScaleFactor: 1,
    mobile: false,
  });
  await pause(300);
  if (command === 'navigate') {
    const [url, prefix] = args;
    if (!/^http:\/\/127\.0\.0\.1:\d+\//.test(url)) throw new Error('Verification navigates only to a local build');
    await call('Page.navigate', { url });
    const deadline = Date.now() + 25000;
    while (Date.now() < deadline) {
      if (await evaluate('document.querySelector("canvas")?.width > 0 && !document.getElementById("converra-loading")')) break;
      await pause(200);
    }
    if (!await evaluate('document.querySelector("canvas")?.width > 0 && !document.getElementById("converra-loading")')) {
      throw new Error('The local GUI did not finish loading');
    }
    await pause(300);
    await snapshot(prefix);
  } else if (command === 'click') {
    const [x, y, prefix] = args;
    await call('Input.dispatchMouseEvent', { type: 'mouseMoved', x: Number(x), y: Number(y) });
    await call('Input.dispatchMouseEvent', { type: 'mousePressed', x: Number(x), y: Number(y), button: 'left', clickCount: 1 });
    await call('Input.dispatchMouseEvent', { type: 'mouseReleased', x: Number(x), y: Number(y), button: 'left', clickCount: 1 });
    await pause(250);
    await snapshot(prefix);
  } else if (command === 'key') {
    const [key, modifiers, prefix] = args;
    const codes = { Enter: 13, Escape: 27, Tab: 9, Backspace: 8, Delete: 46, ArrowDown: 40, ArrowUp: 38 };
    const code = codes[key] ?? key.toUpperCase().charCodeAt(0);
    await call('Input.dispatchKeyEvent', { type: 'rawKeyDown', key, modifiers: Number(modifiers), windowsVirtualKeyCode: code });
    await call('Input.dispatchKeyEvent', { type: 'keyUp', key, modifiers: Number(modifiers), windowsVirtualKeyCode: code });
    await pause(250);
    await snapshot(prefix);
  } else if (command === 'text') {
    const [text, prefix] = args;
    await call('Input.insertText', { text });
    await pause(250);
    await snapshot(prefix);
  } else if (command === 'upload' || command === 'pick') {
    const [path, prefix] = command === 'pick' ? args.slice(2) : args;
    if (command === 'pick') {
      const [x, y] = args;
      await call('Input.dispatchMouseEvent', { type: 'mouseMoved', x: Number(x), y: Number(y) });
      await call('Input.dispatchMouseEvent', { type: 'mousePressed', x: Number(x), y: Number(y), button: 'left', clickCount: 1 });
      await call('Input.dispatchMouseEvent', { type: 'mouseReleased', x: Number(x), y: Number(y), button: 'left', clickCount: 1 });
      await pause(400);
    }
    const { root } = await call('DOM.getDocument');
    const { nodeIds } = await call('DOM.querySelectorAll', { nodeId: root.nodeId, selector: 'input[type=file]' });
    if (!nodeIds.length) throw new Error('Open the app file picker before uploading');
    await call('DOM.setFileInputFiles', { nodeId: nodeIds.at(-1), files: [resolve(path)] });
    await pause(400);
    await snapshot(prefix);
  } else if (command === 'resize') {
    const [width, height, prefix] = args;
    await call('Emulation.setDeviceMetricsOverride', { width: Number(width), height: Number(height), deviceScaleFactor: 1, mobile: false });
    await pause(300);
    await snapshot(prefix);
  } else if (command === 'downloads') {
    await call('Browser.setDownloadBehavior', { behavior: 'allow', downloadPath: resolve(args[0]), eventsEnabled: true });
    console.log(JSON.stringify({ downloadDirectory: resolve(args[0]) }));
  } else if (command === 'scroll') {
    const [x, y, deltaY, prefix] = args;
    await call('Input.dispatchMouseEvent', { type: 'mouseMoved', x: Number(x), y: Number(y) });
    await pause(40);
    await call('Input.dispatchMouseEvent', { type: 'mouseWheel', x: Number(x), y: Number(y), deltaX: 0, deltaY: Number(deltaY) });
    await pause(300);
    await snapshot(prefix);
  } else if (command === 'snapshot') {
    await snapshot(args[0]);
  } else throw new Error(`Unknown command ${command}`);
} finally {
  socket.close();
}
