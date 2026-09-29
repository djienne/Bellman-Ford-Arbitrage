use crate::{
    config::{dec, Config},
    market::{Edge, Universe},
};
use anyhow::{ensure, Context, Result};
use rust_decimal::{prelude::ToPrimitive, Decimal};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Level {
    pub px: Decimal,
    pub sz: Decimal,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Observation {
    pub exchange_ms: u64,
    pub receipt_ns: u64,
    pub available_ns: u64,
    pub levels: [Vec<Level>; 2],
    pub log_bid: f64,
    pub log_ask: f64,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Book {
    pub bbo: Option<Observation>,
    pub depth: Option<Observation>,
    pub reason: Option<String>,
    pub last_exchange_ms: u64,
}
#[derive(Clone, Debug)]
pub struct Update {
    pub market: usize,
    pub depth: bool,
    pub observation: Observation,
}

/// A single observed side. Never splice a changed BBO into older deeper levels.
pub struct ExecutionView<'a> {
    pub observation: &'a Observation,
    pub price_observation: &'a Observation,
    pub rows: &'a [Level],
    pub channel: &'static str,
    pub scope: &'static str,
}

pub fn parse(
    text: &str,
    u: &Universe,
    receipt_ns: u64,
    available_ns: u64,
) -> Result<Option<Update>> {
    let v: Value = serde_json::from_str(text)?;
    let channel = v["channel"].as_str().context("missing channel")?;
    ensure!(
        channel != "error",
        "exchange subscription error: {}",
        v["data"]
    );
    if channel != "bbo" && channel != "l2Book" {
        return Ok(None);
    }
    let d = &v["data"];
    let coin = d["coin"].as_str().context("missing coin")?;
    let Some(market) = u.markets.iter().position(|m| m.coin == coin) else {
        return Ok(None);
    };
    let time = d["time"]
        .as_u64()
        .filter(|t| *t > 0)
        .context("invalid exchange time")?;
    let depth = channel == "l2Book";
    let sides = d[if depth { "levels" } else { "bbo" }]
        .as_array()
        .filter(|a| a.len() == 2)
        .context("invalid book sides")?;
    let mut levels: [Vec<Level>; 2] = [Vec::new(), Vec::new()];
    for (side, out) in levels.iter_mut().enumerate() {
        let rows = if depth {
            sides[side].as_array().context("invalid depth")?.clone()
        } else if sides[side].is_null() {
            Vec::new()
        } else {
            vec![sides[side].clone()]
        };
        ensure!(rows.len() <= 100, "excess depth");
        for row in rows {
            let px = dec(row["px"].as_str().context("missing price")?)?;
            let sz = dec(row["sz"].as_str().context("missing quantity")?)?;
            ensure!(
                px > Decimal::ZERO
                    && sz > Decimal::ZERO
                    && px <= Decimal::from(1_000_000_000_000_000_000u64)
                    && sz <= Decimal::from(1_000_000_000_000_000_000u64),
                "invalid price/size"
            );
            if let Some(prev) = out.last() {
                let prev: &Level = prev;
                ensure!(
                    if side == 0 {
                        prev.px > px
                    } else {
                        prev.px < px
                    },
                    "unsorted or duplicate depth"
                );
            }
            out.push(Level { px, sz });
        }
    }
    // Locked books are permitted: with nonnegative fees their direct round trip cannot profit.
    if let (Some(b), Some(a)) = (levels[0].first(), levels[1].first()) {
        ensure!(b.px <= a.px, "crossed book");
    }
    let log_bid = levels[0]
        .first()
        .map(|l| l.px.to_f64().unwrap().ln())
        .unwrap_or(f64::NEG_INFINITY);
    let log_ask = levels[1]
        .first()
        .map(|l| l.px.to_f64().unwrap().ln())
        .unwrap_or(f64::INFINITY);
    Ok(Some(Update {
        market,
        depth,
        observation: Observation {
            exchange_ms: time,
            receipt_ns,
            available_ns,
            levels,
            log_bid,
            log_ask,
        },
    }))
}
impl Book {
    pub fn invalidate(&mut self, why: &str) {
        self.bbo = None;
        self.depth = None;
        self.reason = Some(why.into());
    }
    pub fn apply(&mut self, update: &Update) -> bool {
        if update.observation.exchange_ms < self.last_exchange_ms {
            return false;
        }
        // Cross-channel ordering matters too: an older BBO must not replenish a
        // shadow book already based on a newer full snapshot (or vice versa).
        if self
            .latest()
            .is_some_and(|old| old.exchange_ms > update.observation.exchange_ms)
        {
            return false;
        }
        let slot = if update.depth {
            &mut self.depth
        } else {
            &mut self.bbo
        };
        if slot
            .as_ref()
            .is_some_and(|old| old.exchange_ms > update.observation.exchange_ms)
        {
            return false;
        }
        *slot = Some(update.observation.clone());
        self.last_exchange_ms = update.observation.exchange_ms;
        self.reason = None;
        true
    }
    fn latest(&self) -> Option<&Observation> {
        match (&self.bbo, &self.depth) {
            (Some(a), Some(b)) => Some(
                if (a.exchange_ms, a.available_ns) > (b.exchange_ms, b.available_ns) {
                    a
                } else {
                    b
                },
            ),
            (Some(a), None) | (None, Some(a)) => Some(a),
            _ => None,
        }
    }
    pub fn top(&self, now: u64, cfg: &Config) -> Option<&Observation> {
        let o = self.latest()?;
        (o.available_ns <= now
            && now.saturating_sub(o.receipt_ns) <= cfg.quote_age_ms * 1_000_000
            && !o.levels[0].is_empty()
            && !o.levels[1].is_empty())
        .then_some(o)
    }
    /// An explicitly empty side is observable no-liquidity, unlike a missing/stale frame.
    pub fn known_empty(&self, buy: bool, now: u64, cfg: &Config) -> bool {
        self.latest().is_some_and(|o| {
            o.available_ns <= now
                && now.saturating_sub(o.receipt_ns) <= cfg.quote_age_ms * 1_000_000
                && o.levels[usize::from(buy)].is_empty()
        })
    }
    pub fn depth(&self, now: u64, cfg: &Config) -> Result<&Observation> {
        let top = self.top(now, cfg).context("quote_unavailable")?;
        let d = self.depth.as_ref().context("depth_missing")?;
        ensure!(
            !d.levels[0].is_empty() && !d.levels[1].is_empty(),
            "depth_empty"
        );
        ensure!(
            d.available_ns <= now
                && now.saturating_sub(d.receipt_ns) <= cfg.depth_age_ms * 1_000_000,
            "depth_stale"
        );
        ensure!(
            top.levels[0][0] == d.levels[0][0] && top.levels[1][0] == d.levels[1][0],
            "bbo_depth_conflict"
        );
        Ok(d)
    }
    pub fn execution_side(
        &self,
        buy: bool,
        now: u64,
        cfg: &Config,
        limit: Option<Decimal>,
        model: u32,
    ) -> Result<ExecutionView<'_>> {
        let side = usize::from(buy);
        if model < 4 {
            let d = self.depth(now, cfg)?;
            return Ok(ExecutionView {
                observation: d,
                price_observation: d,
                rows: &d.levels[side],
                channel: "l2Book",
                scope: "l2",
            });
        }
        let top = self.latest().context("quote_unavailable")?;
        ensure!(
            top.available_ns <= now
                && now.saturating_sub(top.receipt_ns) <= cfg.quote_age_ms * 1_000_000,
            "quote_unavailable"
        );
        let channel = if self.depth.as_ref().is_some_and(|d| std::ptr::eq(d, top)) {
            "l2Book"
        } else {
            "bbo"
        };
        let rows = &top.levels[side];
        if rows.is_empty()
            || limit.is_some_and(|l| if buy { rows[0].px > l } else { rows[0].px < l })
        {
            return Ok(ExecutionView {
                observation: top,
                price_observation: top,
                rows: &[],
                channel,
                scope: "zero",
            });
        }
        if let Some(d) = self.depth.as_ref().filter(|d| {
            d.available_ns <= now
                && now.saturating_sub(d.receipt_ns) <= cfg.depth_age_ms * 1_000_000
                && d.levels[side].first() == rows.first()
        }) {
            return Ok(ExecutionView {
                observation: d,
                price_observation: top,
                rows: &d.levels[side],
                channel: "l2Book",
                scope: "l2",
            });
        }
        ensure!(
            now.saturating_sub(top.receipt_ns) <= cfg.depth_age_ms * 1_000_000,
            "quantity_stale"
        );
        Ok(ExecutionView {
            observation: top,
            price_observation: top,
            rows: &rows[..1],
            channel,
            scope: "top",
        })
    }
    pub fn price(&self, e: Edge, now: u64, cfg: &Config) -> Option<Decimal> {
        Some(self.top(now, cfg)?.levels[usize::from(e.buy)][0].px)
    }
}
