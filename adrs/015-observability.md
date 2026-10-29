# ADR-015: Observability Sampling, Sinks and Alerting

**Status:** Accepted — implemented in part (#266)
**Date:** 2026-10-29
**Issue:** #265
**Depends on:** ADR-008
**Amends:** ADR-008 (Axiom as a sink; retention; operational logging)
**Implementation:** `wrangler.toml` `[observability]`

---

## Context

From 2026-12-01 Cloudflare bills Workers logs and traces by volume rather than by event ([announcement](https://blog.cloudflare.com/one-observability-platform/), [pricing](https://developers.cloudflare.com/observability/pricing/)). A paid account gets 50 GB ingested and 10 GB-month stored per billing cycle. Beyond that it pays $0.25 per GB ingested, measured uncompressed, and $0.10 per GB-month stored. Logs and traces are kept for 7 days.

Measured in production from 2026-09-29 to 2026-10-28, with logs and traces both head-sampled at 25%:

| | |
|---|---|
| Requests | 986M per 30 days (~33M/day) |
| Logs stored | ~42 GB/day at ~5.2 KB per event. **~99.9% are invocation logs**, ~37.8M a day when scaled up from the sample. |
| The proxy's own log lines | ~29k `WARN` and ~60 `ERROR` per day across all traffic |
| Traces stored | ~80 GB/day at ~1.6 KB per span, **~6 automatic spans per request** |
| Total | ~122 GB/day, ~3.7 TB/month |
| Staging | ~2M requests, ~1.3 GB of telemetry per 30 days |
| 5xx responses | ~4.7k/day (33,100 over 7 days, from Analytics Engine) |

All figures are from Cloudflare's own reporting for the account: request counts from the Worker metrics, telemetry bytes from Workers Observability usage, and the 5xx count from the Analytics Engine dataset. Observability usage records sizes only from 2026-10-06, so byte figures average 2026-10-07 to 2026-10-28. #266 lists the exact queries.

Kept unchanged, that configuration would cost about **$990 a month**: ~$900 for ingestion and ~$85 for storage. Nearly all of it pays for two things we don't rely on:

- **Invocation logs** are one record per request with its URL, status, timing and the `cf` object. They duplicate the request event ADR-008 already writes to Analytics Engine, unsampled, for every product request.
- **Automatic trace spans** at a quarter of all traffic. A request's shape doesn't change between a 1-in-4 sample and a 1-in-100 sample.

The log lines we actually debug with — the per-5xx `WARN` that inlines the span fields and relayed upstream headers (ADR-008, Structured Operational Logging) — are a few hundred MB a month. Sampling at 25% has been throwing away three-quarters of them.

**The Axiom export has failed since 2026-10-05.** Both destinations report "Destination delivery failed. Check the receiver configuration and credentials." Nothing noticed for 24 days, which is the best evidence that nothing depends on it.

---

## Decision

### Analytics Engine is the record of every request

The ADR-008 request event — status, duration, bytes, path, product, method, range — is the source for traffic, error-rate and latency questions. Workers logs and traces only carry what Analytics Engine can't: the context behind a failure and the inside of an individual request.

### Logs: every line we write, no invocation logs

Production logs are sampled at 100% with `invocation_logs = false`. Every `WARN` and `ERROR` the proxy emits is kept, where 25% sampling previously dropped three-quarters of them. The per-request invocation records are dropped because they duplicate Analytics Engine.

Head sampling decides per request before the outcome is known, so it can't keep "only the failures". Turning off invocation logs and keeping our own lines achieves that, because the proxy only logs at `WARN` and above in production when something is wrong.

### Traces: 1% baseline

Production traces are sampled at 1%, about 330k traced requests a day. That's ample for understanding latency and what a request is made of.

To trace a particular request, the intended mechanism is a zone **Trace Rule** that samples 100% of requests carrying a debug header. `coolnewgeo.com` is on the Free plan, and whether Trace Rules are available there is unverified.

### Query strings are redacted

`redact_query_string = true` in every environment, including PR previews (`wrangler.preview.toml`). Traces capture full URLs, and presigned requests carry `X-Amz-Signature` and `X-Amz-Credential` in the query string; without redaction both would sit in telemetry for its retention period. We lose the list parameters (`list-type`, `prefix`, `continuation-token`) in traces. Analytics Engine never recorded them either.

### Workers Issues is enabled

Issues groups uncaught exceptions, failed invocations, 5xx responses and `console.error` lines. It recovers what dropping invocation logs would otherwise lose: a record of invocations that fail outright, before the proxy can log anything. It doesn't count 4xx responses, which are ~41% of production traffic.

Issues is free during the open beta. It is listed under the shared allowance, so afterwards it will likely be billed by volume. At ~5k occurrences a day that is between ~$0.20 and ~$2 a month, depending on occurrence size, which Cloudflare doesn't publish.

### Axiom is dropped; Cloudflare's own storage is the only sink

The `axiom-logs` and `axiom-traces` destinations are removed from `wrangler.toml`. Cloudflare's Observability now provides SQL queries, dashboards and alerts over the same data, and adding Axiom back would only duplicate them. Retention is Cloudflare's 7 days. Cloudflare has announced configurable 30-, 90- and 365-day retention; that, not a second vendor, is the way to keep more history if it's ever needed.

The destination objects live at the account level and must be deleted in the dashboard. `wrangler.toml` only stops referencing them.

### Staging captures everything

Staging samples logs, invocation logs and traces at 100%. At ~2M requests a month that is ~5 GB, and full fidelity is worth having where integration tests and pre-release changes run.

### `public-log-stream` gets no Workers observability

The live-globe Worker isn't critical infrastructure and stays without an `[observability]` block. Cloudflare's built-in Worker metrics are free and already show its request and error counts.

### Alerts are Monitors, defined in the repository

Alerts run as Cloudflare **Monitors** (beta): a scheduled SQL query against `source_data_proxy_production` with threshold, anomaly or SLO detection. The first two are:

- the 5xx rate, as an SLO on `double2`;
- p99 `duration_ms`.

Through the API, a Monitor is a notification policy with `alert_type = "custom_analytics"`. Its query, source, detection method, operator, windows and run interval are all policy `filters`. The public OpenAPI schema documents only some of those filters, and the Terraform provider is generated from that schema, so Terraform can't fully express a Monitor yet.

Monitors are therefore defined as policy JSON checked into the repository and applied with the alerting API from CI. The first Monitor is created in the dashboard and then exported, because the format of the `selectors` filter isn't documented. They can move to Terraform once the schema documents the remaining filters.

---

## Consequences

**Benefits**

- Projected telemetry cost falls from ~$990 to **~$15 a month**: ~103 GB ingested (traces ~96, our logs ~2, staging ~5) and ~24 GB-month stored.
- Every 5xx keeps its diagnostic `WARN` line, rather than one in four.
- Request signatures no longer reach telemetry storage.
- Failures surface as grouped Issues and as alerts, instead of waiting for someone to look.
- One telemetry vendor. A broken export can no longer fail silently for weeks.

**Costs / Risks**

- **No per-request invocation records.** Questions about a specific request that never logged anything go to Analytics Engine, which has the request but not the `cf` object or headers, or to its 1-in-100 trace if it was sampled.
- **Retention is 7 days and still set by the vendor.** ADR-008's position that retention isn't owned here still holds, now for a single sink.
- **Analytics Engine is unbilled today, and this design leans on it harder.** If billing starts at the published rates, ~986M writes a month cost ~$244 a month. At that point, revisit sampling at the binding or by product.
- **Traffic growth scales trace volume linearly.** At about 2× today's traffic, traces alone exceed the included 50 GB by enough to matter. Lower the trace rate before raising the budget.
- **Log volume is unbounded.** At 100% sampling, log bytes follow the number of `WARN` lines, not traffic. An incident that fails every request, or a flood of refused `/.sts` exchanges (each refusal, including `rate_limited`, logs a `WARN`), would be billed line for line. Alert on log ingestion, and drop the log rate during such an incident.
- **Monitors and Issues are both beta.** Their API shape and pricing can change under us.

---

## Alternatives Considered

**Keep invocation logs and sample all logs at 1%** — rejected. It costs about the same as this design but keeps only 1 in 100 of the `WARN` lines that explain a 5xx. Logs can't be sampled differently by type, and our own lines are the part worth keeping.

**Fix the Axiom credentials and keep exporting** — rejected for now. At the new volumes the export would cost only ~$7 a month, so cost isn't the reason. It duplicates what Cloudflare now does natively and adds a second place for data to go stale. Revisit if retention beyond Cloudflare's offering becomes a requirement.

**Export only, with `persist = false`** — rejected. It saves Cloudflare storage but gives up Cloudflare's dashboards, Issues and alerts, and makes a third-party export a hard dependency.

**Terraform for Monitors** — deferred. `cloudflare_notification_policy` is generated from a schema that doesn't yet document the `custom_analytics` filters. Nothing in this repository uses Terraform today, either.

**A scheduled Worker that checks Analytics Engine and posts to a webhook** — rejected. It would be fully defined in code, but it rebuilds thresholds, anomaly detection, deduplication and recovery notifications that Monitors already provide.
