import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";

const source = readFileSync(new URL("../public/src/js/core.js", import.meta.url), "utf8");

// The meter window as it hears from the backend, with the ids it asks the
// backend to take as the local player's recorded.
function meter() {
  const bound = [];
  const context = vm.createContext({
    console: { log() {} },
    window: {
      addEventListener() {},
      javaBridge: { updateOverlaySize() {}, bindLocalActorId: (id) => bound.push(Number(id)), setLocalPlayerId: (id) => bound.push(Number(id)) },
    },
    document: {
      readyState: "loading", addEventListener() {},
      documentElement: { classList: { toggle() {} } }, body: { classList: { toggle() {} } },
    },
    requestAnimationFrame: () => 1, cancelAnimationFrame() {},
    cachedDpsJson: null,
  });
  vm.runInContext(source, context);
  vm.runInContext("window.dpsData = { getDpsData: () => cachedDpsJson };", context);
  const app = vm.runInContext("new DpsApp()", context);
  app.elList = { querySelector: () => ({}) };
  app.detailsUI = { close() {}, isOpen: () => false };
  app.meterUI = { updateFromRows() {}, onResetMeterUi() {} };
  app.battleTime = { setVisible() {}, update() {} };
  for (const method of ["updateConnectionStatusUi", "updateBossHpBar", "updateMeterTotalBar", "logDebug", "refreshConnectionInfo"]) app[method] = () => {};
  app.syncCharacterNameFromGame = () => true;
  app.isOutOfCombatState = () => true;
  app.getTargetLabel = () => "";
  app.getRowsSummary = () => ({ listSignature: "" });
  app.USER_NAME = "Nullception";
  let tick = 0;
  const send = (rows, localPlayerId) => {
    const map = Object.fromEntries(rows.map(([id, name, amount]) => [id, { nickname: name, job: "Chanter", dps: amount, amount }]));
    tick += 1;
    context.cachedDpsJson = JSON.stringify({ targetMode: "allTargets", targetId: 0, battleTime: tick, map, localPlayerId });
    app.fetchDps();
  };
  return { app, send, bound };
}

// 2026-10-14, 22:12: a second run of a dungeon gave the player a new id
// (1566 -> 4369) while the first run's rows were still on the meter, the old
// id's under the player's name. The window took the old row for the player
// on every update and asked the backend for 1566, twice a poll (228 times in
// two minutes), flipping back to 4369 each time the backend's id came in.
test("the backend's id stays the player's, though an older row has the player's name", () => {
  const m = meter();
  for (let i = 0; i < 5; i++) m.send([[1566, "Nullception", 2_000_000], [14329, "Da****z", 2_300_000]], 4369);
  assert.equal(m.app.localPlayerId, 4369);
  assert.ok(!m.bound.includes(1566), `asked for the old id: ${m.bound}`);
});

// Before the backend knows (no self record yet), the row with the player's
// name still says who they are.
test("without the backend's id, the row with the player's name is the player", () => {
  const m = meter();
  m.send([[1566, "Nullception", 2_000_000], [14329, "Da****z", 2_300_000]], null);
  assert.equal(m.app.localPlayerId, 1566);
});
