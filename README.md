# Bellman-Ford Arbitrage Screener (Hyperliquid spot)

This is an **arbitrage opportunity screener and paper trader** for Hyperliquid spot markets, built on the **Bellman–Ford** view of currency arbitrage: tokens are graph nodes, every market is a pair of directed edges weighted by `−ln(rate × (1 − fee))`, and a round trip that ends with more than it started with is a **negative-weight cycle**. It watches the public WebSocket feed, finds fee-positive conversion cycles of 3–6 trades (e.g. `USDC → HYPE → USDT0 → USDC`), and simulates executing them against recorded order books with realistic latency, depth, lot sizes and fees.

**Paper only.** There are no order endpoints, no signing code, no keys and no real accounts. The point is to measure *whether, how often and for how long* such opportunities exist and survive latency, not to trade them.

> **Status (28 Sep 2026, [`DOCS/VALIDATION.md`](DOCS/VALIDATION.md)):** engineering checks and smoke runs pass, but **no positive fee-net cycle has been observed yet**. Quiet observations do not prove that rare opportunities never occur, and public-feed paper results say nothing about live fill profitability.

## How it works

### The math

A market with best bid `b` and best ask `a` gives two edges: selling the base token converts at `b`, buying it converts at `1/a`. Each trade also pays a fractional taker fee `f`. With edge weight `w = −ln(rate) − ln(1 − f)`, a cycle's product of rates exceeds 1 exactly when `Σ w < 0`. Bellman–Ford finds such a negative cycle in O(V·E).

### Bellman–Ford vs. the live detector

Bellman–Ford is a **reference cross-check, not the live detector.** It returns one witness cycle rather than every cycle, and it ignores order-book depth, lot sizes, minimum notionals and latency, which decide whether an opportunity is real. So the live path does this instead:

1. **Discover** all spot markets and enumerate every simple directed cycle of 3–6 trades once (`src/market.rs`). Rotations are deduplicated; direction and market identity are kept. The saved metadata gives **210 routes across 29 markets**: 28 triangles, 90 four-leg, 44 five-leg and 48 six-leg cycles. That is topology, not 210 profitable opportunities. Enumeration ceilings fail loudly rather than publish a partial inventory.
2. On every book update, **re-evaluate every affected route** (including overlapping routes) as a sum of per-edge log terms with cached fee logs. No route is skipped to save time.
3. A positive fee-net route gets a full **Decimal** evaluation against real depth, rounding and fees at each candidate size, then a paper attempt if it clears the entry threshold.

The from-scratch Bellman–Ford (`market::negative_cycle`) is kept for offline cross-checks and is exercised by the test suite.

### Hot and cold paths

1. **Receiver:** stamps UTC and monotonic receipt times *before* parsing, then handles subscriptions, pings, watchdogs and reconnects.
2. **Cold normalizer:** parses and validates books into primitive log prices with preallocated output buffers.
3. **Hot worker:** owns dense quote arrays, fixed-size cycle edges, fee logs and market-to-route indices. The numeric kernel has no allocations, locks, JSON, Decimal math, logging or busy loops (`tests/hot_alloc.rs` counts allocations over 200,000 updates).
4. **Cold evaluator:** depth, quantities, rankings, route episodes and paper accounts.
5. **Background recorder:** append-only records. Overflow or a write failure ends the segment explicitly.

Measured latencies and CPU use are in [`DOCS/VALIDATION.md`](DOCS/VALIDATION.md); the allocation claim covers the numeric kernel only, not the full feed pipeline.

## Run

Everything runs through Docker Compose from this directory:

```powershell
docker compose build screener
docker compose run --rm screener discover               # list markets, fees, routes
docker compose --profile tools run --rm check           # release test suite
docker compose up -d screener                           # continuous paper run
..\cpu_limits.bat apply --live --only bellman-hyperliquid-screener --no-stats
docker compose logs --tail 5 screener
docker compose stop screener                            # stop this service only
```

The service is `bellman-hyperliquid-screener`: persistent `runs/` volume, `unless-stopped`, 1 CPU and 512 MiB. The CPU limit belongs to the central `cpu_limits.bat`; do not hand-edit it.

Bounded test and replay:

```powershell
# 10-minute public test with a controlled reconnect (seconds count after discovery/recovery)
docker compose run --rm screener run --runs runs/smoke --duration 600 --reconnect-after 300
# Replay a recording (replace run-... with a directory in runs/)
docker compose run --rm screener replay runs/run-... --verify
docker compose run --rm screener replay runs/run-... --latency-ms 100,250,500 --quote-age-ms 500 --depth-age-ms 500
```

`--verify` re-checks every recorded engine event under the original assumptions. Other replay options run on fresh, independent accounts and never modify the evidence.

To exercise execution on negative-edge data, force a route (take a real ID from `discover`; the one below is illustrative):

```powershell
docker compose run --rm screener replay runs/run-... --force-route '1B>2S>3S' --force-amount-usdc 100 --force-after-ns 0
```

Forced replay uses separate accounts, one attempt per scenario, normal entries off and profit filters bypassed. Its report is a diagnostic and **not** an arbitrage-performance claim. `--verify` and live forced entries are rejected.

## Configuration (`config.toml`)

| Parameter | Default |
|---|---|
| Cycle length | 3–6 trades |
| Candidate input | 25, 100, 250, 1,000 USDC |
| Paper funding | 10,000 virtual USDC per independent account |
| Latency scenarios | 100, **250**, 500 ms each way, per order |
| Entry threshold | strictly > 5 bps after size, fee and rounding |
| Adverse IOC limit / unwind | 2 bps / 50 bps |
| Depth feed | `l2_fast = true` (five levels per side) |
| Quote and depth max age | 1 s, independently |
| Socket watchdog | 30 s |
| Recording rotation / cap | 1 h or 256 MiB / 50 GiB (stop, never delete) |
| Dust budget | 10 USDC per account, never spendable |

Decimal quantities and bps are strings. Set `l2_fast = false` for the slower twenty-level feed; see the [feed comparison](DOCS/DEPTH_FEED_CHECK.md).

## Paper-trading model

- **Order flow.** Each order fixes quantity and limit at submission and fills against the levels available at arrival, after the assumed outbound delay. Only confirmed net proceeds fund the next leg. A triangle needs at least 1.5 s of assumed network delay plus real processing delay. One timer per earliest deadline drives execution; observations processed after a deadline cannot improve that fill.
- **Books.** Actual bids/asks and all received levels. BBO and L2 have separate clocks; a fresh BBO does not refresh depth, and depth counts only if its top agrees with the BBO. Empty sides, stale data, crosses and disconnects invalidate routes. A fresh explicit empty side is a known zero fill; missing data at arrival makes the attempt **unobservable** and excludes it from performance claims.
- **Fees.** Fractional per market: 7 bps base spot taker, with the documented 80% reduction for quote-token pairs inferred from token identities. Unverified account, staking and referral discounts are unapplied. `discover` shows each rate's provenance. The fee is charged on the received token, rounded up. Lot size, price precision and the 10-quote-token minimum notional apply to every leg.
- **Shadow liquidity.** Each latency scenario keeps its own finite depth shared across routes. Fills consume it; an identical snapshot does not refill it, and levels beyond a truncated snapshot cannot manufacture replenishment. It persists across restarts. The scenarios are alternatives: **never sum their profit**.
- **Failures and unwinds.** A partial or rejected leg abandons the route and unwinds through the completed legs under the same latency rules. Confirmed, untradeable dust worth at most the dust budget (fresh direct-USDC bid marks) does not block new entries; anything larger stays blocked. An unobservable fill keeps its reservation and pending order rather than returning hypothetical cash, and no failed attempt resets to starting funds.

This is a local receipt-time model, not a reconstruction of the matching engine. Public books reveal neither queue priority nor true executable latency.

## Recorded evidence

Each `runs/run-...` directory contains:

- `manifest.json`: raw metadata, market/token identities, fees, configuration, source hashes, CPU quota and restored account state.
- `events-*.jsonl`: ordered raw frames with clocks, decisions, orders, fills, episode boundaries and balances.
- `status.json`: minute summary, rankings, accounts, inventory marks, and separate structural / dormant / fresh-price / executable-depth coverage.
- `final.json`: shutdown result. A clean run needs `recording_complete = true` and a checkpoint matching the durable terminal record; the file's existence alone proves nothing.

New recordings use manifest **format 3**. Formats 1 and 2 still verify with their original behavior.

**Reading the numbers.** `realized_usdc` is the confirmed USDC cash change of observable attempts. Residual inventory is valued at separate indicative marks; pending reservations and unresolved exposure are not profit. Episodes end on an observed failure and are censored by missing data, with no grace period inflating durations. Exchange timestamps keep millisecond resolution; receipt clocks are stored in nanoseconds, which is storage resolution, **not** nanosecond accuracy. Replay follows recorded order and processing times, never exchange-time order.

## Operations

- **Recovery** follows the durable `runs/current.json` pointer. A validated format-3 terminal checkpoint restores all accounts; unclean or legacy segments use verified prefix replay. Remapping uses token and market identities and keeps reservations, pending orders, dust and shadow depletion. Metadata and fees refresh on restart and start a new segment.
- **Blocked state.** At the recording cap, `runs/RECORDING_LIMIT_REACHED` blocks startup before discovery. Persistent recording or recovery errors leave the service waiting for an operator, with the reason in the logs and `runs/blocked.json`. Fix the cause and restart explicitly; nothing is deleted, reset or restart-looped. Bounded runs exit non-zero instead.
- **Transient outages** (connection failures, timeouts, 5xx, rate limiting) on the metadata fetch retry with capped backoff, including when Docker starts before networking. Malformed metadata is a blocking error.
- **Dust recheck.** Stop the service, then run `run --reconcile-dust 250` against the existing `runs/` (250 identifies the scenario). It waits up to 30 s for marks and journals its decision; it never edits balances or clears unresolved attempts.

Review coverage after 24 hours, and accumulate at least seven **healthy** recording days across activity conditions before drawing conclusions.

## Layout

| Path | Contents |
|---|---|
| `src/market.rs` | token/market universe, cycle enumeration, Bellman–Ford cross-check |
| `src/hot.rs` | allocation-free numeric detector and the normalizer/worker pipeline |
| `src/book.rs`, `src/hyperliquid.rs` | BBO/L2 parsing and validation; WebSocket transport and clocks |
| `src/engine.rs`, `src/paper.rs`, `src/quantity.rs` | cold evaluator, paper accounts, Decimal IOC accounting |
| `src/journal.rs` | recording, checkpoints, recovery and replay |
| `src/config.rs`, `config.toml` | configuration |
| `tests/` | research checks and the zero-allocation hot benchmark |
| `DOCS/` | [`PRD.md`](DOCS/PRD.md) (spec), [`VALIDATION.md`](DOCS/VALIDATION.md) (measurements), [`DEPTH_FEED_CHECK.md`](DOCS/DEPTH_FEED_CHECK.md) |
| `review/` | historical review evidence, including the original Binance prototype (its code has been removed) |

The executable keeps its historical name, `bellman-arb`.

## References

[Spot metadata](https://hyperliquid.gitbook.io/hyperliquid-docs/for-developers/api/info-endpoint/spot) · [WebSocket subscriptions](https://hyperliquid.gitbook.io/hyperliquid-docs/for-developers/api/websocket/subscriptions) · [Fees](https://hyperliquid.gitbook.io/hyperliquid-docs/trading/fees) · [Tick and lot size](https://hyperliquid.gitbook.io/hyperliquid-docs/for-developers/api/tick-and-lot-size) · [Order constraints](https://hyperliquid.gitbook.io/hyperliquid-docs/for-developers/api/error-responses)
