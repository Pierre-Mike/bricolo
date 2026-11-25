// The two Tauri APIs the pages use, from the global that
// `withGlobalTauri` injects. No bundler, no npm.

const tauri = window.__TAURI__;

export const invoke = (cmd, args) => tauri.core.invoke(cmd, args);
export const listen = (event, fn) => tauri.event.listen(event, (e) => fn(e.payload));
export const windowLabel = () => tauri.window.getCurrentWindow().label;

// Sends one request to the node through bricolo-ui; resolves to its answer.
export const request = (req) => invoke("request", { request: req });
