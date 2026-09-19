# Parity run quick-20260919

protocol: **quick** (warm-up 3s, measured 10s, 3 runs, soak 60 s); host AMD Ryzen 9 8940HX with Radeon Graphics, 32 threads, governor powersave, load 14.87 -> 68.26; go1.26.2, rustc 1.98.1 (48a229cea 2026-09-01); commit 1e91e02

> **Exploratory — no ratio here may be quoted as a result:** load average 14.87 is above 25% of 32 threads; load average was 68.26 after; governor is powersave (energy preference power); mean CPU clock 544 MHz.

## P1/P2/P4 closed loop (whole mix)

| c | metric | grpc-gateway | abada | abada/gw | verdict |
|---|---|---|---|---|---|
| 1 | req/s | 476.7 [441-487] | 524.3 [518-540] | 1.10 | AHEAD |
| 1 | p50 µs | 1880.6 [1845-2026] | 1849.5 [1794-1866] | 0.98 | AT LEVEL (intervals overlap) |
| 1 | p99 µs | 7378.4 [7024-7808] | 3213.3 [3112-3278] | 0.44 | AHEAD |
| 1 | p99.9 µs | 8824.1 [8462-9805] | 3838.3 [3769-4034] | 0.43 | AHEAD |
| 1 | CPU ms/req | 3.0 [3-3] | 1.4 [1-1] | 0.46 | AHEAD |
| 1 | unexpected status | 0.0 [0-0] | 0.0 [0-0] | nan | AT LEVEL (intervals overlap) |
| 8 | req/s | 2110.5 [2108-2221] | 2476.4 [2445-2632] | 1.17 | AHEAD |
| 8 | p50 µs | 3351.0 [3175-3373] | 3046.8 [2869-3093] | 0.91 | AHEAD |
| 8 | p99 µs | 11739.5 [11335-11992] | 6243.8 [5874-6307] | 0.53 | AHEAD |
| 8 | p99.9 µs | 14271.5 [13826-14418] | 8501.2 [7589-8601] | 0.60 | AHEAD |
| 8 | CPU ms/req | 2.6 [3-3] | 1.2 [1-1] | 0.46 | AHEAD |
| 8 | unexpected status | 0.0 [0-0] | 0.0 [0-0] | nan | AT LEVEL (intervals overlap) |
| 32 | req/s | 2729.5 [2708-2880] | 3302.5 [2197-4259] | 1.21 | AT LEVEL (intervals overlap) |
| 32 | p50 µs | 10283.6 [9684-10627] | 8972.6 [7065-12840] | 0.87 | AT LEVEL (intervals overlap) |
| 32 | p99 µs | 29947.7 [27284-42381] | 22793.3 [16408-39979] | 0.76 | AT LEVEL (intervals overlap) |
| 32 | p99.9 µs | 44662.5 [37946-72118] | 37763.9 [24215-55162] | 0.85 | AT LEVEL (intervals overlap) |
| 32 | CPU ms/req | 2.0 [2-2] | 0.9 [1-1] | 0.47 | AHEAD |
| 32 | unexpected status | 0.0 [0-0] | 0.0 [0-0] | nan | AT LEVEL (intervals overlap) |
| 128 | req/s | 5352.9 [2894-5410] | 4910.4 [3929-5889] | 0.92 | AT LEVEL (intervals overlap) |
| 128 | p50 µs | 22029.8 [22008-39483] | 25333.0 [21421-29899] | 1.15 | AT LEVEL (intervals overlap) |
| 128 | p99 µs | 54089.7 [50228-118053] | 47908.9 [35773-75255] | 0.89 | AT LEVEL (intervals overlap) |
| 128 | p99.9 µs | 73827.9 [64521-162692] | 73960.7 [42751-91690] | 1.00 | AT LEVEL (intervals overlap) |
| 128 | CPU ms/req | 1.7 [2-2] | 0.9 [1-1] | 0.54 | AHEAD |
| 128 | unexpected status | 0.0 [0-0] | 0.0 [0-0] | nan | AT LEVEL (intervals overlap) |

## P1/P4 open loop, fixed rate, latency from the intended start

| rate | metric | grpc-gateway | abada | abada/gw | verdict |
|---|---|---|---|---|---|
| 5000 | — | — | — | — | **INVALID: grpc-gateway and abada did not sustain 5000 req/s, so the latencies measure a growing queue** |
| 15000 | — | — | — | — | **INVALID: grpc-gateway and abada did not sustain 15000 req/s, so the latencies measure a growing queue** |

## P1 per request (closed loop, c=8): p50 / p99 µs

| request | grpc-gateway | abada | p50 abada/gw |
|---|---|---|---|
| get_small | 3139 / 6305 | 3021 / 6244 | 0.96 |
| get_query | 3280 / 6609 | 2971 / 6053 | 0.91 |
| post_1k | 3593 / 6901 | 3105 / 6208 | 0.86 |
| post_64k | 10807 / 15131 | 3914 / 7257 | 0.36 |
| patch_mask | 3455 / 6696 | 3036 / 5996 | 0.88 |
| error_status | 3199 / 6694 | 2938 / 5872 | 0.92 |

## P3 memory: RSS during the soak (MB)

| side | first sample | last sample | max | growth first→last |
|---|---|---|---|---|
| grpc-gateway | 58.9 | 68.8 | 75.7 | +16.8% |
| abada | 73.9 | 83.9 | 86.7 | +13.5% |

**INVALID soak for abada:** it sustained 3960 of 5000 req/s; the RSS includes a queue.

Flat means the last quarter of the series does not trend up; read the series in `raw/soak-*.json`.

## P5 cold start: spawn to first correct answer (ms)

| side | median | min-max |
|---|---|---|
| grpc-gateway | 60.0 | 57-64 |
| abada | 65.7 | 59-77 |
