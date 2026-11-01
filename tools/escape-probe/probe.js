// Dev only (audit H2): what a plugin page can reach of the app's IPC. Load
// this folder in developer mode (its window may stay hidden); the results go
// to plugin-data/escape-probe/probe.json, and plugin-data/escape-probe/rawipc.json
// appears only if a call reached the backend. Results: docs/plugins-design.md.
const r = { at: new Date().toISOString(), origin: String(location.origin), href: location.href, ua: navigator.userAgent };
const tryIt = async (name, fn) => {
  try {
    const v = await Promise.race([fn(), new Promise((_, no) => setTimeout(() => no(new Error("timeout 4s")), 4000))]);
    r[name] = { ok: true, value: v === undefined ? "undefined" : JSON.parse(JSON.stringify(v ?? null)) };
  } catch (e) {
    r[name] = { ok: false, error: String(e && (e.message || e)) };
  }
};
(async () => {
  r.globals = {
    __TAURI__: typeof window.__TAURI__,
    __TAURI_INTERNALS__: typeof window.__TAURI_INTERNALS__,
    __TAURI_IPC__: typeof window.__TAURI_IPC__,
    ipc: typeof window.ipc,
    chromeWebview: typeof window.chrome?.webview,
    webkitHandlers: typeof window.webkit?.messageHandlers,
    invokeKey: typeof window.__TAURI_INTERNALS__?.invokeKey,
  };
  const inv = window.__TAURI_INTERNALS__?.invoke || window.__TAURI__?.core?.invoke;
  if (inv) {
    // A plugin-surface command: answers if the IPC itself is reachable.
    await tryIt("invokeAllowed", () => inv("plugin_widgets_locked"));
    // A command plugin windows are refused (allowed_from).
    await tryIt("invokeRefused", () => inv("get_app_version"));
    // An allowed command with a mark left behind: plugin-data/escape-probe/rawipc.json
    // appears if this reached the backend, reply or not.
    await tryIt("invokeStorage", () => inv("plugin_storage_set", { plugin: "escape-probe", key: "rawipc", value: JSON.stringify("reached " + new Date().toISOString()) }));
  }
  await tryIt("fetchIpc", async () => {
    const res = await fetch("http://ipc.localhost/plugin_widgets_locked", { method: "POST", body: "{}" });
    return { status: res.status, body: (await res.text()).slice(0, 200) };
  });
  await tryIt("fetchIpcScheme", async () => {
    const res = await fetch("ipc://localhost/plugin_widgets_locked", { method: "POST", body: "{}" });
    return { status: res.status };
  });
  await tryIt("rawPostMessage", async () => {
    if (!window.chrome?.webview?.postMessage) return "no chrome.webview";
    window.chrome.webview.postMessage(JSON.stringify({ cmd: "get_app_version", callback: 1, error: 2, payload: {} }));
    return "posted (no reply expected)";
  });
  await tryIt("rawWebkit", async () => {
    const h = window.webkit?.messageHandlers?.ipc;
    if (!h) return "no webkit.messageHandlers.ipc";
    // Tauri's message, without the invoke key the frame doesn't have.
    h.postMessage(JSON.stringify({ cmd: "plugin_storage_set", callback: 1, error: 2,
      payload: { plugin: "escape-probe", key: "rawipc", value: JSON.stringify("reached by webkit post") }, options: {} }));
    return "posted to webkit ipc";
  });
  await tryIt("parentDocument", async () => typeof window.parent.document);
  await tryIt("topTauri", async () => typeof window.top.__TAURI__);
  document.getElementById("out").textContent = JSON.stringify(r, null, 2);
  await a2.storage.set("probe", r).catch((e) => { document.getElementById("out").textContent += "\nstorage: " + e; });
})();
