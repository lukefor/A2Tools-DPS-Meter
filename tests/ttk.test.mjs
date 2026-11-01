import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";

function estimator(options) {
  const context = vm.createContext({});
  vm.runInContext(readFileSync(new URL("../public/src/js/ttk.js", import.meta.url), "utf8"), context);
  return vm.runInContext("createTtkEstimator", context)(options);
}

// A boss of `max` HP taking `perSecond` damage, sampled once a second from
// `from` to `to` seconds; returns the last estimate.
function run(ttk, { id = 1, max, perSecond, from = 1, to, hpAt }) {
  let out = null;
  for (let s = from; s <= to; s += 1) {
    const hp = hpAt ? hpAt(s) : max - perSecond * s;
    out = ttk.update(id, s * 1000, Math.max(0, hp), max);
  }
  return out;
}

test("a steady fight predicts its own length", () => {
  const ttk = estimator();
  // 1,000,000 HP at 5,000/s: 200 s.
  const total = run(ttk, { max: 1_000_000, perSecond: 5000, to: 60 });
  assert.ok(Math.abs(total - 200_000) < 1000, `${total}`);
});

test("nothing for the first seconds, for a dead boss, or without HP", () => {
  const ttk = estimator();
  assert.equal(ttk.update(1, 2000, 990_000, 1_000_000), null, "too early");
  assert.equal(ttk.update(1, 10_000, 0, 1_000_000), null, "dead");
  assert.equal(estimator().update(1, 10_000, 500, 0), null, "no max HP");
});

test("a burst does not cut the estimate short", () => {
  const max = 3_000_000;
  // 10,000/s for 60 s, then 10 s at three times that.
  const hpAt = (s) => (s <= 60 ? max - 10_000 * s : max - 600_000 - 30_000 * (s - 60));
  const ttk = estimator();
  const before = run(ttk, { max, perSecond: 10_000, to: 60 });
  const after = run(ttk, { max, hpAt, from: 61, to: 70 });
  // At the burst's rate the rest (2.1 M HP) would take 70 s; at the steady
  // rate it takes 210 s. Unsmoothed, the estimate would drop to 140 s.
  const naive = 70_000 + 2_100_000 / 30;
  assert.ok(before > 290_000 && before < 310_000, `${before}`);
  assert.ok(after > naive + 60_000, `the burst moved it to ${after}`);
  assert.ok(after < before, "but it does move");
});

test("a slower party lengthens it sooner than a faster one shortens it", () => {
  const max = 3_000_000;
  const steady = (s) => max - 10_000 * s;
  const ttkSlow = estimator();
  run(ttkSlow, { max, perSecond: 10_000, to: 60 });
  const slower = run(ttkSlow, { max, hpAt: (s) => steady(60) - 5_000 * (s - 60), from: 61, to: 80 });
  const ttkFast = estimator();
  run(ttkFast, { max, perSecond: 10_000, to: 60 });
  const faster = run(ttkFast, { max, hpAt: (s) => steady(60) - 20_000 * (s - 60), from: 61, to: 80 });
  // Remaining time at the steady rate.
  const base = 300_000;
  assert.ok(slower - base > base - faster, `slower +${slower - base}, faster -${base - faster}`);
});

test("another boss, or the timer starting over, is a new fight", () => {
  const ttk = estimator();
  run(ttk, { id: 1, max: 1_000_000, perSecond: 5000, to: 30 });
  // A new boss halfway down a slower fight: estimated from its own HP.
  const other = run(ttk, { id: 2, max: 1_000_000, perSecond: 1000, to: 30 });
  assert.ok(Math.abs(other - 1_000_000) < 20_000, `${other}`);
  assert.equal(ttk.update(2, 1000, 999_000, 1_000_000), null, "timer went back");
});

test("a heal measures from the healed HP instead of counting negative damage", () => {
  const ttk = estimator();
  const max = 1_000_000;
  run(ttk, { max, perSecond: 5000, to: 40 });
  // Healed back up 100k at 41 s, then 5,000/s again.
  const after = run(ttk, { max, hpAt: (s) => max - 200_000 + 100_000 - 5000 * (s - 40), from: 41, to: 60 });
  assert.ok(after !== null && after > 200_000, `${after}`);
});
