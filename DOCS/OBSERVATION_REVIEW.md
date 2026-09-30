# First coverage review — 29 September 2026

**Update, 30 September local time:** format 4 is deployed in independent epoch `bbo-ioc-v4-2026-09-30`, current run `runs/run-1790723635423981499`. The paused model-3 accounts described below remain preserved in `run-1790631982022891512`; their unknown fills and retained inventory were not cleared. Three fresh, separately labeled 10,000-USDC accounts now receive paper candidates. The ten-minute public smoke verified exactly, with five positive fee-net episodes (peak +4.4674 bps), no entries and no malformed books. The daily monitor tracks models/epochs separately. See [format-4 validation](VALIDATION.md#execution-observability-format-4-and-independent-epoch) for the failure-window diagnostics, conservation checks, timing and cutover evidence. The rest of this document preserves the first review's fixed interval and original outcomes.

**Positive fee-net price intervals have appeared, but no full arbitrage cycle has completed in paper trading. All three paper accounts are now paused on unresolved execution or retained inventory; the screener continues recording.** No balances, execution assumptions, application code or services were changed by this review.

## Interval and evidence

- Current model-3 run: `runs/run-1790631982022891512`. Fixed review prefix: first frame **2026-09-28 21:46:22.965 UTC**, last frame **2026-09-29 20:49:41.027 UTC**; **23.05875 monotonic hours**, through sequence **7,555,628**. UTC stepped during the run, so durations use monotonic time.
- This is the first roughly 24-hour review of collection: fast mode began on 28 September at 20:32 UTC. The current accounting model has less than a full day; older formats and smoke runs are not pooled into its execution results.
- Evidence: `runs/observation-2026-09-29/`. `status-reference.json` and `manifest-reference.json` freeze the review assumptions and account state. `audit-summary.json`, `candidate-episodes.json`, `eligible-episodes.json`, `paper-events.json` and `findings.json` cover **all 24 rotated parts** through that prefix, not only the first file. Per-route price/depth coverage for all 30 priced routes is in `findings.json`; all 210 structural routes remain in the saved status.
- The read-only audit checked contiguous sequence numbers, counted frames and episode starts independently, and matched the saved status: **5,895,364 frames**, **5,890,678 accepted books**, **194,008,938 route reevaluations**, **1,275 positive episodes**. Reevaluations and episodes are not independent trading opportunities.
- Original-assumption replay exited **0**, verifying every engine event through a slightly later live prefix: **23.18629 engine hours / 5,923,621 frames / 1,276 episodes**. Balances, pending attempts, holdings, pause reasons and outcome counters match the fixed review snapshot. Replay took **346.65 seconds** with a 1-CPU quota while the separate audit also ran. Its exact endpoint differs from the fixed snapshot; it is not presented as an identical-length report comparison.
- An independent Python Decimal check reconciled **five reservations and seven confirmed fills** to all final free balances and open holdings. Recorded fees, token accounting, quantity increments, fixed order limits, budgets and confirmation deadlines passed. This checks paper accounting, not actual exchange fills or matching-engine liquidity.

## Candidates and entry feasibility

The first positive episode began at **05:34:22 UTC on 29 September**, after the earlier morning check. Five directed routes had positive fee-net price intervals:

| Route from/to USDC | Episodes | Peak fee-net return | Positive route-seconds | Price coverage | Execution-depth coverage |
|---|---:|---:|---:|---:|---:|
| HYPE → USDT0 | 1 | +19.8969 bps | 0.734 | 99.82% | 33.24% |
| HYPE → USDE | 4 | +19.6909 bps | 21.270 | 99.79% | 37.74% |
| USDE → HYPE | 1,047 | +15.7183 bps | 8,432.625 | 99.79% | 37.74% |
| USDT0 → HYPE → USDE | 10 | +5.6692 bps | 13.791 | 99.79% | 62.59% |
| USDE → HYPE → USDT0 | 213 | +4.2320 bps | 264.900 | 99.79% | 62.59% |

These returns use displayed best bid/ask and configured fees. They precede size/depth costs, quantity rounding and delayed execution. Positive route-seconds can overlap across routes and must not be added as unique wall-clock opportunity time.

Only **three entry-eligibility episodes** were recorded. All were `USDC → USDE → HYPE → USDC` (`107S>150B>255B`). At their starts, only the **1,000-USDC** size cleared the strict >5-bps cash threshold:

| UTC on 29 September | Estimated cash profit at episode start | Return after modeled depth, fees and rounding |
|---|---:|---:|
| 09:49:38.621 | +0.65042016 USDC | +6.5042 bps |
| 16:15:52.663 | +0.55189311 USDC | +5.5189 bps |
| 16:16:00.181 | +0.86325500 USDC | +8.6326 bps |

These are estimates at episode onset, not fills or within-episode maxima. The third episode received no attempt because all accounts were already paused.

## Paper outcomes — independent alternatives

| Latency each way | Attempts | Observed outcome | Current state |
|---|---:|---|---|
| 100 ms | 1 | First conversion to USDE confirmed; HYPE purchase unobservable at arrival because BBO and L2 disagreed. | Paused since **09:49:39.023 UTC**. Free balance 9,000 USDC. Last-confirmed attempt holdings: 999.9699846 USDE + 0.190033 USDC; next fill remains unknown. |
| 250 ms (primary) | 2 | First attempt rejected the HYPE leg before submission, then unwound: **−0.54693142 USDC cash change**, retaining 0.0099846 USDE dust. Second attempt's first fill was unobservable. | Paused since **16:15:53.163 UTC**. Free balance 8,999.45306858 USDC + dust; **1,000 USDC remains reserved and unresolved**. |
| 500 ms | 2 | First attempt unwound with the same −0.54693142 USDC cash change and dust. Second bought USDE, then partially bought **0.17 HYPE gross / 0.169881 net**; the reverse order could not be prepared because BBO and L2 disagreed. | Paused since **16:15:54.666 UTC** with 8,999.55306858 USDC, 985.1326846 USDE and 0.169881 HYPE. |

There are **zero completed full cycles**. Both unobservable executions and all three preparation rejections report `bbo_depth_conflict`. There were two confirmed returns through unwind; the dust rule correctly allowed those accounts to continue afterward. The later pauses involve unknown execution or material inventory, not the previous tiny-dust pause problem.

For the 500-ms account, `realized_usdc = -1000.44693142` is a **cash-change counter**, not a 1,000-USDC economic loss. Its retained tokens had an indicative bid value of **999.15935540 USDC** at the snapshot, giving cash-plus-inventory change approximately **−1.28757602 USDC**. This is a mark, not realized liquidation proceeds. Do not assign settled P&L to the 100/250-ms accounts' unknown fills, and never sum the scenarios' outcomes.

**Actionable limitation:** paper execution is now censored after the pause times. Continued screening is useful, but later candidates are not receiving fresh paper attempts. Preserve the unknown orders and inventory; they cannot be reconciled by resetting funding or treating missing observations as zero fills. Review the recorded failure windows before deciding on any account reconciliation or model change. No such change was performed here.

## Follow-up: failure-window diagnosis

A read-only reconstruction of all five `unobservable` / `order_rejected` windows is saved in `runs/failure-review-2026-09-29/failures.json`. Its runnable check uses the recorded owner sequence and books admitted **before** the event-producing input; no subsequent quote changes a past arrival.

- All five failures involve a change on the order's own side. Four involve changed prices; one is a quantity-only mismatch. Merely ignoring changes on the opposite side would not have prevented these failures.
- At the 100-ms account's 09:49:38.923 UTC arrival, the latest HYPE/USDE ask was **88.483**, above the fixed IOC buy limit **88.317**, with a 15.5-ms local quote age. Under the existing local receipt-time model this supports an observed zero fill, without needing older L2 levels. The current implementation instead asks for matching L2 first and marks it unobservable.
- At the primary account's 16:15:52.913 UTC arrival, USDE/USDC's ask price remained **0.9999**. Only quantity differed: **122,081.42** in BBO versus **123,084.58** in L2, against an order of roughly **1,000** USDE. The top was 181.4 ms old and depth 250.4 ms old. Exact quantity equality discards useful current top-level evidence. A bounded top-level execution model must still enforce fixed limits, budgets, fees, lots and the existing shadow depletion.
- At the 500-ms account's failed unwind, the latest HYPE/USDE bid was **86.315 / 288.88 HYPE**, while L2 still showed **86.502 / 58.07**. Fresh top-level data might support preparing its small reverse order; using the stale higher L2 price would overstate liquidation value. This does not establish the eventual delayed unwind fill.

Recommended next change: distinguish fresh BBO-supported zero fills and bounded top-level execution from genuinely missing deeper liquidity, with side-specific validity and explicit source clocks. Never stitch changed BBO prices into old deeper levels. Validate these exact windows and a negative-edge case before any cutover; preserve format-3 replay and the existing unresolved accounts. New assumptions must be a separate model segment. Further hot-kernel optimization is not the present measurement bottleneck. No production code or accounts were changed by this follow-up.

## Data quality, performance and storage

- **210 structural routes / 29 markets; only 30 routes had usable prices.** Nine markets returned both sides empty in every observed L2 snapshot: `@232`, `@233`, `@234`, `@235`, `@250`, `@254`, `@275`, `@283`, `@294` (roughly 152,800 snapshots each). They account for the other 180 dormant routes and remain subscribed for reactivation.
- Across the 30 priced routes, fresh-price coverage was **99.803%** and execution-depth coverage **64.950%**. Across all 210 structural route-time combinations, these were **14.258% / 9.279%**. Candidate-bearing HYPE triangles had only **33–38%** depth coverage; the aggregate is helped by inactive/wide-spread routes and should not substitute for candidate-specific coverage.
- Fast L2 remained fast: HYPE (`@107`) receipt spacing **539 ms median / 872 ms p99**, exchange-time spacing **539 / 631 ms**, over **152,859 snapshots**. HYPE's maximum receipt gap was 4.332 s; later subscriptions had gaps up to about 7.18 s around reconnect/resubscription. A fresh BBO still cannot refresh missing/conflicting depth.
- **Eight server-initiated disconnects**, all recovered; **12.239 s** total Close→Open time, approximately 1.5 s each. Maximum gap between any received frames was **4.328 s**. Subscription warmup and book-level gaps are additionally reflected in route coverage.
- **Zero malformed books**, nine older observations rejected, zero container restarts, no blocked status or recording-cap marker, and no recording/overflow error in the dedicated container log. Durable status and journal writes were still fresh at the final check around **21:02 UTC**.
- Seven UTC backsteps were recorded, largest **794.622 ms**. Sequence/monotonic processing and replay stayed consistent. UTC labels do not establish exchange-clock synchronization.

| Local measurement | Median | p99 / maximum |
|---|---:|---:|
| Receipt → decision | 0.553 ms | 1.767 ms p99; 12.350 ms max |
| Queue age | 0.528 ms | 1.734 ms p99 |
| Receipt → hot result | 0.150 ms | 0.559 ms p99 |
| Cold processing | 0.022 ms | 0.170 ms p99 |
| Paper deadline dispatch lag | 1.331 ms | 1.971 ms max; only 18 samples |

The service retained **1 CPU** (`100000 100000`), used about **1.2–1.4% of one core** and **9.4–9.6 MiB** in samples. Cgroup counters showed only two throttled periods (~121 ms total). Assumed outbound/confirmation delays are separate from these local timings; actual exchange execution latency is unverified.

The reviewed journal prefix contains **4,111,266,577 bytes (3.829 GiB)**, about **3.99 GiB/day**. Project `runs/` occupied approximately **4.21 GiB** during review, leaving roughly **11.5 days** under the 50-GiB cap at that rate; host free space was about **318 GiB**. No evidence was removed. The 5m47s replay gives a first near-day estimate of unclean recovery cost, under concurrent audit load; clean terminal-checkpoint recovery does not require that replay.

## Progress toward seven healthy recording days

This current-model prefix provides about **0.9606 connected recording days**, excluding the measured disconnect intervals. That is collection progress, not seven days of complete executable coverage or uninterrupted paper trading. Older model-1/model-2 intervals are kept separate; no full seven-day milestone is claimed.

Full monotonic hours ranged from **232,267 to 279,741 frames/hour**. This demonstrates changing message activity, not established coverage of varied market-volatility regimes. Continue the daily follow-up and public screener. The next reviews should track candidate-specific book coverage, remaining storage and paused-execution censoring. The earlier "no positive event observed" conclusion no longer describes the full dataset; equally, positive quoted cycles and reproducible paper calculations do not establish a tradable live edge.

### Bounded real execution evidence, 30 September 2026

The separately authorized session `runs/live/spot-live-20260930-1350` completed both requested 50-USDC triangles and an interrupted first-leg recovery/unwind. It used eight real fills, with an independently conserved token ledger. Its net change including fresh indicative dust marks was approximately −0.286 USDC. Forced tests bypassed profitability only and are excluded from screened-opportunity or paper-performance claims. Native activity occurred in the public collection interval beginning 13:50 UTC and during cleanup around 13:58 UTC; retain this annotation when interpreting quote episodes.

The real subaccount retains only confirmed dust and 71.50019996 USDC; no open orders, unresolved attempts or live runner remain. Production live trading was not started. The public screener and all three paper scenarios continued unchanged. See `DOCS/VALIDATION.md` for timings, native exposure between the test processes, recording paths and the earlier zero-loss filesystem/freshness failure.