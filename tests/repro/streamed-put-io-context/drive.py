"""Fire concurrent streamed PUTs at both arms of the reproducer and tally results.

Usage:
    python3 drive.py [--arm after|before|both] [--concurrency N] [--rounds N] [--size-mib N]

Exit status is 1 if the `after` arm produced any failure, so this doubles as a
regression check once the upstream fix lands.
"""

import argparse
import collections
import http.client
import sys
import threading

WORKER_HOST = "127.0.0.1"
WORKER_PORT = 8787


def one_put(arm, size_bytes, results, lock):
    payload = b"\0" * size_bytes
    try:
        conn = http.client.HTTPConnection(WORKER_HOST, WORKER_PORT, timeout=120)
        # Explicit content-length: this is the FixedLengthStream branch, the
        # same one a 16 MiB UploadPart takes.
        conn.request(
            "PUT",
            f"/{arm}",
            body=payload,
            headers={"content-length": str(size_bytes)},
        )
        resp = conn.getresponse()
        body = resp.read().decode("utf-8", "replace")[:300]
        key = f"{resp.status} {body}" if resp.status != 200 else "200 ok"
        conn.close()
    except Exception as e:  # connection reset, etc. — the 520 analogue
        key = f"EXC {type(e).__name__}: {e}"[:300]
    with lock:
        results[key] += 1


def run(arm, concurrency, rounds, size_bytes):
    results = collections.Counter()
    lock = threading.Lock()
    for _ in range(rounds):
        threads = [
            threading.Thread(target=one_put, args=(arm, size_bytes, results, lock))
            for _ in range(concurrency)
        ]
        for t in threads:
            t.start()
        for t in threads:
            t.join()
    return results


def report(arm, results):
    total = sum(results.values())
    bad = total - results.get("200 ok", 0)
    print(f"\n=== /{arm} — {total} requests, {bad} failed ===")
    for key, count in results.most_common():
        print(f"  {count:4d}  {key}")
    return bad


if __name__ == "__main__":
    p = argparse.ArgumentParser()
    p.add_argument("--arm", default="both", choices=["after", "before", "both"])
    p.add_argument("--concurrency", type=int, default=8)
    p.add_argument("--rounds", type=int, default=4)
    p.add_argument("--size-mib", type=int, default=16)
    args = p.parse_args()

    size = args.size_mib * 1024 * 1024
    arms = ["before", "after"] if args.arm == "both" else [args.arm]

    failures = {}
    for arm in arms:
        failures[arm] = report(arm, run(arm, args.concurrency, args.rounds, size))

    print()
    if failures.get("after"):
        print("REPRODUCED: the post-await arm failed.")
        if not failures.get("before"):
            print("Control arm (pre-await) was clean — timing, not the stream itself.")
        sys.exit(1)
    print("No failures on the post-await arm.")
