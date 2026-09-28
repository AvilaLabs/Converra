// Converra search worker — runs a coupled search off the page thread.
// The page posts {case_json, dataset_json|null}; the worker replies
// {status:"ok",record} or {status:"err",error}. Cancellation is
// terminate() — a blocked wasm thread cannot poll a flag.
//
// The wasm-bindgen assets ship unhashed ([build] filehash = false in
// Trunk.toml) so this module can name them.
import init, { converra_search_worker_main } from './optcoil-app.js';

await init('./optcoil-app_bg.wasm');
converra_search_worker_main();
