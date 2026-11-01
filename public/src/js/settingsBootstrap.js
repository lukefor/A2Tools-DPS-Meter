// Runs in <head>, before the first layout. The one place that asks for Linux;
// the other scripts read the class.
(() => {
  // styles.css drops Windows/macOS-only fonts from Linux stacks.
  if (/Linux/.test(globalThis.navigator?.userAgent || "")) document.documentElement.classList.add("linux");
  const view = window.__A2_VIEW__ || new URLSearchParams(window.location.search).get("view");
  if (view !== "settings") return;
  // This window shares the source markup, but never uses the combat panels or
  // meter promos. Drop them before localization and SVG icon creation so the
  // form has no unused DOM, icons or layout work.
  document.addEventListener("DOMContentLoaded", () => {
    const meter = document.querySelector(".meter");
    if (meter) for (const child of [...meter.children]) {
      if (!child.classList.contains("settingsPanel")) child.remove();
    }
    document.querySelectorAll(".updateModal, .discordPromo").forEach(element => element.remove());
  }, { once: true });
})();
