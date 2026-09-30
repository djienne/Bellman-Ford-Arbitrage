# First coverage review — 29 September 2026

**Latest update, 30 September 22:08 UTC:** model 5 continues the same funding epoch. All three accounts completed confirmed-inventory cleanup and are available again, with only sub-lot residuals and no pending orders. Balances were preserved without fresh funding. See the [cutover below](#model-5-cutover-30-september-2026-2207-utc).

**30 September 20:49 UTC, model-4 cutoff:** its first positive completed paper cycle appeared in the 100-ms scenario, but all three accounts were then paused with confirmed inventory. See the [dated follow-up](#30-september-2026-follow-up); historical model-3 outcomes remain separate.

**Format-4 cutover, 30 September local time:** independent epoch `bbo-ioc-v4-2026-09-30` started with three separately funded 10,000-USDC accounts; `runs/current.json` identifies its latest segment. The paused model-3 accounts below remain preserved in `run-1790631982022891512`. The ten-minute format-4 smoke verified exactly, with five positive fee-net episodes (peak +4.4674 bps), no entries and no malformed books. See [format-4 validation](VALIDATION.md#execution-observability-format-4-and-independent-epoch) for the diagnostics and cutover. The review below describes the fixed model-3 interval; the final section separately records the bounded real tests.

**At the 29 September model-3 cutoff, positive fee-net price intervals had appeared, but no full paper cycle had completed. All three model-3 accounts were paused on unresolved execution or retained inventory.** No balances, execution assumptions, application code or services were changed by that review.

## Interval and evidence

- Reviewed model-3 run: `runs/run-1790631982022891512`. Fixed review prefix: first frame **2026-09-28 21:46:22.965 UTC**, last frame **2026-09-29 20:49:41.027 UTC**; **23.05875 monotonic hours**, through sequence **7,555,628**. UTC stepped during the run, so durations use monotonic time.
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

**Model-3 limitation:** execution observations are censored after those pause times. Later candidates in that epoch received no fresh paper attempts. Unknown orders and inventory remain preserved; format 4 was evaluated on the recorded failure windows and deployed as a separate epoch, as linked above.

## Follow-up: failure-window diagnosis

A read-only reconstruction of all five `unobservable` / `order_rejected` windows is saved in `runs/failure-review-2026-09-29/failures.json`. Its runnable check uses the recorded owner sequence and books admitted **before** the event-producing input; no subsequent quote changes a past arrival.

- All five failures involve a change on the order's own side. Four involve changed prices; one is a quantity-only mismatch. Merely ignoring changes on the opposite side would not have prevented these failures.
- At the 100-ms account's 09:49:38.923 UTC arrival, the latest HYPE/USDE ask was **88.483**, above the fixed IOC buy limit **88.317**, with a 15.5-ms local quote age. Under the local receipt-time model this supports an observed zero fill, without needing older L2 levels. The reviewed format-3 implementation instead required matching L2 and marked it unobservable; format 4 corrects this.
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

The service retained **1 CPU** (`100000 100000`), used about **1.2–1.4% of one core** and **9.4–9.6 MiB** in samples. Cgroup counters showed only two throttled periods (~121 ms total). Assumed outbound/confirmation delays are separate from these local timings; this public-data review did not measure actual order execution latency.

The reviewed journal prefix contains **4,111,266,577 bytes (3.829 GiB)**, about **3.99 GiB/day**. Project `runs/` occupied approximately **4.21 GiB** during review, leaving roughly **11.5 days** under the 50-GiB cap at that rate; host free space was about **318 GiB**. No evidence was removed. The 5m47s replay gives a first near-day estimate of unclean recovery cost, under concurrent audit load; clean terminal-checkpoint recovery does not require that replay.

## Progress toward seven healthy recording days

This reviewed model-3 prefix provides about **0.9606 connected recording days**, excluding the measured disconnect intervals. That is collection progress, not seven days of complete executable coverage or uninterrupted paper trading. Older model-1/model-2 intervals are kept separate; no full seven-day milestone is claimed.

Full monotonic hours ranged from **232,267 to 279,741 frames/hour**. This demonstrates changing message activity, not established coverage of varied market-volatility regimes. Continue the daily follow-up and public screener. The next reviews should track candidate-specific book coverage, remaining storage and paused-execution censoring. The earlier "no positive event observed" conclusion no longer describes the full dataset; equally, positive quoted cycles and reproducible paper calculations do not establish a tradable live edge.

### Bounded real execution evidence, 30 September 2026

The separately authorized session `runs/live/spot-live-20260930-1350` completed both requested 50-USDC triangles and an interrupted first-leg recovery/unwind. It used eight real fills, with an independently conserved token ledger. Its net change including fresh indicative residual marks was approximately −0.286 USDC. Forced tests bypassed profitability only and are excluded from screened-opportunity or paper-performance claims. Native activity occurred in the public collection interval beginning 13:50 UTC and during cleanup around 13:58 UTC; retain this annotation when interpreting quote episodes.

At that session's end, the real subaccount retained 71.50019996 USDC and confirmed subminimum inventory; there were no open orders or unresolved attempts. The original "dust" classification was subsequently found too broad: the HYPE and USDE balances contained whole lots, and the user manually sold those through the frontend. Production live trading was not started. See `DOCS/VALIDATION.md` for the preserved balances, timing and classification correction.

The separate residual-fix test `runs/live/spot-residual-20260930-153531` ran real fills during **15:36:24.379–15:36:36.429 UTC**. Both triangles completed once. Immediate `FrontendMarket` sells closed the deliberately retained 0.01 HYPE lot and a natural 0.01 USDE remainder, without extra purchases. Ten independently reconstructed fills across eight orders conserved balances. Cash changed by −0.04629831 USDC; the change including fresh opening/ending inventory marks was −0.1098151349765 USDC. Final balances were 70.95442902 USDC and only sub-lot HYPE/USDE/USDT0, with no open orders or unresolved exposure. These forced execution tests are excluded from arbitrage-profitability and paper results. The continuous paper epoch, 1-CPU container and all three 10,000-USDC accounts continued unchanged.

## 30 September 2026 follow-up

**Current epoch: `bbo-ioc-v4-2026-09-30`, execution model 4.** Fixed prefix of `runs/run-1790736569099900517`: **02:49:30.187–20:49:42.499 UTC**, 18.007 monotonic hours, sequence **5,933,609**, **4,637,063 frames** across 19 parts. Evidence and runnable independent checks are in `runs/observation-2026-09-30/`: frozen manifest/status, `audit-summary.json`, `findings.json`, `paper-events.json`, `ledger-check.json`, `preceding-segments.json` and `replay.json`.

**Paper execution needs attention.** All five attempts used 1,000 USDC on `USDC → USDE → HYPE → USDC`. The 100-ms account completed its first attempt at approximately **15:49:13.463 UTC**, returning **+0.96413750 USDC cash** (9.6414 bps), plus retained inventory. Its subsequent attempt lost 3.37810720 USDC cash and paused. The other scenarios completed no full cycles:

| Each-way latency | Completed / failed | Cash balance / change from funding | Confirmed HYPE / USDE retained | Final pause, UTC |
|---|---:|---|---|---|
| 100 ms | 1 / 1 | 9,997.58603030 / −2.41396970 USDC | 0.031936 / 1.0055480 | `unwound`, 15:49:14.175 |
| 250 ms | 0 / 1 | 9,000.1958860 / −999.8041140 USDC | 11.481957 / 1.0032540 | `unwind_unavailable`, 15:49:13.861 |
| 500 ms | 0 / 2 | 9,994.29905186 / −5.70094814 USDC | 0.031936 / 0.00656435 | `unwound`, 15:49:19.869 |

The primary account's cash decrease is predominantly **capital retained in HYPE**, not a realized 999.80-USDC loss. At the frozen endpoint, fresh indicative inventory marks were **3.8835 / 1,035.8602 / 2.8849 USDC** for 100/250/500 ms; cash-plus-mark changes were **+1.4695 / +36.0561 / −2.8160 USDC**. These include unliquidated inventory and subsequent price movement, not closed arbitrage profit. Scenarios are alternatives and are never summed. All accounts have `attempt = null` and zero unobservable attempts: the remaining exposure is confirmed inventory, not unknown fills.

The primary closing sell and reverse unwind were rejected for `insufficient_top_depth`. The other failed attempts encountered partial execution and `minimum_notional` on a small reverse conversion. Their final guard is `inventory_not_confirmed_dust`; being worth less than the 10-USDC paper budget alone does not satisfy that rule. The new live-only pooling/cleanup changes did not change this historical paper model. **Further paper execution is censored after these pause times.** Resuming it requires an explicit model/account decision; this review did not reset, reconcile, unwind or replace any account, including the older model-3 predecessor.

**Opportunity and observation coverage.** There were **1,364 positive fee-net episodes and 32 entry-eligibility episodes**, all entry episodes on the USDE/HYPE triangle. The first size-feasible estimate was +1.14768892 USDC at 1,000 USDC; later entry episodes continued through 16:25:20 UTC while accounts were paused. Best quoted fee-net peaks were **47.210 bps** for USDE→HYPE, **34.159 bps** for USDT0→HYPE, and **26.030 bps** for USDT0→XAUT0, each closing through USDC. These peaks precede quantity and delayed-execution costs.

Of **210 structural routes**, **180 remained dormant** through nine continuously empty markets; 30 received prices. Time-weighted fresh-price/full-L2 coverage was **99.698% / 71.920% among those 30**, or **14.243% / 10.274% across all 210**. The entry-bearing USDE/HYPE route had only **52.197% full-L2 coverage**. Its bounded BBO availability is not counted as full depth. The 15 confirmed simulated arrivals used **14 `l2Book:l2` and one `bbo:top`** observations. There were **zero observed zero fills, zero unobservable attempts, and zero `unknown_deeper_liquidity` execution events** in these five attempts; this is not a claim of complete depth availability for all screened candidates. Independent Decimal checks conserved all five reservations and 15 fills. Exact replay verified a slightly later endpoint of 4,659,659 frames in **260.108 s**, with matching balances, pauses and outcome counters; later evolving shadow books/marks were not compared as if endpoints were identical.

**Process and recording health.** The dedicated container was still running on **1 CPU**, zero Docker restarts since its 02:48 UTC start, with fresh durable writes at the final 20:58 UTC check. Sample usage was **1.24% of one core / 11.24 MiB**. Six server-initiated disconnects recovered, totaling **9.159 s Close→Open**; the largest inter-frame receipt gap was **9.885 s**. The prefix had zero malformed books, eight rejected older observations and nine UTC backsteps (largest **1.728 s**); durations use monotonic clocks. HYPE depth spacing was **538 ms median / 959 ms p99**, with a roughly 10-second maximum gap. No disk, channel-overflow, blocked-state or recording-cap error was found.

Receipt→decision p50/p99 was **0.628 / 2.003 ms**; queue age **0.599 / 1.959 ms**; deadline dispatch lag **0.824 / 1.956 ms**, only **30 samples**. Current-prefix volume was **3.027 GiB**, about **4.03 GiB/day**. All project runs occupied **8.24 GiB** under the 50-GiB cap, approximately **10.35 days remaining** at that rate; host free space was about **231 GiB**. These are activity-dependent estimates. Full monotonic hours ranged from 234,262 to 299,996 frames, which measures message activity rather than proving broad volatility-regime coverage.

### Recording-day progress by model and epoch

Connected recording exposure below excludes recorded Close→Open intervals, inter-run gaps and smoke/forced/live-validation runs. It is **not** time with every route executable or every paper account active.

| Model / paper epoch | UTC interval bounds across continuous segments | Connected recording days |
|---|---|---:|
| 1 / legacy accounts | 28 Sep 19:22:28–20:32:13 | 0.04836 |
| 2 / legacy accounts | 28 Sep 20:32:21–21:33:22 | 0.04238 |
| 3 / preserved predecessor | 28 Sep 21:46:23–29 Sep 23:11:50 | 1.05936 |
| 4 / `bbo-ioc-v4-2026-09-30` | 29 Sep 23:11:56–30 Sep 20:49:42 | **0.89667** |

Model 4 consists of `run-1790723514787134963`, the recovered unclean prefix `run-1790723635423981499`, and current `run-1790736569099900517`. The **02:42:58.931–02:49:29.929 UTC gap on 30 September** is excluded. Earlier source runs are enumerated in `preceding-segments.json`; model 3 is `run-1790631982022891512`. Keep balances, outcomes and recording-day counts separate by model and epoch. **The seven-healthy-day milestone is not reached.** Keep the follow-up and public recorder running; flag the paused current-epoch execution for review.
## Model-5 cutover: 30 September 2026, 22:07 UTC

The preceding model-4 findings remain historical evidence. Its segment `runs/run-1790736569099900517` ended at **22:04:23.426 UTC** with a validated durable checkpoint. Model 5 starts at **22:07:41.424 UTC** in `runs/run-1790806059942522210`, preserving funding epoch **`bbo-ioc-v4-2026-09-30`**, all balances, cumulative outcomes and shadow liquidity. The 197.998-second cutover gap is excluded; unbuffered Windows-mounted JSON reads were corrected before successful startup. Archived model-3 accounts were untouched.

The first fixed prefix ends at **22:08:34.892 UTC**, about **53.47 connected seconds (0.00062 days)** for model 5. This is the start of its observation period, not seven healthy days and not a continuation of model-4 performance statistics. Exact replay and an independent five-fill Decimal ledger audit passed. Confirmed inherited inventory was cleaned up automatically; all three scenarios are available, with no pending orders, unknown fills or whole-lot exposure. Cash balances are **10,001.30894260 / 10,043.36592563 / 9,997.02249411 USDC** for 100/250/500 ms each way. Residual marks are about **0.18 USDC per account**, each holding below its actual 0.01 lot. Recovery proceeds are **3.72291230 / 1,043.17003963 / 2.72344225 USDC**, separately recorded; new model-5 closed-cycle profit and outcome-count deltas are zero.

The separate ten-minute model-5 smoke (`runs/model5-smoke-final/run-1790805197145583230`, **21:53:17.716–22:03:17.014 UTC**) had one controlled reconnect, 37,016 frames and verified replay. Among 30 priced routes out of 210 structural routes, fresh-price/full-L2 coverage was **98.11% / 80.48%**; 180 routes remained dormant. Receipt-to-decision p50/p99 was **0.801/2.961 ms**, queue age **0.768/2.910 ms**, and measured CPU **1.47% of one core**, quota 1 CPU. It wrote 25.59 MiB and observed no positive episodes. The continuous cleanup produced 10 timer samples, p50/p99 **0.793/1.794 ms**. Quiet observations do not rule out rare opportunities.

The 15:49 UTC model-4/model-5 comparison is counterfactual: the primary scenario continues after cleanup, while two alternatives eventually retain genuinely unknown fill outcomes. These results are not new live profitability evidence. Tokyo timing remains unverified; the same 100/250/500-ms-each-way grid remains a sensitivity assumption. The daily monitor was updated to track model-5 cleanup, residual lots, inherited recovery cash, unresolved fills and model-period outcomes separately. Details and source paths: [validation](VALIDATION.md#paper-model-5-sizing-and-continuous-cleanup-30-september-2026-utc).
