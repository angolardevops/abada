# Parity run full-20260919

protocol: **full** (warm-up 10s, measured 30s, 3 runs, soak 300 s); host AMD Ryzen 9 8940HX with Radeon Graphics, 32 threads, governor performance, load 16.71 -> 7.25; go1.26.2, rustc 1.98.1 (48a229cea 2026-09-01); commit 99fe9d2

> **Exploratory — no ratio here may be quoted as a result:** load average 16.71 is above 25% of 32 threads.

## P1/P2/P4 closed loop (whole mix)

| c | metric | grpc-gateway | abada | abada/gw | verdict |
|---|---|---|---|---|---|
| 1 | req/s | 5311.9 [2208-5646] | 6106.7 [1648-6239] | 1.15 | AT LEVEL (intervals overlap) |
| 1 | p50 µs | 159.2 [149-240] | 153.9 [147-363] | 0.97 | AT LEVEL (intervals overlap) |
| 1 | p99 µs | 744.3 [716-4113] | 334.0 [307-5019] | 0.45 | AT LEVEL (intervals overlap) |
| 1 | p99.9 µs | 1084.1 [1037-10036] | 546.4 [498-10290] | 0.50 | AT LEVEL (intervals overlap) |
| 1 | CPU ms/req | 0.2 [0-0] | 0.1 [0-0] | 0.53 | AHEAD |
| 1 | unexpected status | 0.0 [0-0] | 0.0 [0-0] | nan | AT LEVEL (intervals overlap) |
| 8 | req/s | 17170.1 [13349-17826] | 23938.7 [21582-25690] | 1.39 | AHEAD |
| 8 | p50 µs | 397.8 [384-501] | 309.6 [291-341] | 0.78 | AHEAD |
| 8 | p99 µs | 1477.8 [1418-1958] | 768.7 [677-905] | 0.52 | AHEAD |
| 8 | p99.9 µs | 1965.8 [1811-2895] | 1408.0 [1080-1595] | 0.72 | AHEAD |
| 8 | CPU ms/req | 0.3 [0-0] | 0.1 [0-0] | 0.49 | AHEAD |
| 8 | unexpected status | 0.0 [0-0] | 0.0 [0-0] | nan | AT LEVEL (intervals overlap) |
| 32 | req/s | 25397.5 [23808-25564] | 29017.7 [25606-32075] | 1.14 | AHEAD |
| 32 | p50 µs | 1134.7 [1132-1206] | 1029.4 [950-1139] | 0.91 | AT LEVEL (intervals overlap) |
| 32 | p99 µs | 3353.6 [3214-3574] | 2443.9 [1956-3236] | 0.73 | AT LEVEL (intervals overlap) |
| 32 | p99.9 µs | 4952.8 [4451-5393] | 3777.2 [3062-6304] | 0.76 | AT LEVEL (intervals overlap) |
| 32 | CPU ms/req | 0.3 [0-0] | 0.2 [0-0] | 0.59 | AHEAD |
| 32 | unexpected status | 0.0 [0-0] | 0.0 [0-0] | nan | AT LEVEL (intervals overlap) |
| 128 | req/s | 36902.9 [36274-42942] | 33831.4 [33408-39239] | 0.92 | AT LEVEL (intervals overlap) |
| 128 | p50 µs | 3262.3 [2807-3321] | 3736.3 [3207-3769] | 1.15 | AT LEVEL (intervals overlap) |
| 128 | p99 µs | 6961.7 [6002-7109] | 6058.0 [5282-6353] | 0.87 | AT LEVEL (intervals overlap) |
| 128 | p99.9 µs | 9329.6 [7965-9454] | 8174.2 [7768-8796] | 0.88 | AT LEVEL (intervals overlap) |
| 128 | CPU ms/req | 0.3 [0-0] | 0.2 [0-0] | 0.74 | AHEAD |
| 128 | unexpected status | 0.0 [0-0] | 0.0 [0-0] | nan | AT LEVEL (intervals overlap) |

## P1/P4 open loop, fixed rate, latency from the intended start

| rate | metric | grpc-gateway | abada | abada/gw | verdict |
|---|---|---|---|---|---|
| 8457 | achieved req/s | 8457.0 [8457-8457] | 8457.0 [8457-8457] | 1.00 | AT LEVEL (intervals overlap) |
| 8457 | p50 µs | 284.7 [267-290] | 238.6 [238-258] | 0.84 | AHEAD |
| 8457 | p99 µs | 1257.2 [1202-1450] | 666.1 [588-852] | 0.53 | AHEAD |
| 8457 | p99.9 µs | 2356.0 [2350-27081] | 2199.8 [1443-2720] | 0.93 | AT LEVEL (intervals overlap) |
| 16915 | achieved req/s | 16915.0 [16915-16915] | 16914.9 [16915-16915] | 1.00 | AT LEVEL (intervals overlap) |
| 16915 | p50 µs | 305.4 [295-329] | 327.6 [268-346] | 1.07 | AT LEVEL (intervals overlap) |
| 16915 | p99 µs | 1394.7 [1211-1656] | 1706.9 [992-11978] | 1.22 | AT LEVEL (intervals overlap) |
| 16915 | p99.9 µs | 3693.7 [1904-12658] | 11472.4 [5211-32240] | 3.11 | AT LEVEL (intervals overlap) |

## P1 per request (closed loop, c=8): p50 / p99 µs

| request | grpc-gateway | abada | p50 abada/gw |
|---|---|---|---|
| get_small | 370 / 976 | 304 / 754 | 0.82 |
| get_query | 386 / 1003 | 300 / 739 | 0.78 |
| post_1k | 420 / 1060 | 315 / 772 | 0.75 |
| post_64k | 1304 / 2107 | 424 / 933 | 0.33 |
| patch_mask | 406 / 1040 | 315 / 778 | 0.78 |
| error_status | 383 / 999 | 301 / 761 | 0.79 |

## P3 memory: RSS during the soak (MB)

| side | first sample | steady state (last quarter mean) | max | last quarter vs the one before | verdict |
|---|---|---|---|---|---|
| grpc-gateway | 47.3 | 48.4 | 72.4 | 0.96x | FLAT at the end |
| abada | 17.1 | 75.2 | 76.5 | 1.03x | FLAT at the end |

Steady-state RSS abada/grpc-gateway: 1.55. The soak is 300 s: a leak slower than about 1 MB per minute cannot be excluded by it. The growth from the first sample to the plateau is not investigated (candidates: allocator arenas per worker thread, connection buffers); read the series in `raw/soak-*.json`.

## P5 cold start: spawn to first correct answer (ms)

| side | median | min-max |
|---|---|---|
| grpc-gateway | 5.7 | 5-6 |
| abada | 6.2 | 5-7 |
