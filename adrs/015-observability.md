# ADR-015: Observability Sampling, Sinks and Alerting

**Status:** Accepted — implemented in part (#266)
**Date:** 2026-10-29
**Issue:** #265
**Depends on:** ADR-008
**Amends:** ADR-008 (Axiom as a sink; retention; operational logging)
**Implementation:** `wrangler.toml` `[observability]`; `monitoring/`; `.github/workflows/production.yml` (Apply Monitors); [source-cooperative/status.source.coop](https://github.com/source-cooperative/status.source.coop) (M4)

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

### Workers Issues is enabled in production

Issues is on for production only. The alert that turns Issues into notifications (M1, below) applies to every Worker in the account and can't be filtered, so Issues in staging or previews would page operators for test failures.

Issues groups uncaught exceptions, failed invocations, 5xx responses and `console.error` lines. It recovers what dropping invocation logs would otherwise lose: a record of invocations that fail outright, before the proxy can log anything. It doesn't count 4xx responses, which are ~41% of production traffic.

Issues is free during the open beta. It is listed under the shared allowance, so afterwards it will likely be billed by volume. At ~5k occurrences a day that is between ~$0.20 and ~$2 a month, depending on occurrence size, which Cloudflare doesn't publish.

### Axiom is dropped; Cloudflare's own storage is the only sink

The `axiom-logs` and `axiom-traces` destinations are removed from `wrangler.toml`. Cloudflare's Observability now provides SQL queries, dashboards and alerts over the same data, and adding Axiom back would only duplicate them. Retention is Cloudflare's 7 days. Cloudflare has announced configurable 30-, 90- and 365-day retention; that, not a second vendor, is the way to keep more history if it's ever needed.

The destination objects live at the account level and must be deleted in the dashboard. `wrangler.toml` only stops referencing them.

### Staging captures everything

Staging samples logs, invocation logs and traces at 100%. At ~2M requests a month that is ~5 GB, and full fidelity is worth having where integration tests and pre-release changes run.

### `public-log-stream` gets no Workers observability

The live-globe Worker isn't critical infrastructure and stays without an `[observability]` block. Cloudflare's built-in Worker metrics are free and already show its request and error counts.

### Tracing across source.coop and the proxy is wanted but not yet possible

We'd like to follow a request across both applications in one trace. Traffic crosses between them in both directions:

- `source.coop`'s server calls the proxy for object browsing and `/.sts`;
- the proxy calls the Source API on `source.coop` whenever its cache misses.

A failure on one side, such as the Source API rejecting the proxy, is often only explained by the other.

Two Cloudflare limits stand in the way:

- **Workers traces don't link to outside traces.** Cloudflare lists W3C trace-context propagation between Workers and services outside Cloudflare as a known limitation, planned but not available. The proxy's spans can't join a trace that started on Vercel, or pass one on to the Source API.
- **Cloudflare can't receive outside traces.** Cloudflare Observability exports OpenTelemetry but documents no way to receive it, so Vercel's spans can't be stored next to the proxy's. A small Worker could accept them and write them as log lines: searchable by trace ID, but with no combined trace view, and with ingestion code for us to maintain. One combined view would instead need a third-party tool receiving traces from both sides, which this ADR moves away from.

The interim plan is to share a trace ID rather than a trace:

- `source.coop` adds `@vercel/otel`, so Next.js sends and continues W3C `traceparent` headers;
- the proxy passes the incoming `traceparent` on to its Source API calls, adds the trace ID to its WARN/ERROR lines, and returns it to clients as `x-source-trace-id`.

A trace in Vercel then covers the page and the API calls, with the proxy as a gap whose log lines carry the same ID. The two sides also sample independently: the proxy traces 1% of requests at random, so a Vercel trace rarely has proxy spans. Its WARN/ERROR lines are always kept, so failures can always be looked up by ID.

Revisit when Cloudflare ships trace-context propagation for Workers. The proxy's spans would then join the trace directly, and choosing one place to view both sides becomes a real decision.

### Alerts are defined in the repository and applied on release

Alerts are Cloudflare notification policies, defined in `monitoring/monitors.json` and applied by `monitoring/apply.py` when a release deploys. The outside uptime check, which must not depend on Cloudflare, runs from a separate repository, `status.source.coop`, using Upptime, which also publishes the public status page. The [Monitors](#monitors) section lists each alert, what triggers it and how it is deployed.

---

## Monitors

### How alerts reach operators

Every alert posts to a Slack channel through one Cloudflare webhook destination, **#devops (Slack)**, whose URL is the `ALERTS_SLACK_WEBHOOK_URL` organization secret. The account's budget alert uses the same destination. Alerts have one of two severities:

- **page:** users are affected now. Slack, plus email to the addresses in the `ALERTS_EMAIL` variable.
- **notify:** something looks wrong. Slack only.

An alert does not repeat for every failed request:

- **Cloudflare policies** notify when they trigger and again when they clear (`ALERT_STATE_EVENT_START` and `…_END` in the webhook payload). SQL-based Monitors also resend while still triggered, every `alert_interval`. Cloudflare's default is `1h`; set it in each entry, `1h` for page and `24h` for notify. `apply.py` doesn't derive it from `severity`.
- **Issues alerts** fire once per new issue group, not per occurrence. A crash loop on one bug sends one message; a recurrence after the issue is resolved sends another.
- **Upptime (M4)** opens an issue in `status.source.coop` and notifies Slack when a check goes down, then closes it and notifies again on recovery. It also notifies when a check is slower than its limit.
- **Muting:** during a known incident or maintenance, mute alerts with a Silence (dashboard: **Alerts › Silences**, or `alerting/v3/silences`).

### The alerts

`monitors.json` defines seven Cloudflare policies of two kinds, which the dashboard lists in different places. The five **SQL Monitors** (`custom_analytics`: M2, M3, M6, M7, M8) appear under **Alerts › Monitors**. The two **Platform Notifications** (M1 and M5, Cloudflare's built-in alert types) appear under **Alerts › Platform Notifications**. M4 is not a Cloudflare policy at all.

| | Alert | Severity | Watches | Fires when | Kind and location |
|---|---|---|---|---|---|
| M1 | Worker issues | page | Workers Issues (`workers_observability_real_time_issue`) | a new issue group opens | Platform Notification, `monitors.json` |
| M2 | Error-rate SLO | page | Analytics Engine: requests with `double2 >= 500` against all requests | under 99.9% success over both the last 10 min and the last hour, with at least 1,000 requests (today: ~99.99%) | SQL Monitor, `monitors.json` |
| M3 | Access denied | notify | Analytics Engine: share of requests answering 403 | above 0.5% over 15 min (2026-11-11 to 14: at most 0.35% in any 15 min) | SQL Monitor, `monitors.json` |
| M4 | Outside uptime check and status page | page | Upptime in `status.source.coop`, every 5 min from GitHub: the website, the proxy, a real object read, the proxy's OIDC discovery document and Ory sign-in | a check fails, returns an unexpected body, or exceeds 5 s | Upptime, `status.source.coop` |
| M5 | Cloudflare incidents | notify | Cloudflare's incident notifications (`incident_alert`), major and critical only | Cloudflare declares an incident | Platform Notification, `monitors.json` |
| M6 | Credential exchange failures | page | Workers Logs (`logs.workersLogs`): WARN/ERROR lines on `/.sts`, excluding `rate_limited` | more than 50 in 10 min (today: none) | SQL Monitor, `monitors.json` |
| M7 | Latency | notify | Analytics Engine: p99 of `double3` (`duration_ms`) for GET and HEAD requests, weighted by `sampleInterval` | above 5 s over 15 min (2026-11-11 to 14: median ~0.9 s, 95th percentile ~1.1 s) | SQL Monitor, `monitors.json` |
| M8 | Analytics rows | notify | Analytics Engine row count | fewer than 50,000 rows in 15 min (2026-11-11 to 14: ~530,000 on average, never below ~91,000) | SQL Monitor, `monitors.json` |

M2, M3, M6, M7 and M8 are SQL Monitors (`custom_analytics`). Cloudflare allows at most five per account, counting any made in the dashboard, and `apply.py` refuses a file with more. Those five slots went to the alerts nothing else replaces: a sixth, M8 Log volume (proxy log lines in Workers Logs), was dropped, so a runaway in billable log lines is caught only by the account's billing budget alert, which also posts to `#devops (Slack)`. Each runs one query on a schedule and compares a single number, so none of them can break results down by product or by Cloudflare location. M2, M3 and M7 query Analytics Engine, which deliberately skips `/.well-known` and `/.sts` (ADR-008); M6 exists because of that gap. Thresholds are set from production values measured on 2026-11-14 (the "today" figures above) and replayed against 2026-11-11 to 14 (the dated figures); revisit them as traffic changes.

M3 counts only 403s. 404s are routine for some products: on 2026-11-14 one product answered 80% 404s across ~710k requests an hour, presumably clients probing for keys that don't exist, and it alone made up most of the overall ~28% 403/404 share. 403s are almost absent (at most 0.35% of requests in any 15 minutes from 2026-11-11 to 14), so a single product being refused shows up overall once it carries more than ~0.5% of traffic. Grouping by product (`index1`) was considered and rejected: per-product 404 shares range from 0% to over 99% in normal operation, so no fixed per-product threshold works.

M7 counts only GET and HEAD. `duration_ms` stops when the gateway returns its response, before a streamed body is sent, so for a read it measures the proxy's work plus the backend's time to first byte, not the object's size or the client's connection. An upload is different: its body streams from the client to the backend inside that window, and the backend answers only once it has all of it, so a PUT's duration grows with its size and the client's upload bandwidth. PUT's p99 is ~6.5 s in normal operation (p50 ~2.7 s), and although uploads are only ~2% of requests, they set the overall p99. Measured over all methods, p99 crossed 5 s in about 20 separate episodes from 2026-11-11 to 14, all but one ordinary upload traffic. Measured over GET and HEAD, it crossed 5 s only during the 2026-11-11 09:30–10:15 UTC incident, which M2 also caught. A slow upload backend therefore goes unalerted unless it also fails requests.

The account's existing billing budget alert is left alone; it isn't a `data-proxy:` policy. It covers all of the account's spend, including Workers request charges, so it's only a coarse signal for telemetry cost, but it is the only one now that log volume has no Monitor.

### What triggers them

**A bug stops the Worker from running.** A panic, an error passed out of `fetch`, or a CPU or memory limit leaves no Analytics Engine row, because the row is written when the response is ready (`src/lib.rs`). A panic still reaches the logs as an ERROR line through `console_error_panic_hook`, and Issues records every failed invocation. **M1** fires with the stack trace. M2 can't see this case, and reads *healthier* while it lasts. If no request succeeds, **M4** fires too.

**A bug returns a 5xx.** The response has an Analytics Engine row and a WARN line carrying the request and relayed upstream headers. **M1** fires (Issues counts 5xx), and **M2** fires if enough requests are affected.

**Vercel's Source API is down or unreachable.** Source API calls fail and the proxy answers 500 (`Internal: API returned 503 for …`), logging the cause at WARN. Answers are cached for 60 s (permissions) to 5 min (products and connections) per Cloudflare location, so the error rate climbs as caches expire. **M1** and **M2** fire, and **M7** may as well, since every cache miss gets slower.

**Vercel rejects the proxy.** If the Source API answers 401 or 403, for example after a signing-key mismatch, the proxy returns 403 `AccessDenied`; a 404 for everything becomes `NoSuchBucket`. Neither is a 5xx or an Issue. A 401/403 from the Source API fires **M3**: every product's requests turn to 403 at once. A 404 for everything does not, because M3 leaves out 404s. Logging unexpected Source API 401/403/404 answers at ERROR would let M1 catch this directly.

**AWS refuses the role.** If the IAM role behind a data connection is deleted, or its trust policy no longer admits the proxy, AWS STS returns `AccessDenied`. The proxy logs ERROR "backend STS rejected the federation exchange" with the STS code and answers 403. Other STS errors, such as the OIDC provider being removed from AWS, answer 502. **M1** fires either way. Credentials are cached until they expire, so this can show up to an hour after the change.

**The role loses its S3 permissions, or a public bucket is made private.** AWS still issues credentials, so S3 itself answers 403, and the proxy relays it unchanged. There is no log line and no Issue. **M3** fires if that product carries more than ~0.5% of traffic, because 403s are otherwise almost absent. A product with less traffic than that goes unalerted. Listing requests go through the storage library instead and probably surface as 503, which also fires M1.

**One Cloudflare location fails.** Anycast routing sends clients to the next location, so no alert fires and users rarely notice. If a location stays up but breaks, for example its outbound DNS fails (the 530s fixed in #212), the proxy returns 5xx carrying a `cf-ray` rather than `server: AmazonS3`, and **M1** and **M2** fire. Analytics Engine doesn't record the location, so the WARN lines are what point to one.

**Cloudflare or Workers is down.** Nothing inside Cloudflare can report it. **M4** fires from GitHub and marks it on the public status page, and **M5** says why.

**The sign-in service or a platform identity provider fails.** Users and the CLI can't get credentials from `/.sts`, though public reads keep working. **M6** fires.

**Slow but not failing.** S3 in one region slows down, or a deploy adds contention on a hot path. **M7** fires.

**Monitoring breaks or overspends.** If the Analytics Engine binding stops writing after a config change, M2, M3 and M7 go quiet rather than alerting, and **M8** fires. A flood of junk `/.sts` keys that logs a WARN per request has no dedicated alert: it shows up in spend, and in the billing budget alert.

Two signals no alert covers:

- **A response that breaks mid-stream** has already recorded a 200 in Analytics Engine; whether Issues counts the failure is unverified.
- **The Cloudflare location of a failure** isn't recorded. Adding a `colo` field to the Analytics Engine row would let M2 and M3 point at one.

### How monitors are defined

`monitoring/monitors.json` lists every Cloudflare policy this repository owns. Each entry is the policy's API body (`name`, `alert_type`, `description`, `filters`, and optionally `enabled` and `alert_interval`) plus a `severity`. The script turns `severity` into delivery targets.

Every name starts with `data-proxy: `. That prefix marks a policy as owned by the file: `apply.py` creates the missing ones, updates those that differ from the file (comparing only the fields the file sets), leaves matching ones alone and deletes owned policies no longer listed. Policies without the prefix are never touched.

SQL Monitors (`custom_analytics`) put their query, data source, detection method, comparison operator, windows and run interval in `filters`. The public API schema documents only some of those filters, and the format of `selectors`, which holds the query, is undocumented. So each SQL Monitor starts in the dashboard (**Alerts › Monitors**) and is then exported into the file. Name the dashboard draft *without* the `data-proxy: ` prefix: a prefixed policy that isn't yet in `monitors.json` is deleted by the next release.

```sh
# Prints the dashboard-made policy as a monitors.json entry, named with the
# data-proxy: prefix. Set its severity, add it to the file, then delete the
# dashboard original: the next release creates the owned copy.
python3 monitoring/apply.py export "M2 Error-rate SLO"
```

From then on the file is the source of truth, and edits made in the dashboard are overwritten on the next release. Terraform can take over once its provider, which is generated from the same schema, can express these filters.

`python3 monitoring/test_apply.py` checks the plan logic and validates the file offline; CI runs it on every pull request.

### How monitors are deployed

`production.yml` runs on every published release. After the deploy job succeeds, the **Apply Monitors** job runs `python3 monitoring/apply.py apply` in the `production` environment. It:

1. creates or updates the Slack webhook destination;
2. creates, updates or deletes the `data-proxy: ` policies to match `monitors.json`.

Re-running it with an unchanged file changes nothing. To preview a run locally, use `python3 monitoring/apply.py plan`, which only reads.

The job needs:

- **Account › Notifications › Edit** added to the existing deploy token, `CLOUDFLARE_API_TOKEN`. That token can already deploy the production Worker and replace its secrets, so a separate alerts-only token would add a secret to manage without reducing what a leaked token could do;
- the `ALERTS_SLACK_WEBHOOK_URL` repository secret, a Slack incoming webhook;
- optionally, the `ALERTS_EMAIL` variable (comma-separated) for page-severity email.

The uptime check (M4) lives outside this repository, in [`source-cooperative/status.source.coop`](https://github.com/source-cooperative/status.source.coop), which runs [Upptime](https://upptime.js.org). Its `.upptimerc.yml` lists the checks:

| Check | URL | What it proves |
|---|---|---|
| Website | `https://source.coop/` | Vercel serves the app |
| Data proxy | `https://data.source.coop/` | Cloudflare runs the Worker; this needs no upstream service |
| Data reads | `https://data.source.coop/radiantearth/landcovernet/README.md` | the whole read path: Source API, AWS credentials, S3 |
| Credential exchange | `https://data.source.coop/.well-known/openid-configuration` | what S3 clients and the CLI discover before `/.sts` |
| Sign-in | `https://auth.source.coop/.well-known/openid-configuration` | Ory answers |

Upptime runs from GitHub Actions every 5 minutes. It records response times in the repository and publishes `status.source.coop` from its `gh-pages` branch. That gives users a public status page and history that need nothing from Cloudflare, Vercel or AWS.

It posts to the same Slack channel through its own `NOTIFICATION_SLACK_WEBHOOK_URL` secret. Changing what it checks is a pull request to that repository; nothing in this repository deploys it.

Two GitHub limits apply: scheduled workflows can start a few minutes late, and they are disabled after 60 days without repository activity. Upptime's own commits count as activity, so the second doesn't arise in practice.

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
- **Log volume is unbounded.** At 100% sampling, log bytes follow the number of `WARN` lines, not traffic. An incident that fails every request, or a flood of refused `/.sts` exchanges (each refusal, including `rate_limited`, logs a `WARN`), would be billed line for line. No monitor watches log volume (see the five-Monitor limit under [The alerts](#the-alerts)); if the billing budget alert or the dashboard shows it, drop the log rate during such an incident.
- **Monitors and Issues are both beta.** Their API shape and pricing can change under us.

---

## Alternatives Considered

**Keep invocation logs and sample all logs at 1%** — rejected. It costs about the same as this design but keeps only 1 in 100 of the `WARN` lines that explain a 5xx. Logs can't be sampled differently by type, and our own lines are the part worth keeping.

**Fix the Axiom credentials and keep exporting** — rejected for now. At the new volumes the export would cost only ~$7 a month, so cost isn't the reason. It duplicates what Cloudflare now does natively and adds a second place for data to go stale. Revisit if retention beyond Cloudflare's offering becomes a requirement.

**Export only, with `persist = false`** — rejected. It saves Cloudflare storage but gives up Cloudflare's dashboards, Issues and alerts, and makes a third-party export a hard dependency.

**Terraform for Monitors** — deferred. `cloudflare_notification_policy` is generated from a schema that doesn't yet document the `custom_analytics` filters. Nothing in this repository uses Terraform today, either.

**A scheduled Worker that checks Analytics Engine and posts to a webhook** — rejected. It would be fully defined in code, but it rebuilds thresholds, anomaly detection, deduplication and recovery notifications that Monitors already provide.
