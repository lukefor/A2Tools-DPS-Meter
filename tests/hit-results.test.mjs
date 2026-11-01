import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";

function details(skills) {
  const window = { addEventListener() {}, _historyDetailsOverride: { skills, battleTime: 1000 } };
  const context = vm.createContext({ window, console, document: { readyState: "loading", addEventListener() {} } });
  vm.runInContext(readFileSync(new URL("../public/src/js/core.js", import.meta.url), "utf8"), context);
  const app = vm.runInContext("Object.create(DpsApp.prototype)", context);
  app.dpsFormatter = new Intl.NumberFormat("en-US");
  return app.getDetails({ id: 1 }, {});
}

test("a skill that only missed keeps its row and adds nothing to the totals", async () => {
  const d = await details([
    { actorId: 1, code: 16000000, name: "Hit", time: 4, dmg: 400, parry: 1, shieldBlock: 2, perfectBlock: 1 },
    { actorId: 1, code: 16330007, name: "Miss only", time: 0, dmg: 0, miss: 3 },
    { actorId: 1, code: 16330008, name: "Nothing", time: 0, dmg: 0 },
  ]);
  assert.deepEqual([...d.skills].map((s) => s.name), ["Hit", "Miss only"]);
  assert.equal(d.totalDmg, 400);
  assert.equal(d.totalHits, 4);
  assert.deepEqual([d.skills[0].shieldBlock, d.skills[0].perfectBlock, d.skills[1].miss], [2, 1, 3]);
});

test("a fight saved before the rename reads its flags under the new names", async () => {
  const d = await details([{ actorId: 1, code: 16000000, name: "Hit", time: 2, dmg: 200, smite: 1, powershard: 2 }]);
  assert.deepEqual([d.skills[0].regeneration, d.skills[0].perfectBlock], [1, 2]);
});
