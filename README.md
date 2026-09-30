# Bellman-Ford Arbitrage Screener (Hyperliquid spot)

This is an **arbitrage opportunity screener and paper trader** for Hyperliquid spot markets, built on the **Bellman–Ford** view of currency arbitrage: tokens are graph nodes, every market is a pair of directed edges weighted by `−ln(rate × (1 − fee))`, and a round trip that ends with more than it started with is a **negative-weight cycle**. It watches the public WebSocket feed, finds fee-positive conversion cycles of 3–6 trades (e.g. `USDC → HYPE → USDT0 → USDC`), and simulates executing them against recorded order books with realistic latency, depth, lot sizes and fees.

**Paper by default.** Continuous collection needs no credentials and sends no orders. An optional live feature provides explicitly armed, bounded spot execution tests and finite live sessions. Production live trading is disabled; the continuous service remains the paper screener.

> **Status (30 Sep 2026 local time):** format 4 is running in independent epoch `bbo-ioc-v4-2026-09-30` with three unpaused 10,000-USDC accounts. The old paused accounts remain preserved. The first model-3 coverage review found 1,275 positive fee-net episodes, three entry-eligibility episodes and five attempts, with no completed full cycles. The format-4 ten-minute smoke and historical replays passed. See [current validation](DOCS/VALIDATION.md#execution-observability-format-4-and-independent-epoch) and [observation review](DOCS/OBSERVATION_REVIEW.md). Counterfactual results are separate from observed paper outcomes; live fill profitability remains unverified.

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
# Counterfactual comparison on fresh accounts; cannot be combined with --verify
docker compose run --rm screener replay runs/run-... --execution-model 4
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
- **Books.** Actual bids/asks and all received levels, with separate BBO/L2 clocks. A fresh traded-side quote outside the fixed IOC limit supports a zero fill. Fresh BBO can support an entire order at the best price; otherwise execution requires coherent traded-side L2. Changed BBO prices/quantities are never stitched into old deeper levels. Unknown deeper remainders are **unobservable**, while a partial fill is observable when the fixed limit excludes all unseen worse prices. Opposite-side updates alone do not invalidate an order. A fresh BBO does not refresh L2; stale data, crosses and disconnects invalidate evidence.
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

New recordings use manifest **format 4**, including source clocks and `bbo:top`, `bbo:zero` or full-L2 execution evidence. Formats 1–3 still verify with their original behavior. Full-L2 coverage excludes BBO-only quantity; successful size estimates identify their execution sources. `--execution-model 3|4` uses fresh comparison accounts and labels the result counterfactual.

**Reading the numbers.** `realized_usdc` is the confirmed USDC cash change of observable attempts. Residual inventory is valued at separate indicative marks; pending reservations and unresolved exposure are not profit. Episodes end on an observed failure and are censored by missing data, with no grace period inflating durations. Exchange timestamps keep millisecond resolution; receipt clocks are stored in nanoseconds, which is storage resolution, **not** nanosecond accuracy. Replay follows recorded order and processing times, never exchange-time order.

## Operations

- **Recovery** follows the durable `runs/current.json` pointer. A validated format-3/4 terminal checkpoint restores all accounts; unclean or legacy segments use verified prefix replay. Remapping uses token and market identities and keeps reservations, pending orders, dust and shadow depletion. Metadata and fees refresh on restart and start a new segment.
- **Independent paper epochs.** After a clean shutdown, `run --new-paper-epoch ID` explicitly starts separate 10,000-USDC accounts and shadow ledgers. The manifest and terminal checkpoint identify the epoch and predecessor run; old accounts and unresolved exposure remain in their original evidence. Reusing the current ID restores balances without adding funds; older IDs are rejected. Ordinary restarts continue the current epoch. Never pool account outcomes across epochs or latency scenarios.
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

## Bounded real spot execution

The default build excludes signing. Build the optional feature explicitly for one-off tests:

```powershell
docker compose build --build-arg FEATURES=live screener
docker compose run --rm --no-deps -T -v ./hyperliquid.env:/run/secrets/hyperliquid.env:ro screener live check
.\live_preflight.ps1
docker compose run --rm --no-deps -T -v ./hyperliquid.env:/run/secrets/hyperliquid.env:ro screener live validate --residual-check --session spot-residual-ID --allow-real-orders
# The targeted check completes both triangles and immediate residual cleanup.
# The original comprehensive recovery test is separate:
.\live_preflight.ps1
docker compose run --rm --no-deps -T -v ./hyperliquid.env:/run/secrets/hyperliquid.env:ro screener live validate --session spot-validation-ID --allow-real-orders
# Validation deliberately exits after a confirmed first leg to test recovery.
docker compose run --rm --no-deps -T -v ./hyperliquid.env:/run/secrets/hyperliquid.env:ro screener live reconcile --session spot-validation-ID
.\live_preflight.ps1
docker compose run --rm --no-deps -T -v ./hyperliquid.env:/run/secrets/hyperliquid.env:ro screener live cleanup --session spot-validation-ID --allow-real-orders
```

Use a fresh ID once. Reusing it cannot fund another account or repeat validation. Credentials stay local, ignored and mounted read-only; they are absent from the continuous service. These commands operate on **mainnet real money**, even though the chosen subaccount is used for testing. The sibling connector is reference code only; the preflight script only reads its Docker state.

Live accounting version 2 pools confirmed strategy leftovers, sizes intermediate buys backward from downstream lots, and includes opening non-USDC inventory in the loss baseline. Live ranking uses the same plan as execution; selling old inventory is not counted as new arbitrage profit. Paper formats 1–4 keep their recorded assumptions. Cleanup sells every remaining whole lot directly to USDC using `FrontendMarket` with the existing 50-bps limit; its sub-$10 exception is restricted to confirmed-inventory sells. Each token gets at most two cleanup submissions, with a durable 120-second deadline and no retry of unknown orders. Only confirmed sub-lot balances may remain, marked separately at fresh bids. Their dollar value depends on the token's lot size, not a fixed dust allowance.

The targeted residual check deliberately retains one extra HYPE lot from the second triangle's closing IOC, then immediately sells it through the cleanup path. It does not repeat the earlier interrupted-first-leg test. A failed cleanup blocks further testing. No extra purchases are made just to dispose of sub-lot dust.

Validation permits 50-USDC opening allocations, a 5-USDC session loss stop, 32 signed actions with unwind reserves, two attempts per required triangle and a 30-minute entry deadline. It tests `USDC → HYPE → USDT0 → USDC` and `USDC → USDE → HYPE → USDC`, post-only cancellation, a non-crossing IOC, deliberately lost acknowledgement, and an interrupted first leg followed by explicit cleanup. Forced losses are diagnostic; they are not observed profitable arbitrage. Stops prevent new entries, not guaranteed maximum loss.

Real legs use actual confirmed fills, fee tokens and reconciled spot balances, without simulated latency. Unknown orders are reconciled by their original IDs and never resent. Version-2 cleanup permits two submissions per token within a durable 120-second deadline; legacy sessions retain their reverse-path recovery. Confirmed sub-lot residuals remain inventory; marks use fresh direct-USDC bids and are never spendable. Unknown marks or remaining whole lots block new trading. The shared XEMM key may be used only while its live trading is inactive; a per-signer file lock prevents duplicate owners in this project. Foreign orders, fills or unexplained balance changes block new trading.

Evidence is separate in `runs/live/<session>/`: durable intents, responses, actual fills, balance checkpoints and an observer-only public/account tape. Its tape can be verified with ordinary `replay --verify`; it cannot create counterfactual paper accounts. Live recovery never advances an old forward route automatically. Storage failure stops inventory orders; only cancellation of this session's resting orders is attempted without a durable intent, leaving recovery explicitly unresolved.

`live run` is available for a future **explicitly authorized finite session**, requiring `--session`, `--allow-real-orders`, `--routes`, `--amount-usdc`, `--loss-usdc` and `--duration`. It retains the five-bps cash threshold and route ranking, with one real account and one attempt per eligibility episode. Size is bounded to 12–50 USDC, loss stop to at most 5 USDC and duration to at most 1,800 seconds. It is not installed as a service and is not started by Compose. The public screener and its three paper accounts continue independently.
