import { test } from "node:test";
import assert from "node:assert/strict";
import { topProducts, buildLocs, freshLocs } from "./aggregate.mjs";

test("topProducts: highest counts first, capped at k", () => {
  const m = new Map([
    ["a/x", 3],
    ["a/y", 10],
    ["a/z", 1],
    ["a/w", 7],
  ]);
  assert.deepEqual(topProducts(m, 2), [
    ["a/y", 10],
    ["a/w", 7],
  ]);
});

test("topProducts: returns all when fewer than k", () => {
  assert.deepEqual(topProducts(new Map([["a/x", 2]]), 5), [["a/x", 2]]);
});

test("buildLocs: one entry per colo with top products", () => {
  const colos = new Map([
    [
      "EWR",
      { n: 5, products: new Map([["nasa/landsat", 4], ["esa/s2", 1]]) },
    ],
  ]);
  assert.deepEqual(buildLocs(colos, 5), [
    { colo: "EWR", n: 5, p: [["nasa/landsat", 4], ["esa/s2", 1]] },
  ]);
});

test("freshLocs: keeps colos seen within ttl and prunes the rest", () => {
  const ewr = { colo: "EWR", n: 2, p: [] };
  const recent = new Map([
    ["EWR", { loc: ewr, at: 9_000 }],
    ["SJC", { loc: { colo: "SJC", n: 1, p: [] }, at: 0 }],
  ]);
  assert.deepEqual(freshLocs(recent, 10_000, 10_000), [ewr]);
  assert.deepEqual([...recent.keys()], ["EWR"]);
});
