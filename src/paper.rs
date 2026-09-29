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
        if self.attempt.is_some() {
            self.paused = Some(format!("unresolved: {why}"));
        }
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
        self.tried.clear(); // Eligibility episodes belong to the previous observation segment.
                            // Old models could end an unobservable attempt without a pause. Its
                            // apparent cash is not reconciled evidence of an actual zero fill.
        if self.unobservable > 0 && self.paused.is_none() {
            self.paused = Some("unresolved: historical unobservable execution".into());
        }
        Ok(())
    }
}
