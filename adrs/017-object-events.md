# ADR-017: Object Events

**Status:** Proposed — draft for discussion
**Date:** 2026-11-21
**Depends on:** ADR-005, ADR-008, ADR-011, ADR-013, ADR-014, ADR-015, ADR-016
**Amends:** ADR-008 (adds a durable event path; Analytics Engine stays)

---

## Context

The proxy sees every read and write. It stores little of this. ADR-008 stores one sampled Analytics Engine row per request. That is enough for dashboards. It is not enough for four needs:

1. **Audit.** A complete, queryable record of who read or wrote each object. We set the retention period. ADR-008 said to add R2 logging if we need full detail or long retention. We now need both.
2. **Usage.** Bytes read and written per principal, close to real time. Analytics Engine is sampled and is not billing-grade.
3. **Analytics.** One event source for current and new analyses.
4. **Event-driven systems.** A GitHub pipeline writes `source/metadata-catalog`. Each write must trigger a reindex into our database. Today we can only poll a listing, or call the indexer from the pipeline. Neither lets a third party react to a product they can read.

**Terms.** A *principal* is the identity that signed a request: a person's identity ID, or a service account ID (ADR-008). A *write event* or *read event* describes one completed operation. The *router* is one Worker that reads the write-event Queue. The *product log* is one Durable Object per product that stores recent write events. The *audit table* is an Iceberg table of events. A *subscriber* is any program that pulls write events from `/.events`.

**Constraints.**

- The proxy sees only proxied operations. A write made directly to a backend bucket is invisible.
- Telemetry must not block a response (ADR-008).
- The request path must not share state between requests. Use Durable Objects or the router.

---

## Decision

### Two classes of event

| | Write events | Read events |
|---|---|---|
| Operations | `PutObject`, `CompleteMultipartUpload`, `CopyObject` (destination key), `DeleteObject`, each key of `DeleteObjects` | `GetObject`, `HeadObject` |
| Volume | Low (to be measured) | About 94% of requests |
| Audited | Every attempt, including 401, 403 and 5xx | Signed requests, every 401 and 403, and 1% of anonymous successes. No 404s. |
| Sent to subscribers | Successful ones only | Never |

A write *succeeds* when the status is 2xx. For `CopyObject` and `CompleteMultipartUpload`, the body must also contain no `<Error>`, because S3 can return 200 with an error body.

The proxy finds the operation from the method, the `uploadId` and `delete` query keys, and the `x-amz-copy-source` header. These operations send nothing: `UploadPart`, listings, `OPTIONS`, keyless writes, and any path that starts with `/.`. This matches the requests that skip Analytics Engine today.

Subscribers get no read events. A read feed shows who reads a product. ADR-008 rejected a per-request public feed for the same reason, and no use case needs it. The aggregate globe does not change.

Read audit is limited to signed requests because they carry the principal that usage tracking needs. Anonymous reads stay in Analytics Engine. The 1% sample and the 404 exclusion are proposals. Measure the signed share before we fix them (see Open Questions).

### Event shape

A write event is a CloudEvents envelope:

```json
{
  "specversion": "1.0",
  "id": "01JD…",
  "type": "coop.source.object.created",
  "source": "/source/metadata-catalog",
  "subject": "records/2026/11/21.parquet",
  "time": "2026-11-21T17:04:05.123Z",
  "data": { "op": "put", "etag": "\"d41d…\"", "size": 1048576,
            "principal": "acme--nightly-sync", "trace_id": "…" }
}
```

- `type` is `.created` or `.deleted`.
- The proxy creates `id` (a ULID) once, before it sends the event. A retry keeps the same `id`.
- `size` comes from `Content-Length` or `x-amz-decoded-content-length`. It is absent when neither header exists.
- `principal` is absent for anonymous requests. `trace_id` is `x-source-trace-id` (ADR-016).

An audit row is flat: `ts` (epoch ms), `type`, `account`, `product`, `key`, `op`, `status`, `principal`, `access_key_id`, `bytes_sent`, `range`, `etag`, `size`, `trace_id`, and `id` for writes. It has no `client_ip_hash` and no `country`. Those stay in Analytics Engine. This keeps the long-lived audit table free of the reversible hash described in ADR-008.

### Capture

The proxy builds each event after it knows the response status. Today `log_analytics` runs inline in `handle()`, and only the location broadcast uses `wait_until`. So this ADR needs new plumbing: pass `ctx` to the emit code and send inside `ctx.wait_until`.

- Every audited event goes to a Pipelines stream.
- A successful write event also goes to the `OBJECT_EVENTS` Queue.
- `DeleteObjects`: the proxy reads each `<Deleted>` key from the response body and sends events with `sendBatch` (100 messages per call).

**Delivery.** The proxy sends each event once. If the send fails, the proxy drops the event, logs a warning, and counts the failure. An ADR-015 monitor alerts on that count. The proxy does not wait for the Queue and does not fail the write. If it did, a Queue outage would stop object storage. After the Queue accepts an event, delivery is at-least-once.

So the audit table can miss events and can hold duplicates. Readers dedupe on `id`. The audit table is the most complete record we have. It is not a billing ledger.

### Router and product log

The router (`workers/object-events`) reads the Queue in batches. For each batch it appends events to the product log and updates the usage Durable Object. The Queue retries a failed batch. A dead-letter queue holds repeated failures. An ADR-015 monitor watches its depth.

The product log is a SQLite-backed Durable Object.

- Each event gets a sequence number. Numbers rise by one, are never reused, and survive pruning.
- An append is idempotent on `id`. A Queue redelivery does not add a second row.
- Order is append order, not write-completion order. Two writes to one key can swap. A subscriber compares `etag` and `time`, or sends `HEAD` on the key.
- An alarm deletes events older than 7 days (proposal). Each product keeps at most 5 million events (proposal).

### Subscription: `/.events`

```
GET /.events/{account}/{product}?after={seq}&limit=100&wait=25
```

- The cursor `after` is the sequence number of the last event the subscriber processed.
- If no newer event exists, the call waits up to `wait` seconds, then returns an empty list.
- If `after` is older than the oldest kept event, the call returns `410 Gone` with the oldest sequence number. The subscriber must then list the product.
- The front Worker holds the connection and checks the product log every 2 seconds. The Durable Object is not held open. A held request keeps a Durable Object awake and bills duration for every watched product.
- Limits: 10 concurrent waits per principal and a fixed cap per product (proposals). The Workers rate-limit binding adds a per-principal request limit. That binding counts per Cloudflare location and fails open. It is a soft bound.

`/.events` needs a new branch in the router, next to `/.sts`. Like other `/.` paths, it sends no analytics.

**Authorization.** Subscribing needs the permission that listing the product needs. The proxy authorizes it as `ListBucket` on the product, through the existing gateway path, so the same SigV4 credentials work. `ReadOnly` roles allow `ListBucket` (ADR-011), so they can subscribe. Service accounts (ADR-014) subscribe with the credentials they get from `/.sts`. A public product needs a signed caller. An anonymous caller is refused (proposal). A private product needs `read` permission.

**Privacy.** Events show only what a reader of the product can already list, plus the time. A `deleted` event shows the name of a key that listing no longer shows. The subscriber had read access to that key, so we accept this. The proxy knows only the `read` and `write` permissions. So `principal` appears only for subscribers that have `write` permission on the product. Events never carry an IP hash or a country.

**Contract.** A subscriber can miss events, get duplicates, and see swapped order. A subscriber that needs every change must also list the product on a schedule.

### Audit table

Pipelines writes the audit table (`object_events`) to R2 Data Catalog as Iceberg. Pipelines partitions the table by ingest day. That is documented. We found no documented way to partition by account or product, so this ADR does not rely on it.

- The sink rolls files every 300 seconds by default. With many active products the files are small. We enable catalog compaction and snapshot expiry.
- We set retention. We drop day partitions after 13 months (proposal).
- Pipelines gives exactly-once delivery from the stream to the sink only. The proxy-to-stream path is at-least-once, so readers dedupe.
- Catalog tokens apply to the whole bucket. We cannot give an owner a token for one product. Owners read their product's audit rows through a Source API endpoint. The API builds the product filter from the authorized product ID, never from user text. The API holds the catalog token. This endpoint is not yet designed.

### Usage tracking

A Durable Object per principal keeps month-to-date counters for each product.

- The router increments write counters from write events.
- A job reads new signed-read rows from the audit table every 15 minutes and adds read counters. It reads by ingest time and never rescans a day. The table holds only signed reads, so each run is small.
- Counters are approximate. They are not billing-grade.

This ADR tracks usage. It does not enforce limits. When a counter crosses a threshold, the Durable Object sends a `coop.source.usage.limit-reached` event. Enforcement needs its own ADR, because open data must stay freely readable (ADR-008).

### Example

1. A GitHub workflow assumes a service account (ADR-013, ADR-014) and writes `records/2026/11/21.parquet`.
2. The proxy sends a `created` event. The router appends it to the `source/metadata-catalog` product log as sequence 4821.
3. The indexer, a service account with `read` permission, calls `/.events/source/metadata-catalog?after=4820&wait=25`. It gets the event, reads the object, updates the database, and stores 4821.
4. Each night the indexer lists the product and indexes anything it lacks. This covers a dropped event.

An external subscriber follows the same steps for any product they can read.

---

## Cost and performance

ADR-015 reports 986 million requests in 30 days (about 380 per second). Uploads are about 2% of requests.

Pipelines billing has been live since 2026-08-03. Pricing: $0.04 per GB for transforms and $0.06 per GB for Iceberg sinks, with 50 GB of each included. Catalog operations cost $9 per million. Compaction costs $0.005 per GB. Cloudflare has renamed the product Basin Pipelines (general availability 2026-10-01).

| Design | Estimate per month |
|---|---|
| Audit every read, any status, CloudEvents envelope | about $175 (range $110–330) |
| This ADR: write events, signed reads, refusals, 1% anonymous | about $30–40 |
| `/.events` per watched product, short poll every 2 s | about $0.20 |
| `/.events` per watched product, held request | about $4 |

All figures are estimates. They assume 10 million write events a month, 550 bytes per read event, and that signed requests are about 10% of reads. The first and third assumptions are unmeasured. Count them in Analytics Engine before we build.

**Performance.** Each audited request adds about 0.1–0.3 ms of CPU. It adds no response latency, because sends run in `wait_until`. Work in `wait_until` stops 30 seconds after the response. One product gives about half of all read traffic today, mostly 404s (ADR-015). Excluding 404s removes most of that load.

---

## Consequences

**Benefits**

- One capture point feeds audit, usage and subscribers. A new target is a router change.
- We control audit retention.
- Subscribers use the credentials and authorization they already have.
- Pull with a client-held cursor means no per-subscriber server state.

**Costs and risks**

- Events can be missed or duplicated. Subscribers must list the product to be exact.
- The stream does not show direct writes to a backend bucket.
- Anonymous reads are not in the audit table.
- A write through a presigned URL has no principal, because multistore treats a presigned URL as anonymous (ADR-008). We have not confirmed that such a write can succeed.
- Usage counters are approximate.
- `/.events` is a new public surface to authorize, rate-limit and document.
- We depend on a renamed Cloudflare product whose old names are "deprecated over time".
- The audit table stores `principal` for 13 months. That is longer than ADR-008 keeps person-linked data.

---

## Alternatives considered

**R2 event notifications.** They cover only R2 products, miss the principal, and have no read events. They can cross-check R2 products later.

**A Queue per subscriber.** HTTP pull needs a Cloudflare API token. A token cannot be limited to one queue, so we cannot give one to external users.

**Webhooks first.** They need subscriber registration, secrets, retry policy, and protection against requests to internal addresses. Pull is a subset of that work. A webhook sender can read the product log later.

**Analytics Engine as the stream.** It is sampled and queried, not subscribed. It cannot carry exact per-key events.

**An external broker.** It adds a vendor and a second authentication system. Revisit if subscribers need exactly-once delivery or long replay.

**Audit every read.** It costs about five times more. Most of that cost buys anonymous 404 traffic that Analytics Engine already counts.

---

## Open questions

1. **Is dropping an event on send failure acceptable for the indexer?** An outbox in the request path closes the gap. It adds a hop to every write.
2. **What is the signed share of reads, and what is the write rate?** The read-audit scope and the cost table depend on both.
3. **Can the Pipelines sink partition by product?** If not, we keep day partitions and rely on compaction.
4. **Should anonymous callers subscribe to public products?** We refuse them to bound abuse. ADR-008 says open data stays freely readable.
5. **Are 13 months of audit retention and 7 days of replay right?** Does a policy set either?
6. **How do product owners read their audit rows?** An API endpoint is proposed. Is it enough?
