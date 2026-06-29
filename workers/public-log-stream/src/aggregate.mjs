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
