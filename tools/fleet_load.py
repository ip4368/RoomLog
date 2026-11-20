#!/usr/bin/env python3
"""Fleet load harness (stdlib only): mostly-idle-tenant model.

Logs in once per active tenant, then loops authenticated + public reads
across the active set for a fixed duration and reports latency percentiles
as JSON. Idle tenants are simply the tenant folders never touched; generate
them by copying a template tenant database before starting the fleet.

Example:
    python3 tools/fleet_load.py --base http://127.0.0.1:18295 \
        --domain example.com --prefix church- --tenants 10000 \
        --active 200 --concurrency 32 --duration 30
"""
import argparse
import http.client
import json
import socket
import statistics
import threading
import time
from urllib.parse import urlparse


def percentiles(values):
    if not values:
        return {"p50": None, "p95": None, "p99": None, "max": None}
    s = sorted(values)
    pick = lambda q: s[min(len(s) - 1, int(q * len(s)))]
    return {"p50": pick(0.50), "p95": pick(0.95), "p99": pick(0.99), "max": s[-1]}


def request(conn, method, path, host, body=None, cookie=None):
    headers = {"Host": host}
    if body is not None:
        headers["Content-Type"] = "application/json"
        body = json.dumps(body)
    if cookie:
        headers["Cookie"] = cookie
    started = time.perf_counter()
    conn.request(method, path, body=body, headers=headers)
    resp = conn.getresponse()
    data = resp.read()
    ms = (time.perf_counter() - started) * 1000
    return resp.status, resp.getheader("Set-Cookie"), data, ms


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--base", required=True)
    ap.add_argument("--domain", default="example.com")
    ap.add_argument("--prefix", default="church-")
    ap.add_argument("--tenants", type=int, required=True, help="total tenants on disk (slugs 1..N)")
    ap.add_argument("--active", type=int, default=200)
    ap.add_argument("--concurrency", type=int, default=32)
    ap.add_argument("--duration", type=float, default=30)
    ap.add_argument("--password", default="correct horse")
    args = ap.parse_args()

    url = urlparse(args.base)
    slugs = [f"{args.prefix}{i:06d}" for i in range(1, args.active + 1)]
    cookies = {}
    login_ms = []
    errors = []

    def connect():
        conn = http.client.HTTPConnection(url.hostname, url.port, timeout=30)
        conn.connect()
        # Otherwise Nagle + delayed ACK adds a ~40ms floor that hides the
        # server's actual latency on keep-alive connections.
        conn.sock.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)
        return conn

    # One login per active tenant (argon2 verify server-side; measured apart).
    for slug in slugs:
        conn = connect()
        try:
            status, set_cookie, _, ms = request(
                conn, "POST", "/api/login", f"{slug}.{args.domain}",
                {"username": "admin", "password": args.password},
            )
            login_ms.append(ms)
            if status != 200 or not set_cookie:
                errors.append(f"login {slug}: {status}")
            else:
                cookies[slug] = set_cookie.split(";")[0]
        finally:
            conn.close()

    latencies = []
    counts = {"requests": 0}
    lock = threading.Lock()
    deadline = time.time() + args.duration
    chunks = [slugs[i:: args.concurrency] for i in range(args.concurrency)]

    def worker(mine):
        conn = connect()
        local = []
        local_errors = []
        n = 0
        i = 0
        try:
            while time.time() < deadline and mine:
                slug = mine[i % len(mine)]
                i += 1
                host = f"{slug}.{args.domain}"
                try:
                    if i % 2:
                        status, _, _, ms = request(conn, "GET", "/api/rooms", host, cookie=cookies.get(slug))
                    else:
                        status, _, _, ms = request(conn, "GET", "/api/public/info", host)
                    local.append(ms)
                    n += 1
                    if status != 200:
                        local_errors.append(f"{slug}: {status}")
                except Exception as e:  # reconnect once per failure
                    local_errors.append(f"{slug}: {e!r}")
                    conn.close()
                    conn = connect()
        finally:
            conn.close()
        with lock:
            latencies.extend(local)
            errors.extend(local_errors)
            counts["requests"] += n

    threads = [threading.Thread(target=worker, args=(c,)) for c in chunks if c]
    started = time.time()
    for t in threads:
        t.start()
    for t in threads:
        t.join()
    elapsed = max(time.time() - started, 0.001)

    print(json.dumps({
        "tenants_on_disk": args.tenants,
        "active_tenants": args.active,
        "concurrency": args.concurrency,
        "duration_s": round(elapsed, 2),
        "requests": counts["requests"],
        "requests_per_s": round(counts["requests"] / elapsed, 1),
        "errors": errors[:10],
        "error_count": len(errors),
        "latency_ms": percentiles(latencies),
        "login_ms": percentiles(login_ms),
        "mean_ms": round(statistics.mean(latencies), 3) if latencies else None,
    }, indent=2))


if __name__ == "__main__":
    main()
