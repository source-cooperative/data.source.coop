// Pure, runtime-free shaping helpers — kept as plain JS so `node --test` can
// exercise them without a TS build step or extra deps. Imported by the DO.

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
 * Fold one posted event into the per-colo accumulator and return how many
 * requests it carried. The proxy posts a batch, `{colo, products: {key: n}}`;
 * a bare `{colo, account_id, product_id}` (older proxy) counts as one request.
 * @param {Map<string, {n: number, products: Map<string, number>}>} colos
 * @param {{colo?: string, products?: Record<string, number>, account_id?: string, product_id?: string}} event
 * @returns {number}
 */
export function addEvent(colos, event) {
  if (!event.colo) return 0;
  const entries = event.products
    ? Object.entries(event.products)
    : [[`${event.account_id ?? ""}/${event.product_id ?? ""}`, 1]];
  const colo = colos.get(event.colo) ?? { n: 0, products: new Map() };
  let total = 0;
  for (const [product, count] of entries) {
    if (!Number.isInteger(count) || count <= 0) continue;
    colo.n += count;
    colo.products.set(product, (colo.products.get(product) ?? 0) + count);
    total += count;
  }
  if (total > 0) colos.set(event.colo, colo);
  return total;
}

/**
 * Shape the per-colo accumulator into the wire payload: one entry per
 * datacenter with its request count and top products this window.
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
