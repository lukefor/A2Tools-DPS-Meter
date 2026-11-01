import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

const html = readFileSync(new URL("../index.html", import.meta.url), "utf8");

// The divs open around each line of the page, counted from <body>.
const divDepths = () => {
  const body = html.indexOf("<body");
  const depth = [];
  let open = 0;
  const tag = /<(\/?)div\b[^>]*>/g;
  let m;
  while ((m = tag.exec(html))) {
    if (m.index < body) continue;
    open += m[1] ? -1 : 1;
    depth.push({ at: m.index, open, tag: m[0] });
  }
  return { depth, open };
};

// The overlay sizes its window to .meter, so the rows must be inside it: a
// stray </div> in Settings closed .meter early and the window stopped below
// the boss HP bar (2.0.78).
test("the meter's rows are inside .meter, and every div closes", () => {
  const { depth, open } = divDepths();
  assert.equal(open, 0, "index.html opens and closes a different number of divs");
  const meter = depth.find((d) => /class="meter"/.test(d.tag));
  const list = depth.find((d) => /class="list"/.test(d.tag));
  assert.ok(meter && list, "index.html has .meter and its .list");
  // .meter is still open where .list starts.
  const closed = depth.find((d) => d.at > meter.at && d.open < meter.open);
  assert.ok(closed.at > list.at, ".meter closes before its row list");
});
