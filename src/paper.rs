use crate::{
    book::Book,
    config::Config,
    market::{Edge, Route, Universe},
    quantity::{self, add, amount, Balances, Fill, Order, Shadow},
};
use anyhow::{ensure, Result};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Pending {
    pub order: Order,
    pub submitted_ns: u64,
    pub arrival_ns: u64,
    pub confirmation_ns: u64,
    pub arrived: bool,
    pub fill: Option<Fill>,
    pub unobservable: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Attempt {
    pub route: String,
    pub start: Decimal,
    pub started_ns: u64,
    pub holdings: Balances,
    pub forward: Vec<Edge>,
    pub done: Vec<Edge>,
    pub next: usize,
    pub unwind: Option<Vec<Edge>>,
    pub pending: Option<Pending>,
    pub tainted: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sizing: Option<Sizing>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Sizing {
    pub orders: Vec<Order>,
    pub opening: Balances,
    pub opening_bids: Balances,
    pub cleanup: bool,
    pub forward_complete: bool,
    pub recovery: bool,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct CleanupRetry {
    pub scope: String,
    pub liquidity: Vec<(Decimal, Decimal)>,
    pub eligible: bool,
    pub submitted: u8,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Model5 {
    pub previous_model: u32,
    pub previous_run: Option<String>,
    pub announced: bool,
    pub opening_cash: Decimal,
    pub opening_completed: u64,
    pub opening_failed: u64,
    pub opening_unobservable: u64,
    pub cash_change: Decimal,
    pub recovery_proceeds: Decimal,
    pub adjusted_closed_profit: Decimal,
    pub retries: BTreeMap<u32, CleanupRetry>,
    pub eligibility: BTreeMap<String, (bool, u64)>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Account {
    pub latency_ms: u64,
    pub balances: Balances,
    pub paused: Option<String>,
    pub attempt: Option<Attempt>,
    pub tried: BTreeMap<String, u64>,
    pub completed: u64,
    pub failed: u64,
    pub unobservable: u64,
    pub realized_usdc: Decimal,
    #[serde(default)]
    pub shadow: Shadow,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entry_guard: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model5: Option<Model5>,
}
impl Account {
    pub fn new(latency_ms: u64, cfg: &Config, usdc: u32) -> Self {
        Self {
            latency_ms,
            balances: Balances::from([(usdc, cfg.starting_usdc)]),
            paused: None,
            attempt: None,
            tried: BTreeMap::new(),
            completed: 0,
            failed: 0,
            unobservable: 0,
            realized_usdc: Decimal::ZERO,
            shadow: Shadow::default(),
            entry_guard: None,
            model5: None,
        }
    }
    fn log(&self, out: &mut Vec<Value>, event: &str, at: u64, data: Value) {
        out.push(
            json!({"type":event,"scenario_each_way_ms":self.latency_ms,"at_ns":at,"data":data}),
        );
    }
    pub fn deadline(&self) -> Option<u64> {
        if self.paused.is_some() {
            return None;
        }
        self.attempt.as_ref()?.pending.as_ref().map(|p| {
            if p.arrived {
                p.confirmation_ns
            } else {
                p.arrival_ns
            }
        })
    }
    pub fn start(
        &mut self,
        r: &Route,
        episode: u64,
        start: Decimal,
        books: &[Book],
        u: &Universe,
        cfg: &Config,
        now: u64,
        out: &mut Vec<Value>,
    ) -> Result<()> {
        self.start_model(r, episode, start, books, u, cfg, now, out, 3)
    }
    pub fn start_model(
        &mut self,
        r: &Route,
        episode: u64,
        start: Decimal,
        books: &[Book],
        u: &Universe,
        cfg: &Config,
        now: u64,
        out: &mut Vec<Value>,
        model: u32,
    ) -> Result<()> {
        if model >= 5 {
            if self.model5.is_none() {
                self.upgrade(5, None, u.usdc);
            }
            let plan = quantity::pooled_plan(
                r,
                start,
                &self.balances,
                books,
                u,
                cfg,
                now,
                Some(&self.shadow),
            )?;
            return self.start_plan(r, episode, plan, books, u, cfg, now, out);
        }
        ensure!(
            self.attempt.is_none() && self.paused.is_none(),
            "account busy"
        );
        ensure!(amount(&self.balances, u.usdc) >= start, "insufficient USDC");
        let forward = r
            .funded_edges(u)
            .ok_or_else(|| anyhow::anyhow!("no USDC route"))?;
        let order = quantity::prepare_model(
            forward[0],
            start,
            &books[forward[0].market],
            u,
            cfg,
            now,
            cfg.slippage_bps,
            model,
        )?;
        add(&mut self.balances, u.usdc, -start);
        self.tried.insert(r.id.clone(), episode);
        self.attempt = Some(Attempt {
            route: r.id.clone(),
            start,
            started_ns: now,
            holdings: Balances::from([(u.usdc, start)]),
            forward,
            done: Vec::new(),
            next: 0,
            unwind: None,
            pending: None,
            tainted: false,
            sizing: None,
        });
        self.log(
            out,
            "reserved",
            now,
            json!({"route":r.id,"amount":start,"free_usdc":amount(&self.balances,u.usdc)}),
        );
        self.submit(order, now, out);
        Ok(())
    }
    fn submit(&mut self, order: Order, now: u64, out: &mut Vec<Value>) {
        let d = self.latency_ms * 1_000_000;
        let pending = Pending {
            order,
            submitted_ns: now,
            arrival_ns: now + d,
            confirmation_ns: now + 2 * d,
            arrived: false,
            fill: None,
            unobservable: false,
        };
        self.log(out, "submitted", now, json!(&pending));
        self.attempt.as_mut().unwrap().pending = Some(pending);
    }
    pub fn advance(
        &mut self,
        now: u64,
        books: &[Book],
        u: &Universe,
        cfg: &Config,
        model: u32,
        out: &mut Vec<Value>,
    ) -> Result<()> {
        // Events run at their deadlines on the book available BEFORE the next input.
        while let Some(at) = self.deadline() {
            if at > now {
                break;
            }
            let mut a = self.attempt.take().unwrap();
            let mut p = a.pending.take().unwrap();
            if !p.arrived {
                p.arrived = true;
                match quantity::execute_model(
                    &p.order,
                    &books[p.order.edge.market],
                    u,
                    cfg,
                    at,
                    Some(&mut self.shadow),
                    model,
                ) {
                    Ok(f) => {
                        if model >= 5 && a.sizing.as_ref().is_some_and(|s| s.cleanup) {
                            if let Ok((scope, liquidity)) = quantity::cleanup_liquidity(
                                p.order.edge,
                                &books[p.order.edge.market],
                                &self.shadow,
                                cfg,
                                at,
                            ) {
                                let retry = self
                                    .model5
                                    .as_mut()
                                    .unwrap()
                                    .retries
                                    .entry(p.order.edge.from(u))
                                    .or_default();
                                retry.scope = scope;
                                retry.liquidity = liquidity;
                            }
                        }
                        self.log(
                            out,
                            "arrived",
                            at,
                            json!({"order":p.order,"fill":f,"processing_lag_ns":now-at}),
                        );
                        p.fill = Some(f);
                    }
                    Err(e) => {
                        p.unobservable = true;
                        a.tainted = true;
                        self.log(
                            out,
                            "unobservable",
                            at,
                            json!({"reason":e.to_string(),"order":p.order}),
                        );
                    }
                }
                a.pending = Some(p);
                self.attempt = Some(a);
                continue;
            }
            let e = p.order.edge;
            if model >= 3 && p.unobservable {
                a.pending = Some(p);
                self.attempt = Some(a);
                self.paused = Some("unresolved: unobservable execution".into());
                self.unobservable += 1;
                self.log(out, "execution_unresolved", at, json!({"reserved":self.attempt.as_ref().unwrap().start,"holdings":self.attempt.as_ref().unwrap().holdings}));
                break;
            }
            let mut full = false;
            if let Some(f) = &p.fill {
                quantity::apply_fill(&mut a.holdings, e, f, u)?;
                full = f.qty == p.order.qty;
                if f.qty > Decimal::ZERO && a.unwind.is_none() {
                    a.done.push(e);
                }
                self.log(out,"confirmed",at,json!({"order":p.order,"fill":f,"holdings":a.holdings,"processing_lag_ns":now-at}));
            }
            if model >= 5 && a.sizing.is_some() {
                let sizing = a.sizing.as_mut().unwrap();
                if !sizing.cleanup {
                    if !full {
                        sizing.cleanup = true;
                    } else {
                        a.next += 1;
                    }
                }
                self.attempt = Some(a);
                self.continue_model5(now, books, u, cfg, out)?;
                continue;
            }
            if a.unwind.is_some() && (!full || p.unobservable) {
                self.attempt = Some(a);
                self.finish(at, u, out, "unwind_incomplete", true, model);
                continue;
            }
            if a.unwind.is_none() && (!full || p.unobservable) {
                a.unwind = Some(a.done.iter().rev().map(|e| e.reverse()).collect());
                a.next = 0;
            } else {
                a.next += 1;
            }
            // Confirmation may reach the owner late. Never backdate a subsequent
            // submission to the ideal deadline and erase real processing/queue delay.
            self.attempt = Some(a);
            self.next_order(now, books, u, cfg, out, model)?;
        }
        Ok(())
    }
    fn next_order(
        &mut self,
        now: u64,
        books: &[Book],
        u: &Universe,
        cfg: &Config,
        out: &mut Vec<Value>,
        model: u32,
    ) -> Result<()> {
        loop {
            let a = self.attempt.as_ref().unwrap();
            let legs = a.unwind.as_ref().unwrap_or(&a.forward);
            if a.next >= legs.len() {
                let status = if a.tainted {
                    "unobservable"
                } else if a.unwind.is_some() {
                    "unwound"
                } else {
                    "completed"
                };
                let retain_unwind_exposure = a.unwind.is_some();
                self.finish(now, u, out, status, retain_unwind_exposure, model);
                return Ok(());
            }
            let e = legs[a.next];
            let input = amount(&a.holdings, e.from(u));
            let unwinding = a.unwind.is_some();
            let order = quantity::prepare_model(
                e,
                input,
                &books[e.market],
                u,
                cfg,
                now,
                if unwinding {
                    cfg.unwind_slippage_bps
                } else {
                    cfg.slippage_bps
                },
                model,
            );
            match order {
                Ok(o) => {
                    self.submit(o, now, out);
                    return Ok(());
                }
                Err(error) => {
                    let reason = error.to_string();
                    self.log(
                        out,
                        "order_rejected",
                        now,
                        json!({"edge":e,"reason":reason}),
                    );
                    if unwinding {
                        if input == Decimal::ZERO
                            || reason == "below_lot"
                            || reason == "minimum_notional"
                        {
                            self.attempt.as_mut().unwrap().next += 1;
                            continue;
                        }
                        self.finish(now, u, out, "unwind_unavailable", true, model);
                        return Ok(());
                    }
                    let a = self.attempt.as_mut().unwrap();
                    a.unwind = Some(a.done.iter().rev().map(|e| e.reverse()).collect());
                    a.next = 0;
                }
            }
        }
    }
    fn finish(
        &mut self,
        now: u64,
        u: &Universe,
        out: &mut Vec<Value>,
        status: &str,
        pause: bool,
        model: u32,
    ) {
        let a = self.attempt.take().unwrap();
        let pnl = amount(&a.holdings, u.usdc) - a.start;
        for (&t, &q) in &a.holdings {
            add(&mut self.balances, t, q);
        }
        if a.tainted {
            self.unobservable += 1;
        } else {
            self.realized_usdc += pnl;
            if status == "completed" {
                self.completed += 1
            } else {
                self.failed += 1
            }
        }
        if pause
            && a.holdings
                .iter()
                .any(|(&t, &q)| t != u.usdc && q > Decimal::ZERO)
        {
            self.paused = Some(status.into());
        }
        if model >= 3 && a.tainted {
            self.paused = Some("unresolved: unobservable execution".into());
        }
        self.log(out,"attempt_ended",now,json!({"route":a.route,"status":status,"duration_ns":now-a.started_ns,"cash_change_usdc":pnl,"included_in_performance":!a.tainted,"holdings":a.holdings,"balances":self.balances,"paused":self.paused}));
    }
    pub fn interrupt(&mut self, why: &str) {
        if self
            .attempt
            .as_ref()
            .is_some_and(|a| a.pending.is_some() || !a.sizing.as_ref().is_some_and(|s| s.cleanup))
        {
            self.paused = Some(format!("unresolved: {why}"));
        }
    }

    pub fn upgrade(&mut self, previous_model: u32, previous_run: Option<String>, usdc: u32) {
        if self.model5.is_some() {
            return;
        }
        self.model5 = Some(Model5 {
            previous_model,
            previous_run,
            announced: false,
            opening_cash: amount(&self.balances, usdc),
            opening_completed: self.completed,
            opening_failed: self.failed,
            opening_unobservable: self.unobservable,
            cash_change: Decimal::ZERO,
            recovery_proceeds: Decimal::ZERO,
            adjusted_closed_profit: Decimal::ZERO,
            retries: BTreeMap::new(),
            eligibility: BTreeMap::new(),
        });
        if self.attempt.is_none()
            && self.unobservable == 0
            && matches!(
                self.paused.as_deref(),
                Some("unwound" | "unwind_unavailable" | "unwind_incomplete")
            )
        {
            self.paused = None;
        }
        self.entry_guard = None;
    }

    pub fn start_plan(
        &mut self,
        r: &Route,
        episode: u64,
        plan: quantity::LivePlan,
        books: &[Book],
        u: &Universe,
        cfg: &Config,
        now: u64,
        out: &mut Vec<Value>,
    ) -> Result<()> {
        ensure!(
            self.attempt.is_none() && self.paused.is_none(),
            "account busy"
        );
        let mut bids = Balances::new();
        for (&t, &q) in &plan.opening {
            ensure!(
                amount(&self.balances, t) >= q,
                "insufficient planned inventory"
            );
            if t != u.usdc && q > Decimal::ZERO {
                let e = quantity::direct_sell(t, u)
                    .ok_or_else(|| anyhow::anyhow!("missing opening inventory market"))?;
                let top = books[e.market]
                    .top(now, cfg)
                    .ok_or_else(|| anyhow::anyhow!("missing opening inventory mark"))?;
                bids.insert(t, top.levels[0][0].px);
            }
        }
        for (&t, &q) in &plan.opening {
            add(&mut self.balances, t, -q);
        }
        self.tried.insert(r.id.clone(), episode);
        self.model5.as_mut().unwrap().retries.clear();
        self.attempt = Some(Attempt {
            route: r.id.clone(),
            start: plan.estimate.start,
            started_ns: now,
            holdings: plan.opening.clone(),
            forward: plan.orders.iter().map(|o| o.edge).collect(),
            done: Vec::new(),
            next: 0,
            unwind: None,
            pending: None,
            tainted: false,
            sizing: Some(Sizing {
                orders: plan.orders,
                opening: plan.opening.clone(),
                opening_bids: bids,
                cleanup: false,
                forward_complete: false,
                recovery: false,
            }),
        });
        self.log(out,"reserved",now,json!({"route":r.id,"amount":plan.estimate.start,"opening":plan.opening,"estimate":plan.estimate,"free_usdc":amount(&self.balances,u.usdc)}));
        self.continue_model5(now, books, u, cfg, out)
    }

    /// Runs only on admitted events/deadlines. Known inventory waits for liquidity;
    /// unresolved arrivals retain their reservation and never enter this path.
    pub fn maintain_model5(
        &mut self,
        now: u64,
        books: &[Book],
        u: &Universe,
        cfg: &Config,
        out: &mut Vec<Value>,
    ) -> Result<()> {
        if self.model5.is_none() {
            self.upgrade(5, None, u.usdc);
        }
        if !self.model5.as_ref().unwrap().announced {
            self.log(out,"paper_model_transition",now,json!({"model":5,"facts":self.model5,"balances":self.balances,"paused":self.paused,"pending":self.attempt,"funding_added":"0"}));
            self.model5.as_mut().unwrap().announced = true;
        }
        if self.paused.is_some() {
            return Ok(());
        }
        if self.attempt.is_none() {
            let mark = quantity::mark_sublots(&self.balances, books, u, cfg, now);
            if !mark.all_dust {
                let holdings: Balances = self
                    .balances
                    .iter()
                    .filter(|(t, q)| **t != u.usdc && **q > Decimal::ZERO)
                    .map(|(&t, &q)| (t, q))
                    .collect();
                for (&t, &q) in &holdings {
                    add(&mut self.balances, t, -q);
                }
                self.attempt = Some(Attempt {
                    route: "inherited_inventory_cleanup".into(),
                    start: Decimal::ZERO,
                    started_ns: now,
                    holdings: holdings.clone(),
                    forward: Vec::new(),
                    done: Vec::new(),
                    next: 0,
                    unwind: None,
                    pending: None,
                    tainted: false,
                    sizing: Some(Sizing {
                        orders: Vec::new(),
                        opening: holdings,
                        opening_bids: Balances::new(),
                        cleanup: true,
                        forward_complete: false,
                        recovery: true,
                    }),
                });
                self.log(
                    out,
                    "cleanup_started",
                    now,
                    json!({"inventory":mark,"inherited":true}),
                );
            } else {
                let guard = if mark.indicative_usdc.is_none() {
                    Some("residual_mark_unavailable")
                } else if !cfg
                    .amounts_usdc
                    .iter()
                    .any(|q| *q <= amount(&self.balances, u.usdc))
                {
                    Some("insufficient_usdc")
                } else {
                    None
                }
                .map(str::to_owned);
                if self.entry_guard != guard {
                    self.entry_guard = guard;
                    self.log(
                        out,
                        "entry_guard",
                        now,
                        json!({"reason":self.entry_guard,"inventory":mark}),
                    );
                }
            }
        }
        if self
            .attempt
            .as_ref()
            .is_some_and(|a| a.pending.is_none() && a.sizing.is_some())
        {
            self.continue_model5(now, books, u, cfg, out)?;
        }
        Ok(())
    }

    fn continue_model5(
        &mut self,
        now: u64,
        books: &[Book],
        u: &Universe,
        cfg: &Config,
        out: &mut Vec<Value>,
    ) -> Result<()> {
        let a = self.attempt.as_ref().unwrap();
        let s = a.sizing.as_ref().unwrap();
        if !s.cleanup && a.next < s.orders.len() {
            let planned = &s.orders[a.next];
            match quantity::reprice_planned(
                planned,
                amount(&a.holdings, planned.edge.from(u)),
                &books[planned.edge.market],
                &self.shadow,
                u,
                cfg,
                now,
            ) {
                Ok(order) => {
                    self.submit(order, now, out);
                    return Ok(());
                }
                Err(e) => self.log(
                    out,
                    "order_rejected",
                    now,
                    json!({"edge":planned.edge,"reason":e.to_string(),"next":"cleanup"}),
                ),
            }
        }
        let a = self.attempt.as_mut().unwrap();
        let s = a.sizing.as_mut().unwrap();
        if !s.cleanup {
            s.forward_complete = a.next == s.orders.len();
            s.cleanup = true;
            let data = json!({"route":a.route,"forward_complete":s.forward_complete,"holdings":a.holdings});
            self.log(out, "cleanup_started", now, data);
        }
        let a = self.attempt.as_ref().unwrap();
        let mark = quantity::mark_sublots(&a.holdings, books, u, cfg, now);
        if mark.all_dust {
            self.finish_model5(now, u, out, mark);
            return Ok(());
        }
        let mut tokens: Vec<_> = a
            .holdings
            .iter()
            .filter(|(t, q)| **t != u.usdc && **q >= Decimal::new(1, u.tokens[t].sz_decimals))
            .map(|(&t, &q)| (t, q))
            .collect();
        tokens.sort_by(|(ta, qa), (tb, qb)| {
            let value = |t, q| {
                quantity::direct_sell(t, u)
                    .and_then(|e| books[e.market].top(now, cfg))
                    .map(|b| q * b.levels[0][0].px)
                    .unwrap_or_default()
            };
            value(*tb, *qb).cmp(&value(*ta, *qa)).then(ta.cmp(tb))
        });
        for (token, qty) in tokens {
            let Some(e) = quantity::direct_sell(token, u) else {
                continue;
            };
            let view = quantity::cleanup_liquidity(e, &books[e.market], &self.shadow, cfg, now);
            let retry = self
                .model5
                .as_mut()
                .unwrap()
                .retries
                .entry(token)
                .or_default();
            let Ok((scope, liquidity)) = view else {
                retry.eligible = false;
                continue;
            };
            if !retry.eligible || retry.scope != scope || retry.liquidity != liquidity {
                retry.submitted = 0;
                retry.scope = scope;
                retry.liquidity = liquidity;
            }
            retry.eligible = true;
            if retry.submitted >= 2 {
                continue;
            }
            if let Ok(order) =
                quantity::prepare_cleanup(e, qty, &books[e.market], &self.shadow, u, cfg, now)
            {
                retry.submitted += 1;
                let count = retry.submitted;
                self.log(out,"residual_cleanup",now,json!({"purpose":"FrontendMarket","reduce_only":false,"token":token,"remaining":qty,"quantity":order.qty,"limit":order.limit,"submission_in_opportunity":count}));
                self.submit(order, now, out);
                return Ok(());
            }
        }
        if self.entry_guard.as_deref() != Some("cleanup_waiting_liquidity") {
            self.entry_guard = Some("cleanup_waiting_liquidity".into());
            self.log(out, "cleanup_waiting", now, json!({"inventory":mark}));
        }
        Ok(())
    }

    fn finish_model5(
        &mut self,
        now: u64,
        u: &Universe,
        out: &mut Vec<Value>,
        mark: quantity::InventoryMark,
    ) {
        let a = self.attempt.take().unwrap();
        let s = a.sizing.unwrap();
        let cash = amount(&a.holdings, u.usdc) - a.start;
        let debit: Decimal = s
            .opening_bids
            .iter()
            .map(|(&t, &bid)| {
                (amount(&s.opening, t) - amount(&a.holdings, t)).max(Decimal::ZERO) * bid
            })
            .sum();
        for (&t, &q) in &a.holdings {
            add(&mut self.balances, t, q);
        }
        let m = self.model5.as_mut().unwrap();
        m.cash_change += cash;
        if s.recovery {
            m.recovery_proceeds += cash;
        } else {
            m.adjusted_closed_profit += cash - debit;
            self.realized_usdc += cash;
            if s.forward_complete {
                self.completed += 1;
            } else {
                self.failed += 1;
            }
        }
        self.entry_guard = mark
            .indicative_usdc
            .is_none()
            .then(|| "residual_mark_unavailable".into());
        self.log(out,"attempt_ended",now,json!({"route":a.route,"status":if s.recovery {"recovery_completed"} else if s.forward_complete {"completed"} else {"cleaned"},"duration_ns":now.saturating_sub(a.started_ns),"cash_change_usdc":cash,"recovery_proceeds_usdc":if s.recovery {cash} else {Decimal::ZERO},"adjusted_closed_profit_usdc":if s.recovery {None} else {Some(cash-debit)},"opening_inventory_debit":debit,"included_in_performance":!s.recovery,"holdings":a.holdings,"balances":self.balances,"inventory":mark,"paused":self.paused}));
    }

    pub fn check_dust(
        &mut self,
        books: &[Book],
        u: &Universe,
        cfg: &Config,
        now: u64,
        reconcile: bool,
        out: &mut Vec<Value>,
    ) {
        if self.attempt.is_some() {
            if reconcile {
                self.log(
                    out,
                    "dust_reconciliation",
                    now,
                    json!({"accepted":false,"reason":"pending_or_unresolved_attempt"}),
                );
            }
            return;
        }
        let mark = quantity::mark_inventory(&self.balances, books, u, cfg, now);
        let allowed = mark.all_dust
            && mark
                .indicative_usdc
                .is_some_and(|v| v <= cfg.dust_limit_usdc);
        let guard = if mark.indicative_usdc.is_none() {
            Some("dust_mark_unavailable")
        } else if !mark.all_dust {
            Some("inventory_not_confirmed_dust")
        } else if !allowed {
            Some("dust_limit_exceeded")
        } else {
            None
        };
        let next = guard.map(str::to_owned);
        if self.entry_guard != next {
            self.entry_guard = next;
            self.log(
                out,
                "entry_guard",
                now,
                json!({"reason":self.entry_guard,"inventory":mark}),
            );
        }
        // Only a fully observable unwind is eligible for automatic dust disposition.
        let known = self.paused.as_deref() == Some("unwound");
        if allowed && known {
            self.paused = None;
            self.log(
                out,
                "dust_resumed",
                now,
                json!({"inventory":mark,"explicit":reconcile}),
            );
        }
        if reconcile {
            self.log(out, "dust_reconciliation", now, json!({"accepted":allowed && self.paused.is_none(),"paused":self.paused,"guard":self.entry_guard}));
        }
    }

    pub fn remap(&mut self, old: &Universe, new: &Universe) -> Result<()> {
        let mut tokens = BTreeMap::new();
        for (&i, t) in &old.tokens {
            let matches: Vec<_> = new
                .tokens
                .iter()
                .filter(|(_, n)| n.token_id == t.token_id)
                .collect();
            ensure!(matches.len() <= 1, "ambiguous token identity");
            if let Some((&j, _)) = matches.first() {
                tokens.insert(i, j);
            }
        }
        let check_token = |i: u32| -> Result<u32> {
            let j = *tokens
                .get(&i)
                .ok_or_else(|| anyhow::anyhow!("missing held/referenced token {i}"))?;
            ensure!(
                (old.tokens[&i].sz_decimals, old.tokens[&i].wei_decimals)
                    == (new.tokens[&j].sz_decimals, new.tokens[&j].wei_decimals),
                "referenced token precision changed"
            );
            Ok(j)
        };
        ensure!(check_token(old.usdc)? == new.usdc, "USDC identity changed");
        let mut markets = BTreeMap::new();
        for (i, m) in old.markets.iter().enumerate() {
            if let Some(j) = new.markets.iter().position(|n| {
                n.index == m.index
                    && tokens.get(&m.base) == Some(&n.base)
                    && tokens.get(&m.quote) == Some(&n.quote)
            }) {
                if check_token(m.base).is_ok() && check_token(m.quote).is_ok() {
                    markets.insert(i, j);
                }
            }
        }
        let balances = |b: &Balances| -> Result<Balances> {
            b.iter()
                .filter(|(t, q)| **q != Decimal::ZERO || tokens.contains_key(t))
                .map(|(&t, &q)| Ok((check_token(t)?, q)))
                .collect()
        };
        let edge = |e: &mut Edge| -> Result<()> {
            let m = &old.markets[e.market];
            check_token(m.base)?;
            check_token(m.quote)?;
            e.market = *markets
                .get(&e.market)
                .ok_or_else(|| anyhow::anyhow!("missing order market"))?;
            Ok(())
        };
        self.balances = balances(&self.balances)?;
        if let Some(a) = &mut self.attempt {
            a.holdings = balances(&a.holdings)?;
            if let Some(s) = &mut a.sizing {
                s.opening = balances(&s.opening)?;
                s.opening_bids = balances(&s.opening_bids)?;
                for o in &mut s.orders {
                    edge(&mut o.edge)?;
                }
                // Waiting cleanup has no missing order outcome. Its elapsed time
                // restarts at this segment's clock origin; prior durations stay recorded.
                if s.cleanup && a.pending.is_none() {
                    a.started_ns = 0;
                }
            }
            for e in a
                .forward
                .iter_mut()
                .chain(a.done.iter_mut())
                .chain(a.unwind.iter_mut().flatten())
            {
                edge(e)?;
            }
            if let Some(p) = &mut a.pending {
                edge(&mut p.order.edge)?;
                if let Some(f) = &mut p.fill {
                    f.fee_token = check_token(f.fee_token)?;
                }
            }
        }
        self.shadow.remap(&markets)?;
        if let Some(m) = &mut self.model5 {
            m.retries = m
                .retries
                .iter()
                .map(|(&t, r)| Ok((check_token(t)?, r.clone())))
                .collect::<Result<_>>()?;
            m.eligibility.clear();
        }
        self.tried.clear(); // Eligibility episodes belong to the previous observation segment.
                            // Old models could end an unobservable attempt without a pause. Its
                            // apparent cash is not reconciled evidence of an actual zero fill.
        if self.unobservable > 0 && self.paused.is_none() {
            self.paused = Some("unresolved: historical unobservable execution".into());
        }
        Ok(())
    }
}
