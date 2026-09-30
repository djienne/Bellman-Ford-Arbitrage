# Hyperliquid arbitrage prototype: detailed fix plan

Prepared 28 September 2026. Historical planning rationale, retained as review evidence. Later approvals added the optimized hot path, paper formats 3–5 and optional bounded real execution. [PRD.md](PRD.md), [README.md](../README.md), and [VALIDATION.md](VALIDATION.md) supersede this plan's original scope, proposed file layout and rollout order.

**Confirmed scope: Hyperliquid spot conversion cycles, with public recording and replay before trading.** The user selected spot conversion cycles. A perpetual position is not a conversion into the underlying token; the existing perpetual discovery and execution adapters will not be used as spot conversion logic.

**Recommended design:** enumerate the actual spot graph's directed simple cycles of three to six legs once at discovery, evaluate the affected routes on each accepted book update, and use a corrected, from-scratch Bellman-Ford implementation as an independent diagnostic. This explicitly replaces the old PRD's Bellman-Ford-only detection requirement and addresses missed routes and threshold selection. Public Hyperliquid measurement comes before the extensive cleanup and quantity/replay implementation below.

## 1. Evidence and scope

The public `spotMetaAndAssetCtxs` response observed at **2026-09-28 17:46:53 UTC** is saved in `review/hyperliquid_spot_snapshot.json`. It contained:

| Quantity | Observed count |
| --- | ---: |
| Token metadata entries | 503 |
| Listed spot markets | 330 |
| Distinct assets appearing in those markets | 317 |
| USDC / USDH / USDT0 / USDE quoted markets | 313 / 11 / 5 / 1 |
| Asset-context entries | 885 |
| Assets / markets participating in simple cycles of length 3–6 | 16 / 29 |
| Directed 3 / 4 / 5 / 6 leg cycles, rotations deduplicated | 28 / 90 / 44 / 48 |
| Total directed cycles within that bound | 210 |

The topology count was checked two ways: leaf pruning plus canonical traversal, and an independent full-graph traversal deduplicating rotations in a Docker check. These are structural candidates, not measured profitable or liquid routes. Listings and counts can change.

One verified example of listed topology is USDC -> HYPE -> USDT0 -> USDC, using spot markets `@107`, `@207`, and `@166`. USDH provides another HYPE triangle through `@232` and `@230`. No stablecoin parity or wrapped-token equivalence is assumed.

The separate XEMM repository was inspected at commit `30b3132e5e3c1c5239f84ce349f49ace63ec1951`, with a clean working tree. Its current public-feed code is useful, but its discovery and order adapters target perpetuals. The plan changes only this prototype; it does not modify, rebuild, restart, or import credentials from that operational stack.

Deliverable scope is a trustworthy public-data research instrument: discovery, recording, bounded candidate detection, quantity estimates, and receipt-ordered replay. Signing, order submission, live balances, transfers, and an automatic executor are outside this fix. They require a later execution design supported by the recorded evidence.

The supplied Binance pilot report and analysis source were also inspected. They report 661,675 observations over 599 seconds, 51 markets, 24 currencies and 542 three/four-leg cycles. No cycle was positive with uniform 10-bps or 7.5-bps fees per leg; the best net readings were -13.87 and -6.36 **log-return basis points** respectively. The sign is unaffected by converting to ordinary return basis points. This is evidence against that sampled Binance taker configuration, not a conclusion about all periods, pair-specific fee schedules, or Hyperliquid. This planning pass inspected the report and computation but did not independently replay its 60 MB recording.

Two concrete lessons from the pilot belong in this implementation: it recorded four wall-clock backsteps, reinforcing the need for monotonic timing; and its largest gross gap had a maximum local quote age of only 4 ms, so quote age alone does not establish that positive gaps are stale artifacts. The report's size is a minimum of midpoint-valued leg quantities, not a full fee/rounding-aware inventory simulation. These limits do not overturn its reported absence of positive fee-net rates.

Pilot source location supplied by the user: `C:/Users/david/AppData/Local/Temp/claude/C--Users-david-Desktop-freqtrade-bellman-ford/d38ef9a9-14a9-45b9-b807-a19c578e5dd0/scratchpad/live/` (`report.txt`, `analyze.py`, `record.py`, `universe.py`, `ticks.csv`). This temporary folder is not a durable production data location. Do not extend Binance collection as part of the Hyperliquid migration.

## 2. Remove Binance completely from the maintained application

Remove `src/collectors/binance.rs`, `examples/binance_test.rs`, its Cargo example entry, and the two bundled Binance API text files. Replace Binance setup, endpoints, examples, terminology, and defaults in README, PRD, configuration comments, and tests.

Make Hyperliquid the only collector. Remove the exchange factory/trait if there is no remaining second implementation; a concrete collector is sufficient. Remove `quote_filter`, Binance hub defaults, Binance volume-selection logic, and regex-like blacklist promises that were only substring matching. Reject obsolete configuration keys with an actionable migration error rather than silently accepting a Binance configuration.

Retain the books, supplied notes, and review evidence as reference material. References to old defects in historical review material are not runtime support. No compatibility switch or dormant Binance code remains.

Remove dependencies only after checking every caller. `simd-json` is currently unused. Remove `crossbeam-utils` if removing the custom ring buffer removes its last use; remove `async-trait` if the collector trait is removed. Refresh the lockfile with the final dependency set. Reuse the existing HTTP, WebSocket, JSON, Tokio, and logging libraries.

**Acceptance:** executable source, active configuration, and maintained run instructions contain no Binance endpoint or collector path; the program starts only the Hyperliquid public collector.

## 3. Adapt the existing Hyperliquid feed, with correct spot discovery

Reuse narrowly from these files in `C:/Users/david/Desktop/freqtrade/XEMM/CROSS_EXCHANGE_MARKET_MAKING_LIGHTER_ASTER`:

| Existing source | Reuse | Adaptation required |
| --- | --- | --- |
| `SCREENER/src/hyperliquid.rs` | Multi-market coin routing, BBO prices/sizes, nullable sides, exchange time, explicit acknowledgement handling, rejection of older books | Preserve the complete L2 snapshot rather than reducing it to the first level; remove trade-processing machinery not needed here |
| `SCREENER/src/feeds.rs` | Application ping, read/write/connect timeouts, reconnect backoff, session-open/close events | Hyperliquid-only subscription handling; do not copy Aster/Lighter branches or their rate settings |
| `LIGHTER_ASTER_BOT/src/connectors/hyperliquid.rs` | BBO/L2 separation, stream-down invalidation, time validation, reconnect lifecycle | Remove `Tap`, hot-book projections, order-engine coupling, and single-market routing assumptions |

Adapt these small pieces into `src/collectors/hyperliquid.rs`; do not create a cross-repository shared crate or depend on the whole bot. Keep the source revision and reuse locations in a short module comment. Port the relevant feed fixtures and extend them for spot IDs.

Discovery uses `spotMeta` or `spotMetaAndAssetCtxs`, not `metaAndAssetCtxs`. Construct token identities from explicit token indices/token IDs, with names used only for display. Construct markets from explicit market indices and their two token references. Do not merge tokens because their symbols match or map UBTC to the BTC perpetual.

The observed context array has 885 entries while the universe has 330: never blindly zip them. Join contexts by their returned `coin` identity, validate uniqueness, and ignore contexts absent from the selected spot universe. An explicit index mapping may be used only with identity checks. Unknown metadata references are errors, not guessed assets.

For subscriptions use the documented spot coin representation: `PURR/USDC` for that special market and `@{market_index}` for the others. Token index, market index, and future order asset ID are distinct concepts.

Discover all markets before any liquidity selection. Build the cycle inventory and subscribe to every market used by the selected cycles, including low-volume bridge pairs. Starting with top-N standalone USDC pairs would recreate the current disconnected-cycle problem. Report full-universe and subscribed coverage separately. In the observed snapshot this reduces the useful subscription set to 29 markets without losing a 3–6-leg topological candidate.

Metadata is fixed within a recording segment. A refresh starts a new segment and rebuilds routes; do not change index meanings beneath active state. If the selected graph has no cycles, report that explicitly. Missing or empty books make the affected routes unavailable.

**Acceptance:** replay the saved metadata fixture, resolve all selected identities, recover the observed 210 routes, and decode spot BBO/L2 messages with correct prices, quantities, coin IDs, and timestamps.

## 4. Use one owner for market state and remove unnecessary concurrency

Keep one collector task, one serial evaluation/state owner, and the existing logging/writer responsibility. Use Tokio's existing bounded channel between collector and evaluator. Remove the custom unsafe SPSC ring buffer, unused overwrite policy, concurrent price atomics, cold-discovery thread, candidate double buffer, and lifetime relaxation counters.

The evaluator waits for input or a freshness timer instead of busy-spinning. It processes accepted observations in receipt order. Initially evaluate each observation, without coalescing away transient opportunities. If later batching is necessary, label that mode and measure its observation loss.

A book update includes market identity, stream kind, bids/asks or BBO, exchange timestamp, local receipt UTC, local monotonic receipt time, and connection generation. State ownership provides an internally consistent view; it does not imply that all markets were simultaneously available at the venue.

Do not silently drop market observations. For this research prototype, input or recording overflow terminates the affected recording segment as incomplete and ends opportunity observations as unknown. Supervise task failure so the evaluator cannot continue treating a dead collector's cache as live. This is simpler than introducing a complex lossy recovery protocol.

Keep BBO and L2 timestamps/provenance separately. An older L2 snapshot must not overwrite a newer BBO. A fresh BBO must not refresh the age of old depth. A newer BBO cannot simply be pasted onto stale lower levels and called a current full book.

Invalid numeric prices or sizes, crossed markets, null sides, and a disconnect immediately invalidate affected routes. Zero-size levels cannot supply liquidity. Permit a locked market only under an explicit, tested validity rule; it must never manufacture profit after nonnegative fees. A malformed frame associated with a known market invalidates that market; an unidentifiable book corruption invalidates the session.

Track socket health separately from market-data health. Pongs show connection liveness, not fresh depth. Use monotonic receipt time for elapsed durations and timers; retain exchange time for ordering and source-age diagnostics. Do not subtract exchange and local clocks as an exact latency measurement without a clock-offset assumption. The old frozen `last_ts_ns` clock must disappear from freshness evaluation.

Measure spot BBO/L2 cadence during the public smoke run and record it. Do not copy the XEMM connector comment's observed 5.3-second cadence as a protocol guarantee. Choose freshness limits from that evidence and evaluate their sensitivity in replay.

## 5. Correct route discovery and profit decisions

Build directed edges from real markets and enumerate simple directed cycles of length 3–6 once per metadata segment. Normalize rotations only; reverse directions remain separate opportunities. A route ID derives from the ordered directed market identities, not just currency names or vector positions. Preserve multigraph edge identity if future duplicate base/quote markets exist; the current snapshot has none. A normal buy/sell round trip in the same market is a no-profit invariant, not a candidate opportunity.

Use a flat route vector and a market-to-route index. On any price, size, health, or fee change, reevaluate every affected route. This also makes timing independent of repeated BF rediscovery. Preallocate from the actual route count; there is no 128-signature lifetime store and no arbitrary 100-candidate truncation.

This is complete only within the selected graph and declared length bound. Never claim coverage of longer cycles. If topology grows beyond a configured startup memory/work cap, report the coverage limitation and stop or require a smaller explicitly selected universe; never silently truncate candidates. The code comment should state this ceiling and that profiling comes before adding more complex search machinery.

For the proportional received-asset fee model:

`SELL rate = bid * (1 - f_sell)`

`BUY rate = (1 - f_buy) / ask`

`cycle return = exp(-sum(edge_weight)) - 1`

Use per-edge fees, compute the unrounded return with numerically stable functions such as `exp_m1`, and compare it directly with the configured economic threshold. Preserve the PRD's strict comparison: `return > min_profit_bps / 10000`. Equivalently compare the weight sum against `-ln_1p(threshold)`. Keep the numerical near-zero tolerance distinct. Integer basis-point rounding is display-only.

Retain a small fresh full-pass BF implementation for offline diagnostics: initialize every distance to zero, relax complete passes on immutable weights, use the Vth-pass criterion, update predecessors, extract a forward connected cycle, and directly verify its weight. Remove the existing incremental implementation and unused decay/reset APIs. BF is not the route-ranking, threshold-completeness, or all-cycle oracle. On small graphs whose possible cycles all fit the bound, compare its zero-threshold existence result with independent enumeration; on larger graphs, a longer BF cycle is not a contradiction of the bounded detector.

**Acceptance:** both overlapping profitable triangles are emitted; an ordinary moving single-market quote never emits profit; outcomes do not depend on batching/chunk boundaries; all extracted routes connect and close; 4.6 bps fails a 5-bps threshold; every route within the selected bound remains observable beyond 128 distinct identities.

## 6. Model fees and finite quantities explicitly

Do not carry over Binance's 10-bps default or the XEMM perpetual fee. Use a dated Hyperliquid spot fee assumption. The currently documented undiscounted base-tier spot taker rate is 7 bps, with separate quote-pair/aligned-quote adjustments. Represent fees per market/direction as fractional basis points. Account discounts may be supplied explicitly or read from `userFees` if an account address is later provided; the public collector needs no credentials.

Implement the current documented fee formula once. Apply only verified market classifications and avoid double-counting discounts. If a discount classification is uncertain, label the conservative undiscounted scenario rather than claiming exact account-net profit. Verify the fee asset and fee rounding convention before labelling quantity results executable; the graph's received-asset formula is an explicit assumption until reconciled with the venue convention. A deployer fee share must not automatically be interpreted as an additional trader surcharge.

Add a small route quantity evaluator. Start with USDC-funded cycles and configured notionals such as 25, 100, 250, and 1,000 USDC. Other starting assets require separate balance and denomination assumptions. Cycle identity is rotation-invariant, but finite-size evaluation uses the actual starting asset and order sequence.

For each leg, consume the correct side of available depth, apply the base-token size increment, fees, price precision for the modeled order bound, and minimum notional in that market's quote token. Current documented spot minimum-notional errors refer to **10 units of the quote token**, not universally 10 USD. Check every leg after fee and size rounding. Keep residual dust and intermediate balances; never round up available inventory to make a later leg feasible.

BBO-only sizing is explicitly limited to the displayed top-level quantities. L2-backed estimates use independently age-checked snapshots and report their resolution. If newer BBO data conflict with the L2 used for size, report depth as indeterminate until a valid snapshot is available. Do not extrapolate liquidity beyond the observed levels.

Use `f64` for graph/log calculations and exact decimal accounting for the small number of quantity simulations. Reusing `rust_decimal` from the existing Rust stack is the one justified additional dependency if needed; do not implement a bespoke decimal library. It does not require importing the trading bot.

Report gross return, modeled fee-net return, size-feasible return, starting/final amount, each fee asset/amount, and leftover inventory separately. Do not add profits of competing routes that consume the same liquidity or capital. Passing this estimator is still not proof of fillability.

## 7. Make opportunity lifetimes and records scientifically usable

Keep a state entry per enumerated route. Record first positive observation, last positive observation, first observed threshold failure, maximum unrounded return, maximum feasible starting size, and termination reason. Reevaluate active routes on every affected book update and on freshness timers.

A known threshold failure ends the positive observation interval immediately. Feed loss, stale depth, missing data, or an incomplete recording marks the interval's end unknown/censored. A grace period may group nearby episodes for presentation, but it must not bridge unprofitable time in the reported positive duration. Label observation-based intervals accurately rather than claiming continuously executable venue lifetime.

Replace opaque-only logs with readable route names plus stable IDs, leg market IDs/directions, prices/sizes, quote provenance and ages, fee assumptions, start notional, and status/rejection reason. Keep raw BF notifications, positive rate cycles, size-feasible candidates, and event counts as distinct statistics. Count a transition or a time-weighted interval, not every repeated detector call as a new opportunity.

Record raw book messages, receipt ordering/times, connection events, metadata, fee/config assumptions, and software revision once per segment. Use a buffered append-only format such as JSONL with existing serde support; no database, dashboard, or new service is needed. Rotate files by time or size. A recorder failure is explicit and the segment remains incomplete. Save enough data to rerun the same detector rather than only saving positive alerts.

## 8. Replay and evaluate the trading hypothesis

Use the same parser, normalized events, state transitions, detector, and quantity evaluator for live collection and replay. Replay follows recorded receipt order and a virtual monotonic clock, including quiet periods and disconnects. Preserve each channel's ordering; never reorder observations by exchange timestamp to give the detector information it did not have at the time.

First validate the collector path with a short public collection of the discovered cycle markets, including at least one intentional reconnect. Then record a 24-hour coverage pilot and run a small exact bounded-cycle analysis with the declared Hyperliquid fee scenarios. This early measurement should precede the full detector/tracker rewrite and advanced quantity replay. Since rare events are the hypothesis, plan at least seven healthy recording days across differing activity conditions after the pilot, and extend when the number of independent episodes remains insufficient. Seven days is an initial observation horizon, not a statistical guarantee. A quiet ten minutes or day is not an automatic stop condition.

Report effective jointly observable time per route, zero-event exposure, and independent episode counts. Thousands of quote updates within one opportunity are not thousands of independent observations. If presenting a zero-event rate bound such as approximately `3 / observed_time` at 95% confidence, state that it assumes a stationary Poisson arrival process; clustered or regime-dependent arbitrage may violate that assumption. Do not use it unqualified. Adding realistic costs cannot rescue the same instantaneous negative-return conversion under unchanged prices and fees, but a negative sample does not exclude future rare dislocations.

Evaluate a declared per-leg latency grid, initially 0/25/50/100/250/500 ms. These are sensitivity scenarios, not measured order latency. Simulate sequential leg arrival and carry inventory through failures. Use only the book state available at each simulated time; report insufficient snapshot resolution or coverage instead of inventing fills between observations. Zero latency is an optimistic bound. A failed leg leaves exposure and an unwind estimate rather than magically restoring starting cash.

Report time-weighted coverage per route, missing/invalid books, reconnect gaps, independent opportunity episodes, duration distributions, return-versus-size curves, fee and latency sensitivity, and incomplete-route exposure. Tick-based recall from the synthetic experiments remains an engineering diagnostic; it is not the economic performance metric. State whether the data support further work, and distinguish insufficient observation from an observed absence of size-feasible opportunities under the tested assumptions.

## 9. File changes and deletion targets

| Area | Planned action |
| --- | --- |
| `src/collectors/binance.rs`, Binance example/API text files | Delete |
| `src/collectors/hyperliquid.rs` | Add the adapted public spot collector and parser fixtures |
| `src/collectors/mod.rs` | Reduce to the one concrete supported collector |
| `src/config.rs`, `config.toml` | Hyperliquid discovery, cycle bound, fee assumptions, starting notionals, freshness, recording; reject obsolete fields |
| `src/types.rs`, `src/graph/builder.rs` | Stable spot token/market identities, quantities, source/receipt times; retain useful graph representation |
| `src/graph/cycles.rs` | Small startup cycle enumeration and market-to-route index |
| `src/bellman_ford/hot.rs`, `cold.rs` | Replace with one correct offline reference; remove hot/cold thread and candidate infrastructure |
| `src/bellman_ford/cycle.rs`, `weights.rs` | Keep useful numerical logic; correct forward extraction, identity, fractional fees and threshold handling |
| `src/state/price_store.rs`, `opportunity_store.rs`, `src/timing/tracker.rs` | Serial book ownership, route-indexed lifecycle state, explicit observation/censoring semantics |
| Small quantity/replay modules | Add only the actual conversion estimator and shared replay entry point; no general execution framework |
| `src/ring_buffer/` | Delete when replaced by the existing runtime channel |
| `src/main.rs`, `logging/`, `stats.rs`, `lib.rs` | Wire the simple owner loop, supervised feed/writer, records and distinct scientific counters |
| `benches/`, `review/check.rs` | Replace obsolete implementation-specific checks with behavior regressions; benchmark changing real-sized books |
| `README.md`, `DOCS/PRD.md`, `Cargo.toml`, `Cargo.lock` | Document the actual contract, remove obsolete machinery/dependencies, provide Docker collection/replay commands |

The new module list is a ceiling, not a demand for wrappers: use existing files when they already contain the same scientific responsibility.

## 10. Implementation order and completion criteria

1. **Correct the specification and remove Binance.** Set the explicit spot, bounded-cycle, observe-only contract. Remove obsolete configuration and runtime branches. Preserve review evidence.
2. **Implement discovery and public recording.** Adapt the proven feed handling, verify spot identity joins, quantities, time handling, invalidation and reconnects. Capture actual market data before extensive strategy machinery.
3. **Measure Hyperliquid rare-event occurrence first.** Run the 24-hour coverage pilot with exact bounded-route recomputation and explicit spot fee scenarios, then plan at least seven healthy recording days. Report coverage, independent episodes, fee-net rate bounds and quote-size limits. If no events appear, distinguish insufficient observation from evidence against a stated economically useful event frequency. This evidence guides further strategy engineering; a quiet day alone does not terminate the study.
4. **Replace the detector and tracker together.** Build route inventory, update affected routes, use exact economic threshold comparisons, and remove the BF/ring/atomic scaffolding made unnecessary by serial ownership.
5. **Add quantity accounting and replay.** Reuse the production path; validate fees, rounding, balance conservation, partial exposure and realistic observation limits.
6. **Assess the recorded results under execution assumptions.** Produce the coverage, liquidity, latency and opportunity evidence needed to decide whether an executor is worth building.

Run builds, checks, replay and benchmarks through this project's Docker Compose configuration, with release-mode Cargo and the lockfile. Do not use the XEMM bot's Compose services for this prototype. Pin the tested Rust image; record the effective CPU quota used for performance results. Do not quietly inherit a fleet quota and then publish unconstrained timing claims.

The compact regression suite must cover: token-name collisions and mismatched context-array lengths; no-arbitrage star graph; valid non-crossed profitable triangle; overlapping routes; long-cycle bound; forward route continuity and rotation identity; fractional fee/threshold boundaries; invalid/empty/stale/out-of-order BBO and L2; pongs versus book health; disconnect/reconnect generation; more than 128 route identities; true observation end versus grace grouping; quantity/fee conservation, lot rounding, per-leg minimum notional and residual dust; replay equality under the same receipt sequence; explicit recorder/channel failure.

Use independent raw bid/ask multiplication for the rate oracle and hand-computable quantity cases for the ledger oracle. Replace the old failing checks when their implementation is removed; preserve the failure scenario, not assertions about obsolete private state. All production paths must also run on representative recorded Hyperliquid messages.

Benchmark changed-price and changed-size events, multiple simultaneously profitable routes, quiet periods and bursty ingestion. Measure p50/p95/p99 receipt-to-decision, queue age, throughput, CPU, recorder lag and data loss. Treat 150 microseconds as a hypothesis to measure, not a reason to compromise correctness. Optimize only a measured bottleneck after these checks pass.

Completion means Binance is removed, the selected bounded universe is completely evaluated, quote/fee/size assumptions are explicit, lifecycle records are interpretable, and live-collected data replay through the same engine. It does not mean profitability or safe autonomous execution has been established.

## Primary references checked for this plan

- [Hyperliquid spot metadata](https://hyperliquid.gitbook.io/hyperliquid-docs/for-developers/api/info-endpoint/spot)
- [Spot versus perpetual coin identifiers](https://hyperliquid.gitbook.io/hyperliquid-docs/for-developers/api/info-endpoint)
- [WebSocket subscriptions and payloads](https://hyperliquid.gitbook.io/hyperliquid-docs/for-developers/api/websocket/subscriptions)
- [Asset IDs](https://hyperliquid.gitbook.io/hyperliquid-docs/for-developers/api/asset-ids)
- [Current fee schedules and formula](https://hyperliquid.gitbook.io/hyperliquid-docs/trading/fees)
- [Price and size precision](https://hyperliquid.gitbook.io/hyperliquid-docs/for-developers/api/tick-and-lot-size)
- [Order rejection rules, including quote-token minimum notional](https://hyperliquid.gitbook.io/hyperliquid-docs/for-developers/api/error-responses)
- [Connection and request limits](https://hyperliquid.gitbook.io/hyperliquid-docs/for-developers/api/rate-limits-and-user-limits)

Local evidence: the original `review/check.rs` (six reproduced failures), now stored as `check.rs` inside `review/prototype-before-hyperliquid.zip`; `review/hyperliquid_spot_snapshot.json`; the supplied review experiments; and the inspected XEMM revision above. The supplied books and notes explain the graph model, not executable market profit.
