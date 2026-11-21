# ADR-017: Object Events — Read/Write Event Stream

**Status:** Proposed — draft for discussion
**Date:** 2026-11-21
**Depends on:** ADR-005, ADR-008, ADR-014, ADR-016
**Amends:** ADR-008 (adds a durable, full-fidelity event path beside Analytics Engine; Analytics Engine stays)
**Related:** ADR-015 (alerting on emit failures)

---

## Context

The proxy sees every read and write of every product, and today records almost none of it durably. ADR-008 gives us one sampled Analytics Engine row per request and a coarse, aggregate public globe. That serves dashboards. It does not serve four needs that have since appeared:

1. **Analytics.** The same rows we already write, but fed from one source of truth instead of a bespoke call site.
2. **Per-product audit.** An unsampled, queryable record of who read or wrote what, with retention we control. ADR-008 recorded that retention is whatever the vendor does and said to revisit R2-based logging "if per-event fidelity or long retention becomes a requirement". This is that trigger.
3. **Usage tracking.** Per-principal egress and write volume, near real time, as the input to limits. ADR-008 is explicit that Analytics Engine is not a billing-grade record and deferred quotas until there was concrete demand.
4. **Event-driven systems.** People want to react to writes. The motivating case: `source/metadata-catalog` is written by a GitHub pipeline, and every update should trigger a reindex into our database. Today the only options are polling a listing or having the pipeline call the indexer itself, which does not generalise and cannot be done by a third party watching someone else's product.

All four are consumers of one fact: *an operation on an object completed*. This ADR makes that fact a first-class event and decides how it is captured, stored, and served.

Constraints:

- **Proxy-only vantage.** Events describe operations that went through the proxy. Writes made directly to a backend bucket (a provider writing to its own bucket, a BYOB owner using their own credentials) are invisible. This is inherent, and the event stream must not claim to be a complete change log of the underlying storage.
- **Telemetry never blocks a response** (ADR-008). Emission is `wait_until`, failure is logged and swallowed.
- **Privacy** (ADR-008). Raw IPs are never stored. A signed request is attributable to a principal; an anonymous one is not. The stream must not widen who can learn that.
- **Workers runtime.** No cross-request shared state in the request path (awaiting an isolate-shared async lock got requests killed in production, fixed in #197); coordination happens in Durable Objects or downstream consumers, never in an isolate-shared lock.

---

## Decision

### Two event classes, deliberately different

| | Write events | Read events |
|---|---|---|
| Operations | `PutObject`, `CompleteMultipartUpload`, `CopyObject` (on the destination), `DeleteObject`, each key of `DeleteObjects` | `GetObject`, `HeadObject` |
| Volume | Low (writes are rare relative to reads) | Very high (roughly the whole request rate; the observability config in `wrangler.toml` implies tens of millions of requests a day) |
| Emitted on | Success only (2xx) | Any status, so audit records refusals too |
| Consumers | Router → event log, audit, usage, subscribers | Audit, usage, analytics |
| Offered to external subscribers | **Yes** | **No** |

`UploadPart`, `ListObjectsV2`, multipart bookkeeping and `/.`-prefixed paths emit nothing. Listings are not object operations; an upload becomes visible when it completes, which is what a subscriber cares about.

External subscribers get write events only. A per-request read feed of who is reading a product is a finer disclosure than the platform should make (the same reasoning ADR-008 used to reject broadcasting every request), and it is not what the stated use cases need. The existing aggregate globe is unchanged.

### Event shape

One envelope, [CloudEvents](https://cloudevents.io/)-compatible so existing tooling can consume it without a custom parser:

```json
{
  "specversion": "1.0",
  "id": "01JD…",                       // ULID; the idempotency key
  "type": "coop.source.object.created", // .created | .deleted | .read
  "source": "/source/metadata-catalog",
  "subject": "records/2026/11/21.parquet",
  "time": "2026-11-21T17:04:05.123Z",
  "data": {
    "op": "put",                        // put | multipart | copy | delete | get | head
    "etag": "\"d41d8cd9…\"",
    "size": 1048576,
    "status": 200,
    "principal": "acme--nightly-sync",  // omitted for anonymous; see Privacy below
    "trace_id": "…"                     // x-source-trace-id (ADR-016)
  }
}
```

Read events add `bytes_sent`, `range`, `duration_ms`, `country` and `content_type`, i.e. the columns Analytics Engine already holds, minus `client_ip_hash` (see Privacy). `subject` is the full key; the 256-byte truncation in Analytics Engine is a blob limit, not a property of the event.

The `trace_id` ties an event back to the request's logs and spans (ADR-016), so a subscriber's surprise can be traced to the exact write.

### Capture: one emit point in the proxy

The proxy builds the event once, after the response status is known, in the same place `log_request` is called today, and hands it to two sinks inside `wait_until`:

- **Write events** → a Cloudflare Queue (`OBJECT_EVENTS`).
- **Read events** → a Pipelines stream (`READ_EVENTS`) directly.

Reads skip the Queue because nothing needs to subscribe to them in real time, a Pipelines stream ingests for free, and a Queue is capped at 5,000 messages/second which a popular product's read rate could approach. Writes go through the Queue because several consumers need them and some need low latency.

`log_request`'s Analytics Engine write stays as is. It is not removed here; it is cheap, already alerted on (ADR-015 monitors read it), and answers dashboard questions without a table scan.

**Delivery guarantee: at-least-once, best effort at the source.** If the Queue `send` fails, the proxy logs a warning and drops the event, per ADR-008. A dropped write event means a subscriber misses a reindex trigger. Two mitigations, both cheap:

1. A failed `send` increments a counter in the existing analytics path and is covered by an ADR-015 monitor, so a sustained failure pages rather than silently degrades.
2. **Consumers are told to reconcile.** The subscriber contract (below) says an event is a hint that something changed, and a consumer that cannot tolerate a missed one must also list the product on a schedule. The metadata-catalog indexer does a nightly full list.

We chose not to await the Queue `send` before responding, or to fail the write when it fails. Writes are rare enough that the latency would be tolerable, but making object storage unavailable because an event bus is down inverts the dependency: the data plane would then be only as available as the telemetry plane.

### Fan-out: a router Worker

One consumer Worker (`workers/object-events`) reads the `OBJECT_EVENTS` Queue in batches and fans out. Each target is independent, so one failing does not block the others, and a failed target is retried by the Queue's retry and dead-letter mechanism rather than by bespoke code.

```
                      ┌─▶ Pipelines stream ─▶ Iceberg audit table (R2 Data Catalog)
proxy ─▶ OBJECT_EVENTS│
 (writes)   Queue ─▶ router ─▶ product event log (DO) ─▶ /.events subscribers
                      └─▶ usage DO (per principal)

proxy ─▶ READ_EVENTS Pipelines stream ─▶ Iceberg audit table
 (reads)                              └──▶ usage rollup (from the table, batch)
```

### Audit log: one Iceberg table, partitioned, not one table per product

Cloudflare Pipelines writes to R2 Data Catalog as Iceberg with exactly-once delivery to the sink and automatic compaction. We use **one `object_events` table partitioned by `(day, account_id, product_id)`**, not a table per product. A table per product is an unbounded number of tables and catalog entries, and every per-product question ("show me product X's writes last month") is already a cheap partition-pruned scan. Both the write and read streams land in it, distinguished by `type`.

This gives us what ADR-008 said we lacked: **retention we own.** Retention is a partition drop and snapshot expiry we run, so a privacy-policy or compliance answer can be established here instead of at a vendor default. The proposed default is 13 months; this is a question for the team (see Open Questions).

The table is the **ledger of record**. Analytics Engine and the usage counters below are approximations derived for speed; where they disagree with the table, the table wins.

Per-product audit *access* is the open design point. R2 Data Catalog tokens are bucket-wide, so a product owner cannot be handed a catalog credential scoped to their own partitions. Product owners read their audit trail through a Source API endpoint that queries the table and filters to their product, not by connecting an engine to the catalog. This is also what keeps one product's audit log from becoming another's. Sketched, not designed, here.

### Usage tracking

A Durable Object per principal (`{account_id}` for people and organisations, the service account id for service accounts) keeps month-to-date counters in SQLite storage: bytes read, bytes written, request counts, per product. The router increments it from write events; read volume is rolled up from read events in a short-interval batch from the Iceberg table, not per request, so a hot product cannot create a hot Durable Object.

Counters are **approximate and eventually consistent**, and the DO dedupes on event `id` within a window so Queue redelivery does not double count. They are explicitly not billing-grade, matching ADR-008's position. A billing path reconciles against the Iceberg table.

**This ADR tracks usage; it does not enforce limits.** When a counter crosses a configured threshold the usage DO emits a `coop.source.usage.limit-reached` event into the same stream, so that enforcement, notification and billing can each be built as a subscriber. Whether and how the proxy then refuses requests is a separate decision that has product consequences (open data must stay freely readable, ADR-008) and gets its own ADR.

### Public subscription: `/.events`

External users subscribe to **write events for a product** through a pull API on the proxy, under a reserved top-level path next to `/.sts`:

```
GET /.events/{account}/{product}?after={cursor}&limit=100&wait=25
```

- **Cursor-based pull, not push.** A per-product event log (a Durable Object, one per product, SQLite-backed) holds recent write events with a monotonically increasing sequence number. The subscriber holds its own cursor and asks for events after it. The server keeps no per-subscriber state, so there is nothing to create, expire, or leak per subscriber, and a subscriber that falls behind or restarts simply resumes from its cursor.
- **Long-poll via `wait`.** The request holds open for up to `wait` seconds for new events and returns early when one arrives. An SSE form (`Accept: text/event-stream`) is the same endpoint and is a later addition, not a v1 requirement.
- **Replay window.** The log keeps 7 days of write events per product (a proposal). A cursor older than the window returns `410 Gone` with the oldest available cursor, so a consumer knows it must reconcile by listing.
- **Ordering.** Events are ordered per product by sequence. There is no ordering across products.
- **Delivery.** At-least-once; consumers dedupe on `id`. Combined with the capture guarantee above, the contract stated to subscribers is: *events are an acceleration, not a ledger. Treat a missed or duplicated event as normal and reconcile by listing if correctness requires it.*

**Authorization.** Subscribing is a read of the product's activity, so it requires what reading the product requires, evaluated by the existing authorization (ADR-005, ADR-011) for the `read_data` action:

- It uses the same SigV4 credentials as object reads, so a person, an API key, or a service account (ADR-014) subscribes with the credentials it already gets from `/.sts`. No new credential type and no new IdP surface.
- A **role ceiling** (ADR-011) applies as it does to reads, so a `ReadOnly` credential can subscribe and a credential scoped to other products cannot.
- For a **public product**, any *signed* caller may subscribe; anonymous callers may not. Event metadata is already discoverable by listing a public product, so this is not a confidentiality line. It exists so that every subscriber has an identity to rate-limit and attribute, which bounds the cost of an anonymous flood on a Durable Object.
- For a **private product**, `read_data` membership (or an owning account) is required.

**Privacy.** What a subscriber sees is derived from what the caller could already learn by listing and `HEAD`ing, plus nothing new about people:

- `principal` is **omitted for subscribers who are not product owners or maintainers.** A reader of a product learns that `records/…` changed, not which person or service account changed it. Owners and maintainers see it.
- `client_ip_hash` and `country` are never in the write-event envelope at all, so no subscriber can see them.

**Rate limit.** A per-principal limit on `/.events` requests, using the same Workers rate-limit binding mechanism as `/.sts`, bounds a polling loop. A `wait` long-poll counts as one request, which is the point of offering it.

### The `source/metadata-catalog` example, end to end

1. The GitHub pipeline `PUT`s a file via a platform-token session as a service account (ADR-013, ADR-014).
2. The proxy emits `object.created` for `records/2026/11/21.parquet` into `OBJECT_EVENTS`.
3. The router appends it to the `source/metadata-catalog` event log, assigning sequence 4821.
4. The indexer, running as a service account with `read_data` on that product, has been long-polling `/.events/source/metadata-catalog?after=4820&wait=25`. It receives the event, fetches the object, upserts the record into the database, and advances its cursor to 4821.
5. Nightly, the indexer lists the product and re-indexes anything it does not have, which covers a dropped event.

An external user building their own pipeline against a product they can read follows steps 4–5 unchanged. Nothing about the indexer is privileged.

---

## Consequences

**Benefits**

- One capture point instead of one call site per consumer; adding a consumer is adding a target in the router, not editing the proxy.
- Audit and analytics at full fidelity with retention we control; ADR-008's "retention is the vendor's" caveat no longer applies to the audit record.
- Event-driven integration is a platform capability with the same credentials and authorization users already have, not a per-integration favour.
- Pull with client-held cursors removes per-subscriber server state, which removes the main operational and abuse surface of a subscription system.
- Reads and writes are separated by volume and by who may see them, so the common high-volume path pays for neither a Queue nor a public API.

**Costs / Risks**

- **Best-effort capture means a missed event is possible.** Subscribers who need exactness must reconcile. This is stated in the contract rather than hidden, but it is a real limit on what "event-driven" can mean here, and consumers that ignore it will see drift.
- **The stream is not a complete change log.** Direct-to-backend writes are invisible. A subscriber to a BYOB product whose owner also writes to the bucket directly will miss those changes.
- **Pipelines cost and maturity.** Pipelines pricing was announced in May 2026 (stateless SQL transforms and sinks are billed per GB; billing was not yet enabled when announced). At the request rate the observability config implies, a read event of a few hundred bytes is on the order of hundreds of GB a month, which prices at tens of dollars, but the real number depends on the production request count, which should be measured before committing. We are also taking a dependency on a young Cloudflare product for the audit path.
- **Per-product event-log DOs are a hot spot for a very write-heavy product.** A single Durable Object serialises one product's appends. Write rates here are low, but a bulk-loading product could stress it; the response is batching appends per Queue batch, which the router already does.
- **Presigned writes are unattributed.** ADR-008 notes that a presigned URL is treated as anonymous, so a write through one carries no principal in its event. The audit record for those is weaker than for signed requests.
- **Usage counters can disagree with the ledger.** They are approximate by design; a consumer treating them as exact will be surprised.
- **A new public surface** (`/.events`) to authorize, rate-limit, version, and document.

---

## Alternatives Considered

**R2 event notifications.** Native, and gives `object-create`/`object-delete` to a Queue for free. Rejected as the mechanism: it only covers R2-backed products (our backends include other providers), fires for writes that bypass the proxy and so cannot name the principal, and cannot express read events at all. It remains useful as a cross-check for R2-backed buckets.

**A Queue per subscriber, consumed over HTTP pull.** Natural fit for "each consumer gets its own durable cursor". Rejected: pulling from a Queue requires a Cloudflare API token with queue read/write permissions, which cannot be scoped to a single subscriber's queue and so cannot be handed to external users. A proxy-fronted pull API with our own authorization is the only shape that lets us use the credentials users already have.

**Webhooks first.** Familiar to integrators, and removes the polling loop. Deferred, not rejected: it needs subscription registration, secret management, signature verification, retry and dead-letter policy, and SSRF controls on user-supplied URLs, all of it new surface. Pull is a strict subset of that work and is what the event log needs anyway; webhooks can be a delivery adapter on the same log later.

**Analytics Engine as the event stream.** Already there. Rejected: it is sampled, queried rather than subscribed to, and bounded by blob limits, so it cannot carry an exact, ordered, per-key event to a subscriber.

**Pipelines only, no Queue or router.** Simplest for audit. Rejected for writes: Pipelines is a batch sink with seconds-to-minutes latency and no subscribe primitive, and the usage DO and event log need per-event delivery.

**An external broker (Kafka, NATS, Redpanda).** Strongest semantics. Rejected: a new vendor, new operational burden and a second authentication system, to solve a delivery-guarantee problem we decided to handle by reconciliation. Revisit if consumers need exactly-once or very long replay.

**Have the GitHub pipeline call the indexer directly.** Zero new infrastructure for the one motivating case. Rejected as the answer to this ADR because it does not let any other party react to a product they can read; it is still a fine stopgap while this is built.

**Offer read events to subscribers.** Rejected, as above: no stated use case needs them, and they would publish a per-request record of who reads what.

---

## Open Questions

These are the decisions we most want review on.

1. **Is best-effort capture acceptable for write events,** or does the indexer use case demand an outbox (write the event durably in the proxy's request path, e.g. to a DO, then relay)? The outbox closes the gap at the cost of a hop on every write.
2. **Retention.** 13 months for the audit table and 7 days for the subscriber replay window are proposals. Is there a policy or compliance driver that sets either?
3. **Audit access for product owners.** An API endpoint over the table is sketched; is that enough, or do owners need to run their own queries (which would need a different isolation story than bucket-wide catalog tokens)?
4. **Account-level subscription.** Should a subscriber be able to follow all of an account's products in one call? v1 is per product; an account stream is a fan-in on the router.
5. **Principal visibility.** Omitting `principal` from non-maintainers is a privacy default. Does the metadata-catalog indexer, or any known consumer, need it?
6. **Anonymous subscription to public products.** Disallowed here to bound abuse. Is that too strict for the open-data mission?
7. **Read events for failed and refused requests.** Included for audit. Do they belong in the same table as successes, or in a separate one for abuse analysis?
8. **Limits.** Which thresholds does the usage DO publish, and who owns the follow-up ADR on enforcement?

## Implementation Sketch

Not part of the decision; for sizing only.

1. `src/events.rs`: build the envelope from the existing `RequestEvent` plus the response; `DeleteObjects` needs per-key results parsed from the response body (the batch handling is near `src/lib.rs:1030`) and `CopyObject` must emit on the destination key.
2. `wrangler.toml`: `OBJECT_EVENTS` Queue producer binding and `READ_EVENTS` Pipelines binding, per environment, beside `ANALYTICS`.
3. `workers/object-events`: Queue consumer, event-log and usage Durable Objects, dead-letter queue.
4. `/.events` route and its authorization, next to `/.sts`.
5. Pipelines stream, sink and pipeline plus R2 Data Catalog, defined in Terraform, with the table's partition spec and a retention job.
6. An ADR-015 monitor on emit failures and on router dead-letter depth.
