# ADR-008: Access Logging and Analytics

**Status:** Accepted — implemented
**Date:** 2026-08-09
**RFC:** RFC-001 §10, §12
**Depends on:** ADR-002
**Implementation:** `src/analytics.rs`, `src/location.rs`, `workers/public-log-stream/`
**Implemented by:** #119 (Analytics Engine request logging), #120 (account/product sampling index), #153 (request duration and hashed client IP), #122 (real-time public log stream via Durable Objects), #171 (aggregate live-globe activity by datacenter)

---

## Context

RFC-001 listed access logging, analytics, and cost attribution as future work needing a dedicated ADR. That work has since been built, so this ADR records the decisions rather than proposing them.

Three needs drove it:

- **Usage analytics** — which products and files are read, by whom, from where.
- **Cost attribution** — distinguishing open-data program buckets, Source Cooperative-owned buckets, and provider-hosted buckets, so egress can be attributed.
- **Public visibility** — Source Cooperative wants to show live platform activity, which needs a real-time stream rather than a query-time aggregate.

The privacy constraint is the binding one: the proxy sits in front of open scientific data whose value depends on being freely readable, and instrumenting it must not turn it into a system that tracks readers.

---

## Decision

### Cloudflare Analytics Engine for Request Events

Every product request writes one data point to an Analytics Engine dataset (`ANALYTICS` binding; separate datasets per environment). Requests to `/.well-known/*` and `/.sts` are excluded — they are not product requests, and logging them would pollute the dataset with an account named `.well-known`.

**Sampling index:** `{account_id}/{product_id}`. Analytics Engine samples at the index, so the boundary is per product — a single high-traffic product cannot cause another product's events to be dropped.

**Schema:**

| Column | Field | Notes |
|---|---|---|
| blob1–2 | `account_id`, `product_id` | |
| blob3 | `file_path` | Truncated to 256 bytes on a char boundary |
| blob4 | `method` | |
| blob5 | `user_id` | Empty for anonymous requests |
| blob6 | `country` | From `cf-ipcountry` |
| blob7 | `content_type` | |
| blob8 | `client_ip_hash` | HMAC-SHA256; empty when the IP is unknown |
| blob9 | `range` | `Range` header with the `bytes=` unit prefix stripped |
| double1–3 | `bytes_sent`, `status_code`, `duration_ms` | |

Retaining `range` and `bytes_sent` is what makes read-amplification analysis possible: range-heavy formats (COG, Zarr, Parquet) are read very differently from whole-object downloads, and the distinction matters for both caching decisions and egress attribution.

### Client IPs Are Never Stored

Raw client IPs do not enter the dataset. Each is replaced by `HMAC-SHA256(salt, ip)`, hex-encoded, keyed by a deployment secret (`IP_HASH_SALT`). This supports counting distinct clients without retaining PII.

- **HMAC, not a bare `SHA256(salt ‖ ip)`.** The keyed-hash construction is robust regardless of how the output is later reused.
- **The salt is load-bearing.** The IPv4 space is small enough to enumerate; an unsalted hash is reversible by brute force. When `IP_HASH_SALT` is unset the proxy still hashes but logs a warning at startup — it degrades to a weaker guarantee rather than silently logging raw IPs.
- **Empty in, empty out.** An unknown IP yields an empty string rather than collapsing every anonymous client onto one shared hash value.

### Analytics Never Blocks a Response

`log_request` cannot fail a request: a missing binding or a failed write is logged at `warn` and swallowed. Telemetry is strictly subordinate to serving data.

### Real-Time Public Log Stream

A separate Worker (`workers/public-log-stream`) holds a Durable Object that fans out live activity to WebSocket subscribers, powering the public activity globe.

- Only **successful GETs of public products** are broadcast. Non-product paths, errors, and writes are excluded.
- The broadcast is fire-and-forget inside `wait_until`, so it never blocks the response.
- Events carry Cloudflare-derived geolocation (latitude, longitude, city, colo) from the request's `cf` object, plus account, product, and key.
- Activity is **aggregated by datacenter** rather than emitted per request, which bounds fan-out cost and coarsens location before it is ever made public.

Broadcasting a coarse datacenter location for a public-product read is a deliberately narrower disclosure than the request event stored in Analytics Engine: nothing user-identifying crosses into the public stream.

### Structured Operational Logging

Distinct from analytics, and aimed at operators: requests emit a tracing span, and 5xx responses re-emit at `WARN` with the span fields inlined, since production runs at `LOG_LEVEL=WARN` and would otherwise record a server error with no context. Those warnings deliberately include relayed upstream response headers (`server`, `cf-ray`, `x-amz-request-id`), which is what distinguishes a genuine upstream 5xx from one minted inside Cloudflare's egress path or synthesised by the runtime with no upstream reply at all.

Logs and traces ship to Axiom with head sampling; analytics data points are unsampled at the binding and sampled by Analytics Engine at the index.

---

## Consequences

**Benefits**

- Per-product usage and egress data without operating a log pipeline.
- Read-amplification and range behaviour are measurable, which directly informs caching work.
- Reader privacy is preserved by construction: raw IPs are never written, and the public stream carries only coarse geography.
- Telemetry failures cannot affect availability.
- Operational 5xx diagnosis is possible at production log levels.

**Costs / Risks**

- Analytics Engine sampling means counts are estimates, not exact ledgers. **This is not a billing-grade record**; metering for billing would need its own accounting path.
- The hashed IP is stable across requests for a given salt, so it remains a pseudonymous identifier — re-identifiable by anyone holding the salt and a candidate IP. Rotating the salt breaks continuity of distinct-client counts, which is the intended trade.
- `IP_HASH_SALT` unset in a deployment degrades the guarantee silently apart from a startup warning.
- The public stream discloses that *someone* near a datacenter read a public product. Coarse, but not nothing.
- Cost attribution is derived from `bytes_sent` on the proxy, not from provider invoices; the two will not reconcile exactly.

---

## Alternatives Considered

**Log to R2 / an object-store log pipeline** — considered. More flexible querying and full-fidelity retention, but requires building ingestion, compaction, and query layers. Analytics Engine is native to the runtime and needs none of them. Revisit if per-event fidelity or long retention becomes a requirement.

**Store raw client IPs** — rejected. The platform serves open data; retaining reader IPs creates a disclosure liability disproportionate to the analytics value, and the hashed form answers the actual question (how many distinct clients).

**Unsalted hash of the client IP** — rejected. The IPv4 space is enumerable, so an unsalted digest is a reversible encoding of the IP rather than a protection.

**Sample by request rather than by product** — rejected. A single hot product would dominate the sample and starve visibility into the long tail, which is precisely where the interesting usage questions live.

**Broadcast every request to the public stream** — rejected. Unbounded fan-out cost, and a per-request public feed of who is reading what is a finer public disclosure than the platform should make. Datacenter aggregation bounds both.

**Emit billing events from this path** — deferred. Sampling makes it unsuitable, and RFC-001 §12 defers rate limiting, quotas, and billing until there is concrete demand.
