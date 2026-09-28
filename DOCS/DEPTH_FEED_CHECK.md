# Independent HYPE spot depth-feed check

28 September 2026. The measured five-second depth cadence is explained by the subscription mode: omitting `fast` is acknowledged by the server as **`fast: false`**. An independent HYPE/USDC connection reproduced the running screener's slow snapshots. Explicit **`fast: true`** returned five levels approximately ten times more often.

**Deployment follow-up:** after the user approved the change, fast mode was enabled on all 29 production markets at approximately 20:32 UTC. The sections below describe the earlier isolated experiment; current deployment evidence is in [VALIDATION.md](VALIDATION.md#fast-depth-deployment).

## Experiment

The probe uses a direct public WebSocket receiver, independently of the application's collector, normalizer, evaluator and recorder. It records receipt monotonic/UTC times before JSON parsing and buffers frames in memory until the sample ends. One extra connection at a time subscribes to HYPE/USDC (`@107`) BBO and L2; the spot identity is checked against the running manifest. No accounts, keys, or orders are involved.

- Default mode: approximately **20:10:59–20:12:59 UTC**, 120 seconds.
- Fast mode: approximately **20:13:04–20:15:04 UTC**, 120 seconds.
- Both use a centrally applied **1-CPU** limit (`100000 100000`).
- Statistics discard the initial five seconds and compare the independent probe with the existing 29-market screener over the same **exchange-time window**. Receipt-interval calculations use each process's own monotonic clock; they do not assume synchronized UTC clocks.

| Feed | Depth observations in comparison window | Levels per side | Median receipt interval | Median exchange-time interval | Receipt interval p99 |
|---|---:|---:|---:|---:|---:|
| Independent, default mode | 22 | 20 | 5.405 s | 5.418 s | 5.650 s |
| Running screener, same window | 22 | 20 | 5.413 s | 5.418 s | 5.594 s |
| Independent, `fast: true` | 212 | 5 | **0.536 s** | **0.538 s** | **0.810 s** |
| Running screener during fast test | 21 | 20 | 5.376 s | 5.412 s | 5.991 s |

All **22 default-mode depth exchange timestamps matched** the running screener. Thus the five-second cadence already exists in the received feed; it is not introduced by the 29-market evaluator or recording queue. During the fast test, the independent stream's median interval fell by about tenfold while the production stream remained slow. Subscription acknowledgements explicitly confirmed false/true modes and the observed depth changed from twenty to five levels. BBO exchange-time medians stayed near 200 ms in both streams.

This is a short HYPE-only experiment. It establishes the mode difference for this endpoint/sample, not performance across every market or conditions. Fast mode also had **one 4.804-second receipt gap**, despite a maximum 0.690-second gap between its successive exchange timestamps. That indicates delayed delivery/processing in the observed path; this experiment cannot localize that isolated stall to server, network, or host. Fast mode does not guarantee every snapshot arrives within one second. Existing freshness rejection remains necessary.

## Implication

The next useful application change is to request `fast: true` explicitly and record that choice in configuration/metadata. This trades twenty levels for five. Candidate sizing and paper fills must use only the received five levels and reject insufficient visible liquidity; slow deeper levels must not be silently treated as equally fresh. The shorter cadence should improve opportunities to obtain eligible observations, but actual route coverage and paper results need a subsequent measurement.

**This diagnostic did not change the running feed mode or restart the screener.** It remained connected with zero restarts, using the same container start time, 19:26:57 UTC. No production Rust source or trading configuration was edited. The temporary probe exited successfully and was removed; its evidence remains on disk.

## Reproduction and evidence

The minimal diagnostic is `examples/depth_probe.rs`, using existing dependencies. Its interval/window self-check passed in release mode. A separate calculation from the saved raw frames reproduced both snapshot counts and both median receipt intervals exactly.

```powershell
docker compose --profile tools run --rm check cargo test --release --locked --example depth_probe
# OUTPUT_DIRECTORY must not already exist. RUN_DIRECTORY is the concurrent screener's run.
docker compose --profile tools run --rm check cargo run --release --locked --example depth_probe -- RUN_DIRECTORY OUTPUT_DIRECTORY 120
```

The measured run is `runs/depth-probe-1790626223/`, containing:

- `default-manifest.json`, `fast-manifest.json`: requested subscriptions, UTC interval and effective quota.
- `default-frames.jsonl`, `fast-frames.jsonl`: original frames and receipt clocks, including server acknowledgements.
- `comparison.json`: both probe/live comparisons and timestamp overlaps.
- `independent-check.json`: separate count/median verification.
- `probe.log`: complete diagnostic output.

Concurrent production data: `runs/run-1790623621537561730/`. The live recorder remained active throughout; comparisons read its durable prefix after a two-second flush allowance.

The current [Hyperliquid subscription documentation](https://hyperliquid.gitbook.io/hyperliquid-docs/for-developers/api/websocket/subscriptions) explicitly describes `fast` as a boolean selecting five versus twenty levels. To minimize incremental usage under the shared [public rate limits](https://hyperliquid.gitbook.io/hyperliquid-docs/for-developers/api/rate-limits-and-user-limits), the probe used two sequential connections and only two subscriptions on its active connection. Both were acknowledged successfully.
