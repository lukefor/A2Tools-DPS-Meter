import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";

const source = readFileSync(new URL("../public/src/js/core.js", import.meta.url), "utf8");

function card() {
  const classes = new Set();
  return {
    classList: {
      add: (c) => classes.add(c),
      remove: (c) => classes.delete(c),
      contains: (c) => classes.has(c),
      toggle() {},
    },
  };
}

// 2.0.65's one-time party popup opened at start on a meter locked from last
// time: the window let every click through, and the card covered the lock
// button, so nothing could be clicked. The backend is told while a card is
// open, and takes the mouse then.
test("the backend hears when a card opens and closes, so a locked window takes the mouse", () => {
  const promo = card();
  const update = card();
  const told = [];
  const observers = [];
  const context = vm.createContext({
    console: { log() {} },
    MutationObserver: class {
      constructor(callback) { this.callback = callback; observers.push(this); }
      observe() {}
    },
    window: { addEventListener() {}, javaBridge: { setOverlayCardOpen: (open) => told.push(open) } },
    document: {
      readyState: "loading", addEventListener() {},
      querySelectorAll: () => [update, promo],
      documentElement: { classList: { toggle() {} } }, body: { classList: { toggle() {} } },
    },
    requestAnimationFrame: () => 1, cancelAnimationFrame() {},
  });
  vm.runInContext(source, context);
  const app = vm.runInContext("new DpsApp()", context);
  app._watchOverlayCards();
  const changed = () => observers.forEach((o) => o.callback());

  assert.deepEqual(told, [], "nothing open at start: nothing to say");
  promo.classList.add("isOpen");
  changed();
  assert.deepEqual(told, [true]);
  update.classList.add("isOpen");
  changed();
  promo.classList.remove("isOpen");
  changed();
  assert.deepEqual(told, [true], "still a card open");
  update.classList.remove("isOpen");
  changed();
  assert.deepEqual(told, [true, false]);
});
