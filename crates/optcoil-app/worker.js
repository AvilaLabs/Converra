// Converra search worker — runs exact-input study searches off the page thread.
// The page posts {kind:"study-search", workspace_json, variant_id}; the
// worker attaches the verified record and retains its private exact-input
// session while idle. The page terminates it on cancellation/error/workspace
// replacement, which also clears the live cache.
//
// The wasm-bindgen assets ship unhashed ([build] filehash = false in
// Trunk.toml) so this module can name them.
import init, { converra_search_worker_main } from './optcoil-app.js';

// Messages can arrive while the wasm module is still being fetched. Install
// a handler before the first await so the initial search is not discarded.
const pendingMessages = [];
self.onmessage = event => pendingMessages.push(event);
await init({ module_or_path: './optcoil-app_bg.wasm' });
converra_search_worker_main();
for (const event of pendingMessages) self.onmessage(event);
