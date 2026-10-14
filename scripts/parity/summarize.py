#!/usr/bin/env python3
"""summarize.py check|report <results dir> — tables from scripts/parity/run.sh.

Every number is the median of the runs with the min-max spread beside it, and
each ratio is abada / grpc-gateway (below 1.0 is abada ahead). A ratio is only
called a difference when the two [min, max] intervals do not overlap.
"""
import glob, json, statistics as st, sys, os, re

def load(pattern):
    return [json.load(open(f)) for f in sorted(glob.glob(pattern))]

def med(xs): return st.median(xs) if xs else float("nan")
def spread(xs): return f"{min(xs):.0f}-{max(xs):.0f}" if xs else "-"

def verdict(ratio, a, g, lower_better=True):
    if not a or not g: return "NOT MEASURED"
    overlap = not (max(a) < min(g) or max(g) < min(a))
    if overlap: return "AT LEVEL (intervals overlap)"
    better = (med(a) < med(g)) == lower_better
    return "AHEAD" if better else "BELOW LEVEL"

def check(out):
    g = json.load(open(f"{out}/raw/check-go.json")); r = json.load(open(f"{out}/raw/check-abada.json"))
    bad = 0
    for a, b in zip(g, r):
        same = a["status"] == b["status"] and a["body"] == b["body"]
        if a["status"] != a["expect"]: same = False
        print(("SAME " if same else "DIFF "), a["name"], a["status"], b["status"])
        bad += not same
    print(f"{len(g) - bad}/{len(g)} requests answered identically")

def capacity(out):
    """Requests/s the slower side sustains at the highest closed-loop level."""
    top = max(int(re.search(r'-c(\d+)-', f).group(1)) for f in glob.glob(f"{out}/raw/closed-*"))
    per = [med([d["overall"]["rps"] for d in load(f"{out}/raw/closed-{s}-c{top}-r*.json")]) for s in ("go", "abada")]
    print(int(min(per)))

def report(out):
    h = json.load(open(f"{out}/host.json"))
    print(f"# Parity run {os.path.basename(out)}\n")
    print(f"protocol: **{h['mode']}** (warm-up {h['warmup']}, measured {h['duration']}, {h['runs']} runs, soak {h['soak_seconds']} s); "
          f"host {h['cpu']}, {h['threads']} threads, governor {h['governor']}, load {h['load_before']} -> {h.get('load_after','?')}; "
          f"{h['go']}, {h['rustc']}; commit {h['commit']}\n")
    why = []
    if h["load_before"] > 0.25 * h["threads"]: why.append(f"load average {h['load_before']} is above 25% of {h['threads']} threads")
    if max(h.get("load_after", 0), 0) > 0.25 * h["threads"]: why.append(f"load average was {h.get('load_after')} after")
    if h.get("governor") not in ("performance",): why.append(f"governor is {h.get('governor')} (energy preference {h.get('energy_preference','?')})")
    mhz = h.get("cpu_mhz_mean_before")
    if mhz is not None and mhz < 2000: why.append(f"mean CPU clock {mhz} MHz")
    if h["mode"].endswith("smoke"): why.append("smoke run: 1-2 s per measurement")
    if why:
        print("> **Exploratory — no ratio here may be quoted as a result:** " + "; ".join(why) + ".\n")

    print("## P1/P2/P4 closed loop (whole mix)\n")
    print("| c | metric | grpc-gateway | abada | abada/gw | verdict |\n|---|---|---|---|---|---|")
    levels = sorted({int(re.search(r'-c(\d+)-', f).group(1)) for f in glob.glob(f"{out}/raw/closed-*")})
    for c in levels:
        G = load(f"{out}/raw/closed-go-c{c}-r*.json"); A = load(f"{out}/raw/closed-abada-c{c}-r*.json")
        for label, key, lower in [("req/s", lambda d: d["overall"]["rps"], False),
                                  ("p50 µs", lambda d: d["overall"]["p50_us"], True),
                                  ("p99 µs", lambda d: d["overall"]["p99_us"], True),
                                  ("p99.9 µs", lambda d: d["overall"]["p999_us"], True),
                                  ("CPU ms/req", lambda d: d.get("gateway_cpu_ms_per_request", 0), True),
                                  ("unexpected status", lambda d: d["overall"]["errors_unexpected_status"], True)]:
            g = [key(d) for d in G]; a = [key(d) for d in A]
            if not g or not a: continue
            ratio = med(a) / med(g) if med(g) else float("nan")
            print(f"| {c} | {label} | {med(g):.1f} [{spread(g)}] | {med(a):.1f} [{spread(a)}] | {ratio:.2f} | {verdict(ratio, a, g, lower)} |")

    print("\n## P1/P4 open loop, fixed rate, latency from the intended start\n")
    print("| rate | metric | grpc-gateway | abada | abada/gw | verdict |\n|---|---|---|---|---|---|")
    for rate in sorted({int(re.search(r'rate(\d+)-', f).group(1)) for f in glob.glob(f"{out}/raw/open-*")}):
        G = load(f"{out}/raw/open-go-rate{rate}-r*.json"); A = load(f"{out}/raw/open-abada-rate{rate}-r*.json")
        short = [n for n, ds in (("grpc-gateway", G), ("abada", A)) if any(d["overall"]["rps"] < 0.95 * rate for d in ds)]
        if short:
            print(f"| {rate} | — | — | — | — | **INVALID: {' and '.join(short)} did not sustain {rate} req/s, so the latencies measure a growing queue** |")
            continue
        for label, key in [("achieved req/s", lambda d: d["overall"]["rps"]), ("p50 µs", lambda d: d["overall"]["p50_us"]),
                           ("p99 µs", lambda d: d["overall"]["p99_us"]), ("p99.9 µs", lambda d: d["overall"]["p999_us"])]:
            g = [key(d) for d in G]; a = [key(d) for d in A]
            ratio = med(a) / med(g) if med(g) else float("nan")
            print(f"| {rate} | {label} | {med(g):.1f} [{spread(g)}] | {med(a):.1f} [{spread(a)}] | {ratio:.2f} | {verdict(ratio, a, g, label != 'achieved req/s')} |")

    print("\n## P1 per request (closed loop, c=8): p50 / p99 µs\n")
    print("| request | grpc-gateway | abada | p50 abada/gw |\n|---|---|---|---|")
    G = load(f"{out}/raw/closed-go-c8-r*.json"); A = load(f"{out}/raw/closed-abada-c8-r*.json")
    if G and A:
        for i, r in enumerate(G[0]["per_request"]):
            g50 = med([d["per_request"][i]["p50_us"] for d in G]); g99 = med([d["per_request"][i]["p99_us"] for d in G])
            a50 = med([d["per_request"][i]["p50_us"] for d in A]); a99 = med([d["per_request"][i]["p99_us"] for d in A])
            print(f"| {r['name']} | {g50:.0f} / {g99:.0f} | {a50:.0f} / {a99:.0f} | {a50/g50:.2f} |")

    print("\n## P3 memory: RSS during the soak (MB)\n")
    print("| side | first sample | steady state (last quarter mean) | max | last quarter vs the one before | verdict |\n|---|---|---|---|---|---|")
    steady = {}
    for s, name in [("go", "grpc-gateway"), ("abada", "abada")]:
        f = f"{out}/raw/soak-{s}.json"
        if not os.path.exists(f): continue
        ser = [v / 1024 for v in json.load(open(f)).get("gateway_rss_series_kb", [])]
        if len(ser) < 8: continue
        q = len(ser) // 4
        q3, q4 = st.mean(ser[2*q:3*q]), st.mean(ser[3*q:])
        steady[s] = q4
        flat = q4 <= 1.05 * q3
        print(f"| {name} | {ser[0]:.1f} | {q4:.1f} | {max(ser):.1f} | {q4/q3:.2f}x | {'FLAT at the end' if flat else 'STILL GROWING'} |")
    if len(steady) == 2:
        print(f"\nSteady-state RSS abada/grpc-gateway: {steady['abada']/steady['go']:.2f}. The soak is {json.load(open(f'{out}/raw/soak-go.json'))['seconds']:.0f} s: a leak slower than about 1 MB per minute cannot be excluded by it. The growth from the first sample to the plateau is not investigated (candidates: allocator arenas per worker thread, connection buffers); read the series in `raw/soak-*.json`.")

    print("\n## P5 cold start: spawn to first correct answer (ms)\n")
    cs = {}
    for line in open(f"{out}/raw/coldstart.txt"):
        s, us = line.split(); cs.setdefault(s, []).append(int(us) / 1000)
    print("| side | median | min-max |\n|---|---|---|")
    for s, name in [("go", "grpc-gateway"), ("abada", "abada")]:
        print(f"| {name} | {med(cs[s]):.1f} | {spread(cs[s])} |")

if __name__ == "__main__":
    {"check": check, "report": report, "capacity": capacity}[sys.argv[1]](sys.argv[2])
