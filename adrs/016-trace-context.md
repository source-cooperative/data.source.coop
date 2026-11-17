# ADR-016: Trace Context Across source.coop and the Proxy

**Status:** Accepted — implemented
**Date:** 2026-11-13
**Depends on:** ADR-008
**Related:** ADR-015 (observability sampling, sinks and alerting; in review as #266)
**Implementation:** `src/trace.rs`, `src/lib.rs` (`fetch`), `src/source_api/auth.rs` and `cache.rs`; source.coop `src/instrumentation.ts` and `src/lib/actions/proxy-credentials.ts`

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

The app sends `traceparent` to the data proxy and nowhere else; third parties such as Ory and AWS don't receive it. The instrumentation's `propagateContextUrls` is set to the proxy's endpoint. Despite its name, the instrumentation covers `node:http` and `node:https` as well as `fetch`, so the S3 SDK's requests carry the header too. It's added after SigV4 signing, so it's never part of the signature, and nothing done to it in transit can fail a signed request.

Tracing records each outgoing request's full URL. So nothing secret may travel in a URL to the proxy: source.coop sends the `/.sts` exchange's `WebIdentityToken` (the user's Ory ID token) in a form-encoded POST body, which the proxy has accepted since #196, never in the query string.

### The proxy continues it

For every request, the proxy takes the trace from a well-formed `traceparent`. When the header is absent or malformed, it starts a new, unsampled trace. Then it:

- **returns** the trace ID as `x-source-trace-id` on every response, alongside `x-request-id` (the `cf-ray`), so a client or a bug report can quote both;
- **logs** it as `trace_id` on the WARN line written for every 5xx, next to `request_id`;
- **sends** a child `traceparent` with every Source API call, with the same trace ID, a new parent span ID per call and the caller's sampled flag. Each call already sends `x-request-id`.

The sampled flag passes through unchanged, so the API records spans for a trace exactly when its caller does, and a page source.coop traces is followed through the proxy into the API. A trace the proxy starts itself, for a direct client such as a browser or the CLI, is unsampled.

The header is untrusted input. It chooses the ID, which nothing relies on for access, and whether the API records the trace. A malformed header is replaced rather than repaired.

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
- **A caller can make the API record its trace.** Any client can send a sampled `traceparent`, and the API then records spans for the calls that request causes. It adds no API calls, only spans for calls that happen anyway, and those are bounded by the proxy's 60 s–5 min API cache. At worst, Vercel records a trace for every API call the proxy makes. Honoring the flag only from source.coop would need a way to authenticate source.coop's server, which anonymous reads don't have, and dropping the flag would lose the page → proxy → API trace.
- **source.coop's traces record every outgoing URL.** That's why the `/.sts` token moved to the request body. Ory's single-use login and consent challenges still travel in URLs during a credential refresh and so reach traces. They are spent within milliseconds, so the exposure is accepted, but any new request that carries a credential must keep it out of the URL.
- **Vercel's sampling decides what the API records.** A proxy-started trace is unsampled, so an API failure caused by a direct client has no Vercel spans. It still has the proxy's 5xx line and `x-request-id`.

---

## Alternatives Considered

**Send Vercel's traces to Cloudflare** — not possible. Cloudflare Observability exports OpenTelemetry but has no ingestion endpoint. A Worker that receives OTLP and writes each span as a log line would make spans searchable in Cloudflare, but not viewable as traces, and it would be ingestion code to maintain.

**A third-party tracing backend for both** (Honeycomb, Grafana Cloud) — deferred. It's the only way to get one trace waterfall view, and it reintroduces the external sink ADR-015 removed. Revisit once the proxy's spans can join an external trace; until then the backend would show the same gap.

**Log `trace_id` on every request** — rejected. At ~33M requests a day, a WARN line per request would undo ADR-015's log budget for a field only failures need.

**Start proxy-originated traces sampled** — rejected for now. It would record Vercel spans for every API call the proxy makes for direct clients, a volume nobody has priced.
