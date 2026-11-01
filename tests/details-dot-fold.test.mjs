import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";

const source = readFileSync(new URL("../public/src/js/details.js", import.meta.url), "utf8");
const context = vm.createContext({ window: {}, console });
vm.runInContext(source, context);

test("a DoT row folds under its skill in every language", () => {
  const fold = vm.runInContext("foldDotRows", context);
  for (const dotName of ["Feuer - DOT", "Feuer - DoT", "Feuer - 継続ダメージ", "Feuer - периодический"]) {
    const rows = fold([
      { code: "16330000", name: "Feuer", isDot: false, dmg: 100 },
      { code: "16330000-dot", name: dotName, isDot: true, dmg: 50 },
      { code: "16990000-dot", name: "Gift - DoT", isDot: true, dmg: 7 },
    ]);
    assert.deepEqual([...rows].map((r) => r.code), ["16330000", "16990000-dot"], dotName);
    assert.equal(rows[0]._dotChild.dmg, 50);
    assert.equal(rows[0]._combinedDmg, 150);
  }
});
