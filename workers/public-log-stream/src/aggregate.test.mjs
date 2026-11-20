import { test } from "node:test";
import assert from "node:assert/strict";
import { topProducts, buildLocs, addEvent } from "./aggregate.mjs";

test("addEvent: a batch adds its exact counts to the colo", () => {
  const colos = new Map();
  assert.equal(addEvent(colos, { colo: "EWR", products: { "a/x": 3, "a/y": 1 } }), 4);
  assert.equal(addEvent(colos, { colo: "EWR", products: { "a/x": 2 } }), 2);
  const e = colos.get("EWR");
  assert.equal(e.n, 6);
  assert.deepEqual([...e.products], [["a/x", 5], ["a/y", 1]]);
});

test("addEvent: a bare event from an older proxy counts as one request", () => {
  const colos = new Map();
  assert.equal(addEvent(colos, { colo: "SJC", account_id: "nasa", product_id: "l8" }), 1);
  assert.deepEqual([...colos.get("SJC").products], [["nasa/l8", 1]]);
});

test("addEvent: no colo or no valid counts leaves the accumulator untouched", () => {
  const colos = new Map();
  assert.equal(addEvent(colos, { products: { "a/x": 1 } }), 0);
  assert.equal(addEvent(colos, { colo: "EWR", products: { "a/x": -1, "a/y": 1.5, "a/z": "3" } }), 0);
  assert.equal(colos.size, 0);
});

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
