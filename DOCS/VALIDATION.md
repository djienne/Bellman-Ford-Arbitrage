# Validation and public-feed observations

28 September 2026. All execution is paper-only. Public observations cannot establish live execution latency, fills, or profitability.

## Reproducible evidence

- Ten-minute smoke: `runs/smoke-final/run-1790622687314035991`, beginning at 19:11:27 UTC, 600.0016 seconds of monotonic engine time, clean exit 0. Controlled reconnect at approximately 300 seconds; two successful connections.
- Smoke image: `sha256:23f820a155e24c7e8ad8e10ee70cbaea3a6dedfd0843115638c4bbfb054f1376`. Exact Rust source hashes are in its manifest.
- `runs/smoke-final/replay-verified.json` and `replay-comparison.json`: the newer release replayed every event without divergence and reproduced all **27** engine-report fields exactly, including accounts, shadow liquidity, route statistics and timing histograms.
- `runs/discovery-validation.json`: 503 tokens, 330 listed markets, 29 subscribed markets, 210 bounded routes. All 210 routes contain USDC. Context arrays are joined/validated by explicit identity, not position.
- `runs/smoke-final/cpu-samples.jsonl`: cgroup CPU evidence. Service limit is **0.1 CPU**, `cpu.max = 10000 100000`, applied using the central CPU helper to this project's container only. Memory ceiling is 512 MiB.

The final startup/recovery code additionally buffers metadata reads, handles insufficient space before a new manifest without a restart loop, and follows a durable current-run pointer across backward UTC jumps. These cold-path fixes were validated with release tests and the deployment/recovery check; the smoke's feed, hot calculation, paper rules and recorded timing engine are identical. No older smoke is represented as evidence for a changed trading model.

## Release checks

`docker compose --profile tools run --rm check` passed **33 release tests** with the locked dependencies (31 research checks, one channel-overflow check, one allocation-counted benchmark). Coverage includes:

- Saved metadata's 210 cycles, all 210 simultaneously observable in a controlled book, duplicate display names, unequal context arrays, valid rotation/direction/closure and explicit inventory ceilings.
- Overlapping profitable routes, a moving one-market quote with no arbitrage, a 4.6-bps return rejected by the strict 5-bps threshold, and the full-pass Bellman–Ford diagnostic.
- Separate BBO/depth clocks, stale depth, acknowledgements/pongs, malformed values/crosses, null/empty sides, and older cross-channel observations.
- Exact 249/250/500-ms boundaries, no quote admitted after an arrival deadline, and preservation of real processing delays through replay.
- Decimal balances, fractional fees, lots/price increments, minimum notionals, fixed IOC tolerance, reservations, partial fills, unwinds, residual exposure and dust.
- Finite shadow liquidity under repeated snapshots and preservation of consumed capacity across restarts.
- Positive episode boundaries without grace inflation, unobservable arrivals, unresolved restart exposure, wall-clock jumps, backward-clock recovery, queue overflow, recording cap and disk errors.
- Live-engine/replay equivalence on synthetic profitable and failing executions, plus exact replay of the public smoke recording.

No live paper attempt occurred during the quiet smoke; execution paths are validated by deterministic synthetic scenarios, not claimed live fills.

## Performance

The allocation-counted numeric benchmark evaluated 200,000 changing inputs against the saved 29-market/210-route topology: **zero hot allocations**, p50 578 ns, p95 1,231 ns, p99 1,663 ns in that measurement (`cpu.max = max 100000`). This measures only the arithmetic kernel, with preallocated inputs/output. It is not a network-to-decision benchmark. Scheduling/cache effects explain why a live worker's measured duration differs.

Ten-minute public workload, 13,825 frames, including acknowledgements and pongs:

| Measured stage | p50 | p95 | p99 | Maximum |
|---|---:|---:|---:|---:|
| Receipt to hot result | 0.119 ms | 0.399 ms | 1.382 ms | 30.010 ms |
| Numeric worker duration | 2 us | 13 us | 25 us | 0.912 ms |
| Receipt to completed decision | 0.300 ms | 1.197 ms | 2.785 ms | 98.761 ms |
| Receipt to cold processing start | 0.251 ms | 1.125 ms | 2.673 ms | 98.696 ms |
| Cold processing | 34 us | 127 us | 249 us | 94.112 ms |

Percentiles have one-microsecond histogram resolution; raw timestamps remain nanoseconds. The host also ran other workloads and release builds during collection. These measured tails include that contention and the CPU quota. They do not meet the obsolete prototype's unsupported 150-us p99 target. The assumed 250+250-ms network delay remains separate from measured local processing.

The sampled 183.30-second CPU interval consumed 1.8903 CPU seconds: **1.03% of one core**, below the 10%-of-one-core ceiling. Observed Docker memory was about 5.8 MiB. Cgroup throttling did occur; a low average CPU load does not eliminate burst scheduling delay.

## Coverage and outcomes

- 13,676 accepted book updates; three older observations rejected; zero malformed books. The hot worker performed 507,794 affected-route evaluations, and the cold evaluator 453,680 evaluations including health/depth expiry.
- Only **30/210 routes** had any jointly fresh price interval. Total joint route observation time was 3,194.78 route-seconds: **2.54%** of all 210 route-time combinations. This is price coverage, not proof of executable depth coverage.
- The two HYPE/USDT0/USDC triangle directions each had 149.02 seconds of price coverage. Their best observed fee-net returns were approximately -14.78 and -13.84 bps.
- In a 36-snapshot HYPE sample, median full-depth receipt spacing was 5.341 seconds and exchange-time spacing 5.389 seconds. BBO receipt spacing was about 201 ms. Nine selected markets returned empty-sided full snapshots throughout the full run. These are observations from this endpoint/run, not universal exchange cadence guarantees.
- Zero positive fee-net episodes, entries, completed cycles, failed cycles or unobservable attempts. Each independent paper account retained 10,000 USDC with no reserved funds or residual inventory. This does not establish the absence of rare opportunities.
- Two UTC receipt-clock backsteps were recorded: 155.239 ms and 10.645 ms. Monotonic ordering and durations stayed valid; replay reproduced the same outcomes. Exchange-to-local timestamp differences must not be interpreted as pure network latency.
- Run files occupied **13,438,967 bytes** (about 12.82 MiB), including metadata and reports. At that workload, the rough recording rate is 1.80 GiB/day; future activity can change it. The 50-GiB cap is enforced rather than deleting old evidence.

The one-second freshness settings intentionally leave many intervals unobservable. Use `replay --quote-age-ms ... --depth-age-ms ...` to study sensitivity, retaining explicit assumptions. Lengthening freshness creates a different research model; it does not prove the old depth was executable.

Follow-up: the [independent depth-feed check](DEPTH_FEED_CHECK.md) identified the subscription-mode cause of the slow cadence. The server defaults the current subscription to `fast: false` (twenty levels); an independent `fast: true` subscription delivered five levels with a 0.536-second median interval. That diagnostic left the production feed unchanged.

## Operation and follow-up

At 19:48:10 UTC on 28 September 2026, the user requested a **1-CPU** limit. The central policy override and Compose configuration were updated, and the running container reported `cpu.max = 100000 100000`, without restarting. The change is recorded in `runs/cpu-limit-changes.jsonl`. The smoke measurements above remain measurements at 0.1 CPU; the ongoing run's startup manifest predates this live quota change.

The persistent service is `bellman-hyperliquid-screener`, with three independent paper accounts and this project's `runs` bind mount. Its first launch encountered an /info timeout, then recovered under restart-on-failure. Initialization is not counted as a healthy recording interval.

Initial deployed image: `sha256:682a2af5f4f5662c7199a9de39665bfe6b291aaf1171185811c1ec708d0b7cf0`. Its embedded source hashes matched the Rust source and Cargo files at that cutover. A controlled cutover at 19:26:57 UTC recovered the completed `runs/run-1790623344011165116` segment into `runs/run-1790623621537561730`. Every restored account field matched the prior final report, including **635 shadow levels per account**; evidence is `runs/restart-comparison.json`. The current pointer was moved to the new segment. New books, durable records and minute reports were verified after recovery. That image also verified the ten-minute recording, saved as `runs/smoke-final/replay-final-image.json`.

The in-chat daily follow-up `hyperliquid-spot-research-coverage` reviews the first approximately 24 hours and accumulates evidence toward seven healthy recording days across activity conditions. It separates gaps, price/depth coverage, modeled accounts and unresolved exposure, and leaves the screener running after the review milestone. Docker and the host must remain available; the app must be available for its scheduled review. No observation milestone or profitable event is claimed complete yet.

Historical evidence remains in `review/external-prototype-review.txt`, `historical-binance-measurement.txt`, `hyperliquid_spot_snapshot.json`, and `prototype-before-hyperliquid.zip`. The earlier unbuffered startup attempt under `runs/smoke/` contains only a partial metadata file and is excluded from healthy market evidence.

## Fast-depth deployment

At **20:32:13 UTC on 28 September 2026**, the service was recreated with image `sha256:3937a7755e0f1c9e938ae9e56d898611625a8a31c12cdf556f4fd453178e582c`. Run `runs/run-1790627540148211239` began at approximately 20:32:20 UTC after verifying and restoring the previous journal. The central **1-CPU** quota remains in effect.

- `l2_fast = true` is recorded explicitly in the new manifest. All **29** depth acknowledgements confirm fast mode, and received books contain at most **five levels per side**. The deployment's source hashes match the local Rust/Cargo files.
- All three restored account states matched the prior final report field for field, including balances and shadow liquidity. Each retained 10,000 USDC. Evidence: `runs/fast-depth-cutover.json`.
- **37 release tests passed**: 34 research checks, two transport/configuration checks and the zero-allocation hot benchmark. New checks cover explicit fast subscriptions, old configuration defaults, replacing old deeper books, refusing fills beyond the five visible levels, and retaining consumed liquidity when a price leaves a truncated window. Both bid/ask sides and five/twenty-level modes are exercised.
- New manifest format 2 selects the corrected shadow model. Format 1 retains its historical pruning semantics for faithful replay; missing `l2_fast` means slow mode. The old ten-minute recording still verifies, and its complete report matches the prior result (`runs/fast-depth-legacy-replay.json`).
- The first **180.103 seconds** of live fast-mode input were copied as a fixed, read-only prefix and replayed. All **15,327 records** verified, and all **27** report fields matched the live minute report exactly. Evidence: `runs/fast-depth-replay-check/verification.json`, its source manifest/records, `status-reference.json` and `replayed.json`. This validation did not interrupt collection.
- A sample of **195 HYPE depth snapshots** after startup gave a median receipt interval of **548.2 ms**, p99 **792.2 ms**, and median exchange-time interval **541 ms**, with at most five levels per side (`runs/fast-depth-live-cadence.json`).
- The three-minute report contains **11,723 frames**, **11,656 accepted books**, zero invalid frames, one connection, and **384,138 route evaluations**. Receipt-to-decision p50/p99 were **0.551/1.653 ms**; receipt-to-hot p50/p99 were **0.162/0.642 ms**.
- Joint fresh-price coverage reached **13.98% of all 210 route-time combinations**; 30 structural routes had observable prices. The earlier slow-feed segment was around 2.7%. These are different short observation windows, not a controlled profitability comparison; empty markets still limit coverage, and price coverage does not guarantee executable depth.
- No positive fee-net episodes or paper attempts occurred in this initial fast sample. CPU was approximately 1.9% of one core and memory approximately 8 MiB at the sampled check. The service remained running with zero unexpected restarts.

The one-second quote/depth limits, configured fees, IOC tolerances, latency scenarios and paper funding were retained. Deeper slow prices are never merged into a fresh five-level execution book. The scheduled coverage review should distinguish slow-format-1 and fast-format-2 segments rather than pool their timing and execution assumptions.

## Second-review fixes: format 3

Completed on 28 September 2026. Existing modules implement durable clean-checkpoint recovery, explicit failure blocking, generation-scoped reconnects, separate cash/inventory reporting, confirmed-dust continuation, one-shot execution deadlines, forced diagnostic replay and separate depth coverage. No dependency or production service was added. The numeric hot kernel is unchanged; `hot.rs` only recognizes the new reconciliation control event.

**Validation:** 46 release checks passed through Docker Compose: 42 research tests, three transport/configuration checks and the allocation-counted numeric benchmark. Checks use production fees for the partial-fill dust case; resulting balances are 9,999.94604798 USDC plus 0.0006 token units. Dust is retained, never refunded. Tests also cover reserved/pending state in a clean checkpoint, explicit refusal to reconcile unknown exposure, depleted shadow remapping, removed unreferenced markets, missing referenced assets, legacy unobservable migration, budget/mark guards and a forced negative-return cycle.

The format-1 ten-minute historical recording and the saved format-2 fast prefix both verified, with complete reports exactly matching their saved references. New checkpoints reject inconsistent terminal records/reports. The final image verified every event of the new format-3 smoke run and its entire report matched the recorded final report.

**Public smoke:** `runs/run-1790631216054513033` ran for **600.001415494 seconds**, starting at approximately **21:33:36 UTC**, with a controlled reconnect at 300 seconds and a clean exit code 0. It received **38,592 frames / 38,443 accepted books**, evaluated **1,258,134 routes**, and recorded **two connections**, zero malformed frames, zero positive episodes and zero live paper attempts. Both initial cutover and subsequent clean restart preserved every account field, including shadow levels. Each independent account retained 10,000 USDC.

| Measurement | Result |
|---|---:|
| Effective CPU quota | `100000 100000` — 1 CPU |
| Receipt to decision, p50 / p95 / p99 | 0.550 / 1.219 / 1.560 ms |
| Receipt to hot, p50 / p99 | 0.155 / 0.511 ms |
| Queue age, p50 / p99 | 0.526 / 1.532 ms |
| Cold processing, p50 / p99 | 0.023 / 0.152 ms |
| Numeric hot kernel, p50 / p99 | 401 / 1,123 ns |
| Hot allocations over 200,000 updates | 0 |
| Controlled one-shot timer dispatch lag | 1.520 ms; one sample, same 1-CPU quota |
| Sampled live CPU usage | 0.72–1.20% of one core |
| Sampled live memory | approximately 9 MiB |
| Recording volume, including metadata/reports | 29,043,877 bytes |
| Short-window volume extrapolation | 3.90 GiB/day; about 12.75 days remaining under the cap |

There were no live order deadlines to measure: the production deadline histogram has **zero samples**, not demonstrated zero latency. The one-shot check is a controlled scheduling measurement. The separate numeric benchmark is not end-to-end latency. The storage projection is activity-dependent and must be refreshed during observation reviews.

All **210 structural routes** remain in the graph; **30** were priced during this run. Fresh-price coverage was **14.07% of all structural route-time**, versus **10.06% executable-depth coverage**. Within the 30 routes ever priced, depth coverage was **70.40%**. These denominators include the reconnect gap. The remaining 180 routes were associated with observed empty markets and stay subscribed for reactivation. Depth coverage is still not proof of a real exchange fill.

**Execution on recorded public data:** forced replay of `107B>207S>166S` at 100 USDC completed once in each 100/250/500-ms scenario. Each showed **-0.96319737 USDC cash change**, **0.8155604450702 USDC indicative residual value**, and **-0.1476369249298 USDC cash-plus-mark change** at the final available marks. These are separate diagnostic alternatives, not discovered opportunities or additive profit. Normal entries were disabled; actual fees, rounding, reservations, shadow liquidity and deadlines remained active.

**Operation:** a separate storage-cap test stayed running in a visibly blocked state with zero restarts, without beginning discovery. A test using `review/2026-09-28-second-fixes/offline-compose.yaml` disabled networking for a disposable instance of the existing service: metadata connection failures retried with 1/2/4/8/16-second backoff rather than permanently blocking or resetting accounts. Both test containers were stopped and removed. No shared Docker daemon or XEMM service was restarted.

The smoke candidate was followed by two narrowly scoped startup safeguards: pausing historical unobservable accounts during remapping, and retrying transient metadata outages. Its feed, detector, timer and fill code did not change. The final image replayed that recording exactly, passed the full release suite, passed the offline-startup probe and restored all checkpoint fields. Source differences are recorded in `runs/review-fixes-validation/clean-restart.json` rather than claiming that different builds are identical.

Continuous service resumed at **21:46:21 UTC**, image **`sha256:f27cb87991c15909fcbd4dbafcb0d6afec7ac93ba044cae1d5be392f305bcd8c`**, run **`run-1790631982022891512`**, with `unless-stopped`, 512 MiB and the central 1-CPU policy. Its source hashes match the workspace. The first minute received 3,929 frames / 3,867 books with zero malformed frames, p99 receipt-to-decision 1.481 ms, no paused/guarded accounts and no unexpected restarts. Its manifest's complete restored accounts equal the smoke's final accounts. Startup through checkpoint recovery, fresh metadata and manifest creation took approximately 0.78 seconds by UTC timestamps; this includes network discovery and is not an isolated recovery benchmark.

Compact test/build/benchmark evidence is in `review/2026-09-28-second-fixes/`; recordings, exact replay outputs, cutover/restart comparisons, diagnostic journal and machine-readable summary are in `runs/review-fixes-validation/`. The first full-day unclean recovery benchmark remains an observation follow-up; periodic checkpoint infrastructure has deliberately not been added. Continue separating model versions in 24-hour/seven-day reviews. Actual exchange execution latency, account-specific fee rounding and market profitability remain unverified.

## Execution observability: format 4 and independent epoch

Completed on **30 September 2026 local time**. Format 4 uses the shared traded-side execution selector for estimates, preparation, arrivals and unwinds. Fresh quotes outside fixed limits establish zero fills; sufficient top-level quantity supports bounded execution. Opposite-side differences do not invalidate coherent traded-side L2. Changed BBO is never merged into older depth. Unknown remainders, including beyond a truncated L2 view, remain unobservable. Each result records its source and distinct price/quantity clocks. The numeric hot kernel and dependencies are unchanged.

**52 release checks passed through Docker Compose** (48 research, three configuration/transport and one allocation check). They cover the recorded price/quantity failures, both order directions, limits, stale quantities, unknown deeper remainders, no shadow consumption on unknown fills, observed removal/replenishment, 250/500-ms boundaries, later-quote exclusion, format-4 replay and epoch funding/recovery. Historical formats 1–3 verified; their complete report fields matched saved references. A same-ID epoch with only 9,500 USDC in the test remains at 9,500 after recovery; an older reused ID and a damaged predecessor checkpoint are rejected.

**Representative public input:** two fixed 40-second windows from `run-1790631982022891512` include the 09:49 and 16:15 UTC failures on 29 September. Their original observations and processing/completion clocks are retained, with fresh accounts and an explicit synthetic connection start. Normal-entry comparisons may enter at different times under the changed eligibility rule. They are counterfactual, not revisions of the original accounts.

Matched-entry forced diagnostics at 1,000 USDC on `107S>150B>255B` isolate the execution changes:

| Window / latency each way | Model 3 | Model 4 |
|---|---|---|
| 09:49 / 100 ms | Unknown second-leg fill; account paused | Observed zero fill, followed by unwind; cash change −0.54693142 USDC |
| 09:49 / 250 and 500 ms | Observable unwind; cash change −0.54693142 each | Observable unwind; same cash change each |
| 16:15 / 100 and 250 ms | Unknown fill; accounts paused | Full cycles complete, with cash change **−2.03743310 USDC each** |
| 16:15 / 500 ms | Partial fill, unavailable unwind, retained inventory | Observable unwind; cash change **−1.71911690 USDC** |

These show improved measurement, including losing outcomes. They neither establish live fills nor promise all future attempts are observable. A separate negative-edge forced replay completes once per scenario with cash change −0.95027014 USDC each; residual inventory remains separately marked. An independent Python Decimal audit checked **34 confirmed fills**, all account/token conservation, configured fees, quantity increments, budgets, fixed limits and confirmation times; **27** format-4 arrivals also passed source-clock causality checks. Scenarios are alternatives and never summed.

**Ten-minute public smoke:** `runs/format4-smoke/run-1790722781878884572`, manifest UTC **2026-09-29 22:59:41.879** to terminal UTC **23:09:41.859**, **600.002091989 monotonic seconds**. UTC and monotonic durations differ slightly; UTC is not used for scheduling. One controlled reconnect produced two connections. **40,380 frames**, zero malformed books; exact event verification and the complete replay report matched the recorded final report.

| Measurement | Result |
|---|---:|
| CPU quota | 1 CPU, `100000 100000` |
| Receipt → decision p50 / p95 / p99 | 0.551 / 1.278 / 1.913 ms |
| Queue age p50 / p99 | 0.521 / 1.866 ms |
| Cold processing p50 / p99 | 0.026 / 0.199 ms |
| Hot kernel p50 / p99, separate 1-CPU benchmark | 421 / 1,433 ns |
| Hot allocations over 200,000 evaluations | 0 |
| Live paper deadline samples | 0; no measured live dispatch-lag percentile |
| Sampled smoke CPU / memory | 0.56–1.32% of one core / approximately 8.2–8.5 MiB |
| Smoke recording volume | 29,554,574 bytes; short-window projection 3.96 GiB/day |
| Fresh-price coverage, among 30 priced routes | 97.98% |
| Full-L2 traded-side coverage, among those routes | 76.63% |
| Fresh-price / full-L2 coverage, all 210 structural routes | 14.00% / 10.95% |

The smoke recorded **five positive fee-net episodes**, peaking at **+4.4674 bps**, below the strict >5-bps entry threshold even before additional size/rounding costs. All accounts retained 10,000 USDC with no attempt. The 180 dormant routes remain subscribed. Coverage is from a different window and model than prior smoke runs; it is not a controlled improvement estimate. General BBO availability is not full-L2 coverage.

**Cutover:** the predecessor `run-1790631982022891512` shut down cleanly at terminal sequence **8,277,195**. Independent validation bound its complete accounts/shadow state to the durable terminal checkpoint and confirmed unchanged balances, reservations, holdings and pending fills. Those old unresolved accounts remain archived. A separately funded epoch **`bbo-ioc-v4-2026-09-30`** began in `run-1790723514787134963`; a controlled restart then restored every account field and the nonempty shadow ledgers into **`run-1790723635423981499`** without additional funding. The same-ID command is idempotent; ordinary startup also restores the epoch.

Continuous collection remains on image `sha256:66569e843ca1ba4e1fed1bc7685d4ba4639cc43829d3963931478e5b3fd0468c`, 1 CPU, 512 MiB and `unless-stopped`. Embedded Rust/Cargo hashes match the workspace. The first two minutes after restart received **7,365 frames**, zero malformed books, p99 decision **1.960 ms**, and three unpaused 10,000-USDC accounts. No new-epoch paper entry was observed in that short interval. The daily follow-up now separates execution sources, epochs and old unresolved exposure. Recording occupied about **4.60 GiB** under the 50-GiB cap, approximately **11.45 days** remaining at the smoke rate; host disk had about 305 GiB free. These projections depend on activity.

Compact release/benchmark/accounting/cutover evidence is in `review/2026-09-30-execution-model-4/`. Raw smoke data, fixed windows, counterfactual reports, diagnostic journals and runnable independent checks are in `runs/format4-validation/` and `runs/format4-smoke/`. Actual exchange execution latency, fee rounding and live profitability remain unverified. Keep all observation periods and paper epochs distinct.

## Bounded mainnet spot execution (30 September 2026)

Implemented the optional live feature without changing the continuous paper service or sibling XEMM stack. Default builds reject live dispatch; the live-capable build requires an explicit command, acknowledgement, session ID and finite limits. The real-money validation used a funded mainnet subaccount and did not invoke production `live run`.

Evidence: `runs/live/spot-live-20260930-1350`, plus the compact public summary and independent calculation in `review/2026-09-30-bounded-live/`. The tested image was `sha256:239079ce1ca4af768d99507daea89ce55c8d8f9d6264df92f861a9184bbfdeaa`. The validation began at **2026-09-30 13:50:01.197 UTC**; its observer recording is `market/run-1790776201245026090`. Explicit recovery/cleanup used `market/run-1790776711999148173`, beginning at **13:58:31.999 UTC**. The interval between processes held confirmed HYPE exposure deliberately; its price change is part of the final result, not triangle profitability.

| Check | Observed result |
|---|---|
| USDC → HYPE → USDT0 → USDC, up to 50 USDC | Three confirmed full IOC legs; one completed cycle |
| USDC → USDE → HYPE → USDC, up to 50 USDC | Three confirmed full IOC legs; one completed cycle |
| Post-only place/cancel | Resting order observed, canceled, zero fills |
| Deliberately non-crossing IOC | Definitive zero-fill rejection, unchanged balances |
| Lost acknowledgement | First USDE-leg response discarded; original order recovered without resubmission |
| Interrupted first leg | Confirmed purchase preserved across process exit; read-only reconciliation followed by explicit reverse IOC |
| Final exposure | No open orders, pending orders or live runner; confirmed bounded dust retained |

The successful session used **11 signed actions and eight actual fills**. A separate Python Decimal calculation read raw WebSocket/REST fill observations, checked each fixed limit and quantity, and reproduced final balances to token precision. No hypothetical fill or initial-funds restoration was used.

Final actual balances: **71.50019996 USDC**, **0.02880303 HYPE**, **0.73474002 USDE**, **0.00105553 USDT0**. USDC cash changed by **−3.50879565**; fresh bid marks valued the retained subminimum inventory at **3.223148286179 USDC**, giving an **indicative total change of −0.285647363821 USDC**. Marks were fresh by 189–393 ms. The inventory is not spendable USDC and its bid value is not realizable liquidation proceeds at these quantities. This result includes the two forced triangles and the interrupted-purchase unwind; it is execution validation, not an arbitrage-performance claim.

| Local measurement, validation process | p50 | p99 | Samples |
|---|---:|---:|---:|
| Receipt to completed detector decision | 0.448 ms | 1.188 ms | 2,111 frames |
| Queue age | 0.420 ms | 1.167 ms | 2,111 frames |
| Order submission to HTTP response | 997.956 ms | 1,134.601 ms | 9 orders |
| Prepared order to terminal fill/balance confirmation | 1,434.304 ms | 2,850.208 ms | 9 resolutions |
| Execution-journal durable write | 3.040 ms | 7.391 ms | 77 events |

The confirmation distribution includes the canceled and zero-fill probes; it is not matching-engine latency or a service guarantee. Cleanup's single reverse order returned in 965 ms and reconciled in 1,035 ms. All one-off execution containers inherited **1 CPU**, `cpu.max = 100000 100000`, and 512 MiB. Native CPU utilization was not captured; the independent continuous screener was sampled at 0.70–1.20% of a core. Live artifacts occupied **9,977,379 bytes** before later analysis outputs. Public-smoke CPU/volume evidence is saved separately. No synthetic latency was imposed on real execution, and no paper-timer lag sample is inferred from the absence of live simulated deadlines.

Before this successful session, `runs/live/spot-live-20260930-1340` exposed an instrument problem: direct streaming serialization issued many small writes on the Windows Docker bind mount. The quote-age guard prevented order submission, and spot balances stayed exactly **75.00899561 USDC**. The journal now encodes one record into a buffer before writing and syncing. The old intent was explicitly reconciled with the venue as absent and its session closed without refunding or replacing accounts. The failed observer shutdown also identified a missing terminal checkpoint; the live observer now uses the existing durable terminal checkpoint contract. Both failed evidence and corrected evidence remain preserved.

Release validation passed **68 tests**: 48 existing research checks, the unchanged allocation-free kernel test, and 19 transport/signing/live checks. These include actual-fee ledger conservation, duplicate/invalid fills, partial terminal recovery, lost-response recovery without submission, account locking, metadata identity remapping, durable-write failure and preservation of reservations. Original historical formats **1–4** verified, and both successful native observer recordings verified with zero paper accounts. Later clock-domain instrumentation excludes cross-process confirmations from latency distributions rather than fabricating a zero delay; CPU counters are included in future live reports.

The continuous service remained running in epoch `bbo-ioc-v4-2026-09-30`, with all three 10,000-USDC paper accounts intact. Real-account balances, forced-test outcomes, retained dust and public/paper observation epochs remain separate.
The final 60.0018-second public smoke (`runs/live-build-validation/public-smoke/run-1790778754041345606`) included one controlled reconnect, 5,198 frames, a clean terminal checkpoint and a verified replay. Of 210 structural routes, 30 received prices; among those, accumulated fresh-price coverage was 85.09% and full-L2 coverage 46.65%, including startup/reconnect downtime. Terminal instantaneous coverage is zero because Stop invalidates books. Receipt-to-decision p50/p99 was 0.466/1.380 ms; queue p50/p99 was 0.432/1.350 ms. There were no paper execution deadlines, so timer lag remains unsampled. Recorded volume was 4,531,739 bytes; sampled CPU was 1.57% of one core with 5.824 MiB resident usage. This smoke ran independently and did not replace the continuous paper epoch. The default image tag is restored to a build without the live feature; the optional validated capability remains explicitly buildable.