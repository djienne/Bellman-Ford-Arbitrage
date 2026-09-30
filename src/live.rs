//! One bounded spot account. Paper deadlines and shadow fills never enter this ledger.
use crate::{
    config::{dec, Config},
    engine::{Engine, Histogram, Input, InputKind},
    hyperliquid::{self, Clock},
    journal::{self, Journal, Manifest, Record},
    live_client::{self as wire, Client, Trade},
    market::{Edge, Route, Universe},
    quantity::{self, Balances, Order},
};
use anyhow::{bail, ensure, Context, Result};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::{mpsc, watch, Mutex, Notify};

const REFERENCE: &str = "XEMM 7863f14bc104c85a5015c2f283f77453f3ad67ec";
const ROOT: &str = "runs/live";
fn utc_ms() -> u64 {
    hyperliquid::utc_ns() / 1_000_000
}

#[derive(Clone, Serialize, Deserialize)]
struct Limits {
    amount: Decimal,
    loss: Decimal,
    duration_secs: u64,
    max_actions: u32,
}
#[derive(Clone, Serialize, Deserialize)]
struct Sent {
    cloid: String,
    nonce: u64,
    expires_ms: u64,
    order: Order,
    alo: bool,
    #[serde(default)]
    purpose: wire::Purpose,
    submitted_ns: u64,
    #[serde(default)]
    clock_id: Option<u64>,
    response: Option<Value>,
    oid: Option<u64>,
    trades: Vec<Trade>,
    applied: bool,
    terminal: Option<String>,
    full: bool,
}
#[derive(Clone, Serialize, Deserialize)]
struct Attempt {
    route: String,
    holdings: Balances,
    done: Vec<Edge>,
    remaining: Vec<Edge>,
    unwind: bool,
    #[serde(default)]
    order_start: usize,
    #[serde(default)]
    cleanup_counts: BTreeMap<u32, u32>,
    #[serde(default)]
    cleanup_until_ms: Option<u64>,
}
#[derive(Clone, Serialize, Deserialize)]
struct State {
    format: u32,
    session: String,
    account: String,
    signer: String,
    created_ms: u64,
    limits: Limits,
    universe: Universe,
    config: Config,
    baseline: Balances,
    #[serde(default)]
    baseline_value: Option<Decimal>,
    balances: Balances,
    orders: Vec<Sent>,
    attempt: Option<Attempt>,
    last_nonce: u64,
    actions: u32,
    route_attempts: BTreeMap<String, u32>,
    completed_routes: BTreeMap<String, u32>,
    tried: BTreeMap<String, u64>,
    phase: String,
    blocked: Option<String>,
    allowed: Vec<Route>,
}
impl State {
    fn remap(&mut self, new: &Universe) -> Result<()> {
        let tokens: BTreeMap<_, _> = self
            .universe
            .tokens
            .iter()
            .filter_map(|(i, t)| {
                new.tokens
                    .iter()
                    .find(|(_, n)| n.token_id == t.token_id)
                    .map(|(j, _)| (*i, *j))
            })
            .collect();
        let markets: BTreeMap<_, _> = self
            .universe
            .markets
            .iter()
            .enumerate()
            .filter_map(|(i, m)| {
                new.markets
                    .iter()
                    .position(|n| {
                        n.index == m.index
                            && Some(&n.base) == tokens.get(&m.base)
                            && Some(&n.quote) == tokens.get(&m.quote)
                    })
                    .map(|j| (i, j))
            })
            .collect();
        let remap_balance = |b: &Balances| -> Result<Balances> {
            b.iter()
                .filter(|(_, q)| **q != Decimal::ZERO)
                .map(|(i, q)| {
                    let j = *tokens.get(i).context("referenced live token removed")?;
                    let (old, next) = (&self.universe.tokens[i], &new.tokens[&j]);
                    ensure!(
                        old.sz_decimals == next.sz_decimals
                            && old.wei_decimals == next.wei_decimals,
                        "referenced live precision changed"
                    );
                    Ok((j, *q))
                })
                .collect()
        };
        let baseline = remap_balance(&self.baseline)?;
        let balances = remap_balance(&self.balances)?;
        let edge = |e: &mut Edge| -> Result<()> {
            let next = *markets
                .get(&e.market)
                .context("referenced live market removed")?;
            let old = &self.universe.markets[e.market];
            for token in [old.base, old.quote] {
                let t = &self.universe.tokens[&token];
                let n = &new.tokens[&tokens[&token]];
                ensure!(
                    t.sz_decimals == n.sz_decimals && t.wei_decimals == n.wei_decimals,
                    "referenced live precision changed"
                );
            }
            e.market = next;
            Ok(())
        };
        let mut orders = self.orders.clone();
        for o in &mut orders {
            edge(&mut o.order.edge)?;
            for f in &mut o.trades {
                f.fee_token = *tokens.get(&f.fee_token).context("fee identity removed")?;
            }
        }
        let mut attempt = self.attempt.clone();
        if let Some(a) = &mut attempt {
            a.holdings = remap_balance(&a.holdings)?;
            a.cleanup_counts = a
                .cleanup_counts
                .iter()
                .map(|(t, n)| Ok((*tokens.get(t).context("cleanup token removed")?, *n)))
                .collect::<Result<_>>()?;
            for e in a.done.iter_mut().chain(&mut a.remaining) {
                edge(e)?;
            }
        }
        let mut allowed = self.allowed.clone();
        for r in &mut allowed {
            for e in &mut r.edges {
                edge(e)?;
            }
        }
        self.baseline = baseline;
        self.balances = balances;
        self.orders = orders;
        self.attempt = attempt;
        self.allowed = allowed;
        self.universe = new.clone();
        Ok(())
    }
}
struct Evidence {
    dir: PathBuf,
    file: File,
    clock: Arc<Clock>,
    writes: Histogram,
    clock_id: u64,
    cpu_start: Option<u64>,
    started_ns: u64,
    failed: bool,
}
impl Evidence {
    fn open(dir: PathBuf, clock: Arc<Clock>) -> Result<Self> {
        Ok(Self {
            failed: false,
            clock_id: hyperliquid::utc_ns(),
            cpu_start: cpu_usage(),
            started_ns: clock.ns(),
            file: OpenOptions::new()
                .append(true)
                .create(true)
                .open(dir.join("execution.jsonl"))?,
            dir,
            clock,
            writes: Histogram::default(),
        })
    }
    fn event(&mut self, kind: &str, data: Value, durable: bool) -> Result<()> {
        let start = self.clock.ns();
        // Docker Desktop bind mounts make many tiny writes expensive. Encode
        // once and issue one write before the durability barrier.
        let mut bytes = serde_json::to_vec(
            &json!({"type":kind,"clock_id":self.clock_id,"receipt_utc_ns":hyperliquid::utc_ns(),"at_ns":start,"data":data}),
        )?;
        bytes.push(b'\n');
        let result = self.file.write_all(&bytes).and_then(|_| {
            if durable {
                self.file.sync_all()
            } else {
                Ok(())
            }
        });
        if result.is_err() {
            self.failed = true;
        }
        result?;
        self.writes.record(self.clock.ns() - start);
        Ok(())
    }
    fn save(&mut self, state: &State, kind: &str) -> Result<()> {
        self.event(kind, serde_json::to_value(state)?, true)?;
        let result = journal::write_json(&self.dir.join("state.json"), state);
        if result.is_err() {
            self.failed = true;
        }
        result
    }
}
#[derive(Default)]
struct AccountStream {
    acknowledged: BTreeSet<String>,
    fills: BTreeMap<(u64, u64), Value>,
    orders: BTreeMap<u64, Value>,
    spot: Option<(u64, Value)>,
    frames: u64,
    connected: bool,
}
struct Market {
    engine: Arc<Mutex<Engine>>,
    account: Arc<Mutex<AccountStream>>,
    failure: Arc<Mutex<Option<String>>>,
    stop: watch::Sender<bool>,
    handle: tokio::task::JoinHandle<Result<()>>,
    changed: Arc<Notify>,
}
impl Market {
    async fn start(
        dir: &Path,
        cfg: Config,
        u: Universe,
        raw: Value,
        user: &str,
        clock: Arc<Clock>,
    ) -> Result<Self> {
        let engine = Engine::scanner(cfg.clone(), u.clone())?;
        let coins = engine.selected_coins();
        let manifest = Manifest {
            format: 4,
            observer_only: true,
            run_id: format!("run-{}", hyperliquid::utc_ns()),
            created_utc_ns: hyperliquid::utc_ns(),
            config: cfg.clone(),
            universe: u.clone(),
            initial_accounts: Some(vec![]),
            source_version: std::fs::read_to_string("SOURCE_SHA256")
                .unwrap_or_else(|_| env!("CARGO_PKG_VERSION").into()),
            cpu_quota: cpu(),
            raw_metadata: Some(raw),
            paper_epoch: None,
            predecessor_run_id: None,
        };
        let run_id = manifest.run_id.clone();
        let journal = Journal::open(&dir.join("market"), manifest)?;
        let engine = Arc::new(Mutex::new(engine));
        let account = Arc::new(Mutex::new(AccountStream::default()));
        let failure = Arc::new(Mutex::new(None));
        let changed = Arc::new(Notify::new());
        let (stop, mut stopped) = watch::channel(false);
        let (tx, rx) = mpsc::channel(cfg.channel_capacity);
        let (reconnect, rrx) = mpsc::channel(1);
        let (mut batches, mut pipeline) =
            crate::hot::launch(u.clone(), u.routes(&cfg)?, cfg.clone(), rx, clock.clone());
        let extra = ["orderUpdates", "userFills", "spotState"]
            .map(|kind| {
                json!({"method":"subscribe","subscription":{"type":kind,"user":user}}).to_string()
            })
            .to_vec();
        let mut feed = tokio::spawn(hyperliquid::follow_extra(
            coins,
            cfg,
            tx,
            clock.clone(),
            rrx,
            None,
            extra,
        ));
        let (owner, stream, failed) = (engine.clone(), account.clone(), failure.clone());
        let change = changed.clone();
        let handle = tokio::spawn(async move {
            let mut seq = 0;
            let mut err = None;
            loop {
                let batch = tokio::select! {biased;
                    _=stopped.changed()=>break,
                    r=&mut feed=>{err=Some(format!("live feed ended: {r:?}"));break},
                    r=&mut pipeline=>{err=Some(format!("live pipeline ended: {r:?}"));break},
                    b=batches.recv()=>match b {Some(b)=>b,None=>{err=Some("live pipeline closed".into());break}},
                };
                seq += 1;
                let input = Input {
                    sequence: seq,
                    generation: batch.wire.generation,
                    receipt_ns: batch.wire.receipt_ns,
                    receipt_utc_ns: batch.wire.utc_ns,
                    process_ns: clock.ns(),
                    hot_started_ns: batch.started_ns,
                    hot_done_ns: batch.done_ns,
                    event: batch.wire.event,
                };
                if matches!(input.event, InputKind::Open | InputKind::Close { .. }) {
                    stream.lock().await.acknowledged.clear();
                }
                if let InputKind::Frame { text } = &input.event {
                    if let Ok(v) = serde_json::from_str::<Value>(text) {
                        let mut a = stream.lock().await;
                        a.frames += 1;
                        if v["channel"] == "subscriptionResponse" {
                            if let Some(kind) = v["data"]["subscription"]["type"].as_str() {
                                a.acknowledged.insert(kind.into());
                            }
                        }
                        if v["channel"] == "userFills" {
                            if let Some(rows) = v["data"]["fills"].as_array() {
                                for f in rows {
                                    if let (Some(oid), Some(tid)) =
                                        (f["oid"].as_u64(), f["tid"].as_u64())
                                    {
                                        a.fills.insert((oid, tid), f.clone());
                                    }
                                }
                            }
                        }
                        if v["channel"] == "orderUpdates" {
                            if let Some(rows) = v["data"].as_array() {
                                for row in rows {
                                    if let Some(oid) = row["order"]["oid"].as_u64() {
                                        a.orders.insert(oid, row.clone());
                                    }
                                }
                            }
                        }
                        if v["channel"] == "spotState" {
                            a.spot = Some((input.receipt_ns, v["data"]["spotState"].clone()));
                        }
                    }
                }
                let mut e = owner.lock().await;
                let was = e.connected;
                let recorded = (|| -> Result<()> {
                    let mut events = e.step_hot(&input, &batch.rates)?;
                    let completed_ns = clock.ns();
                    e.complete_step(&input, completed_ns, &mut events)?;
                    if matches!(input.event, InputKind::Frame { .. }) {
                        e.stats
                            .receipt_to_decision
                            .record(completed_ns - input.receipt_ns);
                        e.stats.processing.record(completed_ns - input.process_ns);
                    }
                    if was && !e.connected {
                        reconnect
                            .try_send(input.generation)
                            .context("live reconnect request failed")?;
                    }
                    journal.record(Record {
                        input,
                        completed_ns,
                        events,
                    })
                })();
                stream.lock().await.connected = e.connected;
                if let Err(error) = recorded {
                    err = Some(format!("{error:#}"));
                    break;
                }
                change.notify_one();
            }
            feed.abort();
            pipeline.abort();
            if let Some(error) = &err {
                *failed.lock().await = Some(error.clone());
            }
            let mut e = owner.lock().await;
            seq += 1;
            let at = clock.ns();
            let input = Input {
                sequence: seq,
                generation: e.generation,
                receipt_ns: at,
                receipt_utc_ns: hyperliquid::utc_ns(),
                process_ns: at,
                hot_started_ns: 0,
                hot_done_ns: 0,
                event: InputKind::Stop {
                    reason: err
                        .clone()
                        .unwrap_or_else(|| "live observer stopped".into()),
                },
            };
            let mut events = e.step(&input)?;
            let completed_ns = clock.ns();
            e.complete_step(&input, completed_ns, &mut events)?;
            events.push(journal::checkpoint(&run_id, &e));
            journal.record(Record {
                input,
                completed_ns,
                events,
            })?;
            let mut report = e.report();
            report["recording_complete"] = err.is_none().into();
            report["recovery"] = journal::checkpoint(&run_id, &e);
            journal.finish(report)?;
            if let Some(error) = err {
                bail!("{error}");
            }
            Ok(())
        });
        Ok(Self {
            engine,
            account,
            failure,
            stop,
            handle,
            changed,
        })
    }
    async fn healthy(&self) -> Result<()> {
        ensure!(
            self.failure.lock().await.is_none(),
            "live recording/feed failed"
        );
        let a = self.account.lock().await;
        ensure!(
            a.connected
                && ["orderUpdates", "userFills", "spotState"]
                    .iter()
                    .all(|k| a.acknowledged.contains(*k)),
            "live market/account stream not ready"
        );
        Ok(())
    }
    async fn finish(self) -> Result<()> {
        let _ = self.stop.send(true);
        self.handle.await??;
        Ok(())
    }
}
fn cpu() -> String {
    std::fs::read_to_string("/sys/fs/cgroup/cpu.max")
        .unwrap_or_else(|_| "unavailable".into())
        .trim()
        .into()
}
fn cpu_usage() -> Option<u64> {
    std::fs::read_to_string("/sys/fs/cgroup/cpu.stat")
        .ok()?
        .lines()
        .find_map(|line| {
            line.strip_prefix("usage_usec ")
                .and_then(|s| s.parse().ok())
        })
}
fn proof() -> Result<Value> {
    let v: Value = serde_json::from_reader(
        File::open(Path::new(ROOT).join("xemm-status.json"))
            .context("run live_preflight.ps1 first")?,
    )?;
    let at = v["checked_utc_ms"]
        .as_u64()
        .context("invalid XEMM preflight proof")?;
    ensure!(
        v["inactive"] == true && utc_ms() >= at && utc_ms() - at < 60_000,
        "XEMM inactivity proof must be fresh (60 seconds)"
    );
    Ok(v)
}
fn terminal(reply: &Value) -> Option<(&str, Decimal, Option<u64>)> {
    let rows = reply.pointer("/response/data/statuses")?.as_array()?;
    let [status] = rows.as_slice() else {
        return None;
    };
    if status["error"].as_str().is_some() {
        return Some(("rejected", Decimal::ZERO, None));
    }
    let f = status.get("filled")?;
    Some((
        "filled",
        dec(f["totalSz"].as_str()?).ok()?,
        f["oid"].as_u64(),
    ))
}
fn status(reply: &Value) -> Option<(&str, Decimal, Option<u64>)> {
    let label = reply
        .pointer("/order/status")
        .or(reply.get("status"))?
        .as_str()?;
    let order = reply.pointer("/order/order");
    let qty = if let Some(o) = order {
        dec(o["origSz"].as_str()?).ok()? - dec(o["sz"].as_str()?).ok()?
    } else {
        Decimal::ZERO
    };
    Some((label, qty, order.and_then(|o| o["oid"].as_u64())))
}
struct Runner {
    client: Client,
    state: State,
    log: Evidence,
    market: Market,
    _lock: File,
    responses: Histogram,
    confirmations: Histogram,
    cleanup_deadline: Option<Instant>,
    entry_deadline: Instant,
}
impl Runner {
    fn owned(&self) -> Balances {
        strategy_inventory(
            &self.state.balances,
            &self.state.allowed,
            &self.state.universe,
        )
    }
    fn mark(&self, balances: &Balances, e: &Engine) -> quantity::InventoryMark {
        let mut m = inventory(balances, &self.state.allowed, e);
        if self.state.format >= 2 {
            m.all_dust = m.indicative_usdc.is_some();
            for row in &mut m.marks {
                let token = row["token"].as_u64().unwrap() as u32;
                let lot = Decimal::new(1, e.universe.tokens[&token].sz_decimals);
                let qty = quantity::amount(balances, token);
                let sub_lot = qty < lot;
                row["lot_size"] = lot.to_string().into();
                row["classification"] = if sub_lot {
                    "sub_lot_residual"
                } else {
                    "whole_lot_exposure"
                }
                .into();
                row["untradeable_dust"] = sub_lot.into();
                m.all_dust &= sub_lot;
            }
        }
        m
    }
    fn equity(&self, e: &Engine) -> Result<Decimal> {
        Ok(quantity::amount(&self.state.balances, e.universe.usdc)
            + self
                .mark(&self.owned(), e)
                .indicative_usdc
                .context("missing residual marks")?)
    }
    fn baseline_value(&self) -> Result<Decimal> {
        if self.state.format < 2 {
            Ok(quantity::amount(
                &self.state.baseline,
                self.state.universe.usdc,
            ))
        } else {
            self.state
                .baseline_value
                .context("missing opening equity baseline")
        }
    }
    fn nonce(&mut self) -> Result<u64> {
        let path = Path::new(ROOT).join(format!("nonce-{}.json", self.state.signer));
        let previous = if path.exists() {
            serde_json::from_reader::<_, u64>(File::open(&path)?)?
        } else {
            0
        };
        let nonce = utc_ms().max(
            self.state
                .last_nonce
                .max(previous)
                .checked_add(1)
                .context("nonce overflow")?,
        );
        journal::write_json(&path, &nonce)?;
        self.state.last_nonce = nonce;
        Ok(nonce)
    }
    async fn check_account(&mut self) -> Result<()> {
        let rows = self.client.user("openOrders").await?;
        let own: BTreeSet<_> = self.state.orders.iter().map(|o| o.cloid.as_str()).collect();
        ensure!(
            rows.as_array()
                .context("open orders response")?
                .iter()
                .all(|o| o["cloid"].as_str().is_some_and(|c| own.contains(c))),
            "foreign open order"
        );
        let recent = self.client.fills(self.state.created_ms).await?;
        let oids: BTreeSet<_> = self.state.orders.iter().filter_map(|o| o.oid).collect();
        ensure!(
            recent
                .iter()
                .all(|f| f["oid"].as_u64().is_some_and(|oid| oids.contains(&oid))
                    || f["cloid"].as_str().is_some_and(|c| own.contains(c))),
            "foreign fill"
        );
        let actual = self.client.balances(&self.state.universe).await?;
        self.log.event(
            "account_audit",
            json!({"open_orders":rows,"fills":recent,"balances":actual}),
            false,
        )?;
        ensure!(
            same_balances(&actual, &self.state.balances, &self.state.universe),
            "unexplained live token balance change"
        );
        self.log
            .event("balance_reconciled", json!({"balances":actual}), true)?;
        Ok(())
    }
    async fn entry_guard(&mut self, legs: usize) -> Result<()> {
        self.market.healthy().await?;
        ensure!(
            self.state.blocked.is_none() && self.state.attempt.is_none(),
            "live account blocked/busy"
        );
        ensure!(
            Instant::now() < self.entry_deadline,
            "session entry deadline reached"
        );
        ensure!(
            self.state.actions + legs as u32 + 2 * legs as u32 + 2 <= self.state.limits.max_actions,
            "insufficient action reserve for unwind"
        );
        self.check_account().await?;
        let e = self.market.engine.lock().await;
        let mark = self.mark(&self.owned(), &e);
        let inventory = mark.indicative_usdc.context("missing residual marks")?;
        ensure!(
            mark.all_dust && (self.state.format >= 2 || inventory <= Decimal::from(5)),
            "material/accumulated residual exposure"
        );
        let change = quantity::amount(&self.state.balances, e.universe.usdc) + inventory
            - self.baseline_value()?;
        ensure!(
            change > -self.state.limits.loss,
            "session loss stop reached"
        );
        ensure!(
            quantity::amount(&self.state.balances, e.universe.usdc) >= self.state.limits.amount,
            "insufficient free USDC"
        );
        Ok(())
    }
    async fn prepare(&self, e: Edge, input: Decimal, unwind: bool) -> Result<Order> {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            if self.market.healthy().await.is_ok() {
                let engine = self.market.engine.lock().await;
                if let Ok(o) = quantity::prepare_model(
                    e,
                    input,
                    &engine.books[e.market],
                    &engine.universe,
                    &engine.config,
                    engine.now,
                    if unwind {
                        engine.config.unwind_slippage_bps
                    } else {
                        engine.config.slippage_bps
                    },
                    4,
                ) {
                    return Ok(o);
                }
            }
            ensure!(
                Instant::now() < deadline
                    && self.cleanup_deadline.is_none_or(|d| Instant::now() < d),
                "no eligible book for live order"
            );
            let until = self
                .cleanup_deadline
                .map(|d| d.min(deadline))
                .unwrap_or(deadline);
            tokio::time::timeout_at(until.into(), self.market.changed.notified())
                .await
                .context("book wait expired")?;
        }
    }
    async fn submit(&mut self, order: Order, alo: bool, discard_reply: bool) -> Result<usize> {
        self.submit_for(
            order,
            if alo {
                wire::Purpose::PostOnly
            } else {
                wire::Purpose::Ioc
            },
            discard_reply,
        )
        .await
    }
    async fn submit_for(
        &mut self,
        order: Order,
        purpose: wire::Purpose,
        discard_reply: bool,
    ) -> Result<usize> {
        self.market.healthy().await?;
        ensure!(
            !self.log.failed,
            "live journal failed; inventory submissions disabled"
        );
        ensure!(
            self.state.orders.iter().all(|o| o.applied),
            "pending order blocks another submission"
        );
        let available = self
            .state
            .attempt
            .as_ref()
            .map(|a| &a.holdings)
            .unwrap_or(&self.state.balances);
        ensure!(
            order.budget <= quantity::amount(available, order.edge.from(&self.state.universe)),
            "order exceeds confirmed allocation"
        );
        ensure!(
            self.state.actions < self.state.limits.max_actions,
            "signed action cap reached"
        );
        ensure!(
            self.cleanup_deadline.is_none_or(|d| Instant::now() < d),
            "cleanup deadline reached"
        );
        let nonce = self.nonce()?;
        let cloid = format!("0x{:016x}{:016x}", self.state.created_ms, nonce);
        let action = wire::order_action_for(&order, &self.state.universe, &cloid, purpose)?;
        let entry = Sent {
            cloid,
            nonce,
            expires_ms: nonce + wire::TTL_MS,
            order,
            alo: purpose == wire::Purpose::PostOnly,
            purpose,
            submitted_ns: self.log.clock.ns(),
            clock_id: Some(self.log.clock_id),
            response: None,
            oid: None,
            trades: vec![],
            applied: false,
            terminal: None,
            full: false,
        };
        self.state.last_nonce = nonce;
        self.state.actions += 1;
        self.state.orders.push(entry);
        let i = self.state.orders.len() - 1;
        self.log.save(&self.state, "durable_order_intent")?;
        if let Some(source) = &self.state.orders[i].order.source {
            let now = self.log.clock.ns();
            ensure!(
                now.saturating_sub(source.price_receipt_ns)
                    <= self.state.config.quote_age_ms * 1_000_000
                    && now.saturating_sub(source.receipt_ns)
                        <= self.state.config.depth_age_ms * 1_000_000,
                "prepared observation expired before send"
            );
        }
        let sent = self.log.clock.ns();
        let reply = self
            .client
            .exchange(&action, nonce, Some(nonce + wire::TTL_MS))
            .await;
        self.responses.record(self.log.clock.ns() - sent);
        self.log.event("submission_response",json!({"cloid":self.state.orders[i].cloid,"sent_ns":sent,"received_ns":self.log.clock.ns(),"discarded_for_test":discard_reply,"response":if discard_reply {Value::Null} else {reply.clone()}}),true)?;
        if !discard_reply {
            self.state.orders[i].oid = reply
                .pointer("/response/data/statuses/0/resting/oid")
                .and_then(Value::as_u64)
                .or_else(|| terminal(&reply).and_then(|(_, _, oid)| oid));
            self.state.orders[i].response = Some(reply);
        }
        self.log.save(&self.state, "order_response_checkpoint")?;
        Ok(i)
    }
    async fn cancel(&mut self, i: usize) -> Result<()> {
        ensure!(
            self.state.actions < self.state.limits.max_actions,
            "no cancel action allowance"
        );
        let o = &self.state.orders[i];
        let action = wire::cancel_action(
            self.state.universe.markets[o.order.edge.market].index,
            &o.cloid,
        );
        let nonce = self.nonce()?;
        self.state.last_nonce = nonce;
        self.state.actions += 1;
        self.log.save(&self.state, "durable_cancel_intent")?;
        let reply = self.client.exchange(&action, nonce, None).await;
        self.log.event(
            "cancel_response",
            json!({"cloid":self.state.orders[i].cloid,"response":reply}),
            true,
        )
    }
    async fn emergency_cancel(&mut self, i: usize) {
        // Cancellation removes risk and is the only signed action allowed when
        // storage cannot record an intent. Its outcome remains unresolved.
        if self.state.actions >= self.state.limits.max_actions {
            return;
        }
        let o = &self.state.orders[i];
        let action = wire::cancel_action(
            self.state.universe.markets[o.order.edge.market].index,
            &o.cloid,
        );
        let nonce = utc_ms().max(self.state.last_nonce.saturating_add(1));
        self.state.last_nonce = nonce;
        self.state.actions += 1;
        let _ = self.client.exchange(&action, nonce, None).await;
        eprintln!("Emergency cancel attempted for this session's resting order; durable recovery required.");
    }
    async fn resolve(&mut self, i: usize) -> Result<bool> {
        if self.state.orders[i].applied {
            return Ok(self.state.orders[i].full);
        }
        let began = Instant::now();
        let mut next_rest = Instant::now();
        while began.elapsed() < Duration::from_secs(60)
            && self.cleanup_deadline.is_none_or(|d| Instant::now() < d)
        {
            let o = &self.state.orders[i];
            let known = o.response.as_ref().and_then(terminal);
            let private = self.market.account.lock().await;
            let ws = o
                .oid
                .and_then(|oid| private.orders.get(&oid))
                .map(|row| json!({"order":row}));
            drop(private);
            let use_rest = Instant::now() >= next_rest;
            let reply = if known.is_some() {
                None
            } else if ws.as_ref().and_then(status).is_some() {
                ws
            } else if use_rest {
                self.client.order_status(&o.cloid).await.ok()
            } else {
                None
            };
            if let Some(v) = &reply {
                self.log
                    .event("order_status", json!({"cloid":o.cloid,"response":v}), false)?;
            }
            let observed = reply.as_ref().and_then(status);
            if !o.alo && observed.is_some_and(|(label,_,_)|label=="open") {
                // IOC/frontend cleanup must not leave an unexpected resting order.
                self.cancel(i).await?;
                continue;
            }
            let end = known
                .or_else(|| observed.filter(|(s, _, _)| !["open", "unknownOid"].contains(s)))
                .or_else(|| {
                    observed
                        .filter(|(s, _, _)| *s == "unknownOid" && utc_ms() > o.expires_ms + 1000)
                });
            if let Some((label, qty, oid)) = end {
                let label = label.to_owned();
                let oid = oid.or(o.oid);
                self.state.orders[i].oid = oid;
                let entry = &self.state.orders[i];
                let mut raw: BTreeMap<(u64, u64), Value> =
                    self.market.account.lock().await.fills.clone();
                let matching = |f: &&Value| {
                    f["oid"].as_u64().is_some_and(|id| Some(id) == oid)
                        || f["cloid"]
                            .as_str()
                            .is_some_and(|c| c.eq_ignore_ascii_case(&entry.cloid))
                };
                let cached: Decimal = raw
                    .values()
                    .filter(matching)
                    .filter_map(|f| f["sz"].as_str().and_then(|s| dec(s).ok()))
                    .sum();
                if cached != qty && use_rest {
                    let recent = self
                        .client
                        .fills(self.state.created_ms.saturating_sub(5000))
                        .await?;
                    self.log.event(
                        "rest_fill_observations",
                        json!({"cloid":entry.cloid,"fills":recent}),
                        false,
                    )?;
                    for f in recent {
                        if let (Some(oid), Some(tid)) = (f["oid"].as_u64(), f["tid"].as_u64()) {
                            raw.insert((oid, tid), f);
                        }
                    }
                }
                let trades: Vec<_> = raw
                    .values()
                    .filter(|f| {
                        f["oid"].as_u64().is_some_and(|id| Some(id) == oid)
                            || f["cloid"]
                                .as_str()
                                .is_some_and(|c| c.eq_ignore_ascii_case(&entry.cloid))
                    })
                    .map(|f| wire::trade(f, &entry.order, &self.state.universe))
                    .collect::<Result<_>>()?;
                let total: Decimal = trades.iter().map(|f| f.qty).sum();
                ensure!(total <= entry.order.qty, "venue reported excess fill");
                if total == qty {
                    let mut balances = self.state.balances.clone();
                    wire::apply_trades(&mut balances, &entry.order, &trades, &self.state.universe)?;
                    let private = self.market.account.lock().await;
                    let snapshot = private
                        .spot
                        .as_ref()
                        .filter(|(at, v)| {
                            *at >= entry.submitted_ns
                                && wire::balances(v, &self.state.universe).is_ok_and(|b| {
                                    same_balances(&b, &balances, &self.state.universe)
                                })
                        })
                        .map(|(_, v)| v.clone());
                    drop(private);
                    let actual_raw = if let Some(v) = snapshot {
                        Ok(v)
                    } else if use_rest {
                        self.client.user("spotClearinghouseState").await
                    } else {
                        Err(anyhow::anyhow!("waiting for confirmed spot state"))
                    };
                    if let Ok(v) = &actual_raw {
                        self.log.event(
                            "spot_state_observation",
                            json!({"cloid":entry.cloid,"state":v}),
                            false,
                        )?;
                    }
                    let actual = actual_raw.and_then(|v| wire::balances(&v, &self.state.universe));
                    if actual
                        .as_ref()
                        .is_ok_and(|a| same_balances(a, &balances, &self.state.universe))
                    {
                        self.state.balances = actual?;
                        if let Some(a) = &mut self.state.attempt {
                            wire::apply_trades(
                                &mut a.holdings,
                                &entry.order,
                                &trades,
                                &self.state.universe,
                            )?;
                            if total > Decimal::ZERO && !a.unwind {
                                a.done.push(entry.order.edge);
                            }
                            if !a.unwind && a.remaining.first() == Some(&entry.order.edge) {
                                a.remaining.remove(0);
                            }
                        }
                        let o = &mut self.state.orders[i];
                        o.trades = trades;
                        o.applied = true;
                        o.terminal = Some(label);
                        o.full = total == o.order.qty;
                        if o.clock_id == Some(self.log.clock_id) {
                            self.confirmations
                                .record(self.log.clock.ns() - o.submitted_ns);
                        }
                        let full = o.full;
                        self.log.save(&self.state, "confirmed_order_and_balances")?;
                        return Ok(full);
                    }
                }
            }
            if use_rest {
                next_rest = Instant::now() + Duration::from_secs(2);
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        bail!(
            "order {} remains unresolved; no resubmission",
            self.state.orders[i].cloid
        )
    }
    async fn establish_baseline(&mut self) -> Result<()> {
        if self.state.format < 2 || self.state.baseline_value.is_some() {
            return Ok(());
        }
        let until = Instant::now() + Duration::from_secs(30);
        loop {
            let e = self.market.engine.lock().await;
            if let Ok(value) = self.equity(&e) {
                let mark = self.mark(&self.owned(), &e);
                ensure!(
                    mark.all_dust,
                    "opening whole-lot inventory requires explicit reconciliation"
                );
                self.state.baseline_value = Some(value);
                self.log.event(
                    "opening_inventory_marks",
                    json!({"equity_usdc":value,"inventory":mark}),
                    true,
                )?;
                return self.log.save(&self.state, "opening_equity_recorded");
            }
            drop(e);
            tokio::time::timeout_at(until.into(), self.market.changed.notified())
                .await
                .context("opening marks unavailable")?;
        }
    }
    async fn cleanup_residuals(&mut self) -> Result<()> {
        ensure!(!self.log.failed, "journal failure blocks cleanup orders");
        ensure!(
            self.state.orders.iter().all(|o| o.applied),
            "unresolved order blocks cleanup"
        );
        self.check_account().await?;
        if self.state.attempt.is_none() {
            return Ok(());
        }
        let a = self.state.attempt.as_mut().unwrap();
        a.unwind = true;
        let end = *a.cleanup_until_ms.get_or_insert_with(|| utc_ms() + 120_000);
        ensure!(utc_ms() < end, "cleanup deadline expired");
        self.cleanup_deadline = Some(Instant::now() + Duration::from_millis(end - utc_ms()));
        self.log.save(&self.state, "residual_cleanup_started")?;
        loop {
            let token = {
                let e = self.market.engine.lock().await;
                let holdings = &self.state.attempt.as_ref().unwrap().holdings;
                let mark = self.mark(holdings, &e);
                mark.indicative_usdc.context("missing cleanup marks")?;
                let mut rows: Vec<_> = mark
                    .marks
                    .iter()
                    .filter(|v| v["classification"] == "whole_lot_exposure")
                    .map(|v| {
                        Ok((
                            v["token"].as_u64().context("cleanup token")? as u32,
                            dec(v["indicative_usdc"].as_str().context("cleanup value")?)?,
                        ))
                    })
                    .collect::<Result<_>>()?;
                rows.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
                rows.first().map(|r| r.0)
            };
            let Some(token) = token else {
                break;
            };
            ensure!(
                self.state
                    .attempt
                    .as_ref()
                    .unwrap()
                    .cleanup_counts
                    .get(&token)
                    .copied()
                    .unwrap_or(0)
                    < 2,
                "cleanup attempts exhausted for token {token}"
            );
            let market = self
                .state
                .universe
                .markets
                .iter()
                .position(|m| m.base == token && m.quote == self.state.universe.usdc)
                .context("no authorized direct USDC cleanup market")?;
            let edge = Edge { market, buy: false };
            let order = loop {
                ensure!(utc_ms() < end, "cleanup deadline expired");
                self.market.healthy().await?;
                let e = self.market.engine.lock().await;
                let qty = quantity::amount(&self.state.attempt.as_ref().unwrap().holdings, token);
                let prepared = quantity::prepare_residual(
                    edge,
                    qty,
                    &e.books[market],
                    &e.universe,
                    &e.config,
                    e.now,
                );
                drop(e);
                if let Ok(order) = prepared {
                    break order;
                }
                tokio::time::timeout_at(
                    self.cleanup_deadline.unwrap().into(),
                    self.market.changed.notified(),
                )
                .await
                .context("cleanup book unavailable")?;
            };
            *self
                .state
                .attempt
                .as_mut()
                .unwrap()
                .cleanup_counts
                .entry(token)
                .or_default() += 1;
            let i = self
                .submit_for(order, wire::Purpose::Residual, false)
                .await?;
            self.resolve(i).await?;
        }
        self.cleanup_deadline = None;
        self.check_account().await
    }
    async fn cycle_refined(
        &mut self,
        route: Route,
        discard: bool,
        interrupt: bool,
        retain_lot: bool,
    ) -> Result<bool> {
        self.entry_guard(route.edges.len()).await?;
        let until = Instant::now() + Duration::from_secs(30);
        let plan = loop {
            let e = self.market.engine.lock().await;
            let plan = quantity::live_plan(
                &route,
                self.state.limits.amount,
                &self.owned(),
                &e.books,
                &e.universe,
                &e.config,
                e.now,
            );
            drop(e);
            if let Ok(plan) = plan {
                break plan;
            }
            tokio::time::timeout_at(until.into(), self.market.changed.notified())
                .await
                .context("route lacks executable lot-aware plan")?;
        };
        {
            let e = self.market.engine.lock().await;
            ensure!(
                self.equity(&e)? - self.baseline_value()? + plan.estimate.profit
                    > -self.state.limits.loss,
                "estimated loss exceeds test budget"
            );
            if self.state.phase == "live_run" && plan.estimate.bps <= e.config.min_profit_bps {
                return Ok(false);
            }
        }
        if !interrupt {
            let count = self
                .state
                .route_attempts
                .entry(route.id.clone())
                .or_default();
            ensure!(
                self.state.phase == "live_run" || *count < 2,
                "triangle attempt limit reached"
            );
            *count += 1;
        }
        self.log.event("entry_plan",json!({"route":route.id,"plan":plan,"diagnostic":self.state.phase!="live_run","controlled_cleanup":retain_lot}),true)?;
        self.state.attempt = Some(Attempt {
            route: route.id,
            holdings: plan.opening.clone(),
            done: vec![],
            remaining: plan.orders.iter().map(|o| o.edge).collect(),
            unwind: false,
            order_start: self.state.orders.len(),
            cleanup_counts: BTreeMap::new(),
            cleanup_until_ms: None,
        });
        self.log.save(&self.state, "opening_allocation_reserved")?;
        for (n, planned) in plan.orders.iter().enumerate() {
            ensure!(
                Instant::now() < self.entry_deadline,
                "entry deadline reached during forward route"
            );
            self.market.healthy().await?;
            let order = {
                let e = self.market.engine.lock().await;
                let input = quantity::amount(
                    &self.state.attempt.as_ref().unwrap().holdings,
                    planned.edge.from(&e.universe),
                );
                prepare_planned(planned, input, &e)
            };
            let mut order = match order {
                Ok(o) => o,
                Err(_) => {
                    self.unwind().await?;
                    return Ok(false);
                }
            };
            if retain_lot && n + 1 == plan.orders.len() {
                ensure!(
                    !order.edge.buy && self.state.universe.markets[order.edge.market].index == 107,
                    "controlled residual requires closing HYPE sell"
                );
                order.qty -= Decimal::new(
                    1,
                    self.state.universe.tokens[&order.edge.from(&self.state.universe)].sz_decimals,
                );
                ensure!(
                    order.qty * order.limit >= Decimal::from(10),
                    "controlled lot would make closing order too small"
                );
            }
            let i = self.submit(order, false, discard && n == 0).await?;
            if !self.resolve(i).await? {
                self.unwind().await?;
                return Ok(false);
            }
            if interrupt && n == 0 {
                self.state.phase = "interrupted_validation".into();
                self.log
                    .save(&self.state, "controlled_stop_after_confirmed_first_leg")?;
                return Ok(false);
            }
        }
        self.cleanup_residuals().await?;
        self.finish_attempt(true).await?;
        Ok(true)
    }
    async fn unwind(&mut self) -> Result<()> {
        if self.state.format >= 2 {
            self.cleanup_residuals().await?;
            return self.finish_attempt(false).await;
        }
        self.cleanup_deadline = Some(Instant::now() + Duration::from_secs(120));
        let Some(a) = &mut self.state.attempt else {
            return Ok(());
        };
        a.unwind = true;
        self.log.save(&self.state, "unwind_started")?;
        let edges = self.state.attempt.as_ref().unwrap().done.clone();
        for edge in edges.into_iter().rev().map(Edge::reverse) {
            for _ in 0..2 {
                let qty = quantity::amount(
                    &self.state.attempt.as_ref().unwrap().holdings,
                    edge.from(&self.state.universe),
                );
                if qty == Decimal::ZERO {
                    break;
                }
                {
                    let engine = self.market.engine.lock().await;
                    let mark = inventory(
                        &Balances::from([(edge.from(&self.state.universe), qty)]),
                        &self.state.allowed,
                        &engine,
                    );
                    if mark.all_dust && mark.indicative_usdc.is_some_and(|v| v <= Decimal::from(5))
                    {
                        break;
                    }
                }
                let order = match self.prepare(edge, qty, true).await {
                    Ok(o) => o,
                    Err(error) => {
                        let engine = self.market.engine.lock().await;
                        let mark = inventory(
                            &Balances::from([(edge.from(&self.state.universe), qty)]),
                            &self.state.allowed,
                            &engine,
                        );
                        if mark.all_dust
                            && mark.indicative_usdc.is_some_and(|v| v <= Decimal::from(5))
                        {
                            break;
                        }
                        return Err(error);
                    }
                };
                let i = self.submit(order, false, false).await?;
                if self.resolve(i).await? {
                    break;
                }
            }
        }
        self.cleanup_deadline = None;
        self.finish_attempt(false).await
    }
    async fn finish_attempt(&mut self, completed: bool) -> Result<()> {
        let e = self.market.engine.lock().await;
        let mark = self.mark(&self.owned(), &e);
        ensure!(
            mark.all_dust
                && mark
                    .indicative_usdc
                    .is_some_and(|v| self.state.format >= 2 || v <= Decimal::from(5)),
            "retained material/unknown residual exposure"
        );
        ensure!(
            self.state.orders.iter().all(|o| o.applied),
            "unresolved order at attempt completion"
        );
        if let Some(a) = &self.state.attempt {
            let times: Vec<_> = self.state.orders[a.order_start..]
                .iter()
                .flat_map(|o| o.trades.iter().map(|f| f.exchange_ms))
                .collect();
            self.log.event("attempt_residuals",json!({"route":a.route,"completed":completed,"inventory":mark,
                "first_fill_exchange_ms":times.iter().min(),"last_fill_exchange_ms":times.iter().max(),
                "fill_to_final_fill_ms":times.iter().max().zip(times.iter().min()).map(|(last,first)|last-first),
                "timing_scope":"exchange fill timestamps; residual sub-lot holdings remain"}),true)?;
        }
        if completed {
            let route = self
                .state
                .attempt
                .as_ref()
                .context("missing live attempt")?
                .route
                .clone();
            *self.state.completed_routes.entry(route).or_default() += 1;
        }
        self.state.attempt = None;
        self.log.save(
            &self.state,
            if completed {
                "cycle_completed"
            } else {
                "observable_unwind_completed"
            },
        )?;
        Ok(())
    }
    async fn probe_zero(&mut self, i: usize, edge: Edge) -> Result<()> {
        if self.state.orders[i].trades.is_empty() {
            return Ok(());
        }
        self.state.attempt = Some(Attempt {
            route: "unexpected_probe_fill_cleanup".into(),
            holdings: self
                .state
                .balances
                .iter()
                .filter(|(t, _)| **t != self.state.universe.usdc)
                .map(|(t, q)| (*t, *q - quantity::amount(&self.state.baseline, *t)))
                .collect(),
            done: vec![edge],
            remaining: vec![],
            unwind: true,
            order_start: i,
            cleanup_counts: BTreeMap::new(),
            cleanup_until_ms: None,
        });
        self.log.save(&self.state, "unexpected_probe_fill")?;
        self.unwind().await?;
        bail!("probe filled unexpectedly; confirmed inventory cleaned up")
    }
    async fn cycle(&mut self, route: Route, discard: bool, interrupt: bool) -> Result<bool> {
        self.cycle_refined(route, discard, interrupt, false).await
    }
}
fn same_balances(a: &Balances, b: &Balances, u: &Universe) -> bool {
    a.keys().chain(b.keys()).all(|t| {
        u.tokens.get(t).is_some_and(|token| {
            (quantity::amount(a, *t) - quantity::amount(b, *t)).abs()
                <= Decimal::new(1, token.wei_decimals)
        })
    })
}
fn strategy_inventory(b: &Balances, allowed: &[Route], u: &Universe) -> Balances {
    let tokens: BTreeSet<_> = allowed
        .iter()
        .flat_map(|r| r.edges.iter().flat_map(|e| [e.from(u), e.to(u)]))
        .collect();
    b.iter()
        .filter(|(t, q)| **t != u.usdc && **q > Decimal::ZERO && tokens.contains(t))
        .map(|(t, q)| (*t, *q))
        .collect()
}
fn prepare_planned(planned: &Order, input: Decimal, e: &Engine) -> Result<Order> {
    let edge = planned.edge;
    let budget = if edge.buy {
        planned.qty * planned.limit
    } else {
        input
    };
    ensure!(
        budget <= input,
        "confirmed proceeds cannot fund planned quantity"
    );
    let mut order = quantity::prepare_model(
        edge,
        budget,
        &e.books[edge.market],
        &e.universe,
        &e.config,
        e.now,
        e.config.slippage_bps,
        4,
    )?;
    ensure!(
        order.qty >= planned.qty,
        "changed book cannot support planned quantity"
    );
    if edge.buy {
        order.limit = order.limit.min(planned.limit);
        order.qty = planned.qty;
    }
    order.budget = input;
    Ok(order)
}
/// Bid marks and legacy dust classification over authorized exits and direct-USDC
/// markets. Runner::mark applies the stricter sub-lot rule for live version 2.
fn inventory(b: &Balances, allowed: &[Route], e: &Engine) -> quantity::InventoryMark {
    let ids: BTreeSet<_> = allowed
        .iter()
        .flat_map(|r| r.edges.iter().map(|edge| edge.market))
        .collect();
    let mut u = e.universe.clone();
    let mut books = vec![];
    u.markets = e
        .universe
        .markets
        .iter()
        .enumerate()
        .filter(|(i, m)| ids.contains(i) || (m.quote == u.usdc && b.contains_key(&m.base)))
        .map(|(i, m)| {
            books.push(e.books[i].clone());
            m.clone()
        })
        .collect();
    quantity::mark_inventory(b, &books, &u, &e.config, e.now)
}
fn routes(u: &Universe, cfg: &Config) -> Result<[Route; 2]> {
    let all = u.routes(cfg)?;
    let find = |wanted: &[(u32, bool)]| -> Result<Route> {
        all.iter()
            .find(|r| {
                r.funded_edges(u).is_some_and(|edges| {
                    edges
                        .iter()
                        .map(|e| (u.markets[e.market].index, e.buy))
                        .eq(wanted.iter().copied())
                })
            })
            .cloned()
            .context("required spot triangle absent")
    };
    let a = find(&[(107, true), (207, false), (166, false)])?;
    let b = find(&[(150, true), (255, true), (107, false)])?;
    for (route, names) in [
        (&a, ["USDC", "HYPE", "USDT0", "USDC"]),
        (&b, ["USDC", "USDE", "HYPE", "USDC"]),
    ] {
        let edges = route.funded_edges(u).unwrap();
        for (i, e) in edges.iter().enumerate() {
            ensure!(
                u.tokens[&e.from(u)].name == names[i] && u.tokens[&e.to(u)].name == names[i + 1],
                "required route token identities changed"
            );
        }
    }
    Ok([a, b])
}
fn value(args: &[String], key: &str) -> Option<String> {
    args.windows(2).find(|a| a[0] == key).map(|a| a[1].clone())
}
pub async fn run(args: &[String]) -> Result<()> {
    let command = args
        .first()
        .map(String::as_str)
        .context("live commands: check, validate, reconcile, cleanup, run")?;
    ensure!(
        ["check", "validate", "reconcile", "cleanup", "run"].contains(&command),
        "unknown live command"
    );
    let mut at = 1;
    while at < args.len() {
        if ["--allow-real-orders", "--residual-check"].contains(&args[at].as_str()) {
            at += 1;
            continue;
        }
        ensure!(
            [
                "--session",
                "--credentials",
                "--amount-usdc",
                "--loss-usdc",
                "--duration",
                "--routes"
            ]
            .contains(&args[at].as_str()),
            "unknown live option"
        );
        ensure!(
            at + 1 < args.len() && !args[at + 1].starts_with("--"),
            "missing live option value"
        );
        at += 2;
    }
    ensure!(
        !args.iter().any(|s| s == "--residual-check") || command == "validate",
        "--residual-check requires live validate"
    );
    let client = Client::load(Path::new(
        &value(args, "--credentials").unwrap_or_else(|| "/run/secrets/hyperliquid.env".into()),
    ))?;
    let mut cfg = Config::load(Path::new("config.toml"))?;
    let fees = client.user("userFees").await?;
    cfg.taker_fee_bps = dec(fees["userSpotCrossRate"]
        .as_str()
        .context("missing effective spot fee")?)?
        * Decimal::from(10000);
    let (u, raw) = hyperliquid::discover(&cfg).await?;
    let required = routes(&u, &cfg)?;
    if command == "check" {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({"network":"mainnet","account":client.account,
            "signer":client.signer,"role":client.user("userRole").await?["role"],
            "spot_balances":client.balances(&u).await?,"open_orders":client.user("openOrders").await?,
            "effective_spot_fee":fees["userSpotCrossRate"],"routes":required,"xemm_proof":proof().ok(),
            "production_started":false}))?
        );
        return Ok(());
    }
    let session = value(args, "--session").context("live session ID required")?;
    ensure!(
        !session.is_empty()
            && session.len() <= 64
            && session
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'),
        "invalid session ID"
    );
    let dir = Path::new(ROOT).join(&session);
    if command == "reconcile" {
        let state: State = serde_json::from_reader(File::open(dir.join("state.json"))?)?;
        ensure!(
            [1, 2].contains(&state.format),
            "unsupported live accounting version"
        );
        ensure!(
            state.account == client.account && state.signer == client.signer,
            "session identity mismatch"
        );
        let mut outcomes = vec![];
        for o in &state.orders {
            outcomes.push(json!({"cloid":o.cloid,"status":client.order_status(&o.cloid).await?,"already_applied":o.applied}));
        }
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({"read_only":true,"orders":outcomes,
            "actual_spot":client.user("spotClearinghouseState").await?,"retained_attempt":state.attempt,"blocked":state.blocked}))?
        );
        return Ok(());
    }
    ensure!(
        args.iter().any(|s| s == "--allow-real-orders"),
        "real orders require explicit --allow-real-orders"
    );
    let inactive = proof()?;
    let allowance = client.user("userRateLimit").await?;
    let remaining = allowance["nRequestsCap"]
        .as_u64()
        .context("action allowance unavailable")?
        .saturating_sub(
            allowance["nRequestsUsed"]
                .as_u64()
                .context("used actions unavailable")?,
        )
        + allowance["nRequestsSurplus"].as_u64().unwrap_or(0);
    ensure!(
        remaining >= 160,
        "insufficient action allowance for bounded tests and cleanup"
    );
    ensure!(
        ["subAccount", "vault", "user"].contains(
            &client.user("userRole").await?["role"]
                .as_str()
                .context("account role unavailable")?
        ),
        "invalid traded account role"
    );
    std::fs::create_dir_all(ROOT)?;
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(Path::new(ROOT).join(format!("{}.lock", client.signer)))?;
    lock.try_lock()
        .context("another live execution owner holds this signer")?;
    let clock = Arc::new(Clock::new());
    let state = if command == "cleanup" {
        let mut saved: State = serde_json::from_reader(File::open(dir.join("state.json"))?)?;
        ensure!(
            [1, 2].contains(&saved.format),
            "unsupported live accounting version"
        );
        ensure!(
            saved.account == client.account && saved.signer == client.signer,
            "cleanup session identity mismatch"
        );
        saved.remap(&u)?;
        saved
    } else {
        ensure!(
            !dir.exists(),
            "session already exists; reconcile/cleanup instead of refunding or repeating tests"
        );
        if let Ok(prior) = std::fs::read_to_string(Path::new(ROOT).join("current.json")) {
            let prior: PathBuf = serde_json::from_str(&prior)?;
            let old: State = serde_json::from_reader(File::open(prior.join("state.json"))?)?;
            ensure!(
                old.attempt.is_none()
                    && old.orders.iter().all(|o| o.applied)
                    && old.blocked.is_none(),
                "prior live session unresolved"
            );
        }
        let limits = Limits {
            amount: dec(&value(args, "--amount-usdc").unwrap_or_else(|| "50".into()))?,
            loss: dec(&value(args, "--loss-usdc").unwrap_or_else(|| "5".into()))?,
            duration_secs: value(args, "--duration")
                .map(|v| v.parse())
                .transpose()?
                .unwrap_or(1800),
            max_actions: 32,
        };
        ensure!(
            limits.amount >= Decimal::from(12)
                && limits.amount <= Decimal::from(50)
                && limits.loss > Decimal::ZERO
                && limits.loss <= Decimal::from(5)
                && (1..=1800).contains(&limits.duration_secs),
            "live limits exceed authorized bounds"
        );
        if command == "run" {
            ensure!(
                ["--amount-usdc", "--loss-usdc", "--duration", "--routes"]
                    .iter()
                    .all(|k| value(args, k).is_some()),
                "live run requires explicit finite limits and route allowlist"
            );
        }
        let balances = client.balances(&u).await?;
        ensure!(
            quantity::amount(&balances, u.usdc) >= limits.amount + Decimal::from(10),
            "insufficient spot test funding"
        );
        ensure!(
            client
                .user("openOrders")
                .await?
                .as_array()
                .is_some_and(Vec::is_empty),
            "open orders block live startup"
        );
        let perps = client.user("clearinghouseState").await?;
        ensure!(
            perps["assetPositions"]
                .as_array()
                .context("perp preflight")?
                .iter()
                .all(|p| p["position"]["szi"].as_str().and_then(|s| dec(s).ok())
                    == Some(Decimal::ZERO)),
            "existing perp exposure blocks shared-account validation"
        );
        std::fs::create_dir(&dir)?;
        let allowed = if command == "run" {
            let ids: BTreeSet<_> = value(args, "--routes")
                .unwrap()
                .split(',')
                .map(str::to_owned)
                .collect();
            let all = u.routes(&cfg)?;
            ensure!(
                ids.iter().all(|id| all.iter().any(|r| &r.id == id)),
                "unknown live route allowlist"
            );
            all.into_iter().filter(|r| ids.contains(&r.id)).collect()
        } else {
            required.to_vec()
        };
        State {
            format: 2,
            session: session.clone(),
            account: client.account.clone(),
            signer: client.signer.clone(),
            created_ms: utc_ms(),
            limits,
            universe: u.clone(),
            config: cfg.clone(),
            baseline: balances.clone(),
            baseline_value: None,
            balances,
            orders: vec![],
            attempt: None,
            last_nonce: 0,
            actions: 0,
            route_attempts: BTreeMap::new(),
            completed_routes: BTreeMap::new(),
            tried: BTreeMap::new(),
            phase: if command == "run" {
                "live_run".into()
            } else {
                "validation".into()
            },
            blocked: None,
            allowed,
        }
    };
    cfg.amounts_usdc = vec![state.limits.amount];
    let mut log = Evidence::open(dir.clone(), clock.clone())?;
    log.event("live_preflight",json!({"inactive":inactive,"reference_connector":REFERENCE,"fees":fees,"cpu_quota":cpu(),"source_version":std::fs::read_to_string("SOURCE_SHA256").ok()}),true)?;
    log.save(&state, "session_checkpoint")?;
    journal::write_json(&Path::new(ROOT).join("current.json"), &dir)?;
    let market = Market::start(&dir, cfg, u, raw, &client.account, clock).await?;
    let ready = Instant::now();
    while market.healthy().await.is_err() {
        ensure!(
            ready.elapsed() < Duration::from_secs(30),
            "live stream did not become ready"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let entry_deadline = Instant::now() + Duration::from_secs(state.limits.duration_secs);
    let mut runner = Runner {
        client,
        state,
        log,
        market,
        _lock: lock,
        responses: Histogram::default(),
        confirmations: Histogram::default(),
        cleanup_deadline: None,
        entry_deadline,
    };
    let result = tokio::select! {result=execute(&mut runner,command,args,required)=>result,
    _=shutdown()=>Err(anyhow::anyhow!("operator stopped live owner; forward execution stopped"))};
    if let Err(error) = &result {
        runner.state.blocked = Some(format!("{error:#}"));
        let _ = runner.log.save(&runner.state, "live_session_blocked");
        for i in 0..runner.state.orders.len() {
            if runner.state.orders[i].alo && !runner.state.orders[i].applied {
                if runner.cancel(i).await.is_err() {
                    runner.emergency_cancel(i).await;
                }
            }
        }
        if runner.state.format >= 2 && !runner.log.failed {
            let cleanup = async {
                runner.cleanup_deadline = Some(Instant::now() + Duration::from_secs(120));
                for i in 0..runner.state.orders.len() {
                    if !runner.state.orders[i].applied {
                        runner.resolve(i).await?;
                    }
                }
                runner.unwind().await
            }
            .await;
            if let Err(error) = cleanup {
                runner.state.blocked = Some(format!(
                    "{}; cleanup: {error:#}",
                    runner.state.blocked.as_deref().unwrap_or("failed")
                ));
            }
            let _ = runner.log.save(&runner.state, "failure_cleanup_outcome");
        }
    }
    let e = runner.market.engine.lock().await;
    let mark = runner.mark(&runner.owned(), &e);
    let report = json!({"diagnostic":command!="run","execution_performance_included":command=="run",
        "state":runner.state,"inventory":mark,"cash_change_usdc":quantity::amount(&runner.state.balances,e.universe.usdc)-quantity::amount(&runner.state.baseline,e.universe.usdc),
        "inventory_adjusted_change_usdc":runner.equity(&e).ok().zip(runner.baseline_value().ok()).map(|(end,start)|end-start),
        "unresolved_exposure":runner.state.attempt.is_some() || runner.state.orders.iter().any(|o|!o.applied),
        "receipt_to_decision":e.stats.receipt_to_decision.report(),"queue_age":e.stats.queue_age.report(),
        "submission_response":runner.responses.report(),"fill_and_balance_confirmation":runner.confirmations.report(),
        "durable_write":runner.log.writes.report(),"cpu_quota":cpu(),
        "cpu_usage":cpu_usage().zip(runner.log.cpu_start).map(|(end,start)|json!({"used_usec":end.saturating_sub(start),"wall_ns":runner.log.clock.ns().saturating_sub(runner.log.started_ns),"scope":"since execution journal opened"})),
        "confirmation_clock_id":runner.log.clock_id,"confirmation_latency_scope":"this monotonic clock domain only; recovered orders excluded",
        "market_report":e.report()});
    drop(e);
    runner.log.event("session_report", report.clone(), true)?;
    journal::write_json(&dir.join("final.json"), &report)?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    runner.market.finish().await?;
    result
}
async fn execute(
    r: &mut Runner,
    command: &str,
    args: &[String],
    required: [Route; 2],
) -> Result<()> {
    r.establish_baseline().await?;
    if command == "cleanup" {
        r.cleanup_deadline = Some(Instant::now() + Duration::from_secs(120));
        for i in 0..r.state.orders.len() {
            if !r.state.orders[i].applied {
                if r.state.orders[i].alo {
                    r.cancel(i).await?;
                }
                r.resolve(i).await?;
            }
        }
        r.state.blocked = None;
        r.unwind().await?;
        r.check_account().await?;
        r.state.phase = if required.iter().all(|route| {
            r.state
                .completed_routes
                .get(&route.id)
                .is_some_and(|n| *n > 0)
        }) {
            "validation_complete".into()
        } else {
            "cleanup_complete".into()
        };
        return r.log.save(&r.state, "explicit_cleanup_complete");
    }
    if command == "run" {
        let allow: BTreeSet<_> = value(args, "--routes")
            .unwrap()
            .split(',')
            .map(str::to_owned)
            .collect();
        {
            let e = r.market.engine.lock().await;
            ensure!(
                allow
                    .iter()
                    .all(|id| e.routes.iter().any(|route| &route.id == id)),
                "unknown route allowlist"
            );
        }
        let mut eligible = BTreeMap::<String, (bool, u64)>::new();
        while Instant::now() < r.entry_deadline {
            let choice = {
                let e = r.market.engine.lock().await;
                let threshold = e.config.min_profit_bps;
                let mut choices = Vec::new();
                for (route, s) in e
                    .routes
                    .iter()
                    .zip(&e.states)
                    .filter(|(route, _)| allow.contains(&route.id))
                {
                    let plan = if e.connected && s.net_bps.is_some_and(|bps| bps > 0.0) {
                        quantity::live_plan(
                            route,
                            r.state.limits.amount,
                            &r.owned(),
                            &e.books,
                            &e.universe,
                            &e.config,
                            e.now,
                        )
                        .ok()
                        .filter(|p| p.estimate.bps > threshold)
                    } else {
                        None
                    };
                    let state = eligible.entry(route.id.clone()).or_insert((false, 0));
                    if plan.is_some() && !state.0 {
                        state.1 += 1;
                    }
                    state.0 = plan.is_some();
                    if let Some(p) = plan {
                        if r.state.tried.get(&route.id) != Some(&state.1) {
                            choices.push((route.clone(), state.1, p.estimate.profit));
                        }
                    }
                }
                choices.sort_by(|a, b| {
                    b.2.cmp(&a.2)
                        .then(a.0.edges.len().cmp(&b.0.edges.len()))
                        .then(a.0.id.cmp(&b.0.id))
                });
                choices.into_iter().next()
            };
            if let Some((route, epoch, _)) = choice {
                r.state.tried.insert(route.id.clone(), epoch);
                r.cycle(route, false, false).await?;
            } else {
                let _ =
                    tokio::time::timeout(Duration::from_secs(1), r.market.changed.notified()).await;
            }
        }
        r.state.phase = "finite_live_run_complete".into();
        return r.log.save(&r.state, "finite_run_complete");
    }
    if args.iter().any(|s| s == "--residual-check") {
        for (n, route) in required.iter().enumerate() {
            let mut passed = false;
            for _ in 0..2 {
                if r.cycle_refined(route.clone(), false, false, n == 1).await? {
                    passed = true;
                    break;
                }
            }
            ensure!(
                passed,
                "residual validation triangle failed within two attempts"
            );
        }
        ensure!(
            r.state
                .orders
                .iter()
                .any(|o| o.purpose == wire::Purpose::Residual
                    && o.trades.iter().any(|f| f.qty * f.px < Decimal::from(10))),
            "no confirmed sub-minimum cleanup fill"
        );
        r.check_account().await?;
        r.state.phase = "residual_validation_complete".into();
        return r.log.save(&r.state, "residual_validation_complete");
    }
    let first = required[0].funded_edges(&r.state.universe).unwrap()[0];
    r.entry_guard(1).await?;
    let mut alo = r.prepare(first, Decimal::from(12), false).await?;
    let dp = r.state.universe.tokens[&r.state.universe.markets[first.market].base].sz_decimals;
    alo.limit = quantity::limit_price(alo.limit * dec("0.9")?, dp, true);
    alo.qty = quantity::floor(Decimal::from(12) / alo.limit, dp);
    alo.budget = Decimal::from(12);
    let i = r.submit(alo, true, false).await?;
    let resting = r.state.orders[i]
        .oid
        .context("post-only test did not rest")?;
    ensure!(
        r.client
            .user("openOrders")
            .await?
            .as_array()
            .context("openOrders")?
            .iter()
            .any(|o| o["oid"] == resting),
        "test order not observed resting"
    );
    r.cancel(i).await?;
    r.resolve(i).await?;
    r.probe_zero(i, first).await?;
    r.check_account().await?;
    let mut zero = r.prepare(first, Decimal::from(12), false).await?;
    zero.limit = quantity::limit_price(zero.limit * dec("0.9")?, dp, true);
    zero.qty = quantity::floor(Decimal::from(12) / zero.limit, dp);
    zero.budget = Decimal::from(12);
    let i = r.submit(zero, false, false).await?;
    r.resolve(i).await?;
    r.probe_zero(i, first).await?;
    for (n, route) in required.iter().enumerate() {
        let mut passed = false;
        for _ in 0..2 {
            if r.cycle(route.clone(), n == 1, false).await? {
                passed = true;
                break;
            }
        }
        ensure!(
            passed,
            "required triangle did not complete within two attempts"
        );
    }
    r.cycle(required[0].clone(), false, true).await?;
    ensure!(
        r.state.phase == "interrupted_validation",
        "controlled first leg did not fill"
    );
    Ok(())
}
async fn shutdown() {
    #[cfg(unix)]
    {
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("SIGTERM handler");
        tokio::select! {_=tokio::signal::ctrl_c()=>{},_=term.recv()=>{}}
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn priced_engine() -> Engine {
        let s = fixture();
        let mut e = Engine::scanner(s.config, s.universe).unwrap();
        e.now = 1;
        e.connected = true;
        for (index, bid, ask) in [
            (107, "86.429", "86.457"),
            (207, "86.473", "86.50"),
            (166, "0.99954", "0.9996"),
            (150, "0.9998", "0.99989"),
            (255, "86.40", "86.418"),
        ] {
            let i = e
                .universe
                .markets
                .iter()
                .position(|m| m.index == index)
                .unwrap();
            let raw = json!({"channel":"l2Book","data":{"coin":format!("@{index}"),"time":1,"levels":[[{"px":bid,"sz":"100000"}],[{"px":ask,"sz":"100000"}]]}}).to_string();
            let update = crate::book::parse(&raw, &e.universe, 1, 1)
                .unwrap()
                .unwrap();
            assert!(e.books[i].apply(&update));
        }
        e
    }
    #[test]
    fn recorded_fills_pool_hype_and_backward_fund_usde() {
        let e = priced_engine();
        let r = routes(&e.universe, &e.config).unwrap();
        let p = quantity::live_plan(
            &r[1],
            Decimal::from(50),
            &Balances::new(),
            &e.books,
            &e.universe,
            &e.config,
            e.now,
        )
        .unwrap();
        assert_eq!(p.orders[0].qty, dec("49.28").unwrap());
        assert_eq!(p.orders[1].qty, dec("0.57").unwrap());
        assert!(p.estimate.residual[&235] < dec("0.05").unwrap());
        let mut balance = Decimal::ZERO;
        let mut sold = vec![];
        for _ in 0..3 {
            balance += dec("0.57").unwrap() - dec("0.00039899").unwrap();
            let q = quantity::floor(balance, 2);
            sold.push(q);
            balance -= q;
        }
        assert_eq!(
            sold,
            vec![
                dec("0.56").unwrap(),
                dec("0.57").unwrap(),
                dec("0.57").unwrap()
            ]
        );
        assert_eq!(balance, dec("0.00880303").unwrap());
        // Actual received USDE fee differs by atomic rounding from the model.
        assert_eq!(
            dec("50").unwrap()
                - dec("0.00699998").unwrap()
                - dec("0.57").unwrap() * dec("86.418").unwrap(),
            dec("0.73474002").unwrap()
        );
        assert_eq!(
            dec("49.28").unwrap() * (Decimal::ONE - dec("0.00014").unwrap())
                - dec("0.57").unwrap() * dec("86.418").unwrap(),
            dec("0.0148408").unwrap()
        );
    }
    #[test]
    fn pooled_plan_does_not_count_old_inventory_as_profit_or_downsize_a_buy() {
        let e = priced_engine();
        let route = routes(&e.universe, &e.config).unwrap()[1].clone();
        let carry = Balances::from([(150, dec("0.02880303").unwrap())]);
        let p = quantity::live_plan(
            &route,
            Decimal::from(50),
            &carry,
            &e.books,
            &e.universe,
            &e.config,
            e.now,
        )
        .unwrap();
        assert_eq!(p.orders[2].qty, dec("0.59").unwrap());
        assert!(p.opening_inventory_debit > Decimal::ONE);
        assert_eq!(p.estimate.profit, p.cash_profit - p.opening_inventory_debit);
        let mut future = priced_engine();
        let i = p.orders[1].edge.market;
        let raw=json!({"channel":"l2Book","data":{"coin":"@255","time":2,"levels":[[{"px":"89","sz":"1000"}],[{"px":"90","sz":"1000"}]]}}).to_string();
        let up = crate::book::parse(&raw, &future.universe, 2, 2)
            .unwrap()
            .unwrap();
        future.books[i].apply(&up);
        future.now = 2;
        assert!(prepare_planned(&p.orders[1], dec("49.28").unwrap(), &future).is_err());
    }
    #[test]
    fn cleanup_sells_whole_lots_below_minimum_with_fixed_limit() {
        let e = priced_engine();
        let edge = Edge {
            market: e
                .universe
                .markets
                .iter()
                .position(|m| m.index == 107)
                .unwrap(),
            buy: false,
        };
        let q = dec("0.02880303").unwrap();
        let o = quantity::prepare_residual(
            edge,
            q,
            &e.books[edge.market],
            &e.universe,
            &e.config,
            e.now,
        )
        .unwrap();
        assert_eq!(o.qty, dec("0.02").unwrap());
        assert!(o.limit >= dec("86.429").unwrap() * dec("0.995").unwrap());
        let cloid = "0x000102030405060708090a0b0c0d0e0f";
        let wire = serde_json::to_value(
            wire::order_action_for(&o, &e.universe, cloid, wire::Purpose::Residual).unwrap(),
        )
        .unwrap();
        assert_eq!(wire["orders"][0]["t"]["limit"]["tif"], "FrontendMarket");
        assert_eq!(wire["orders"][0]["r"], false);
        assert_eq!(wire["orders"][0]["p"], o.limit.normalize().to_string());
        assert!(wire::order_action_for(&o, &e.universe, cloid, wire::Purpose::Ioc).is_err());
        assert!(quantity::prepare_residual(
            edge,
            dec("0.00880303").unwrap(),
            &e.books[edge.market],
            &e.universe,
            &e.config,
            e.now
        )
        .is_err());
        let mut less = o.clone();
        less.qty = dec("0.01").unwrap();
        assert!(
            wire::order_action_for(&less, &e.universe, cloid, wire::Purpose::Residual).is_err()
        );
        let mut buy = o;
        buy.edge.buy = true;
        assert!(wire::order_action_for(&buy, &e.universe, cloid, wire::Purpose::Residual).is_err());
    }
    fn fixture() -> State {
        let config = Config::default();
        let u = Universe::parse(
            &serde_json::from_str(include_str!("../review/hyperliquid_spot_snapshot.json"))
                .unwrap(),
            &config,
        )
        .unwrap();
        State {
            format: 2,
            session: "test".into(),
            account: "account".into(),
            signer: "signer".into(),
            created_ms: 1,
            limits: Limits {
                amount: Decimal::from(50),
                loss: Decimal::from(5),
                duration_secs: 1800,
                max_actions: 32,
            },
            baseline: Balances::from([(u.usdc, Decimal::from(75))]),
            baseline_value: Some(Decimal::from(75)),
            balances: Balances::from([(u.usdc, Decimal::from(25))]),
            orders: vec![],
            attempt: Some(Attempt {
                route: "test".into(),
                holdings: Balances::from([(u.usdc, Decimal::from(50))]),
                done: vec![],
                remaining: routes(&u, &config).unwrap()[0].funded_edges(&u).unwrap(),
                unwind: false,
                order_start: 0,
                cleanup_counts: BTreeMap::new(),
                cleanup_until_ms: None,
            }),
            allowed: routes(&u, &config).unwrap().to_vec(),
            universe: u,
            config,
            last_nonce: 100,
            actions: 1,
            route_attempts: BTreeMap::new(),
            completed_routes: BTreeMap::new(),
            tried: BTreeMap::new(),
            phase: "validation".into(),
            blocked: None,
        }
    }
    #[test]
    fn screening_constructor_never_funds_accounts() {
        let s = fixture();
        assert!(Engine::scanner(s.config.clone(), s.universe.clone())
            .unwrap()
            .accounts
            .is_empty());
        assert!(Engine::new(s.config, s.universe, Some(vec![])).is_err());
    }
    #[test]
    fn checkpoint_preserves_reservation_and_unresolved_attempt() {
        let s = fixture();
        let restored: State = serde_json::from_value(serde_json::to_value(&s).unwrap()).unwrap();
        assert_eq!(restored.balances[&0], Decimal::from(25));
        assert_eq!(restored.attempt.unwrap().holdings[&0], Decimal::from(50));
        assert_eq!(restored.last_nonce, 100);
    }
    #[test]
    fn metadata_reordering_keeps_explicit_route_and_balance_identities() {
        let mut s = fixture();
        let mut new = s.universe.clone();
        new.markets.reverse();
        s.remap(&new).unwrap();
        let a = s.attempt.as_ref().unwrap();
        assert_eq!(s.universe.markets[a.remaining[0].market].index, 107);
        assert_eq!(s.balances[&0], Decimal::from(25));
        assert_eq!(a.holdings[&0], Decimal::from(50));
        let token = s.universe.markets[a.remaining[0].market].base;
        new.tokens.get_mut(&token).unwrap().sz_decimals += 1;
        assert!(s.remap(&new).is_err());
    }
    #[test]
    fn unrelated_metadata_removal_does_not_erase_exposure() {
        let mut s = fixture();
        let mut new = s.universe.clone();
        new.markets
            .retain(|m| [107, 150, 166, 207, 255].contains(&m.index));
        s.remap(&new).unwrap();
        assert!(s.attempt.is_some());
        assert_eq!(s.balances[&0], Decimal::from(25));
        new.markets.retain(|m| m.index != 107);
        assert!(s.remap(&new).is_err());
    }
    #[test]
    fn terminal_status_keeps_unknown_distinct_from_zero_fill() {
        assert!(terminal(
            &json!({"response":{"data":{"statuses":[{"error":"one"},{"error":"two"}]}}})
        )
        .is_none());
        assert!(terminal(&json!({"transport_error":"lost"})).is_none());
        assert_eq!(
            terminal(&json!({"response":{"data":{"statuses":[{"error":"no immediate match"}]}}}))
                .unwrap()
                .1,
            Decimal::ZERO
        );
        assert_eq!(
            status(&json!({"status":"unknownOid"})).unwrap().0,
            "unknownOid"
        );
        assert_eq!(
            status(
                &json!({"order":{"status":"filled","order":{"oid":1,"origSz":"0.12","sz":"0"}}})
            )
            .unwrap()
            .1,
            dec("0.12").unwrap()
        );
    }
    #[test]
    fn signer_lock_rejects_a_second_execution_owner() {
        let path = std::env::temp_dir().join(format!("live-lock-{}", hyperliquid::utc_ns()));
        let a = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&path)
            .unwrap();
        a.try_lock().unwrap();
        let b = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        assert!(b.try_lock().is_err());
        drop(a);
        b.try_lock().unwrap();
        drop(b);
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn failed_durable_write_cannot_be_successful_intent() {
        let mut log = Evidence {
            failed: false,
            dir: PathBuf::from("/tmp"),
            file: OpenOptions::new().write(true).open("/dev/full").unwrap(),
            clock: Arc::new(Clock::new()),
            writes: Histogram::default(),
            clock_id: 1,
            cpu_start: None,
            started_ns: 0,
        };
        assert!(log.save(&fixture(), "intent").is_err());
        assert!(log.failed);
    }
    async fn recovered_fill(partial: bool) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let mut s = fixture();
        let edge = s.attempt.as_ref().unwrap().remaining[0];
        let base = s.universe.markets[edge.market].base;
        s.balances = s.baseline.clone();
        s.attempt.as_mut().unwrap().holdings = Balances::from([(0, Decimal::from(12))]);
        let order = Order {
            edge,
            qty: dec("0.12").unwrap(),
            limit: Decimal::from(100),
            budget: Decimal::from(12),
            source: None,
        };
        s.orders.push(Sent {
            cloid: "0x000102030405060708090a0b0c0d0e0f".into(),
            nonce: 1,
            expires_ms: 2,
            order,
            alo: false,
            purpose: wire::Purpose::Ioc,
            submitted_ns: 0,
            clock_id: None,
            response: None,
            oid: None,
            trades: vec![],
            applied: false,
            terminal: None,
            full: false,
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let mut kinds = vec![];
            for _ in 0..3 {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut bytes = vec![];
                let (header, len) = loop {
                    let mut buf = [0; 4096];
                    let n = socket.read(&mut buf).await.unwrap();
                    assert!(n > 0);
                    bytes.extend_from_slice(&buf[..n]);
                    if let Some(header) = bytes.windows(4).position(|v| v == b"\r\n\r\n") {
                        let text = String::from_utf8_lossy(&bytes[..header]).to_ascii_lowercase();
                        let len: usize = text
                            .lines()
                            .find_map(|s| s.strip_prefix("content-length: "))
                            .unwrap()
                            .parse()
                            .unwrap();
                        if bytes.len() >= header + 4 + len {
                            break (header, len);
                        }
                    }
                };
                let body: Value =
                    serde_json::from_slice(&bytes[header + 4..header + 4 + len]).unwrap();
                let kind = body["type"].as_str().unwrap().to_string();
                kinds.push(kind.clone());
                let reply=match kind.as_str() {
                    "orderStatus"=>json!({"order":{"status":if partial {"canceled"} else {"filled"},"order":{"oid":777,"origSz":"0.12","sz":if partial {"0.06"} else {"0"}}}}),
                    "userFillsByTime"=>json!([{"coin":"@107","oid":777,"tid":55,"side":"B","px":"100","sz":if partial {"0.06"} else {"0.12"},"fee":if partial {"0.000042"} else {"0.000084"},"feeToken":"HYPE","time":100}]),
                    "spotClearinghouseState"=>json!({"balances":[{"token":0,"total":if partial {"69"} else {"63"},"hold":"0"},{"token":base,"total":if partial {"0.059958"} else {"0.119916"},"hold":"0"}]}),
                    _=>panic!("unexpected request; resolver must never submit/retry an order"),
                }.to_string();
                socket
                    .write_all(
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                            reply.len(),
                            reply
                        )
                        .as_bytes(),
                    )
                    .await
                    .unwrap();
            }
            kinds
        });
        let dir = std::env::temp_dir().join(format!("live-recovery-{}", hyperliquid::utc_ns()));
        std::fs::create_dir(&dir).unwrap();
        let clock = Arc::new(Clock::new());
        let engine = Arc::new(Mutex::new(
            Engine::scanner(s.config.clone(), s.universe.clone()).unwrap(),
        ));
        let (stop, _) = watch::channel(false);
        let market = Market {
            engine,
            account: Arc::new(Mutex::new(AccountStream::default())),
            failure: Arc::new(Mutex::new(None)),
            stop,
            handle: tokio::spawn(async { Ok(()) }),
            changed: Arc::new(Notify::new()),
        };
        let mut r = Runner {
            client: Client::testing(url),
            state: s,
            log: Evidence::open(dir.clone(), clock).unwrap(),
            market,
            _lock: File::open("/dev/null").unwrap(),
            responses: Histogram::default(),
            confirmations: Histogram::default(),
            cleanup_deadline: None,
            entry_deadline: Instant::now() + Duration::from_secs(30),
        };
        assert_eq!(r.resolve(0).await.unwrap(), !partial);
        assert_eq!(
            r.confirmations.count, 0,
            "old clock domains must not fabricate zero confirmation latency"
        );
        assert_eq!(
            r.state.balances[&base],
            dec(if partial { "0.059958" } else { "0.119916" }).unwrap()
        );
        let confirmed = r.state.balances.clone();
        assert_eq!(r.resolve(0).await.unwrap(), !partial);
        assert_eq!(r.state.balances, confirmed);
        assert_eq!(r.state.attempt.as_ref().unwrap().done.len(), 1);
        assert_eq!(
            server.await.unwrap(),
            vec!["orderStatus", "userFillsByTime", "spotClearinghouseState"]
        );
        let mut e = priced_engine();
        assert!(
            !r.mark(&Balances::from([(base, dec("0.02880303").unwrap())]), &e)
                .all_dust
        );
        assert!(
            r.mark(&Balances::from([(base, dec("0.00880303").unwrap())]), &e)
                .all_dust
        );
        r.state.baseline_value = Some(dec("75.75").unwrap());
        r.state.balances = Balances::from([(0, Decimal::from(75))]);
        assert_eq!(
            r.equity(&e).unwrap() - r.baseline_value().unwrap(),
            dec("-0.75").unwrap(),
            "pre-existing inventory loss must not disappear from the baseline"
        );
        e.universe.tokens.get_mut(&base).unwrap().sz_decimals = 5;
        assert!(
            r.mark(&Balances::from([(base, dec("0.00000999").unwrap())]), &e)
                .all_dust
        );
        assert!(
            !r.mark(&Balances::from([(base, dec("0.00001001").unwrap())]), &e)
                .all_dust
        );
        e.now = 2_000_000_000;
        assert!(
            !r.mark(&Balances::from([(base, dec("0.00000999").unwrap())]), &e)
                .all_dust,
            "missing marks cannot establish safe residuals"
        );
        drop(r);
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[tokio::test]
    async fn lost_ack_restart_recovers_actual_fill_once_without_submission() {
        recovered_fill(false).await;
    }
    #[tokio::test]
    async fn partial_terminal_recovers_net_holdings_for_unwind() {
        recovered_fill(true).await;
    }
}
