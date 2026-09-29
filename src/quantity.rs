//! Decimal IOC accounting. Fee model: charged in the received asset, rounded up
//! to its atomic unit. This conservative paper convention is recorded, not a fill guarantee.
use crate::{
    book::{Book, ExecutionView, Level, Update},
    config::Config,
    market::{Edge, Route, Universe},
};
use anyhow::{ensure, Context, Result};
use rust_decimal::{prelude::ToPrimitive, Decimal, RoundingStrategy};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub type Balances = BTreeMap<u32, Decimal>;
pub fn amount(b: &Balances, t: u32) -> Decimal {
    b.get(&t).copied().unwrap_or_default()
}
pub fn add(b: &mut Balances, t: u32, q: Decimal) {
    *b.entry(t).or_default() += q;
}
fn mul(a: Decimal, b: Decimal) -> Result<Decimal> {
    a.checked_mul(b).context("decimal overflow")
}
fn div(a: Decimal, b: Decimal) -> Result<Decimal> {
    a.checked_div(b).context("decimal division overflow")
}
pub fn floor(q: Decimal, dp: u32) -> Decimal {
    q.round_dp_with_strategy(dp, RoundingStrategy::ToZero)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Order {
    pub edge: Edge,
    pub qty: Decimal,
    pub limit: Decimal,
    pub budget: Decimal,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<ExecutionSource>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExecutionSource {
    pub market_index: u32,
    pub buy: bool,
    pub channel: String,
    pub scope: String,
    pub exchange_ms: u64,
    pub receipt_ns: u64,
    pub available_ns: u64,
    pub price_exchange_ms: u64,
    pub price_receipt_ns: u64,
    pub price_available_ns: u64,
    pub price_age_ns: u64,
    pub quantity_age_ns: u64,
}
fn source(
    view: &ExecutionView<'_>,
    e: Edge,
    u: &Universe,
    now: u64,
    model: u32,
) -> Option<ExecutionSource> {
    (model >= 4).then(|| ExecutionSource {
        market_index: u.markets[e.market].index,
        buy: e.buy,
        channel: view.channel.into(),
        scope: view.scope.into(),
        exchange_ms: view.observation.exchange_ms,
        receipt_ns: view.observation.receipt_ns,
        available_ns: view.observation.available_ns,
        price_exchange_ms: view.price_observation.exchange_ms,
        price_receipt_ns: view.price_observation.receipt_ns,
        price_available_ns: view.price_observation.available_ns,
        price_age_ns: now.saturating_sub(view.price_observation.receipt_ns),
        quantity_age_ns: now.saturating_sub(view.observation.receipt_ns),
    })
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Fill {
    pub qty: Decimal,
    pub spent: Decimal,
    pub gross: Decimal,
    pub fee: Decimal,
    pub received: Decimal,
    pub fee_token: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<ExecutionSource>,
}

/// Each scenario owns one shared shadow book across all of its routes.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Shadow {
    #[serde(with = "shadow_levels")]
    levels: BTreeMap<(usize, bool, Decimal), (Decimal, Decimal)>,
}
mod shadow_levels {
    use super::*;
    use serde::{Deserializer, Serializer};
    pub fn serialize<S: Serializer>(
        v: &BTreeMap<(usize, bool, Decimal), (Decimal, Decimal)>,
        s: S,
    ) -> Result<S::Ok, S::Error> {
        v.iter().collect::<Vec<_>>().serialize(s)
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(
        d: D,
    ) -> Result<BTreeMap<(usize, bool, Decimal), (Decimal, Decimal)>, D::Error> {
        Ok(
            Vec::<((usize, bool, Decimal), (Decimal, Decimal))>::deserialize(d)?
                .into_iter()
                .collect(),
        )
    }
}
impl Shadow {
    pub fn remap(&mut self, markets: &BTreeMap<usize, usize>) -> Result<()> {
        let mut levels = BTreeMap::new();
        for (&(m, side, px), &(public, left)) in &self.levels {
            if let Some(&new) = markets.get(&m) {
                levels.insert((new, side, px), (public, left));
            } else {
                ensure!(
                    left == public,
                    "removed market has depleted shadow liquidity"
                );
            }
        }
        self.levels = levels;
        Ok(())
    }
    pub fn update(&mut self, up: &Update) {
        self.update_depth(up, Some(20));
    }
    pub fn update_model(&mut self, up: &Update, limit: Option<usize>, model: u32) {
        if model >= 4 && !up.depth {
            for side in 0..2 {
                if let Some(top) = up.observation.levels[side].first() {
                    let buy = side == 1;
                    // A worse best price proves better levels absent. An improved
                    // best price says nothing about previously consumed worse levels.
                    self.levels.retain(|&(m, b, px), _| {
                        m != up.market || b != buy || if buy { px >= top.px } else { px <= top.px }
                    });
                }
            }
        }
        self.update_depth(up, limit);
    }
    // None is solely for replaying the original format-1 accounting model.
    pub(crate) fn update_depth(&mut self, up: &Update, limit: Option<usize>) {
        for side in 0..2 {
            let buy = side == 1;
            let rows = &up.observation.levels[side];
            if up.depth || rows.is_empty() {
                self.levels.retain(|&(m, b, p), value| {
                    let (public, left) = *value;
                    m != up.market || b != buy || rows.iter().any(|l| l.px == p)
                        // A truncated snapshot cannot prove cancellation beyond
                        // its worst visible price. Retain only consumed capacity;
                        // unconsumed hidden levels need no historical storage.
                        || (left < public && limit.is_some_and(|n| rows.len() >= n)
                            && rows.last().is_some_and(|l| if buy {p > l.px} else {p < l.px}))
                });
            }
            for l in rows {
                let key = (up.market, buy, l.px);
                let old = self.levels.get(&key).copied();
                let available = old
                    .map(|(public, left)| (left + l.sz - public).max(Decimal::ZERO).min(l.sz))
                    .unwrap_or(l.sz);
                self.levels.insert(key, (l.sz, available));
            }
        }
    }
    pub fn available(&self, e: Edge, l: &Level) -> Decimal {
        self.levels
            .get(&(e.market, e.buy, l.px))
            .map(|(_, a)| (*a).min(l.sz))
            .unwrap_or_default()
    }
    pub fn consume(&mut self, e: Edge, px: Decimal, q: Decimal) {
        if let Some((_, left)) = self.levels.get_mut(&(e.market, e.buy, px)) {
            *left = (*left - q).max(Decimal::ZERO);
        }
    }
}

/// Spot price precision: <=5 significant digits, <=8-szDecimals decimal places;
/// integer prices are always allowed. Round inward so tick rounding cannot
/// exceed the chosen adverse-price tolerance.
pub fn limit_price(px: Decimal, sz_decimals: u32, buy: bool) -> Decimal {
    let magnitude = px.to_f64().unwrap().log10().floor() as i32;
    let dp = (4 - magnitude).max(0) as u32;
    px.round_dp_with_strategy(
        dp.min(8 - sz_decimals),
        if buy {
            RoundingStrategy::ToNegativeInfinity
        } else {
            RoundingStrategy::ToPositiveInfinity
        },
    )
}
pub fn prepare(
    e: Edge,
    input: Decimal,
    book: &Book,
    u: &Universe,
    cfg: &Config,
    now: u64,
    slippage: Decimal,
) -> Result<Order> {
    prepare_model(e, input, book, u, cfg, now, slippage, 3)
}
pub fn prepare_model(
    e: Edge,
    input: Decimal,
    book: &Book,
    u: &Universe,
    cfg: &Config,
    now: u64,
    slippage: Decimal,
    model: u32,
) -> Result<Order> {
    ensure!(input > Decimal::ZERO, "no_balance");
    let view = book.execution_side(e.buy, now, cfg, None, model)?;
    let rows = view.rows;
    ensure!(!rows.is_empty(), "no_liquidity");
    let dp = u.tokens[&u.markets[e.market].base].sz_decimals;
    let mut budget = input;
    let mut qty = Decimal::ZERO;
    let mut marginal = rows[0].px;
    for l in rows {
        let take = if e.buy {
            div(budget, l.px)?.min(l.sz)
        } else {
            budget.min(l.sz)
        };
        if take <= Decimal::ZERO {
            break;
        }
        qty += take;
        budget -= if e.buy { mul(take, l.px)? } else { take };
        marginal = l.px;
        if budget <= Decimal::ZERO {
            break;
        }
    }
    ensure!(
        !e.buy || budget <= mul(Decimal::new(1, dp), marginal)?,
        "insufficient_depth"
    );
    // Never silently size down a sell to observed depth: IOC must expose partial execution.
    if !e.buy {
        qty = input;
    }
    let factor = Decimal::ONE + if e.buy { slippage } else { -slippage } / Decimal::from(10_000);
    let limit = limit_price(mul(marginal, factor)?, dp, e.buy);
    ensure!(limit > Decimal::ZERO, "invalid_limit");
    if e.buy {
        qty = qty.min(div(input, limit)?);
    }
    qty = floor(qty, dp);
    ensure!(
        view.scope != "top" || qty <= rows[0].sz,
        "insufficient_top_depth"
    );
    ensure!(qty > Decimal::ZERO, "below_lot");
    ensure!(mul(qty, limit)? >= Decimal::from(10), "minimum_notional");
    Ok(Order {
        edge: e,
        qty,
        limit,
        budget: input,
        source: source(&view, e, u, now, model),
    })
}
pub fn execute(
    order: &Order,
    book: &Book,
    u: &Universe,
    cfg: &Config,
    now: u64,
    shadow: Option<&mut Shadow>,
) -> Result<Fill> {
    execute_model(order, book, u, cfg, now, shadow, 3)
}
pub fn execute_model(
    order: &Order,
    book: &Book,
    u: &Universe,
    cfg: &Config,
    now: u64,
    mut shadow: Option<&mut Shadow>,
    model: u32,
) -> Result<Fill> {
    if model < 4 && book.known_empty(order.edge.buy, now, cfg) {
        return Ok(Fill {
            fee_token: order.edge.to(u),
            ..Fill::default()
        });
    }
    let view = book.execution_side(order.edge.buy, now, cfg, Some(order.limit), model)?;
    let e = order.edge;
    let m = &u.markets[e.market];
    let dp = u.tokens[&m.base].sz_decimals;
    let mut fill = Fill {
        fee_token: e.to(u),
        source: source(&view, e, u, now, model),
        ..Fill::default()
    };
    let mut debits = Vec::new();
    for l in view.rows {
        if (e.buy && l.px > order.limit) || (!e.buy && l.px < order.limit) {
            break;
        }
        let visible = shadow.as_ref().map(|s| s.available(e, l)).unwrap_or(l.sz);
        let q = floor((order.qty - fill.qty).min(visible), dp);
        if q <= Decimal::ZERO {
            continue;
        }
        fill.qty += q;
        let notional = mul(q, l.px)?;
        fill.spent += if e.buy { notional } else { q };
        fill.gross += if e.buy { q } else { notional };
        debits.push((l.px, q));
        if fill.qty == order.qty {
            break;
        }
    }
    // A truncated view cannot establish the fill of the unseen remainder.
    // Do not consume shadow capacity or book hypothetical proceeds on this path.
    if model >= 4 && fill.qty < order.qty {
        let truncated = view.scope == "top"
            || (view.scope == "l2" && view.rows.len() >= if cfg.l2_fast { 5 } else { 20 });
        ensure!(
            !truncated
                || !view.rows.last().is_some_and(|l| if e.buy {
                    l.px < order.limit
                } else {
                    l.px > order.limit
                }),
            "unknown_deeper_liquidity"
        );
    }
    ensure!(fill.spent <= order.budget, "paper_budget_exceeded");
    fill.fee = mul(fill.gross, m.fee)?.round_dp_with_strategy(
        u.tokens[&fill.fee_token].wei_decimals,
        RoundingStrategy::ToPositiveInfinity,
    );
    fill.received = fill.gross - fill.fee;
    ensure!(fill.received >= Decimal::ZERO, "fee_exceeds_output");
    if let Some(s) = shadow.as_mut() {
        for (px, q) in debits {
            s.consume(e, px, q);
        }
    }
    Ok(fill)
}
pub fn apply_fill(b: &mut Balances, e: Edge, f: &Fill, u: &Universe) -> Result<()> {
    ensure!(
        amount(b, e.from(u)) >= f.spent,
        "insufficient paper inventory"
    );
    add(b, e.from(u), -f.spent);
    add(b, e.to(u), f.received);
    Ok(())
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Estimate {
    pub start: Decimal,
    pub final_usdc: Decimal,
    pub profit: Decimal,
    pub bps: Decimal,
    pub residual: Balances,
    pub fees: Vec<(u32, Decimal)>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inventory: Option<InventoryMark>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_sources: Option<Vec<ExecutionSource>>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct InventoryMark {
    pub indicative_usdc: Option<Decimal>,
    pub all_dust: bool,
    pub marks: Vec<serde_json::Value>,
}
/// Bid marks are indicative, not liquidation proceeds. Unknown books never prove dust.
pub fn mark_inventory(
    balances: &Balances,
    books: &[Book],
    u: &Universe,
    cfg: &Config,
    now: u64,
) -> InventoryMark {
    let mut result = InventoryMark {
        indicative_usdc: Some(Decimal::ZERO),
        all_dust: true,
        marks: Vec::new(),
    };
    for (&token, &qty) in balances
        .iter()
        .filter(|(t, q)| **t != u.usdc && **q > Decimal::ZERO)
    {
        let direct = u
            .markets
            .iter()
            .enumerate()
            .find(|(_, m)| m.base == token && m.quote == u.usdc);
        let mark = direct.and_then(|(i, m)| books[i].top(now, cfg).map(|b| (m, b)));
        let value = mark.and_then(|(_, b)| qty.checked_mul(b.levels[0][0].px));
        result.indicative_usdc = result
            .indicative_usdc
            .zip(value)
            .and_then(|(a, b)| a.checked_add(b));
        // Inspect all conversion exits. A missing exit quote cannot establish that
        // inventory is below that market's quote-token minimum.
        let dust = u
            .markets
            .iter()
            .enumerate()
            .filter(|(_, m)| m.base == token || m.quote == token)
            .all(|(i, m)| {
                if m.base == token {
                    let rounded = floor(qty, u.tokens[&token].sz_decimals);
                    rounded == Decimal::ZERO
                        || books[i]
                            .top(now, cfg)
                            .and_then(|book| rounded.checked_mul(book.levels[0][0].px))
                            .is_some_and(|n| n < Decimal::from(10))
                } else {
                    qty < Decimal::from(10)
                }
            });
        result.all_dust &= value.is_some() && dust;
        result.marks.push(serde_json::json!({"token":token,"quantity":qty,"indicative_usdc":value,"source":mark.map(|(m,_)|&m.coin),"exchange_ms":mark.map(|(_,b)|b.exchange_ms),"receipt_age_ns":mark.map(|(_,b)|now.saturating_sub(b.receipt_ns)),"untradeable_dust":dust}));
    }
    result
}
pub fn estimate(
    route: &Route,
    start: Decimal,
    books: &[Book],
    u: &Universe,
    cfg: &Config,
    now: u64,
) -> Result<Estimate> {
    estimate_model(route, start, books, u, cfg, now, 3)
}
pub fn estimate_model(
    route: &Route,
    start: Decimal,
    books: &[Book],
    u: &Universe,
    cfg: &Config,
    now: u64,
    model: u32,
) -> Result<Estimate> {
    let edges = route.funded_edges(u).context("not_usdc_funded")?;
    let mut balances = Balances::from([(u.usdc, start)]);
    let mut fees = Vec::new();
    let mut input = start;
    let mut sources = Vec::new();
    for e in edges {
        let order = prepare_model(
            e,
            input,
            &books[e.market],
            u,
            cfg,
            now,
            cfg.slippage_bps,
            model,
        )?;
        let f = execute_model(&order, &books[e.market], u, cfg, now, None, model)?;
        ensure!(f.qty == order.qty, "insufficient_depth");
        apply_fill(&mut balances, e, &f, u)?;
        fees.push((f.fee_token, f.fee));
        if let Some(s) = f.source {
            sources.push(s);
        }
        input = f.received;
    }
    let final_usdc = amount(&balances, u.usdc);
    let profit = final_usdc - start;
    balances.remove(&u.usdc);
    balances.retain(|_, q| *q > Decimal::ZERO);
    Ok(Estimate {
        start,
        final_usdc,
        profit,
        bps: div(profit, start)? * Decimal::from(10_000),
        residual: balances,
        fees,
        inventory: None,
        execution_sources: (model >= 4).then_some(sources),
    })
}
