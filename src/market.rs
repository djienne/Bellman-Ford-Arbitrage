use crate::config::Config;
use anyhow::{ensure, Context, Result};
use rust_decimal::{prelude::ToPrimitive, Decimal};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Token {
    pub index: u32,
    pub name: String,
    pub token_id: String,
    pub sz_decimals: u32,
    pub wei_decimals: u32,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Market {
    pub index: u32,
    pub coin: String,
    pub base: u32,
    pub quote: u32,
    pub fee: Decimal,
    pub fee_provenance: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Universe {
    pub tokens: BTreeMap<u32, Token>,
    pub markets: Vec<Market>,
    pub usdc: u32,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct Edge {
    pub market: usize,
    pub buy: bool,
}
impl Edge {
    pub fn from(self, u: &Universe) -> u32 {
        let m = &u.markets[self.market];
        if self.buy {
            m.quote
        } else {
            m.base
        }
    }
    pub fn to(self, u: &Universe) -> u32 {
        self.reverse().from(u)
    }
    pub fn reverse(self) -> Self {
        Self {
            market: self.market,
            buy: !self.buy,
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Route {
    pub id: String,
    pub name: String,
    pub edges: Vec<Edge>,
}

impl Universe {
    pub fn parse(value: &Value, cfg: &Config) -> Result<Self> {
        let meta = value.get(0).unwrap_or(value);
        let mut tokens = BTreeMap::new();
        for t in meta["tokens"].as_array().context("missing spot tokens")? {
            let i = u32::try_from(t["index"].as_u64().context("token index")?)?;
            let token = Token {
                index: i,
                name: t["name"].as_str().context("token name")?.into(),
                token_id: t["tokenId"].as_str().context("token id")?.into(),
                sz_decimals: u32::try_from(t["szDecimals"].as_u64().context("size decimals")?)?,
                wei_decimals: u32::try_from(t["weiDecimals"].as_u64().context("wei decimals")?)?,
            };
            ensure!(
                token.sz_decimals <= 8
                    && token.wei_decimals <= 28
                    && token.sz_decimals <= token.wei_decimals,
                "unsupported decimal precision"
            );
            ensure!(tokens.insert(i, token).is_none(), "duplicate token index");
        }
        ensure!(
            tokens.get(&0).is_some_and(|t| t.name == "USDC"),
            "native USDC token missing"
        );
        let rows = meta["universe"]
            .as_array()
            .context("missing spot universe")?;
        let quotes: BTreeSet<u32> = rows
            .iter()
            .filter_map(|m| m["tokens"][1].as_u64().and_then(|x| u32::try_from(x).ok()))
            .collect();
        let mut markets = Vec::new();
        let mut ids = BTreeSet::new();
        for row in rows {
            let index = u32::try_from(row["index"].as_u64().context("market index")?)?;
            ensure!(ids.insert(index), "duplicate market index");
            let base = u32::try_from(row["tokens"][0].as_u64().context("base token")?)?;
            let quote = u32::try_from(row["tokens"][1].as_u64().context("quote token")?)?;
            ensure!(
                base != quote && tokens.contains_key(&base) && tokens.contains_key(&quote),
                "invalid spot token reference"
            );
            let coin = if index == 0 {
                "PURR/USDC".into()
            } else {
                format!("@{index}")
            };
            ensure!(
                row["name"].as_str() == Some(&coin),
                "unexpected market name for index {index}"
            );
            let (fee, provenance) = if let Some(bps) = cfg.fee_overrides_bps.get(&coin) {
                (
                    *bps / Decimal::from(10_000),
                    "explicit market override".to_string(),
                )
            } else {
                let stable = quotes.contains(&base) && quotes.contains(&quote);
                let aligned = cfg.aligned_quote_token_ids.contains(&quote);
                let rate = cfg.taker_fee_bps / Decimal::from(10_000)
                    * if stable {
                        Decimal::new(2, 1)
                    } else {
                        Decimal::ONE
                    }
                    * if aligned {
                        Decimal::new(8, 1)
                    } else {
                        Decimal::ONE
                    };
                (rate,format!("base spot taker; quote-pair={stable}; verified aligned override={aligned}; account discounts unapplied"))
            };
            markets.push(Market {
                index,
                coin,
                base,
                quote,
                fee,
                fee_provenance: provenance,
            });
        }
        markets.sort_by_key(|m| m.index);
        // Contexts may include unlisted entries. Validate identity; never zip by position.
        if let Some(ctx) = value.get(1).and_then(Value::as_array) {
            let mut seen = BTreeSet::new();
            for c in ctx {
                if let Some(coin) = c["coin"].as_str() {
                    ensure!(seen.insert(coin), "duplicate context coin");
                }
            }
        }
        Ok(Self {
            tokens,
            markets,
            usdc: 0,
        })
    }
    pub fn routes(&self, cfg: &Config) -> Result<Vec<Route>> {
        let mut adj: BTreeMap<u32, Vec<Edge>> = BTreeMap::new();
        for i in 0..self.markets.len() {
            for buy in [false, true] {
                let e = Edge { market: i, buy };
                adj.entry(e.from(self)).or_default().push(e);
            }
        }
        let mut paths = Vec::new();
        let mut work = 0usize;
        fn dfs(
            u: &Universe,
            adj: &BTreeMap<u32, Vec<Edge>>,
            cfg: &Config,
            start: u32,
            node: u32,
            seen: &mut BTreeSet<u32>,
            path: &mut Vec<Edge>,
            out: &mut Vec<Vec<Edge>>,
            work: &mut usize,
        ) -> Result<()> {
            for &e in &adj[&node] {
                *work += 1;
                ensure!(
                    *work <= 20_000_000,
                    "cycle traversal ceiling exceeded; no partial universe published"
                );
                let v = e.to(u);
                if v == start && path.len() + 1 >= cfg.min_cycle_len {
                    let mut c = path.clone();
                    c.push(e);
                    out.push(c);
                    ensure!(
                        out.len() <= cfg.max_cycles,
                        "cycle inventory ceiling exceeded"
                    );
                } else if v > start && path.len() + 1 < cfg.max_cycle_len && seen.insert(v) {
                    path.push(e);
                    dfs(u, adj, cfg, start, v, seen, path, out, work)?;
                    path.pop();
                    seen.remove(&v);
                }
            }
            Ok(())
        }
        // Exponential ceiling: fail visibly on a much denser universe; profile before
        // upgrading the search. Never truncate a claimed complete cycle inventory.
        for &s in adj.keys() {
            dfs(
                self,
                &adj,
                cfg,
                s,
                s,
                &mut BTreeSet::from([s]),
                &mut Vec::new(),
                &mut paths,
                &mut work,
            )?;
        }
        let mut routes = Vec::new();
        for edges in paths {
            let keys: Vec<_> = edges
                .iter()
                .map(|e| (self.markets[e.market].index, e.buy))
                .collect();
            let n = keys.len();
            let pos = (0..n)
                .min_by_key(|&i| (0..n).map(|j| keys[(i + j) % n]).collect::<Vec<_>>())
                .unwrap();
            let id = (0..n)
                .map(|j| {
                    let (k, b) = keys[(pos + j) % n];
                    format!("{k}{}", if b { 'B' } else { 'S' })
                })
                .collect::<Vec<_>>()
                .join(">");
            let name = edges
                .iter()
                .map(|e| self.tokens[&e.from(self)].name.clone())
                .chain(std::iter::once(
                    self.tokens[&edges[0].from(self)].name.clone(),
                ))
                .collect::<Vec<_>>()
                .join(" -> ");
            routes.push(Route { id, name, edges });
        }
        routes.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(routes)
    }
}
impl Route {
    pub fn funded_edges(&self, u: &Universe) -> Option<Vec<Edge>> {
        let p = self.edges.iter().position(|e| e.from(u) == u.usdc)?;
        Some(
            (0..self.edges.len())
                .map(|i| self.edges[(p + i) % self.edges.len()])
                .collect(),
        )
    }
}
/// Full-pass Bellman-Ford on immutable weights. One witness, not an all-cycle search.
pub fn negative_cycle(u: &Universe, weights: &[(Edge, f64)]) -> Result<Option<Vec<Edge>>> {
    let ids: BTreeMap<_, _> = u.tokens.keys().enumerate().map(|(i, &n)| (n, i)).collect();
    let n = ids.len();
    let mut d = vec![0.0; n];
    let mut pred: Vec<Option<Edge>> = vec![None; n];
    let mut last = None;
    for _ in 0..n {
        last = None;
        for &(e, w) in weights {
            if !w.is_finite() {
                continue;
            }
            let (a, b) = (ids[&e.from(u)], ids[&e.to(u)]);
            if d[a] + w < d[b] - 1e-12 {
                d[b] = d[a] + w;
                pred[b] = Some(e);
                last = Some(b);
            }
        }
        if last.is_none() {
            return Ok(None);
        }
    }
    let mut v = last.unwrap();
    for _ in 0..n {
        v = ids[&pred[v].context("BF predecessor missing")?.from(u)];
    }
    let start = v;
    let mut cycle = Vec::new();
    loop {
        let e = pred[v].context("BF predecessor missing")?;
        cycle.push(e);
        v = ids[&e.from(u)];
        if v == start {
            break;
        }
        ensure!(cycle.len() <= n, "invalid BF chain");
    }
    cycle.reverse();
    let sum: f64 = cycle
        .iter()
        .map(|e| weights.iter().find(|(x, _)| x == e).unwrap().1)
        .sum();
    ensure!(sum < 0.0, "nonnegative BF witness");
    Ok(Some(cycle))
}
pub fn fee_f64(m: &Market) -> f64 {
    m.fee.to_f64().unwrap()
}
