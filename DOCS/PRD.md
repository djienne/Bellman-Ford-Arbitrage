# Hyperliquid spot screener and paper research

Updated 30 September 2026. This document replaces the original Binance/Bellman–Ford-only product specification. The default application is a public, continuously running spot-cycle screener and causal paper simulator. An optional, explicitly armed live feature adds bounded spot execution; it does not enable production trading or change the paper accounts.

## Purpose and scope

Measure the frequency, duration, size feasibility and modeled execution outcome of triangular and longer spot conversion opportunities. Discover all spot markets, enumerate every directed simple cycle within the configured 3–6-leg bound, and subscribe to BBO plus full received L2 for participating markets. Normalize rotations without merging direction or market identity. Completeness applies only to this graph and length bound. Explicit enumeration ceilings must stop discovery rather than hide missing routes.

## Performance

Keep a dedicated numeric hot path, fed by cold normalization and followed by cold accounting/recording. Precompute adjacency, cycle edges and fee logs. Allocate buffers outside the hot calculation. Evaluate every affected cycle on each accepted update and health transition, without custom unsafe queues, concurrent price atomics, lifetime signature limits, busy waiting, or heuristic work truncation. Retain a corrected from-scratch Bellman–Ford implementation only for offline diagnostics.

Measure numeric kernel latency separately from receipt-to-hot, receipt-to-completed-decision, processing and queue delays. Publish distributions with actual CPU quotas, workload and coverage. No fixed latency target may be represented as achieved without representative measurements.

## Data and clocks

Join by explicit token/market/context identity. Do not zip unequal arrays or join display names. Receipt UTC and monotonic clocks precede parsing; retain exchange timestamps at their original resolution. Maintain separate BBO/depth clocks, independent one-second freshness limits and a 30-second socket watchdog. Reject impossible prices/quantities/crosses, invalidate on disconnect/corruption, and distinguish known empty liquidity from unavailable evidence. Enforce causal order at every simulated deadline. Clock storage resolution is not accuracy.

The approved fast-depth update explicitly requests `l2Book` with `fast: true` (five levels per side). Preserve all received levels and size/fill only against them; never extend a fresh five-level snapshot with older deeper prices. Record the mode and preserve the old twenty-level behavior when replaying historical recordings. Truncation beyond the worst visible price does not prove cancellation or replenish consumed paper liquidity.

## Paper model

Default independent accounts: 10,000 USDC at 100/250/500 ms each way per order. Candidate amounts: 25/100/250/1,000 USDC. Record all positive fee-net candidates; enter only if the unrounded size-feasible return exceeds 5 bps. Rank by absolute estimated USDC profit, then fewer legs, then stable identity. Permit one attempt per continuous eligibility episode and one cycle in flight per account.

Reserve immediately. Fix IOC quantities/limits using current eligible depth, a 2-bps adverse tolerance, exact lots, price precision and the 10-quote-token minimum. Format 4 shares one traded-side execution view across estimates, preparation and fills: fresh BBO supports an entire top-level order or a known zero fill outside its fixed limit; deeper execution requires fresh coherent L2. Never splice changed BBO into old deeper levels, refresh L2 with BBO, or treat unknown deeper remainders as observed partial fills. Ignore changes only on the opposite side. Execute against scenario shadow liquidity at arrival and make only confirmed net proceeds available after confirmation. Journal source clocks and distinguish bounded BBO outcomes from full-L2 coverage. Use Decimal balances and fractional per-market fees with recorded provenance. Unverified discounts remain unapplied.

Partial/failing forward legs trigger reverse-path unwinds at 50-bps tolerance with identical delays. Preserve dust and losses. Unfinished unwind exposure pauses the account while screening continues. Missing arrival observations make an attempt unobservable, excluding it from execution-performance claims. Maintain shared per-account shadow liquidity across routes and restarts; identical snapshots cannot repeatedly fund fills.

## Evidence and operation

The existing executable supplies `discover`, `run`, and `replay`. Store append-only raw inputs, clocks, configuration, metadata, order/fill/balance and episode events. Replay must preserve original processing delays and deadlines, and support latency/freshness sensitivity runs. Record minute summaries, full route coverage, rankings, separate accounts, rejected opportunities, open exposure and unobservable attempts.

Use this project's Docker Compose service with persistent `runs`, `unless-stopped` and the centrally managed 1-CPU limit. Rotate at one hour/256 MiB and block at 50 GiB without deleting evidence or looping restarts. Storage errors and bounded-channel overflows invalidate the segment. Validated format-3/4 clean checkpoints restore all account facts; older/unclean runs fall back to verified replay. Identity remapping must preserve inventory and shadow depletion. The sibling XEMM project remains untouched.

`replay --execution-model 3|4` compares fresh accounts under explicitly counterfactual assumptions and cannot verify original events. `run --new-paper-epoch ID` starts a separate, explicitly funded experiment only after validating the predecessor's durable checkpoint. Same-ID restarts restore accounts; older IDs cannot receive fresh funding. Preserve old unresolved accounts in their original evidence and keep epoch outcomes separate.

The paper model allows confirmed residuals below its lot/minimum-notional constraints up to a fresh indicative total of 10 USDC per account, rechecked before entry. This differs from the live version-2 sub-lot rule. Marks are separate from cash profit and cannot fund orders. Unknown fills keep their reservation and pending order unresolved; dust reconciliation cannot clear them. One-shot order deadlines expose dispatch lag. Replay-only forced diagnostics use separate accounts, and reports distinguish structural, dormant, fresh-price and executable-depth coverage. Historical formats retain original behavior under verification.

## Acceptance

Release Docker checks must cover saved metadata's 210 cycles, explicit identity joins, overlapping routes, direction/rotation, more than 128 identities, threshold precision, book health, latency boundaries, no future quotes, Decimal conservation, partial fills/unwinds, dust/minimums, finite shadow liquidity, durable recovery, replay equivalence, clocks, overflow, disk failure and recording limits. An allocation-counted hot benchmark complements the live measurements.

Complete a ten-minute public smoke run with controlled reconnect, verify replay, then start continuous collection. Review 24-hour coverage and collect seven healthy days across conditions. These observation milestones do not block engineering completion. Quiet periods do not rule out rare opportunities, and public-feed paper outcomes do not establish live fill profitability.

Implementation and operational details: [README](../README.md). Measured checks: [validation](VALIDATION.md). Historical rationale: [original fix plan](HYPERLIQUID_FIX_PLAN.md).

## Optional bounded real execution

The optional `live` build feature adds one concrete Hyperliquid spot owner with explicit acknowledgement and finite limits. It reuses discovery, public books, the hot detector and Decimal order preparation; actual private fills, fee tokens and reconciled balances replace paper arrivals. Signing and HTTP stay outside the hot kernel. Nonces and intents are durable before submission; unknown outcomes are reconciled without resending. API order/fill/spot-state subscriptions support confirmation, with bounded REST recovery.

Validation uses the two authorized 50-USDC triangles, a five-USDC loss stop, 32-action cap, no concurrent real attempts and no funding transfers. The original comprehensive mode deliberately exits after a confirmed first leg and requires reconciliation plus explicit cleanup; `--residual-check` completes its tests without that pause. Live evidence and pointers stay under `runs/live`, independently of paper epochs. The default service remains paper, has no mounted credentials and never automatically starts or restarts a live command. Real-money tests provide operational evidence, separate from profitability claims.

Live accounting version 2 replaces the live subminimum-value dust allowance with an asset-specific sub-lot condition. Confirmed strategy inventory is pooled across attempts; intermediate buys are minimized against the feasible downstream lot quantities. Live estimates and submissions share the plan, and opening inventory cannot inflate entry profit or the loss allowance. Remaining whole lots are sold directly to USDC with bounded `FrontendMarket` cleanup, including the observed sub-$10 sell exception; ordinary order minimums remain. Partial cleanup uses only confirmed remaining quantities, at most twice per token. Unknown outcomes, failed recording or exhausted limits block further inventory submissions. Historical live intents and paper formats are preserved. The `--residual-check` validation completes both triangles and immediately exercises one additional HYPE-lot cleanup without an intentional holding pause.
