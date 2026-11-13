# ADR-016: Trace Context Across source.coop and the Proxy

**Status:** Accepted — implemented
**Date:** 2026-11-13
**Depends on:** ADR-008
**Related:** ADR-015 (observability sampling, sinks and alerting; in review as #266)
**Implementation:** `src/trace.rs`, `src/lib.rs` (`fetch`), `src/source_api/auth.rs` and `cache.rs`; source.coop `src/instrumentation.ts` and `src/lib/storage/s3.ts`

---

## Context

Requests cross between the two applications in both directions:

- **source.coop → proxy.** The app's server lists and reads objects through the proxy with the S3 SDK to render product pages, and exchanges credentials at `/.sts`.
- **Proxy → source.coop.** On every cache miss the proxy asks `/api/v1` for products, permissions, data connections, key standings and trusts.

Each side records what it does in its own place: the proxy in Cloudflare Workers Observability, the app in Vercel. Until now, nothing tied a page render on Vercel to the proxy requests it made, or a proxy request to the API calls it caused. A failure that crossed the boundary was matched up by timestamp.

The standard for joining the two is W3C Trace Context: a `traceparent` header carrying a trace ID, which each hop continues. Next.js supports it through OpenTelemetry. Workers tracing does not yet: Cloudflare lists propagation to services outside Cloudflare as a known limitation, so the proxy's automatic spans can neither join an incoming trace nor be joined by the API's. Sending Vercel's spans to Cloudflare isn't possible either; Cloudflare exports OpenTelemetry but does not ingest it.

---

## Decision

**One trace ID per request, carried across both applications, even though the proxy's own spans can't join it yet.**

### source.coop starts and propagates the trace

source.coop registers OpenTelemetry through `@vercel/otel`. Each request the app serves gets a trace, and an incoming `traceparent`, such as the proxy's, is continued.

The app sends `traceparent` to the data proxy and nowhere else. Third parties such as Ory and AWS don't receive it:

- `fetch` calls get it through the instrumentation's `propagateContextUrls`, set to the proxy's endpoint.
- The S3 client gets it from an SDK middleware, because the SDK sends through `node:http`, which `@vercel/otel` doesn't instrument. The middleware runs after SigV4 signing, so the header is never signed and nothing done to it in transit can fail a request.

### The proxy continues it

For every request, the proxy takes the trace from a well-formed `traceparent`. When the header is absent or malformed, it starts a new, unsampled trace. Then it:

- **returns** the trace ID as `x-source-trace-id` on every response, alongside `x-request-id` (the `cf-ray`), so a client or a bug report can quote both;
- **logs** it as `trace_id` on the WARN line written for every 5xx, next to `request_id`;
- **sends** a child `traceparent` with every Source API call, with the same trace ID, a new parent span ID per call and the caller's sampled flag. Each call already sends `x-request-id`.

The sampled flag passes through unchanged, so the API records spans only for traces its caller records. A trace the proxy starts itself, for a direct client such as a browser or the CLI, is unsampled. That keeps Vercel's trace volume tied to Vercel's own traffic rather than to every proxy cache miss.

The header is untrusted input, and only ever chooses an ID that nothing relies on for access. A malformed one is replaced rather than repaired.

### Finding a request on both sides

- **From a trace in Vercel:** its trace ID appears as `trace_id` on any proxy 5xx line.
- **From a proxy log line:** every WARN line carries `request_id`, and the response to that request carried both IDs.
- **From a user report:** the `x-source-trace-id` and `x-request-id` they quote find the request in Vercel and in Cloudflare respectively.

---

## Consequences

**Benefits**

- A failure that crosses the boundary can be followed by ID rather than by timestamp.
- Nothing new is logged per request. The trace ID rides on the 5xx line that already exists, so ADR-015's log budget is unchanged.
- When Cloudflare ships W3C propagation for Workers, the proxy's own spans can join the same trace without changing what source.coop sends.

**Costs / Risks**

- **The proxy is a gap in every trace.** Its own spans keep Cloudflare's trace IDs, not this one, until Workers supports external propagation. Within the proxy, the trace ID is in its logs, not its traces.
- **Only 5xx lines carry `trace_id`.** A 403 or 404 is found through `request_id` and the response headers, not through a log search by trace ID.
- **A caller can choose its trace ID.** That can mislead someone correlating, but grants nothing.
- **Vercel's sampling decides what the API records.** A proxy-started trace is unsampled, so an API failure caused by a direct client has no Vercel spans. It still has the proxy's 5xx line and `x-request-id`.

---

## Alternatives Considered

**Send Vercel's traces to Cloudflare** — not possible. Cloudflare Observability exports OpenTelemetry but has no ingestion endpoint. A Worker that receives OTLP and writes each span as a log line would make spans searchable in Cloudflare, but not viewable as traces, and it would be ingestion code to maintain.

**A third-party tracing backend for both** (Honeycomb, Grafana Cloud) — deferred. It's the only way to get one trace waterfall view, and it reintroduces the external sink ADR-015 removed. Revisit once the proxy's spans can join an external trace; until then the backend would show the same gap.

**Log `trace_id` on every request** — rejected. At ~33M requests a day, a WARN line per request would undo ADR-015's log budget for a field only failures need.

**Start proxy-originated traces sampled** — rejected for now. It would record Vercel spans for every API call the proxy makes for direct clients, a volume nobody has priced.
