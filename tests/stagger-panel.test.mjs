import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";

const source = readFileSync(new URL("../public/src/js/staggerPanel.js", import.meta.url), "utf8");
const context = vm.createContext({ window: {} });
vm.runInContext(source, context);
const get = (name) => vm.runInContext(name, context);
const plain = (value) => JSON.parse(JSON.stringify(value));

// The first miniboss of the 2026-10-22 capture, its break shortened: Taunt,
// Punishment, two Pummels and Annihilate breaking it at the rest.
const miniboss = {
  entity: 24627, mobCode: 2310403, max: 150, start: 1300, end: 10900, result: "break", ended: "break", missing: 0,
  hits: [
    { t: 1300, actor: 5336, skill: 12120010, value: 20 },
    { t: 4300, actor: 5336, skill: 12090243, value: 35 },
    { t: 8200, actor: 5336, skill: 12040140, value: 3 },
    { t: 9300, actor: 5336, skill: 12040140, value: 3, ambiguous: true },
    { t: 9800, actor: 0, skill: 0, value: 80 },
    { t: 10900, actor: 7001, skill: 12300340, value: 9 },
  ],
};

test("a window adds up by player and by skill, the unknown last", () => {
  const sum = plain(get("summarizeStaggerWindow")(miniboss));
  assert.equal(sum.total, 150);
  assert.equal(sum.ambiguous, 1);
  assert.equal(sum.unknown, 80);
  assert.deepEqual(sum.actors.map((a) => [a.actor, a.total]), [[5336, 61], [7001, 9], [0, 80]]);
  const pummel = sum.actors[0].skills.find((s) => s.skill === 12040140);
  assert.deepEqual([pummel.total, pummel.count], [6, 2]);
  assert.equal(sum.actors[0].skills[0].skill, 12090243, "most first");
  assert.equal(Math.round(sum.actors[0].share * 100), 41);
});

test("how a window ended", () => {
  const key = get("staggerEndKey");
  assert.equal(key(miniboss), "break");
  assert.equal(key({ result: "missed", ended: "death" }), "death");
  assert.equal(key({ result: "missed", ended: "fightEnd" }), "fightEnd");
  assert.equal(key({ result: "missed", ended: "nonsense" }), "reset");
});

test("the target's windows first, then by start; times on the fight's axis", () => {
  const order = get("orderStaggerWindows");
  const windows = [
    { entity: 2, start: 500, hits: [] },
    { entity: 9, start: 900, hits: [] },
    { entity: 3, start: 100, hits: [] },
    { entity: 9, start: 100, hits: [] },
    { entity: 4, start: 0 },
  ];
  assert.deepEqual(plain(order(windows, 9)).map((w) => [w.entity, w.start]), [[9, 100], [9, 900], [3, 100], [2, 500]]);
  const pct = get("staggerPct");
  assert.equal(pct(5000, 20000), 25);
  assert.equal(pct(-10, 20000), 0);
  assert.equal(pct(30000, 20000), 100);
  assert.equal(pct(10, 0), 0);
  assert.equal(get("formatStaggerTime")(55751), "0:55.8");
  assert.equal(get("formatStaggerTime")(61000), "1:01.0");
});
