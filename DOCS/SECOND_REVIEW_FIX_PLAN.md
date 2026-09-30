# Second review: checked findings and minimal fix plan

**Implemented and validated on 28 September 2026.** That release introduced format-3 recovery, the 10-USDC paper residual policy, one-shot deadlines and diagnostic replay. This audit preserves its historical reasoning, including then-unfixed issues. Later paper format 4 and live accounting version 2 are covered by [VALIDATION.md](VALIDATION.md) and the current [README](../README.md).

28 September 2026. Scope: audit and plan, following the user's request to avoid code bloat. The already-approved fast-feed deployment remains running. This review did not change production source, configuration, account state, or container settings. Diagnostics ran in disposable Docker containers; evidence is preserved under `review/2026-09-28-external-audit/`.

## Verdict

The review contains useful operational and accounting findings. It also includes outdated measurements, overbroad conclusions, and one fill-model recommendation that should not be implemented as written.

The defensible economic finding is **no positive fee-net cycle observed in the audited intervals under the configured fees**. It is not proof that rare opportunities cannot occur. Correct measurement and reliable collection remain worthwhile even if the strategy ultimately has no useful edge.

The fast-depth problem is already fixed. All 29 subscriptions acknowledge `fast: true`; the live HYPE sample has about 0.55-second depth spacing. The user's required hot/cold separation and sequential 250-ms outbound + 250-ms confirmation model remain requirements.

## Evidence checked

I inspected the supplied `feed_audit.py`, reran it through Docker on the exact 58,231,648-byte live prefix and the complete smoke run, and preserved the outputs. Recomputed maxima differed from the supplied script outputs by less than 1e-12 bps, consistent with floating-point/library differences.

The supplied `replay-smoke2.json` is byte-identical to our saved smoke replay. More usefully, I replayed the **same** 125,923-record live prefix used by the Python calculation: all events verified and all 30 priced routes' maximum returns matched the new independent calculation exactly at the displayed double precision. The matched interval was 2,870.052 seconds and its best fee-net return was **-12.2749185 bps**.

The quoted 0.22-bps comparison was not a comparison of identical intervals: the supplied live replay spans 3,024.403 seconds, versus the script's 2,870.052 seconds. Comparing those supplied file versions gives a maximum difference of about 0.281 bps. This does not indicate detector arithmetic error; aligning the interval removes the discrepancy.

Four additional runnable checks against the actual Rust modules passed in a disposable release build:

1. **Default-fee dust pause:** 7-bps base fees, 2-bps IOC tolerance, and one-second freshness. A partial fill unwound into 9,999.94604798 USDC plus 0.0006 units of a token quoted near 9.99 USDC. The remaining token was worth about **0.006 USDC**, yet the account stayed paused with reason `unwound`.
2. **Cash versus inventory:** in a deliberately synthetic profitable triangle with a BTC-like lot size, a 25-USDC attempt estimated **-0.77534292 USDC cash change**, but retained tokens with an indicative bid mark of **0.83261357 USDC**. Cash return was -310.14 bps; cash plus the mark was +22.91 bps. The mark is not immediately executable cash: minimum notionals and lots still apply. At 1,000 USDC the same synthetic case retained about 0.15 USDC, showing that dust cost is not always a fixed one-lot amount.
3. **Timer and causality:** the current idle tick can dispatch a due arrival about **49.999997 ms late** and delay the next submission similarly. A price change after arrival but before confirmation changed what a confirmation-time fill would be, proving that those two books are not interchangeable.
4. **Fee floor:** the saved graph's triangles lose **15.39314 bps** to fees alone; four-leg routes lose 16.79099–27.97061 bps, five-leg routes about 29.36670 bps, and six-leg routes about 41.92657 bps. Longer cycles do not all have the same three-leg fee composition.

These are controlled correctness checks, not live profitable trades. The diagnostic source and output are `review_audit.rs` and `review-audit-output.txt` in the preserved evidence folder.

## Assessment of the review

| Claim | Current assessment |
|---|---|
| Default depth arrives every 5.4 seconds | Confirmed historically; already corrected by fast mode. The independent default subscription matched all 22 production snapshot timestamps in its comparison window. |
| Quiet BBO means a valid quote cannot become stale | Too strong. BBO is sent on changes, but silence alone does not certify that the receiver has a complete, current book. The code already accepts a fresh unchanged L2 snapshot as confirmation and does not require a recent BBO message in addition. Keep separate feed health, last change, and last observation times. |
| HYPE executable-depth availability was 4.6% | Supported for the old slow sample. Applying the supplied audit to the first three fast-mode minutes gives **99.8% depth-age coverage and 45.4% matching-depth availability** for HYPE. The stricter price/size agreement check still matters; fresh depth alone is not enough. |
| 180 routes are dead and should be removed | Nine empty-sided markets explain 180 unpriceable structural routes in these samples. They are observed dormant, not proven permanently dead. Do not permanently remove them and lose detection when liquidity returns. |
| Fill using the book available at confirmation | Reject as stated. Confirmation is when proceeds become available; arrival is when the IOC is modeled to execute. Later books can contain market changes after arrival. Actual feed delay remains an important limitation of the current receipt-time proxy, but this substitution does not calibrate it. |
| Residual tokens are valued at zero | Cash profit excludes them, but the tokens are preserved in `Estimate.residual` and account balances. The missing feature is separate portfolio-value reporting; the ledger is not destroying the tokens. |
| A triangle takes at least 1.5 seconds | Correct and intentionally required by the approved sequential model. Do not silently change to simultaneous legs. |
| `on-failure` does not restart after Docker restarts | Confirmed by the Compose file and Docker documentation. This is a real unattended-operation gap in the initially requested policy. |
| Paper accounts pause permanently | Confirmed: no resume/reconciliation operation clears `paused`. Small dust can trigger it, as reproduced. Material or uncertain exposure should still pause; a successful small-dust disposition needs a defined continuation rule. |
| Any code or metadata change breaks startup | Overstated. Our fast-mode/model-version deployment restored all account fields exactly, and compatible changes already replay correctly. However, startup remains coupled to detector replay, and dense metadata-position checks can block some changes or removals. New appended listings are already permitted. |
| Storage lasts four weeks | That was a slow-mode estimate. A measured fast-mode sample recorded 30.80 MB in 665.65 seconds: about **3.72 GiB/day**, implying roughly **13.4 days** of remaining 50-GiB capacity at that rate. Activity can change it. The unsaved replay-speed claim is not a verified benchmark; 61 MB / 4.75 s is approximately 12.8 MB/s, not 16. |
| Corruption can cause an extra reconnect | Confirmed control-path risk: `Notify` stores a permit, and the request has no connection generation. Old queued frames can leave a request for the next socket. Not observed as a production outage in this audit. |
| `complete_step` can bypass clean finalization | Confirmed error-path gap. Its `?` exits before the shared Stop/final-report path. The terminal Stop-record result is also ignored, which can incorrectly leave `recording_complete` true. The error is not expected in ordinary monotonic operation, but the path should be correct. |
| The 50-ms timer is counted as computation | Partly correct. Quiet periods add up to about 50 ms of nominal dispatch delay, plus scheduling delay. Per-order `processing_lag_ns` mixes this delay; the separate CPU-processing histogram is not simply inflated by 50 ms. |
| Lowering the replay profit threshold would exercise fills | An override is missing, and lowering the threshold alone is insufficient: `engine.rs` also skips size calculations when the fee-net upper bound is nonpositive. A clearly labeled forced diagnostic entry is needed for negative-edge data. |
| Delete the hot path | Reject. The user explicitly required it. Live `step_hot` consumes cached hot results; the fallback arithmetic branch is used for reference/replay paths, not an unconditional second live calculation. Its allocation-counted check is useful. |
| Clutter and another active session | `target/` and the Compose backup are build/operations artifacts, not the measurement problem. The independent probe was our diagnostic and its stopped container has already been removed. Do not delete unrelated operational artifacts as part of these fixes. |

The external script is useful but not a general oracle: it reads only the first rotated file, fixes freshness to one second, and uses a separate discovery inventory. It counts older observations without applying all engine rejection rules. Its `engine_depth_ok` calculation also considers two empty tops equal, incorrectly assigning coverage to empty books. Its HYPE statistics are unaffected by that empty-side bug. Its approximate top-level bottleneck omits order rounding, minimum notionals and full-depth execution. Preserve these limitations with the results.

## Minimal implementation plan

Use existing modules and tests. Add no dependencies, service, database, generic exchange abstraction, dashboard or recovery framework. Keep the feed, graph and hot kernel. Each change gets one focused runnable regression; promote the relevant temporary audit checks rather than creating a second test suite.

### 1. Make unattended operation and failure reporting reliable

Files: `compose.yaml`, `src/main.rs`, `src/hyperliquid.rs`; shared journal shutdown only where necessary.

- Change the service to `unless-stopped`, preserving intentional operator stops and the central 1-CPU policy. This revises the original `on-failure` choice. Verify only this project; do not restart the shared Docker daemon or modify sibling services.
- Pair that change with explicit handling for persistent blocked conditions. A storage-cap marker or unrecoverable startup state must produce a clear blocked status and wait for operator action, instead of repeatedly exiting and restarting. Check the cap marker before discovery. Do not delete evidence or reset accounts.
- Route `complete_step`, terminal-record and finalization failures through one cleanup path. A missing/unwritten Stop or failed journal flush must prevent a clean-completion claim.
- Replace the untagged reconnect notification with a small bounded request carrying the connection generation. Request once on a connected-to-invalid transition; ignore requests for older generations.
- Use the existing status/log output and scheduled review for visibility; do not build a separate monitoring service. Docker must itself be running for its restart policy to help.

Checks: corruption burst followed by a clean reconnect produces one reconnect; stale-generation requests cannot close the new socket; shutdown/recording failures cannot produce a clean checkpoint; the cap remains blocked without a restart loop. Verify project stop/start behavior without a host reboot.

### 2. Restore durable account facts independently of new detector logic

Files: `src/journal.rs`, `src/main.rs`; small identity-mapping helpers in `src/market.rs` only if needed.

- Reuse the existing atomic final report as a **validated clean-shutdown checkpoint**. Add the minimum identity/version fields needed to bind its complete account state to the run, durable final sequence, accounting format and old metadata. Include reservations, pending execution state, residuals, pause reasons and shadow depletion; restoring only the USDC balance is insufficient.
- Prefer that checkpoint on clean restarts. Keep `replay --verify` as an independent audit command and as the conservative fallback for older/unclean recordings. A changed detector should not need to reproduce every old decision merely to read an already settled balance.
- Resolve market/token references through explicit stable identities. An unrelated new listing or removed zero-exposure market must not reset balances or silently reinterpret dense indices. Missing held assets or unresolved order identities remain blocked and visible.
- On a recording gap, preserve unfinished attempts as unresolved. Do not invent the missing fill or restore initial funding. Unsupported recovery should produce an actionable state, not a silent restart cycle.
- Do not add periodic checkpoint machinery yet. Measure unclean recovery on the first full-day dataset; extend the existing checkpoint only if that measured cost warrants it.

Checks: clean cutover after a compatible code change preserves all account fields; old formats still replay; appended and reordered metadata map by identity; missing held assets and a crash during an attempt do not fabricate cash; a mismatched/corrupt terminal checkpoint is rejected.

### 3. Distinguish cash, marked inventory and unresolved exposure

Files: `src/quantity.rs`, `src/paper.rs`, the existing report in `src/engine.rs`.

- Keep current cash profit/bps and confirmed free USDC. Add a separate indicative residual bid value and cash-plus-inventory change, including the mark's source/age. Use an eligible direct USDC market where available; otherwise report the mark as unavailable. Do not build a portfolio optimizer.
- Marking dust does not make it spendable. Do not silently use the marked return for the existing 5-bps cash entry threshold, or claim liquidation below lot/minimum-notional constraints.
- Separate known, confirmed sub-minimum dust from material/unresolved exposure. **Recommended policy change for approval with this plan:** permit new USDC-funded attempts after a completed unwind only when all remaining non-USDC holdings are confirmed, untradeable dust and their fresh total mark is at most **10 USDC per account** (0.1% of its initial paper funding). Recheck this bound before new entries. Leave the dust in the existing balance map; never top up cash. This is one recorded research setting, not a new account subsystem.
- Unknown marks, material inventory, missing confirmations and recording-gap attempts stay paused pending explicit reconciliation. Provide one narrow, journaled resume/reconciliation path; do not add an automatic exposure-management strategy. This deliberately revises the original rule that every residual, however tiny, pauses forever; it must be model-versioned.

Checks: reproduce the 0.0006-token dust case with production fees; bounded dust continues without balance restoration; crossing the dust budget or having uncertain exposure pauses; indicative marks never fund an order; all fees, lots, cash and token balances reconcile. Keep the original strict-pause behavior in historical replay.

### 4. Remove artificial timer delay and exercise the real execution path

Files: `src/main.rs`, `src/paper.rs`, existing replay handling and tests.

- Add a one-shot timer for the earliest pending arrival/confirmation in the serial owner. Retain the existing health wakeup as needed. Do not shrink the recurring tick to a high-frequency polling loop.
- Record scheduled deadline, actual wakeup/dispatch and computation separately. Preserve the fixed arrival book and release proceeds only after confirmation; next submissions include actual dispatch/processing delay.
- Add an explicitly diagnostic, replay-only forced entry for a specified route/size at an eligible recorded point. Use separate paper state and label every result diagnostic. It must bypass both the entry threshold and the nonpositive screening shortcut for that chosen attempt, while retaining book health, fee, depth, lot, reservation and latency rules. Do not alter normal live entry criteria or count forced entries as observed arbitrage opportunities.
- Expose structural, dormant, fresh-price and execution-depth coverage separately. Keep the 210-route structural inventory and subscriptions so previously empty markets can reactivate automatically. Avoid permanent filters based on one quiet sample or labels such as “junk.”

Checks: quiet-market arrival/confirmation does not acquire a systematic 50-ms tick delay; a post-arrival quote cannot change the arrival fill; live/replay results match with recorded dispatch times; a negative-edge diagnostic executes the normal fee/rounding/unwind path but is excluded from profitability statistics; a dormant route wakes when valid books return.

## Model choices retained

Keep the primary sequential 250+250-ms-per-order scenario, comparison scenarios, strict freshness, actual received depth, undiscounted fee assumptions and the dedicated hot path. Actual fee-token rounding and execution latency remain unverified paper conventions.

The current arrival-book rule is a **receipt-time execution proxy**, not a reconstruction of the exchange's exact book at arrival. A later-received snapshot could only inform such a reconstruction if its exchange timestamp is demonstrably at/before modeled arrival under a calibrated clock mapping. Merely choosing the newest book at confirmation does not satisfy that condition. For now, report feed-age/clock uncertainty and run explicit timing sensitivity; defer exchange-clock reconstruction until evidence justifies its complexity.

## Delivery boundary

Implement the four small groups in order, with release Docker checks and versioned record/account migrations. Preserve the active journal and paper state at cutover. Repeat a bounded public reconnect check, verify an exact recorded prefix through replay, and leave collection running. Recompute storage projections using fast-mode data and keep slow/fast/model versions separate in the 24-hour and seven-day reviews.

No profitability milestone gates these correctness fixes, and no short quiet interval establishes permanent absence of an edge. No cleanup of `target/`, broad refactor, new dependency or new service belongs in this plan.

Primary references: [Docker restart-policy semantics](https://docs.docker.com/engine/containers/start-containers-automatically/), [Hyperliquid subscriptions](https://hyperliquid.gitbook.io/hyperliquid-docs/for-developers/api/websocket/subscriptions), [Hyperliquid fee schedule](https://hyperliquid.gitbook.io/hyperliquid-docs/trading/fees), [Tokio notification-permit semantics](https://docs.rs/tokio/latest/tokio/sync/struct.Notify.html).
