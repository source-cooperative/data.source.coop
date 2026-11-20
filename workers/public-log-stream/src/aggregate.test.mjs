import { test } from "node:test";
import assert from "node:assert/strict";
import {
  SPREAD_SLOTS as K,
  topProducts,
  buildLocs,
  totalRequests,
  spreadCount,
  addBatchSpread,
  mergeSlots,
  dueSlots,
  replayWindow,
  pruneSlots,
  hasPending,
} from "./aggregate.mjs";

const sum = (xs) => xs.reduce((a, b) => a + b, 0);
// Total n and per-product counts for a colo across every slot.
const across = (slots, colo) => {
  const m = mergeSlots(slots.values());
  return m.get(colo);
};

test("spreadCount: parts sum to the count and differ by at most 1", () => {
  for (let count = 0; count <= 50; count++) {
    for (let cursor = 0; cursor < K; cursor++) {
      const { parts } = spreadCount(count, K, cursor);
      assert.equal(parts.length, K);
      assert.equal(sum(parts), count);
      assert.ok(Math.max(...parts) - Math.min(...parts) <= 1);
    }
  }
});

test("spreadCount: a count of 1 lands on exactly one slot, at the cursor", () => {
  const { parts, cursor } = spreadCount(1, K, 4);
  assert.deepEqual(parts, [0, 0, 0, 0, 1, 0]);
  assert.equal(cursor, 5);
});

test("spreadCount: the cursor wraps and count-1 calls fill slots round-robin", () => {
  let cursor = 4;
  const hits = [];
  for (let i = 0; i < K; i++) {
    const r = spreadCount(1, K, cursor);
    hits.push(r.parts.indexOf(1));
    cursor = r.cursor;
  }
  assert.deepEqual(hits.sort(), [0, 1, 2, 3, 4, 5]);
  assert.equal(cursor, 4);
});

test("addBatchSpread: counts are conserved across slots and start next tick", () => {
  const slots = new Map();
  const { total } = addBatchSpread(
    slots,
    { colo: "EWR", products: { "a/x": 40, "a/y": 3 } },
    10,
    K,
    0,
  );
  assert.equal(total, 43);
  assert.deepEqual([...slots.keys()].sort((a, b) => a - b), [11, 12, 13, 14, 15, 16]);
  const e = across(slots, "EWR");
  assert.equal(e.n, 43);
  assert.deepEqual([...e.products].sort(), [["a/x", 40], ["a/y", 3]]);
});

test("addBatchSpread: a bare event from an older proxy counts as one request", () => {
  const slots = new Map();
  const { total } = addBatchSpread(
    slots,
    { colo: "SJC", account_id: "nasa", product_id: "l8" },
    0,
    K,
    0,
  );
  assert.equal(total, 1);
  assert.equal(slots.size, 1);
  assert.deepEqual([...across(slots, "SJC").products], [["nasa/l8", 1]]);
});

test("addBatchSpread: rejects a missing colo and bad counts", () => {
  const slots = new Map();
  assert.equal(addBatchSpread(slots, { products: { "a/x": 1 } }, 0, K, 0).total, 0);
  const bad = { "a/x": 0, "a/y": -1, "a/z": 1.5, "a/w": "3", "a/v": Number.MAX_SAFE_INTEGER + 2 };
  assert.equal(addBatchSpread(slots, { colo: "EWR", products: bad }, 0, K, 0).total, 0);
  assert.equal(slots.size, 0);
});

test("addBatchSpread: the key cap drops products but keeps n whole", () => {
  const slots = new Map();
  const products = {};
  for (let i = 0; i < 150; i++) products[`a/p${i}`] = 6;
  const { total } = addBatchSpread(slots, { colo: "EWR", products }, 0, K, 0);
  assert.equal(total, 900);
  const e = across(slots, "EWR");
  assert.equal(e.n, 900);
  assert.equal(e.products.size, 100);
});

test("addBatchSpread: no slot ever carries a zero count", () => {
  const slots = new Map();
  addBatchSpread(slots, { colo: "EWR", products: { "a/x": 2 } }, 0, K, 0);
  for (const slot of slots.values()) {
    for (const e of slot.values()) assert.ok(e.n >= 1);
  }
});

test("mergeSlots then top-k differs from truncating each slot", () => {
  const slots = new Map([
    [1, new Map([["EWR", { n: 3, products: new Map([["a/x", 2], ["a/y", 1]]) }]])],
    [2, new Map([["EWR", { n: 3, products: new Map([["a/z", 2], ["a/y", 1]]) }]])],
    [3, new Map([["EWR", { n: 3, products: new Map([["a/w", 2], ["a/y", 1]]) }]])],
  ]);
  const merged = mergeSlots(slots.values());
  assert.deepEqual(topProducts(merged.get("EWR").products, 1), [["a/y", 3]]);
});

test("dueSlots: a late alarm catches up on every missed tick, once", () => {
  const slots = new Map([
    [11, new Map([["EWR", { n: 1, products: new Map([["a/x", 1]]) }]])],
    [12, new Map([["EWR", { n: 2, products: new Map([["a/x", 2]]) }]])],
    [14, new Map([["EWR", { n: 4, products: new Map([["a/x", 4]]) }]])],
  ]);
  assert.equal(dueSlots(slots, 10, 13).get("EWR").n, 3);
  assert.equal(dueSlots(slots, 13, 14).get("EWR").n, 4);
  assert.equal(dueSlots(slots, 14, 14).size, 0);
});

test("replayWindow: only ticks already emitted and at most k old", () => {
  const slot = (n) => new Map([["EWR", { n, products: new Map([["a/x", n]]) }]]);
  const slots = new Map([
    [3, slot(1)], // older than the window
    [5, slot(2)],
    [10, slot(4)],
    [11, slot(8)], // not yet emitted: must not be replayed
  ]);
  const r = replayWindow(slots, 10, 6);
  assert.equal(r.get("EWR").n, 6);
});

test("replayWindow: nothing recent gives an empty result", () => {
  assert.equal(replayWindow(new Map(), 10, K).size, 0);
});

test("pruneSlots bounds the map and hasPending sees only future ticks", () => {
  const slots = new Map();
  for (let i = 1; i <= 20; i++) slots.set(i, new Map());
  pruneSlots(slots, 14);
  assert.deepEqual([...slots.keys()], [15, 16, 17, 18, 19, 20]);
  assert.equal(hasPending(slots, 19), true);
  assert.equal(hasPending(slots, 20), false);
});

test("a sparse trickle leaves later slots pending until the last is due", () => {
  const slots = new Map();
  addBatchSpread(slots, { colo: "EWR", products: { "a/x": 1 } }, 10, K, 0);
  // The single request landed on one slot; ticks before it are empty but pending.
  for (let tick = 10; tick < 11; tick++) assert.equal(hasPending(slots, tick), true);
  assert.equal(hasPending(slots, 11), false);
});

test("totalRequests sums n across colos", () => {
  const colos = new Map([
    ["EWR", { n: 5, products: new Map() }],
    ["SJC", { n: 2, products: new Map() }],
  ]);
  assert.equal(totalRequests(colos), 7);
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
