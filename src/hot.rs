//! Dedicated numeric detector. No JSON, Decimal arithmetic, logging, locks, or
//! allocations in Core::evaluate. The cold normalizer supplies an output buffer.
use crate::{
    book,
    config::Config,
    engine::InputKind,
    hyperliquid::{Clock, Wire},
    market::{fee_f64, Edge, Route, Universe},
};
use anyhow::{Context, Result};
use std::sync::Arc;
use tokio::{
    sync::mpsc,
    time::{interval, Duration},
};

#[derive(Clone, Copy, Debug, Default)]
pub struct Rate {
    pub route: usize,
    pub gross_log: f64,
    pub net_log: f64,
    pub valid: bool,
    pub valid_until_ns: u64,
}
#[derive(Clone, Copy)]
pub enum Signal {
    Open(u64),
    Close,
    Clock,
    None,
    Invalid {
        generation: u64,
        market: Option<usize>,
    },
    Quote {
        generation: u64,
        market: usize,
        exchange_ms: u64,
        receipt_ns: u64,
        log_bid: f64,
        log_ask: f64,
    },
}
#[derive(Clone, Copy, Default)]
struct Quote {
    exchange_ms: u64,
    receipt_ns: u64,
    log_bid: f64,
    log_ask: f64,
    valid: bool,
}
struct Cycle {
    edges: [Edge; 8],
    len: usize,
}
pub struct Core {
    quotes: Vec<Quote>,
    cycles: Vec<Cycle>,
    by_market: Vec<Vec<usize>>,
    fees: Vec<f64>,
    dirty: Vec<usize>,
    marks: Vec<u64>,
    version: u64,
    generation: u64,
    connected: bool,
    age_ns: u64,
}
impl Core {
    pub fn new(u: &Universe, routes: &[Route], cfg: &Config) -> Self {
        let mut by_market = vec![Vec::new(); u.markets.len()];
        let mut cycles = Vec::with_capacity(routes.len());
        for (i, r) in routes.iter().enumerate() {
            let mut edges = [Edge {
                market: 0,
                buy: false,
            }; 8];
            for (j, &e) in r.edges.iter().enumerate() {
                edges[j] = e;
                by_market[e.market].push(i);
            }
            cycles.push(Cycle {
                edges,
                len: r.edges.len(),
            });
        }
        Self {
            quotes: vec![Quote::default(); u.markets.len()],
            cycles,
            by_market,
            fees: u.markets.iter().map(|m| (-fee_f64(m)).ln_1p()).collect(),
            dirty: Vec::with_capacity(routes.len()),
            marks: vec![0; routes.len()],
            version: 0,
            generation: 0,
            connected: false,
            age_ns: cfg.quote_age_ms * 1_000_000,
        }
    }
    fn market_dirty(&mut self, m: usize) {
        for &i in &self.by_market[m] {
            if self.marks[i] != self.version {
                self.marks[i] = self.version;
                self.dirty.push(i);
            }
        }
    }
    pub fn evaluate(&mut self, signal: Signal, now: u64, out: &mut Vec<Rate>) {
        assert!(
            out.capacity() >= self.cycles.len(),
            "cold output buffer must be preallocated"
        );
        out.clear();
        self.dirty.clear();
        self.version = self.version.wrapping_add(1);
        if self.version == 0 {
            self.marks.fill(0);
            self.version = 1;
        }
        match signal {
            Signal::Open(g) => {
                self.generation = g;
                self.connected = true;
                self.quotes.fill(Quote::default());
                self.dirty.extend(0..self.cycles.len());
            }
            Signal::Close => {
                self.connected = false;
                self.quotes.fill(Quote::default());
                self.dirty.extend(0..self.cycles.len());
            }
            Signal::Quote {
                generation,
                market,
                exchange_ms,
                receipt_ns,
                log_bid,
                log_ask,
            } if self.connected && generation == self.generation => {
                if exchange_ms >= self.quotes[market].exchange_ms {
                    self.quotes[market] = Quote {
                        exchange_ms,
                        receipt_ns,
                        log_bid,
                        log_ask,
                        valid: log_bid.is_finite() && log_ask.is_finite(),
                    };
                    self.market_dirty(market);
                }
            }
            Signal::Invalid { generation, market } if generation == self.generation => {
                if let Some(m) = market {
                    self.quotes[m].valid = false;
                    self.market_dirty(m);
                } else {
                    self.connected = false;
                    self.quotes.fill(Quote::default());
                    self.dirty.extend(0..self.cycles.len());
                }
            }
            _ => {}
        }
        for m in 0..self.quotes.len() {
            if self.quotes[m].valid && now.saturating_sub(self.quotes[m].receipt_ns) > self.age_ns {
                self.quotes[m].valid = false;
                self.market_dirty(m);
            }
        }
        for &i in &self.dirty {
            let c = &self.cycles[i];
            let mut rate = Rate {
                route: i,
                valid: self.connected,
                valid_until_ns: u64::MAX,
                ..Rate::default()
            };
            for e in &c.edges[..c.len] {
                let q = self.quotes[e.market];
                if !q.valid {
                    rate.valid = false;
                    break;
                }
                let gross = if e.buy { -q.log_ask } else { q.log_bid };
                rate.gross_log += gross;
                rate.net_log += gross + self.fees[e.market];
                rate.valid_until_ns = rate.valid_until_ns.min(q.receipt_ns + self.age_ns);
            }
            out.push(rate);
        }
    }
}
/// Cold parsing, allocations and protocol validation finish before the hot timer starts.
pub fn normalize(event: &InputKind, u: &Universe, generation: u64, receipt_ns: u64) -> Signal {
    match event {
        InputKind::Open => Signal::Open(generation),
        InputKind::Close { .. } | InputKind::Stop { .. } => Signal::Close,
        InputKind::Clock | InputKind::ReconcileDust { .. } => Signal::Clock,
        InputKind::Frame { text } => match book::parse(text, u, receipt_ns, receipt_ns) {
            Ok(Some(up)) => Signal::Quote {
                generation,
                market: up.market,
                exchange_ms: up.observation.exchange_ms,
                receipt_ns,
                log_bid: up.observation.log_bid,
                log_ask: up.observation.log_ask,
            },
            Ok(None) => Signal::None,
            Err(_) => {
                let coin = serde_json::from_str::<serde_json::Value>(text)
                    .ok()
                    .and_then(|v| v["data"]["coin"].as_str().map(str::to_owned));
                Signal::Invalid {
                    generation,
                    market: coin.and_then(|c| u.markets.iter().position(|m| m.coin == c)),
                }
            }
        },
    }
}
pub struct Batch {
    pub wire: Wire,
    pub signal: Signal,
    pub rates: Vec<Rate>,
    pub started_ns: u64,
    pub done_ns: u64,
}
pub fn launch(
    u: Universe,
    routes: Vec<Route>,
    cfg: Config,
    mut wire_rx: mpsc::Receiver<Wire>,
    clock: Arc<Clock>,
) -> (mpsc::Receiver<Batch>, tokio::task::JoinHandle<Result<()>>) {
    let count = routes.len();
    let capacity = cfg
        .channel_capacity
        .min((64 * 1024 * 1024 / (count * std::mem::size_of::<Rate>()).max(1)).max(1));
    let core = Core::new(&u, &routes, &cfg);
    let (work_tx, mut work_rx) = mpsc::channel::<Batch>(capacity);
    let (out_tx, out_rx) = mpsc::channel(capacity);
    let hot_clock = clock.clone();
    let handle = tokio::spawn(async move {
        let mut worker = tokio::task::spawn_blocking(move || -> Result<()> {
            let mut core = core;
            while let Some(mut batch) = work_rx.blocking_recv() {
                batch.started_ns = hot_clock.ns();
                core.evaluate(batch.signal, batch.started_ns, &mut batch.rates);
                batch.done_ns = hot_clock.ns();
                out_tx.try_send(batch).map_err(|_| {
                    anyhow::anyhow!("hot-to-cold overflow/disconnected: incomplete segment")
                })?;
            }
            Ok(())
        });
        let normalize = async {
            let mut tick = interval(Duration::from_millis(50));
            let mut generation = 0;
            loop {
                let wire = tokio::select! {w=wire_rx.recv()=>w.context("collector closed normalizer")?,_=tick.tick()=>Wire{generation,receipt_ns:clock.ns(),utc_ns:0,event:InputKind::Clock}};
                generation = wire.generation;
                let signal = normalize(&wire.event, &u, generation, wire.receipt_ns);
                // Allocation occurs on this cold task; the hot worker only fills the buffer.
                let batch = Batch {
                    wire,
                    signal,
                    rates: Vec::with_capacity(count),
                    started_ns: 0,
                    done_ns: 0,
                };
                work_tx.try_send(batch).map_err(|_| {
                    anyhow::anyhow!("normalizer-to-hot overflow/disconnected: incomplete segment")
                })?;
            }
            #[allow(unreachable_code)]
            Ok::<(), anyhow::Error>(())
        };
        tokio::select! {r=&mut worker=>r.context("hot worker panic")?,r=normalize=>r}
    });
    (out_rx, handle)
}
