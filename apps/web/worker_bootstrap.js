console.log("[Bootstrap] Starting...");
try {
    importScripts('./worker.js');
    console.log("[Bootstrap] worker.js imported. Global wasm_bindgen type:", typeof wasm_bindgen);
} catch (e) {
    console.error("[Bootstrap] importScripts failed:", e);
}

// wasm_bindgen is the global init function exposed by the no-modules target
wasm_bindgen('./worker_bg.wasm').then(() => {
    console.log("[Bootstrap] WASM Initialized (Classic)");
    // Explicitly call the worker entry point if it's not auto-started
    // In our Rust code, main() calls console log? No, worker_entry.js spawns local.
    // The WASM start function (main) is usually called by init.
}).catch(e => {
    console.error("[Bootstrap] WASM Init Failed:", e);
});
