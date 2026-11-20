# ADR-008: Access Logging and Analytics

**Status:** Accepted — implemented
**Date:** 2026-08-09
**RFC:** RFC-001 §10, §12
**Depends on:** ADR-002
**Implementation:** `src/analytics.rs`, `src/location.rs`, `src/location_batch.rs`, `workers/public-log-stream/`
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

Most product requests write one data point to an Analytics Engine dataset (`ANALYTICS` binding; separate datasets per environment). Any path beginning `/.` is excluded — this is meant to drop `/.well-known/*` and `/.sts`, which are not product requests and would pollute the dataset with an account named `.well-known`, but as written it also drops any product path starting with a dot.

Three request shapes short-circuit before logging and so are never recorded at all: OPTIONS preflights, the 501 for an unconfigured `/.sts`, and the 400 returned for a keyless write — the last of which *is* a product request.

**Sampling index:** `{account_id}/{product_id}`. Analytics Engine samples at the index, so the boundary is per product — a single high-traffic product cannot cause another product's events to be dropped.

**Schema:**

| Column | Field | Notes |
|---|---|---|
| blob1–2 | `account_id`, `product_id` | |
| blob3 | `file_path` | Truncated to 256 bytes on a char boundary |
| blob4 | `method` | |
| blob5 | `user_id` | The principal the request's SigV4 signature verified as; empty for anonymous requests. See *Who Made a Request* |
| blob6 | `country` | From `cf-ipcountry` |
| blob7 | `content_type` | |
| blob8 | `client_ip_hash` | HMAC-SHA256; empty when the IP is unknown |
| blob9 | `range` | `Range` header with the `bytes=` unit prefix stripped |
| blob10 | `access_key_id` | The access key ID that signed the request; empty for anonymous requests |
| double1–3 | `bytes_sent`, `status_code`, `duration_ms` | `bytes_sent` is the response's declared `Content-Length`, recorded as 0 when absent — a chunked response therefore logs zero egress |

Retaining `range` and `bytes_sent` is what makes read-amplification analysis possible: range-heavy formats (COG, Zarr, Parquet) are read very differently from whole-object downloads, and the distinction matters for both caching decisions and egress attribution.

### Who Made a Request

A signed request records who signed it. `user_id` is the `principal_name` multistore verified from the request's SigV4 signature: the identity sealed into the session token when `/.sts` minted the credentials, which a client cannot alter. The gateway hands it only to `SourceCoopRegistry::get_bucket`, which keeps it for the request; a request refused before that point, or made anonymously, records none. Two signed shapes record none either: an account-level listing (`/{account}`), which the router answers before multistore resolves the signature, and a presigned URL, which multistore does not authenticate and so treats as anonymous.

What the principal is depends on how the session was obtained. For a person who signed in to source.coop, it is their Ory identity ID, a UUID, which the accounts table maps to their account through `identity_id`. API keys and platform tokens only ever act as a service account (ADR-013, ADR-014), so their principal is the service account's id, `{owner_account_id}--{name}`, which names its owner too. A service account may hold several keys and trust several platform subjects, so for these sessions `access_key_id` (blob10) identifies the session, and the exchange that issued it writes one `credentials issued` log line carrying the same `access_key_id` with the API key's ID (`via: api_key`) or the platform token's issuer and subject (`via: platform`). The line is written straight to the console, outside the `LOG_LEVEL` filter, so production keeps it without counting it as a warning; there is one per session, not per request. Joining the two answers which key or workflow made a request, for as long as Workers Logs retains the line.

Anonymous reads stay anonymous: none of this applies to a request with no signature.

### Client IPs Are Never Stored

Raw client IPs do not enter the dataset. Each is replaced by `HMAC-SHA256(salt, ip)`, hex-encoded, keyed by a deployment secret (`IP_HASH_SALT`). This supports counting distinct clients without retaining PII.

- **HMAC, not a bare `SHA256(salt ‖ ip)`.** The keyed-hash construction is robust regardless of how the output is later reused.
- **The salt is load-bearing.** The IPv4 space is small enough to enumerate; an unsalted hash is reversible by brute force. When `IP_HASH_SALT` is unset the proxy still hashes but logs a warning the first time an isolate builds its config — there is no deploy-time gate, so the signal arrives mixed into request traffic rather than at release — it degrades to a weaker guarantee rather than silently logging raw IPs.
- **Empty in, empty out.** An unknown IP yields an empty string rather than collapsing every anonymous client onto one shared hash value.

### Retention and Salt Rotation

**There is no retention policy, and the salt is not rotated.** Both are deliberate positions rather than open questions.

> **Amended by ADR-015 (Observability Sampling, Sinks and Alerting).** Axiom is no longer a sink. Workers logs and traces are kept only in Cloudflare Observability, for its 7 days; retention is still set by the vendor rather than here.

Retention is whatever each sink applies by default: Analytics Engine keeps data points for its product-defined window, and Axiom retains logs and traces per the account's plan. Neither is configured in this repository, and nothing here shortens or extends them. The practical consequence is that the retention period is set by the vendors and can change without a change on our side.

`IP_HASH_SALT` is set once per environment and left. The hashed IP is therefore a **stable pseudonymous identifier for the full life of the retained data** — the same client hashes to the same value across the whole dataset, which is what makes distinct-client counting work at all. Rotation would break that continuity, and counting distinct clients is the reason the field exists.

What that costs is worth stating plainly: anyone holding the salt can confirm whether a given IP appears in the dataset, by hashing that IP and looking for it. The IPv4 space is enumerable, so with the salt the mapping is reversible in bulk. The salt is the whole protection, it is never re-keyed, and it guards data whose lifetime we do not control. Two things follow. Treat `IP_HASH_SALT` as a long-lived secret on the footing of the signing keys, not as a tuning knob. And an unset salt is materially worse than the startup warning implies, because there is no later rotation to recover from it — the dataset is then effectively storing recoverable IPs for its full retention.

### Analytics Never Blocks a Response

`log_request` cannot fail a request: a missing binding or a failed write is logged at `warn` and swallowed. Telemetry is strictly subordinate to serving data.

### Real-Time Public Log Stream

A separate Worker (`workers/public-log-stream`) holds a Durable Object that fans out live activity to WebSocket subscribers, powering the public activity globe.

- Only **GETs of public products returning a status below 400** are broadcast. Non-product paths, errors, and writes are excluded; note that a 3xx (for example a 304) counts as success, and HEAD reads are never broadcast.
- The broadcast is fire-and-forget inside `wait_until`, so it never blocks the response.
- The proxy posts only the datacenter (`colo`, from the request's `cf` object) and a count per `account/product`. No coordinates, city, country, or key leave the proxy, so requester geolocation finer than the datacenter is never published. The Durable Object also ignores any other field it receives. (Proxies that predate batching posted one event per request with latitude, longitude, city, country, and key; the Durable Object discarded those on arrival and still accepts the bare `account_id`/`product_id` form.)
- Activity is **batched by datacenter** rather than emitted per request. An isolate runs in one datacenter, so each isolate gathers its public reads for three seconds and posts one batch of exact counts. A busy datacenter collapses into few posts, a quiet one still reports its first request, and nothing is sampled away. Each post is a billed Durable Object request plus the CPU of both Workers (a service-binding call is not billed as a separate Worker request). Posting per request cost about $145 a month (Oct 16–Nov 15: $132 of Durable Object requests, about $14 of CPU) and overloaded the single global Durable Object at peak.
- The Durable Object **spreads each batch over the next six 500 ms ticks** (three seconds) rather than emitting it in one tick, so viewers see a steady trickle instead of a burst each time a proxy isolate flushes. Counts are conserved, and every emitted datacenter entry has a count of at least one. Together with the proxy's three-second window, activity can reach viewers up to about six seconds after the requests. Ticks are numbered by absolute time, so an alarm that fires late catches up on the ticks it missed. The one empty "idle" tick that lets the globe fade out is sent only once nothing is queued.
- A viewer who connects receives **one replay message** straight away: the last three seconds of already-emitted activity, merged, in the shape of a tick with `replay: true` and `windowMs`. It re-sends data already public and never overlaps what the viewer then receives live. The alarm runs only while someone is watching or activity is queued, so an idle hub still stops and hibernates.
- Counts held by an isolate that is evicted before its window ends, and activity queued in a Durable Object that is evicted, are lost, which is acceptable for a decorative display.

Broadcasting a coarse datacenter location for a public-product read is a deliberately narrower disclosure than the request event stored in Analytics Engine: nothing user-identifying crosses into the public stream.

### Structured Operational Logging

Distinct from analytics, and aimed at operators: requests emit a tracing span, and 5xx responses re-emit at `WARN` with the span fields inlined, since production runs at `LOG_LEVEL=WARN` and would otherwise record a server error with no context. Those warnings deliberately include relayed upstream response headers (`server`, `cf-ray`, `x-amz-request-id`), which is what distinguishes a genuine upstream 5xx from one minted inside Cloudflare's egress path or synthesised by the runtime with no upstream reply at all.

> **Amended by ADR-015 (Observability Sampling, Sinks and Alerting).** Production now keeps every log line the proxy writes, with invocation logs off; samples traces at 1%; redacts query strings; and exports nothing. Staging captures everything. The paragraph below describes the configuration before 2026-10-29.

In production, logs and traces ship to Axiom with head sampling; staging is configured with no destinations, so nothing leaves it. Analytics data points are unsampled at the binding and sampled by Analytics Engine at the index.

---

## Consequences

**Benefits**

- Per-product usage and egress data without operating a log pipeline.
- Read-amplification and range behaviour are measurable, which directly informs caching work.
- Reader privacy is preserved by construction for anonymous reads: raw IPs are never written, and the public stream carries only coarse geography.
- A signed request can be traced to who made it: the principal and access key it was signed with, and for an API key or platform token, which one.
- Telemetry failures cannot affect availability.
- Operational 5xx diagnosis is possible at production log levels.

**Costs / Risks**

- Analytics Engine sampling means counts are estimates, not exact ledgers. **This is not a billing-grade record**; metering for billing would need its own accounting path.
- The hashed IP remains a pseudonymous identifier, re-identifiable by anyone holding the salt and a candidate IP. Because the salt is never rotated and retention is vendor-default, that identifier is stable for the full life of the data. This is accepted: rotation would defeat distinct-client counting, which is the only reason the field exists.
- Retention is not owned by this project. If a retention commitment is ever needed — for a privacy policy, or a compliance answer — it has to be established at the sinks first; nothing in this repository constrains it today.
- `IP_HASH_SALT` unset in a deployment degrades the guarantee silently apart from a startup warning.
- The public stream discloses that *someone* near a datacenter read a public product. Coarse, but not nothing.
- Signed requests, reads of restricted products among them, are attributable to a named principal for as long as Analytics Engine keeps the row. That is the point of the field, and it applies only to callers who signed in or presented a key; it is still a record of who read what.
- Cost attribution is derived from `bytes_sent` on the proxy, not from provider invoices; the two will not reconcile exactly, and a response sent without a `Content-Length` contributes zero.

---

## Alternatives Considered

**Log to R2 / an object-store log pipeline** — considered. More flexible querying and full-fidelity retention, but requires building ingestion, compaction, and query layers. Analytics Engine is native to the runtime and needs none of them. Revisit if per-event fidelity or long retention becomes a requirement.

**Store raw client IPs** — rejected. The platform serves open data; retaining reader IPs creates a disclosure liability disproportionate to the analytics value, and the hashed form answers the actual question (how many distinct clients).

**Unsalted hash of the client IP** — rejected. The IPv4 space is enumerable, so an unsalted digest is a reversible encoding of the IP rather than a protection.

**Sample by request rather than by product** — rejected. A single hot product would dominate the sample and starve visibility into the long tail, which is precisely where the interesting usage questions live.

**Broadcast every request to the public stream** — rejected. Unbounded fan-out cost, and a per-request public feed of who is reading what is a finer public disclosure than the platform should make. Datacenter aggregation bounds both.

**Emit billing events from this path** — deferred. Sampling makes it unsuitable, and RFC-001 §12 defers rate limiting, quotas, and billing until there is concrete demand.
