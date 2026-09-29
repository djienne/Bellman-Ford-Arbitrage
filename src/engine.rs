use crate::{
    book::{self, Book},
    config::Config,
    market::{fee_f64, Route, Universe},
    paper::Account,
    quantity::{self, Estimate},
};
use anyhow::{ensure, Result};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum InputKind {
    Open,
    Close { reason: String },
    Frame { text: String },
    Clock,
    ReconcileDust { latency_ms: u64 },
    Stop { reason: String },
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Input {
    pub sequence: u64,
    pub generation: u64,
    pub receipt_ns: u64,
    pub receipt_utc_ns: u64,
    pub process_ns: u64,
    #[serde(default)]
    pub hot_started_ns: u64,
    #[serde(default)]
    pub hot_done_ns: u64,
    pub event: InputKind,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Histogram {
    pub count: u64,
    pub max_ns: u64,
    pub total_ns: u128,
    pub us: BTreeMap<u64, u64>,
}
impl Histogram {
    pub fn record(&mut self, ns: u64) {
        self.count += 1;
        self.max_ns = self.max_ns.max(ns);
        self.total_ns += u128::from(ns);
        *self.us.entry((ns / 1000).min(10_000_000)).or_default() += 1;
    }
    pub fn report(&self) -> Value {
        let q = |p: u64| {
            let goal = (self.count * p).div_ceil(100);
            let mut n = 0;
            for (&us, &count) in &self.us {
                n += count;
                if n >= goal {
                    return us;
                }
            }
            0
        };
        json!({"samples":self.count,"p50_us":q(50),"p95_us":q(95),"p99_us":q(99),"max_ns":self.max_ns,"mean_ns":if self.count==0{0}else{self.total_ns/u128::from(self.count)},"percentile_resolution_us":1})
    }
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct RouteState {
    pub gross_bps: Option<f64>,
    pub net_bps: Option<f64>,
    pub rejection: Option<String>,
    pub sizes: Vec<SizeResult>,
    pub positive_since: Option<u64>,
    pub last_positive: Option<u64>,
    pub episodes: u64,
    pub eligible: bool,
    pub entry_epoch: u64,
    pub coverage_ns: u64,
    pub positive_ns: u64,
    pub last_account_ns: u64,
    pub valid_until_ns: u64,
    pub eligibility_until_ns: u64,
    pub peak_bps: Option<f64>,
    pub depth_observable: bool,
    pub depth_coverage_ns: u64,
    pub depth_valid_until_ns: u64,
    pub dormant: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Diagnostic {
    pub route: String,
    pub amount: Decimal,
    pub after_ns: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SizeResult {
    pub start: Decimal,
    pub estimate: Option<Estimate>,
    pub rejection: Option<String>,
}
#[derive(Default, Clone, Debug, Serialize, Deserialize)]
pub struct Stats {
    pub frames: u64,
    pub accepted_books: u64,
    pub invalid: u64,
    pub older_books: u64,
    pub reconnects: u64,
    pub cycles_evaluated: u64,
    pub utc_backsteps: u64,
    pub queue_age: Histogram,
    pub receipt_to_decision: Histogram,
    pub processing: Histogram,
    pub hot_compute: Histogram,
    pub receipt_to_hot: Histogram,
    pub hot_cycles_evaluated: u64,
    pub deadline_lag: Histogram,
    pub arrival_sources: BTreeMap<String, u64>,
}

pub struct Engine {
    pub config: Config,
    pub universe: Universe,
    pub routes: Vec<Route>,
    pub books: Vec<Book>,
    pub states: Vec<RouteState>,
    pub accounts: Vec<Account>,
    pub stats: Stats,
    pub now: u64,
    pub generation: u64,
    pub connected: bool,
    pub last_sequence: u64,
    pub last_utc: u64,
    by_market: Vec<Vec<usize>>,
    fee_logs: Vec<f64>,
    hot_rates: Vec<crate::hot::Rate>,
    use_hot: bool,
    pub(crate) legacy_shadow: bool,
    pub model_version: u32,
    pub paper_epoch: Option<String>,
    pub predecessor_run_id: Option<String>,
    pub diagnostic: Option<Diagnostic>,
    pub diagnostic_journal: Vec<Value>,
    reconcile: Option<(u64, u64)>,
}
impl Engine {
    pub fn new(config: Config, universe: Universe, accounts: Option<Vec<Account>>) -> Result<Self> {
        config.validate()?;
        let routes = universe.routes(&config)?;
        ensure!(
            !routes.is_empty(),
            "selected spot graph has no bounded cycles"
        );
        let mut by_market = vec![Vec::new(); universe.markets.len()];
        for (i, r) in routes.iter().enumerate() {
            for e in &r.edges {
                by_market[e.market].push(i);
            }
        }
        let accounts = accounts.unwrap_or_else(|| {
            config
                .latency_ms
                .iter()
                .map(|&l| Account::new(l, &config, universe.usdc))
                .collect()
        });
        ensure!(
            accounts.iter().map(|a| a.latency_ms).collect::<Vec<_>>() == config.latency_ms,
            "paper account scenarios differ from persisted run; use replay for alternative latency"
        );
        let fee_logs = universe
            .markets
            .iter()
            .map(|m| (-fee_f64(m)).ln_1p())
            .collect();
        Ok(Self {
            hot_rates: vec![crate::hot::Rate::default(); routes.len()],
            use_hot: false,
            legacy_shadow: false,
            model_version: 3,
            paper_epoch: None,
            predecessor_run_id: None,
            diagnostic: None,
            diagnostic_journal: Vec::new(),
            reconcile: None,
            states: vec![RouteState::default(); routes.len()],
            books: vec![Book::default(); universe.markets.len()],
            config,
            universe,
            routes,
            accounts,
            stats: Stats::default(),
            now: 0,
            generation: 0,
            connected: false,
            last_sequence: 0,
            last_utc: 0,
            by_market,
            fee_logs,
        })
    }
    pub fn selected_coins(&self) -> Vec<String> {
        self.by_market
            .iter()
            .enumerate()
            .filter(|(_, r)| !r.is_empty())
            .map(|(m, _)| self.universe.markets[m].coin.clone())
            .collect()
    }
    /// Earliest execution deadline; quiet feeds must not delay paper orders.
    pub fn deadline(&self) -> Option<u64> {
        self.accounts.iter().filter_map(Account::deadline).min()
    }
    /// Commit newly prepared orders at actual decision completion, not at the
    /// beginning of computation. Replay supplies the recorded completion stamp.
    pub fn complete_step(
        &mut self,
        input: &Input,
        completed_ns: u64,
        events: &mut [Value],
    ) -> Result<()> {
        ensure!(
            completed_ns >= input.process_ns,
            "completion precedes processing"
        );
        for a in &mut self.accounts {
            if let Some(attempt) = &mut a.attempt {
                if let Some(p) = &mut attempt.pending {
                    if p.submitted_ns == input.process_ns && !p.arrived {
                        p.submitted_ns = completed_ns;
                        p.arrival_ns = completed_ns + a.latency_ms * 1_000_000;
                        p.confirmation_ns = completed_ns + 2 * a.latency_ms * 1_000_000;
                        if attempt.started_ns == input.process_ns {
                            attempt.started_ns = completed_ns;
                        }
                        for event in events
                            .iter_mut()
                            .filter(|v| v["scenario_each_way_ms"].as_u64() == Some(a.latency_ms))
                        {
                            if event["type"] == "submitted" {
                                event["at_ns"] = completed_ns.into();
                                event["data"] = serde_json::to_value(&*p)?;
                            }
                            if event["type"] == "reserved" {
                                event["at_ns"] = completed_ns.into();
                            }
                        }
                    }
                }
            }
        }
        if self.diagnostic.is_some() {
            self.diagnostic_journal.extend(
                events
                    .iter()
                    .filter(|v| {
                        matches!(
                            v["type"].as_str(),
                            Some(
                                "reserved"
                                    | "submitted"
                                    | "arrived"
                                    | "confirmed"
                                    | "unobservable"
                                    | "execution_unresolved"
                                    | "order_rejected"
                                    | "attempt_ended"
                            )
                        )
                    })
                    .cloned(),
            );
        }
        Ok(())
    }
    fn account_time(&mut self, now: u64) {
        for s in &mut self.states {
            if s.depth_observable {
                s.depth_coverage_ns += now
                    .min(if self.model_version >= 4 {
                        s.depth_valid_until_ns
                    } else {
                        s.eligibility_until_ns
                    })
                    .saturating_sub(s.last_account_ns);
            }
            let end = now.min(s.valid_until_ns);
            let dt = end.saturating_sub(s.last_account_ns);
            if s.net_bps.is_some() {
                s.coverage_ns += dt;
                if s.positive_since.is_some() {
                    s.positive_ns += dt;
                }
            }
            s.last_account_ns = now;
        }
    }
    pub fn step(&mut self, input: &Input) -> Result<Vec<Value>> {
        self.use_hot = false;
        self.step_inner(input)
    }
    pub fn step_hot(&mut self, input: &Input, rates: &[crate::hot::Rate]) -> Result<Vec<Value>> {
        self.use_hot = true;
        for &r in rates {
            self.hot_rates[r.route] = r;
        }
        self.stats.hot_cycles_evaluated += rates.len() as u64;
        if matches!(input.event, InputKind::Frame { .. }) && !rates.is_empty() {
            self.stats
                .hot_compute
                .record(input.hot_done_ns.saturating_sub(input.hot_started_ns));
            self.stats
                .receipt_to_hot
                .record(input.hot_done_ns.saturating_sub(input.receipt_ns));
        }
        self.step_inner(input)
    }
    fn step_inner(&mut self, input: &Input) -> Result<Vec<Value>> {
        ensure!(
            input.sequence > self.last_sequence
                && input.process_ns >= self.now
                && input.receipt_ns <= input.process_ns,
            "nonmonotonic engine input"
        );
        self.last_sequence = input.sequence;
        if input.receipt_utc_ns > 0 {
            if self.last_utc > input.receipt_utc_ns {
                self.stats.utc_backsteps += 1;
            }
            self.last_utc = input.receipt_utc_ns;
        }
        let mut events = Vec::new();
        self.account_time(input.process_ns);
        for a in &mut self.accounts {
            if let Some(d) = a.deadline().filter(|d| *d <= input.process_ns) {
                self.stats.deadline_lag.record(input.process_ns - d);
            }
            a.advance(
                input.process_ns,
                &self.books,
                &self.universe,
                &self.config,
                self.model_version,
                &mut events,
            )?;
        }
        self.now = input.process_ns;
        let mut affected = Vec::new();
        match &input.event {
            InputKind::Open => {
                self.generation = input.generation;
                self.connected = true;
                self.stats.reconnects += 1;
                for b in &mut self.books {
                    b.invalidate("new_session");
                    b.last_exchange_ms = 0;
                }
                affected = (0..self.routes.len()).collect();
            }
            InputKind::Close { reason } | InputKind::Stop { reason } => {
                self.connected = false;
                for b in &mut self.books {
                    b.invalidate(reason);
                }
                affected = (0..self.routes.len()).collect();
                if matches!(input.event, InputKind::Stop { .. }) {
                    for a in &mut self.accounts {
                        a.interrupt(reason);
                    }
                }
            }
            InputKind::Frame { text } => {
                self.stats.frames += 1;
                self.stats
                    .queue_age
                    .record(input.process_ns - input.receipt_ns);
                if self.connected && input.generation == self.generation {
                    match book::parse(text, &self.universe, input.receipt_ns, input.process_ns) {
                        Ok(Some(up)) => {
                            let m = up.market;
                            if self.books[m].apply(&up) {
                                self.stats.accepted_books += 1;
                                for a in &mut self.accounts {
                                    a.shadow.update_model(
                                        &up,
                                        if self.legacy_shadow {
                                            None
                                        } else {
                                            Some(if self.config.l2_fast { 5 } else { 20 })
                                        },
                                        self.model_version,
                                    );
                                }
                                affected = self.by_market[m].clone();
                            } else {
                                self.stats.older_books += 1;
                            }
                        }
                        Ok(None) => {}
                        Err(err) => {
                            self.stats.invalid += 1;
                            let coin = serde_json::from_str::<Value>(text)
                                .ok()
                                .and_then(|v| v["data"]["coin"].as_str().map(str::to_owned));
                            if let Some(m) = coin.and_then(|c| {
                                self.universe.markets.iter().position(|m| m.coin == c)
                            }) {
                                self.books[m].invalidate("invalid_book");
                                affected = self.by_market[m].clone();
                            } else {
                                self.connected = false;
                                for b in &mut self.books {
                                    b.invalidate("unidentified_corruption");
                                }
                                affected = (0..self.routes.len()).collect();
                            }
                            events.push(json!({"type":"invalid_frame","at_ns":self.now,"reason":err.to_string()}));
                        }
                    }
                }
            }
            InputKind::Clock => {}
            InputKind::ReconcileDust { latency_ms } => {
                ensure!(
                    self.accounts.iter().any(|a| a.latency_ms == *latency_ms),
                    "unknown reconciliation scenario"
                );
                self.reconcile = Some((*latency_ms, self.now));
            }
        }
        // A monotonic timer expires quiet routes even if there are no new market frames.
        for (i, s) in self.states.iter().enumerate() {
            if (s.net_bps.is_some() && s.valid_until_ns < self.now)
                || (s.eligible && s.eligibility_until_ns < self.now)
                || (self.model_version >= 3
                    && s.depth_observable
                    && (if self.model_version >= 4 {
                        s.depth_valid_until_ns
                    } else {
                        s.eligibility_until_ns
                    }) < self.now)
            {
                affected.push(i);
            }
        }
        affected.sort_unstable();
        affected.dedup();
        for i in affected {
            self.evaluate(i, &mut events);
        }
        // Decide after state processing, using only books already admitted to this owner.
        for a in &mut self.accounts {
            if self.model_version >= 3 && self.config.dust_limit_usdc > Decimal::ZERO {
                let requested = self.reconcile.is_some_and(|(l, _)| l == a.latency_ms);
                let ready = requested
                    && (a.attempt.is_some()
                        || a.paused.as_ref().is_some_and(|p| p != "unwound")
                        || quantity::mark_inventory(
                            &a.balances,
                            &self.books,
                            &self.universe,
                            &self.config,
                            self.now,
                        )
                        .indicative_usdc
                        .is_some()
                        || self
                            .reconcile
                            .is_some_and(|(_, at)| self.now - at >= 30_000_000_000));
                a.check_dust(
                    &self.books,
                    &self.universe,
                    &self.config,
                    self.now,
                    ready,
                    &mut events,
                );
                if ready {
                    self.reconcile = None;
                }
            }
            if a.paused.is_some()
                || a.attempt.is_some()
                || a.entry_guard.is_some()
                || !self.connected
            {
                continue;
            }
            if let Some(d) = &self.diagnostic {
                if self.now >= d.after_ns && !a.tried.contains_key(&d.route) {
                    let r = self.routes.iter().find(|r| r.id == d.route).unwrap();
                    if quantity::estimate_model(
                        r,
                        d.amount,
                        &self.books,
                        &self.universe,
                        &self.config,
                        self.now,
                        self.model_version,
                    )
                    .is_ok()
                        && quantity::amount(&a.balances, self.universe.usdc) >= d.amount
                    {
                        a.start_model(
                            r,
                            0,
                            d.amount,
                            &self.books,
                            &self.universe,
                            &self.config,
                            self.now,
                            &mut events,
                            self.model_version,
                        )?;
                    }
                }
                continue;
            }
            let mut choices = Vec::new();
            for (r, s) in self.routes.iter().zip(&self.states) {
                if !s.eligible || a.tried.get(&r.id) == Some(&s.entry_epoch) {
                    continue;
                }
                for q in &s.sizes {
                    if let Some(e) = &q.estimate {
                        if e.bps > self.config.min_profit_bps
                            && e.start <= quantity::amount(&a.balances, self.universe.usdc)
                        {
                            choices.push((r, s.entry_epoch, e));
                        }
                    }
                }
            }
            choices.sort_by(|(ra, _, a), (rb, _, b)| {
                b.profit
                    .cmp(&a.profit)
                    .then(ra.edges.len().cmp(&rb.edges.len()))
                    .then(ra.id.cmp(&rb.id))
                    .then(a.start.cmp(&b.start))
            });
            if let Some((r, epoch, e)) = choices.first() {
                a.start_model(
                    r,
                    *epoch,
                    e.start,
                    &self.books,
                    &self.universe,
                    &self.config,
                    self.now,
                    &mut events,
                    self.model_version,
                )?;
            }
        }
        if self.model_version >= 3 {
            for event in &mut events {
                if self.model_version >= 4 && event["type"] == "arrived" {
                    let src = &event["data"]["fill"]["source"];
                    let key = format!(
                        "{}:{}",
                        src["channel"].as_str().unwrap(),
                        src["scope"].as_str().unwrap()
                    );
                    *self.stats.arrival_sources.entry(key).or_default() += 1;
                }
                if matches!(
                    event["type"].as_str(),
                    Some("arrived" | "confirmed" | "unobservable")
                ) {
                    event["dispatch_ns"] = input.process_ns.into();
                    event["scheduled_ns"] = event["at_ns"].clone();
                }
                if self.diagnostic.is_some() {
                    event["diagnostic"] = true.into();
                }
            }
        }
        Ok(events)
    }
    fn evaluate(&mut self, i: usize, events: &mut Vec<Value>) {
        self.stats.cycles_evaluated += 1;
        let r = &self.routes[i];
        let s = &mut self.states[i];
        let was_positive = s.positive_since.is_some();
        let was_eligible = s.eligible;
        let mut gross = 0.0;
        let mut net = 0.0;
        let mut valid_until = u64::MAX;
        let mut valid = self.connected;
        let no_liquidity = self.connected
            && r.edges
                .iter()
                .any(|e| self.books[e.market].known_empty(e.buy, self.now, &self.config));
        s.dormant = no_liquidity;
        if self.use_hot {
            let rate = self.hot_rates[i];
            gross = rate.gross_log;
            net = rate.net_log;
            valid_until = rate.valid_until_ns;
            valid = self.connected && rate.valid && self.now <= valid_until;
        } else {
            for &e in &r.edges {
                if let Some(q) = self.books[e.market].top(self.now, &self.config) {
                    let g = if e.buy { -q.log_ask } else { q.log_bid };
                    gross += g;
                    net += g + self.fee_logs[e.market];
                    valid_until =
                        valid_until.min(q.receipt_ns + self.config.quote_age_ms * 1_000_000);
                } else {
                    valid = false;
                    break;
                }
            }
        }
        s.valid_until_ns = if valid { valid_until } else { self.now };
        s.eligibility_until_ns = r
            .edges
            .iter()
            .map(|e| {
                self.books[e.market]
                    .depth
                    .as_ref()
                    .map(|d| d.receipt_ns + self.config.depth_age_ms * 1_000_000)
                    .unwrap_or(0)
            })
            .min()
            .unwrap_or(0)
            .min(s.valid_until_ns);
        s.depth_observable = valid
            && r.edges
                .iter()
                .all(|e| self.books[e.market].depth(self.now, &self.config).is_ok());
        if self.model_version >= 4 {
            let views: Option<Vec<_>> = r
                .edges
                .iter()
                .map(|e| {
                    self.books[e.market]
                        .execution_side(e.buy, self.now, &self.config, None, self.model_version)
                        .ok()
                })
                .collect();
            s.depth_observable = valid
                && views
                    .as_ref()
                    .is_some_and(|v| v.iter().all(|v| v.scope == "l2"));
            s.eligibility_until_ns = views
                .as_ref()
                .and_then(|v| {
                    v.iter()
                        .map(|v| v.observation.receipt_ns + self.config.depth_age_ms * 1_000_000)
                        .min()
                })
                .unwrap_or(self.now)
                .min(s.valid_until_ns);
            s.depth_valid_until_ns = s.eligibility_until_ns;
        }
        s.sizes.clear();
        if valid {
            s.gross_bps = Some(gross.exp_m1() * 10_000.0);
            let bps = net.exp_m1() * 10_000.0;
            s.net_bps = Some(bps);
            s.peak_bps = Some(s.peak_bps.unwrap_or(f64::NEG_INFINITY).max(bps));
            s.rejection = None;
            // The touch-rate product is an upper bound: taking deeper prices,
            // rounding down quantities and paying fees cannot rescue a losing cycle.
            for &start in &self.config.amounts_usdc {
                if net <= 1e-12 {
                    s.sizes.push(SizeResult {
                        start,
                        estimate: None,
                        rejection: Some("nonpositive_fee_net_upper_bound".into()),
                    });
                    continue;
                }
                match quantity::estimate_model(
                    r,
                    start,
                    &self.books,
                    &self.universe,
                    &self.config,
                    self.now,
                    self.model_version,
                ) {
                    Ok(mut e) => {
                        if self.model_version >= 3 {
                            e.inventory = Some(quantity::mark_inventory(
                                &e.residual,
                                &self.books,
                                &self.universe,
                                &self.config,
                                self.now,
                            ));
                        }
                        s.sizes.push(SizeResult {
                            start,
                            estimate: Some(e),
                            rejection: None,
                        });
                    }
                    Err(e) => s.sizes.push(SizeResult {
                        start,
                        estimate: None,
                        rejection: Some(e.to_string()),
                    }),
                }
            }
        } else {
            s.gross_bps = None;
            s.net_bps = None;
            s.rejection = Some(
                if no_liquidity {
                    "no_liquidity"
                } else {
                    "quote_unavailable"
                }
                .into(),
            );
        }
        let positive = valid && net > 1e-12;
        s.eligible = s.sizes.iter().any(|s| {
            s.estimate
                .as_ref()
                .is_some_and(|q| q.bps > self.config.min_profit_bps)
        });
        if positive {
            if !was_positive {
                s.positive_since = Some(self.now);
                s.episodes += 1;
                events.push(json!({"type":"episode_started","at_ns":self.now,"route":r.id,"name":r.name,"net_bps":s.net_bps}));
            }
            s.last_positive = Some(self.now);
        } else if let Some(start) = s.positive_since.take() {
            events.push(json!({"type":"episode_ended","route":r.id,"at_ns":self.now,"first_positive_ns":start,"last_positive_ns":s.last_positive,"observed_span_ns":s.last_positive.unwrap_or(start)-start,"first_failure_ns":if valid || no_liquidity{Some(self.now)}else{None},"censored":!valid && !no_liquidity}));
        }
        if s.eligible && !was_eligible {
            s.entry_epoch += 1;
            events.push(json!({"type":"eligible_episode","at_ns":self.now,"route":r.id,"episode":s.entry_epoch,"sizes":s.sizes}));
        }
    }
    pub fn report(&self) -> Value {
        let mut ranking: Vec<_> = self.routes.iter().zip(&self.states).collect();
        ranking.sort_by(|(_, a), (_, b)| {
            b.net_bps
                .unwrap_or(f64::NEG_INFINITY)
                .total_cmp(&a.net_bps.unwrap_or(f64::NEG_INFINITY))
        });
        let best:Vec<_>=ranking.into_iter().take(20).map(|(r,s)|json!({"route":r.id,"name":r.name,"gross_bps":s.gross_bps,"fee_net_bps":s.net_bps,"sizes":s.sizes,"coverage_ns":s.coverage_ns,"positive_ns":s.positive_ns,"episodes":s.episodes,"peak_bps":s.peak_bps,"rejection":s.rejection,
            "legs":r.edges.iter().map(|e|{let b=&self.books[e.market];let q=b.top(self.now,&self.config);json!({"coin":self.universe.markets[e.market].coin,"buy":e.buy,"top":q.map(|q|&q.levels),"exchange_ms":q.map(|q|q.exchange_ms),"receipt_age_ns":q.map(|q|self.now.saturating_sub(q.receipt_ns)),"depth_age_ns":b.depth.as_ref().map(|d|self.now.saturating_sub(d.receipt_ns))})}).collect::<Vec<_>>() })).collect();
        let mut report = json!({"format":1,"engine_time_ns":self.now,"connected":self.connected,"markets_subscribed":self.selected_coins().len(),"routes":self.routes.len(),"observable_routes":self.states.iter().filter(|s|s.net_bps.is_some()).count(),"size_eligible_routes":self.states.iter().filter(|s|s.eligible).count(),"positive_episodes":self.states.iter().map(|s|s.episodes).sum::<u64>(),"joint_route_observation_ns":self.states.iter().map(|s|s.coverage_ns).sum::<u64>(),"frames":self.stats.frames,"accepted_books":self.stats.accepted_books,"invalid":self.stats.invalid,"older_books":self.stats.older_books,"connections":self.stats.reconnects,"cycles_evaluated":self.stats.cycles_evaluated,"hot_cycles_evaluated":self.stats.hot_cycles_evaluated,"wall_clock_backsteps":self.stats.utc_backsteps,"queue_age":self.stats.queue_age.report(),"receipt_to_decision":self.stats.receipt_to_decision.report(),"processing":self.stats.processing.report(),"hot_compute":self.stats.hot_compute.report(),"receipt_to_hot":self.stats.receipt_to_hot.report(),"accounts":self.accounts,"ranking":best,
            "route_statistics":self.routes.iter().zip(&self.states).map(|(r,s)|json!({"id":r.id,"name":r.name,"coverage_ns":s.coverage_ns,"positive_ns":s.positive_ns,"episodes":s.episodes,"peak_bps":s.peak_bps})).collect::<Vec<_>>(),
            "fee_model":"configured spot taker; output-asset fee rounded up; unknown discounts unapplied; paper estimates, not verified fills","clock_note":"monotonic nanosecond resolution; exchange millisecond timestamps; no claim of synchronized clock accuracy"});
        if self.model_version >= 3 {
            report["model_version"] = self.model_version.into();
            report["coverage"] = json!({"structural_routes":self.routes.len(),"dormant_routes":self.states.iter().filter(|s|s.dormant).count(),"fresh_price_routes":self.states.iter().filter(|s|s.net_bps.is_some()).count(),"executable_depth_routes":self.states.iter().filter(|s|s.depth_observable).count(),"price_route_ns":self.states.iter().map(|s|s.coverage_ns).sum::<u64>(),"depth_route_ns":self.states.iter().map(|s|s.depth_coverage_ns).sum::<u64>()});
            report["deadline_dispatch_lag"] = self.stats.deadline_lag.report();
            report["diagnostic"] = json!(self.diagnostic);
            if self.diagnostic.is_some() {
                report["diagnostic_journal"] = json!(self.diagnostic_journal);
            }
            report["execution_performance_included"] = self.diagnostic.is_none().into();
            report["inventory_marks"] = json!(self.accounts.iter().map(|a| {
                let mut holdings = a.balances.clone();
                if let Some(p) = &a.attempt { for (&t,&q) in &p.holdings { quantity::add(&mut holdings,t,q); } }
                let mark = quantity::mark_inventory(&holdings,&self.books,&self.universe,&self.config,self.now);
                json!({"latency_ms":a.latency_ms,"cash_plus_inventory_change_usdc":mark.indicative_usdc.map(|v|quantity::amount(&holdings,self.universe.usdc)+v-self.config.starting_usdc),"inventory":mark,"open_exposure":a.attempt.is_some() || a.paused.is_some(),"indicative_only":true})
            }).collect::<Vec<_>>());
            for (row, s) in report["route_statistics"]
                .as_array_mut()
                .unwrap()
                .iter_mut()
                .zip(&self.states)
            {
                row["depth_coverage_ns"] = s.depth_coverage_ns.into();
                row["dormant"] = s.dormant.into();
            }
        }
        if self.model_version >= 4 {
            report["paper_epoch"] = json!(self.paper_epoch);
            report["predecessor_run_id"] = json!(self.predecessor_run_id);
            report["arrival_observation_counts"] = json!(self.stats.arrival_sources);
            report["coverage"]["depth_scope"] =
                "full_l2_traded_side; excludes BBO-only quantity".into();
            report["execution_model_note"] = "local receipt-time proxy; bounded BBO or coherent L2; unknown deeper remainder excluded".into();
        }
        report
    }
}
