# Streamed-PUT failure reproducer

Minimal Workers reproducer for the write-path failures behind the residual ~0.1%
error rate on `data.source.coop`. No S3, no auth, no registry — the only variable
is *when* the inbound request body stream is touched relative to an await.

## What it does

Two arms, same code path, selected by request path:

| arm | behaviour | mirrors |
| --- | --- | --- |
| `/before` | attach the inbound stream to the outbound request **before** any await | `ProxyGateway::op_needs_buffered_body` — the multipart control ops and batch delete |
| `/after` | await first (standing in for the registry lookup and the STS exchange), **then** attach the stream | `WorkerBackend::forward` for `PutObject` / `UploadPart`, which the classifier explicitly excludes |

`forward()` copies `WorkerBackend::forward` faithfully, including the
`FixedLengthStream` wrapper and its dropped `pipe_to` promise.

## Running

```sh
python3 origin.py &                 # fake backend on :9101 (REPRO_SLOW_SECONDS tunes the await)
worker-build --release
npx wrangler@4 dev --port 8787 --local &
python3 drive.py --concurrency 8 --rounds 4 --size-mib 16
```

`drive.py` exits non-zero if the post-await arm failed.

## What it has actually shown

At concurrency 8 × 4 rounds × 16 MiB it produced **1 failure in 32** on `/after`
and **0 in 32** on `/before`:

```
[after] forward failed: Error: Network connection lost. - Cause: Error: Network connection lost.
```

client-side surfacing as `BrokenPipeError`. It is intermittent — a later run of
96/96 on both arms was clean — so treat a green run as inconclusive, not as a fix.

**Scope, honestly.** `Network connection lost` on the outbound fetch maps to
`ProxyError::BackendError` → **503**. That is the *minority* prod failure mode
(2 of 23 in one sample). It does **not** reproduce the majority 520 mode.

In prod the 520 is *relayed from the backend fetch*: `GatewayResponse::Forward`
passes the upstream status through verbatim (`response_from_forward`), the worker's
own 5xx log records `status=520`, and `handle_request` returns normally. So for the
520s the worker is not being killed — it is faithfully forwarding a 520 that the
Worker→S3 subrequest produced. tessera's backend is plain S3 in us-west-2, which
does not emit 520, so that status is synthesized by the runtime for a subrequest
the worker itself issued. Why, is still open.

The `TypeError: Can't read from request stream after responding with an exception`
that accompanies the 520s fires ~1 ms *after* the response is committed. It is the
orphaned `pipe_to` still reading the inbound body — a consequence, not the cause,
but it is what destroys the connection so the client sees a reset instead of the
relayed status.
