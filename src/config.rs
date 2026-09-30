use anyhow::{ensure, Context, Result};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::Path, str::FromStr};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub min_cycle_len: usize,
    pub max_cycle_len: usize,
    pub max_cycles: usize,
    pub starting_usdc: Decimal,
    pub amounts_usdc: Vec<Decimal>,
    pub latency_ms: Vec<u64>,
    pub min_profit_bps: Decimal,
    pub dust_limit_usdc: Decimal,
    pub taker_fee_bps: Decimal,
    pub fee_overrides_bps: BTreeMap<String, Decimal>,
    pub aligned_quote_token_ids: Vec<u32>,
    pub slippage_bps: Decimal,
    pub unwind_slippage_bps: Decimal,
    pub quote_age_ms: u64,
    pub depth_age_ms: u64,
    pub l2_fast: bool,
    pub socket_timeout_secs: u64,
    pub channel_capacity: usize,
    pub record_limit_bytes: u64,
    pub rotate_bytes: u64,
    pub rotate_secs: u64,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            min_cycle_len: 3,
            max_cycle_len: 6,
            max_cycles: 200_000,
            starting_usdc: Decimal::from(10_000),
            amounts_usdc: [25, 100, 250, 1000].map(Decimal::from).to_vec(),
            latency_ms: vec![100, 250, 500],
            min_profit_bps: Decimal::from(5),
            // Formats 3–4 use this value-based dust policy; model 5 uses sub-lots.
            dust_limit_usdc: Decimal::ZERO,
            taker_fee_bps: Decimal::from(7),
            fee_overrides_bps: BTreeMap::new(),
            aligned_quote_token_ids: vec![],
            slippage_bps: Decimal::from(2),
            unwind_slippage_bps: Decimal::from(50),
            quote_age_ms: 1000,
            depth_age_ms: 1000,
            // Missing in old manifests: preserve their default twenty-level feed.
            // The shipped config explicitly opts into the fast five-level feed.
            l2_fast: false,
            socket_timeout_secs: 30,
            channel_capacity: 4096,
            record_limit_bytes: 50 * 1024 * 1024 * 1024,
            rotate_bytes: 256 * 1024 * 1024,
            rotate_secs: 3600,
        }
    }
}
impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        let c: Self = toml::from_str(&std::fs::read_to_string(path)?)
            .context("invalid configuration: only Hyperliquid spot is supported")?;
        c.validate()?;
        Ok(c)
    }
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.min_cycle_len >= 3
                && self.max_cycle_len >= self.min_cycle_len
                && self.max_cycle_len <= 8,
            "cycle bound must be 3..8"
        );
        ensure!(
            self.max_cycles > 0 && self.max_cycles <= 1_000_000,
            "invalid cycle ceiling"
        );
        ensure!(
            self.starting_usdc > Decimal::ZERO && !self.amounts_usdc.is_empty(),
            "paper funding required"
        );
        ensure!(
            self.amounts_usdc
                .iter()
                .all(|v| *v > Decimal::ZERO && *v <= self.starting_usdc),
            "invalid starting amount"
        );
        ensure!(
            !self.latency_ms.is_empty() && self.latency_ms.iter().all(|v| *v <= 60_000),
            "invalid latency"
        );
        let mut ls = self.latency_ms.clone();
        ls.sort();
        ls.dedup();
        ensure!(ls.len() == self.latency_ms.len(), "duplicate account");
        for fee in std::iter::once(&self.taker_fee_bps).chain(self.fee_overrides_bps.values()) {
            ensure!(
                *fee >= Decimal::ZERO && *fee < Decimal::from(1000),
                "invalid fee"
            );
        }
        ensure!(
            self.min_profit_bps >= Decimal::ZERO
                && self.dust_limit_usdc >= Decimal::ZERO
                && self.dust_limit_usdc <= self.starting_usdc
                && self.slippage_bps >= Decimal::ZERO
                && self.slippage_bps < Decimal::from(1000)
                && self.unwind_slippage_bps >= Decimal::ZERO
                && self.unwind_slippage_bps < Decimal::from(1000),
            "invalid threshold/slippage"
        );
        ensure!(
            self.quote_age_ms > 0
                && self.depth_age_ms > 0
                && self.socket_timeout_secs >= 5
                && self.channel_capacity > 0,
            "invalid freshness/channel"
        );
        ensure!(
            self.rotate_secs > 0
                && self.rotate_bytes > 0
                && self.record_limit_bytes > self.rotate_bytes,
            "invalid recording limits"
        );
        Ok(())
    }
}
pub fn dec(s: &str) -> Result<Decimal> {
    Decimal::from_str(s).context("invalid decimal")
}
