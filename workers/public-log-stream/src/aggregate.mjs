// Pure, runtime-free shaping helpers — kept as plain JS so `node --test` can
// exercise them without a TS build step or extra deps. Imported by the DO.
//
// Activity arrives as batches (one per proxy isolate, i.e. one datacenter) and
// is spread over the next few ticks so viewers get a steady trickle rather than
// a burst. Ticks are numbered by absolute index, floor(ms / interval), and each
// tick's activity lives in a "slot": Map<colo, {n, products}>.

/** Ticks a batch is spread over: 6 x 500 ms = 3 s, the proxy's batching window. */
export const SPREAD_SLOTS = 6;

// A batch names a few products per datacenter; these only bound a bad sender.
const MAX_PRODUCT_KEYS = 100;
const MAX_KEY_LENGTH = 128;

/**
 * Top-k products by request count, count descending.
 * @param {Map<string, number>} products
 * @param {number} k
 * @returns {[string, number][]}
 */
export function topProducts(products, k) {
  return [...products.entries()].sort((a, b) => b[1] - a[1]).slice(0, k);
}

/**
 * Shape a colo accumulator into the wire payload: one entry per datacenter
 * with its request count and top products.
 * @param {Map<string, {n: number, products: Map<string, number>}>} colos
 * @param {number} maxProducts
 * @returns {{colo: string, n: number, p: [string, number][]}[]}
 */
export function buildLocs(colos, maxProducts) {
  return [...colos.entries()].map(([colo, e]) => ({
    colo,
    n: e.n,
    p: topProducts(e.products, maxProducts),
  }));
}

/**
 * Total requests across a colo accumulator.
 * @param {Map<string, {n: number}>} colos
 */
export function totalRequests(colos) {
  let total = 0;
  for (const e of colos.values()) total += e.n;
  return total;
}

/**
 * Split `count` into `k` near-equal parts (they differ by at most 1). The extra
 * units go to consecutive slots starting at `cursor`, and the cursor moves on,
 * so a run of count-1 batches fills the slots round-robin.
 * @param {number} count
 * @param {number} k
 * @param {number} cursor
 * @returns {{parts: number[], cursor: number}}
 */
export function spreadCount(count, k, cursor) {
  const parts = new Array(k).fill(Math.floor(count / k));
  const rem = count % k;
  for (let i = 0; i < rem; i++) parts[(cursor + i) % k]++;
  return { parts, cursor: (cursor + rem) % k };
}

function addTo(slots, idx, colo, n, product) {
  const slot = slots.get(idx) ?? new Map();
  const e = slot.get(colo) ?? { n: 0, products: new Map() };
  e.n += n;
  if (product !== null) {
    e.products.set(product, (e.products.get(product) ?? 0) + n);
  }
  slot.set(colo, e);
  slots.set(idx, slot);
}

/**
 * Spread one posted event over the ticks after `nowTick`. The proxy posts a
 * batch, `{colo, products: {key: n}}`; a bare `{colo, account_id, product_id}`
 * (older proxy) counts as one request. Counts are conserved: a colo's `n`
 * always includes products dropped by the key cap.
 * @param {Map<number, Map<string, {n: number, products: Map<string, number>}>>} slots
 * @param {{colo?: string, products?: Record<string, number>, account_id?: string, product_id?: string}} event
 * @param {number} nowTick
 * @param {number} k
 * @param {number} cursor
 * @returns {{total: number, cursor: number}}
 */
export function addBatchSpread(slots, event, nowTick, k, cursor) {
  if (!event.colo || typeof event.colo !== "string") return { total: 0, cursor };
  const raw = event.products
    ? Object.entries(event.products)
    : [[`${event.account_id ?? ""}/${event.product_id ?? ""}`, 1]];
  const valid = raw
    .filter(([, count]) => Number.isSafeInteger(count) && count > 0)
    .map(([key, count]) => [key.slice(0, MAX_KEY_LENGTH), count])
    .sort((a, b) => b[1] - a[1]);

  const place = (count, product) => {
    const spread = spreadCount(count, k, cursor);
    cursor = spread.cursor;
    spread.parts.forEach((n, j) => {
      if (n > 0) addTo(slots, nowTick + 1 + j, event.colo, n, product);
    });
  };

  let total = 0;
  let other = 0;
  valid.forEach(([key, count], i) => {
    total += count;
    if (i < MAX_PRODUCT_KEYS) place(count, key);
    else other += count;
  });
  if (other > 0) place(other, null);
  return { total, cursor };
}

/**
 * Merge several slots into one colo accumulator.
 * @param {Iterable<Map<string, {n: number, products: Map<string, number>}>>} slotMaps
 */
export function mergeSlots(slotMaps) {
  const out = new Map();
  for (const slot of slotMaps) {
    for (const [colo, e] of slot) {
      const t = out.get(colo) ?? { n: 0, products: new Map() };
      t.n += e.n;
      for (const [p, c] of e.products) {
        t.products.set(p, (t.products.get(p) ?? 0) + c);
      }
      out.set(colo, t);
    }
  }
  return out;
}

/**
 * Activity of ticks in (afterIdx, uptoIdx], merged. A late alarm passes the
 * gap it missed and catches up in one tick.
 */
export function dueSlots(slots, afterIdx, uptoIdx) {
  return mergeSlots(
    [...slots].filter(([i]) => i > afterIdx && i <= uptoIdx).map(([, m]) => m),
  );
}

/**
 * The last `k` emitted ticks, merged, for a viewer who has just connected.
 * Never includes a tick after `lastEmitted`, so it cannot overlap what the
 * viewer receives live.
 */
export function replayWindow(slots, lastEmitted, k) {
  return dueSlots(slots, lastEmitted - k, lastEmitted);
}

/** Drop ticks at or before `uptoIdx`. */
export function pruneSlots(slots, uptoIdx) {
  for (const i of [...slots.keys()]) if (i <= uptoIdx) slots.delete(i);
}

/** True while activity is queued for a tick after `nowTick`. */
export function hasPending(slots, nowTick) {
  for (const i of slots.keys()) if (i > nowTick) return true;
  return false;
}
