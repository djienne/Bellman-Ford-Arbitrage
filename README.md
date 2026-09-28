# Hyperliquid spot arbitrage research

A public WebSocket screener for directed spot conversion cycles, with continuous paper trading and causal replay. The executable retains its historical name, `bellman-arb`. There are no order endpoints, signing code, keys, or real accounts. Binance's application code and dependencies have been removed; historical review evidence remains in `review/` and the original prototype archive.

Every selected simple cycle of 3–6 trades is enumerated once per metadata segment. Rotations are deduplicated, direction and market identity are preserved. A changed market triggers **all affected routes**, including overlapping routes. The saved metadata produces **210 routes across 29 markets**: 28 triangles, 90 four-leg, 44 five-leg, and 48 six-leg cycles. This is topology, not 210 profitable opportunities. Other graph components and lengths outside the configured bound are outside the completeness claim. Enumeration ceilings fail explicitly instead of publishing a partial inventory.

## Run

From this directory, with Docker running:

```powershell
docker compose build screener
docker compose run --rm screener discover
docker compose --profile tools run --rm check
docker compose up -d screener
..\cpu_limits.bat apply --live --only bellman-hyperliquid-screener --no-stats
docker compose logs --tail 5 screener
```

The dedicated service is `bellman-hyperliquid-screener`. It uses this project's persistent `runs` directory and `unless-stopped` restart policy. The central CPU helper owns its **1 CPU** quota; do not hand-edit it. The XEMM stack is only a source reference and is not used for deployment.

```powershell
# Bounded public test; seconds are measured after discovery/recovery.
docker compose run --rm screener run --runs runs/smoke --duration 600 --reconnect-after 300
# Replace run-... with an existing directory from runs.
docker compose run --rm screener replay runs/run-... --verify
docker compose run --rm screener replay runs/run-... --latency-ms 100,250,500 --quote-age-ms 500 --depth-age-ms 500
```

`--verify` requires the original assumptions and checks every recorded engine event. Alternative latency/freshness runs create independent fresh accounts. Replay preserves recorded input processing time and inserts the alternative orders' deadlines; additional timer processing delay is assumed zero and labeled. Replay prints a report without modifying evidence. To stop this service only: `docker compose stop screener`.

To exercise execution on negative-edge data, choose an ID from `discover`:

```powershell
docker compose run --rm screener replay runs/run-... --force-route '1B>2S>3S' --force-amount-usdc 100 --force-after-ns 0
```

The route above is illustrative: use an actual recorded route ID. Forced replay uses separate accounts, permits one attempt per scenario at the first eligible observation, disables normal entries, and bypasses profit filters only. Its report includes a diagnostic order/fill journal and is excluded from arbitrage-performance claims. `--verify` and live forced entries are rejected.

## Hot and cold paths

1. The public receiver captures UTC and monotonic receipt times before parsing and places frames on a bounded channel. It handles subscriptions, application pings, watchdogs, and reconnects.
2. A cold normalizer parses/validates full books and supplies primitive log prices and preallocated output buffers.
3. A dedicated hot worker owns dense quote arrays, fixed-size cycle edges, cached fee logs, and market-to-route indices. Its numeric calculation has **no allocations, locks, JSON, Decimal arithmetic, logging, exponentials, or busy loop**. It calculates every affected route; it does not discard work to meet a relaxation budget.
4. One cold evaluator owns depth, route episodes, quantity calculations, rankings, and paper accounts. It consumes hot results in order. Nonpositive touch-rate bounds reject all sizes cheaply; positive bounds receive full Decimal depth/rounding/fee evaluation.
5. A bounded background recorder writes append-only records and durable account events. Overflow or recording failure ends the segment explicitly.

The allocation claim applies to the numeric kernel, not channel scheduling or the complete feed pipeline. `tests/hot_alloc.rs` checks this with an allocation counter and 200,000 updates. Real receipt-to-hot and receipt-to-completed-decision distributions are recorded separately; consult `DOCS/VALIDATION.md` for measured values and CPU quota. The offline, from-scratch Bellman–Ford diagnostic is retained as a correctness cross-check, not the live detector.

## Assumptions

| Parameter | Default |
|---|---|
| Candidate input | 25, 100, 250, 1,000 USDC |
| Independent account funding | 10,000 virtual USDC each |
| Network assumptions | 100, **250**, 500 ms outbound and the same confirmation delay per order |
| Entry | Strictly greater than 5 bps after size, fee and quantity rounding |
| Observation | Every positive fee-net touch-rate episode, including below entry threshold |
| IOC adverse limit | 2 bps; unwind 50 bps |
| Depth subscription | `l2_fast = true`, five levels per side |
| Quote and depth maximum age | 1 second independently |
| Socket watchdog | 30 seconds |
| Recording rotation | 1 hour or 256 MiB |
| Recording cap | 50 GiB; stop, never delete evidence |
| Confirmed dust budget | 10 USDC per account; indicative value is never spendable |

The primary triangle requires at least 1.5 seconds of assumed network delays, plus actual processing/queue/timer delays. Each order fixes quantity and limit at submission. Only confirmed net proceeds fund the next leg. The serial owner arms a one-shot timer for the earliest order deadline; the health tick does not schedule execution. Scheduled deadlines, actual dispatch and processing completion are recorded separately. At arrival, available levels within that fixed limit determine the fill. An observation processed after a deadline cannot improve that fill. This is a local receipt-time proxy, not a calibrated reconstruction of matching-engine state.

Books use actual bids/asks and all received depth. BBO and L2 have separate clocks and versions: a fresh BBO does not refresh depth. Older cross-channel data cannot overwrite or replenish newer observations. Depth is eligible only when its top agrees with the current BBO, including size. Empty sides, stale data, invalid quantities, crosses and disconnects invalidate routes. A fresh explicit empty side gives a known zero fill; insufficient observations at arrival produce an **unobservable** attempt, excluded from performance claims. Pongs do not refresh books.

The shipped configuration explicitly requests fast five-level L2 snapshots. Orders and size estimates use only those received levels; an older twenty-level book is replaced, never merged into the new snapshot. Set `l2_fast = false` to request the slower twenty-level feed. The mode is saved in each run's configuration and server acknowledgements. See the [independent feed comparison](DOCS/DEPTH_FEED_CHECK.md) for the measured cadence and tradeoff.

Fees are fractional per market. The initial base spot taker fee is 7 bps, with the documented 80% quote-token-pair reduction inferred from explicit metadata token identities. Aligned-quote reductions require verified configured token IDs; account, staking, referral and other unverified discounts are unapplied. `discover` and each manifest show effective rates and provenance. The conservative paper convention charges the received token, rounded upward to its atomic precision. Actual account-specific fee rounding and execution are unverified. Size increments, price precision and the 10-quote-token minimum notional apply to every leg.

Each latency scenario has its own shared shadow liquidity across routes. Fills deduct capacity. An identical public snapshot does not refill it; observed increases and genuinely removed levels that reappear do. Consumed levels outside a truncated snapshot's visible price range retain their depletion, so slipping past level five cannot manufacture replenishment. This also applies to twenty-level mode. Shadow consumption persists across restarts. These scenarios are alternatives: **never sum their profit**.

Partial or rejected legs abandon the forward route and unwind through reversed completed legs, using the same latency rules. Confirmed dust following a completed observable unwind may continue only when every holding is provably below conversion size/minimum constraints and its total fresh direct-USDC bid mark is at most `dust_limit_usdc`. Recheck accumulated dust before every new entry. Missing marks/uncertain dust block entry until the guard passes; material or unresolved exposure remains blocked. An absent cross quote cannot prove a minimum-notional failure, although metadata can still prove a sub-lot quantity. Setting the dust budget to zero retains strict unwind pauses.

An unobservable fill retains the pending order, reserved funds and established holdings, rather than returning hypothetical cash. A recording gap likewise leaves unfinished attempts unresolved. No failed attempt is reset to starting funding. For an explicit dust recheck, stop this service and start `run --reconcile-dust 250` using the existing runs directory (250 identifies the scenario). The request waits up to 30 seconds for marks and journals acceptance/rejection; it never edits balances or clears unresolved attempts. Fully confirmed dust is also checked automatically.

## Evidence and interpretation

Each `runs/run-...` contains:

- `manifest.json`: raw metadata, explicit market/token identities, fees, configuration, source hashes, CPU quota and restored account state.
- `events-*.jsonl`: ordered raw frames with clocks, connection/timer events, decisions, reservations, fixed orders, arrivals, fills, confirmations, episode boundaries and balances.
- `status.json`: minute summary, rankings, independent accounts, indicative inventory marks, and separate structural/dormant/fresh-price/executable-depth coverage. Depth coverage requires nonempty matching books and is bounded by freshness expiry.
- `final.json`: shutdown result, recording completeness and terminal account checkpoint. Its presence alone is not proof of a clean run: `recording_complete` must be true and the checkpoint must match the durable terminal record.

New recordings use manifest format **3** for explicit account recovery, bounded dust and unresolved-fill reservations. Formats 1 and 2 still verify with their original behavior; format 1 includes the original shadow model and missing `l2_fast` means slow mode. Alternative/diagnostic runs use the current model and label their assumptions.

Exchange timestamps retain their provided millisecond resolution. UTC receipt and monotonic times are stored in nanoseconds; that is storage resolution, **not nanosecond exchange accuracy**. UTC backsteps are counted. Replay follows the recorded order and observed processing times, never sorting by exchange time. Receipt clocks are a local information model; exchange-to-local offsets mix clock skew and transit. Public books do not reveal matching-engine queue priority or true executable latency.

`realized_usdc` is the confirmed USDC cash change of observable attempts. Residuals have separate indicative direct-USDC bid marks with source and age; missing marks remain unavailable. Cash-plus-inventory change is not executable cash or a performance claim for unresolved exposure. Pending reservations and unresolved exposure are not profits. A touch-rate opportunity can fail because of fees, depth, lots, minimum notional, stale/conflicting data, or latency. Episodes end on observed failure and are censored by missing data; no grace interval inflates positive duration.

Recovery follows the durable `runs/current.json` pointer. A validated format-3 terminal checkpoint restores complete accounts without rerunning detector decisions. Legacy/unclean segments use verified durable-prefix replay. Remapping uses token identities and explicit market identities, retaining reservations, pending orders, dust and shadow depletion. Unrelated listings/removals are allowed; missing referenced identities or changed referenced precision block recovery. Metadata/fees refresh on restart and define a new segment. Periodic checkpoints are intentionally omitted until measured unclean-recovery cost warrants them.

At the cap, `runs/RECORDING_LIMIT_REACHED` blocks startup before discovery. Persistent recording/recovery errors leave the continuous process waiting for operator action, with a reason in logs and `runs/blocked.json` when writable. Resolve the cause and explicitly restart; no automatic deletion, funding reset or restart loop. Bounded test runs instead return a failing exit code. Disk errors and overflows invalidate the segment; later healthy input cannot retroactively repair it.

Transient public-metadata connection failures, timeouts, server errors and rate limiting retry with a capped backoff, including when Docker starts before networking is ready. Malformed metadata remains a blocking error. Legacy accounts with a recorded unobservable attempt are held unresolved during migration rather than assuming that the old model's returned cash proves a zero fill.

Review coverage after 24 hours; accumulate at least seven **healthy** recording days across activity conditions. Engineering tests and quiet observations do not establish profitability or the absence of rare opportunities.

Primary references: [spot metadata](https://hyperliquid.gitbook.io/hyperliquid-docs/for-developers/api/info-endpoint/spot), [WebSocket subscriptions](https://hyperliquid.gitbook.io/hyperliquid-docs/for-developers/api/websocket/subscriptions), [fees](https://hyperliquid.gitbook.io/hyperliquid-docs/trading/fees), [precision](https://hyperliquid.gitbook.io/hyperliquid-docs/for-developers/api/tick-and-lot-size), [order constraints](https://hyperliquid.gitbook.io/hyperliquid-docs/for-developers/api/error-responses).
