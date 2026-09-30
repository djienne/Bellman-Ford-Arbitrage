//! Concrete Hyperliquid spot wire client, adapted from XEMM 7863f14b.
//! No transfers, leverage changes, builder fees, or arbitrary signed actions.
use crate::{
    config::dec,
    hl_sign,
    market::Universe,
    quantity::{self, Balances, Order},
};
use anyhow::{ensure, Context, Result};
use k256::ecdsa::SigningKey;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{collections::BTreeMap, path::Path, time::Duration};

pub const BASE: &str = "https://api.hyperliquid.xyz";
pub const TTL_MS: u64 = 5000;
pub struct Client {
    pub account: String,
    pub signer: String,
    vault: Option<[u8; 20]>,
    key: SigningKey,
    http: reqwest::Client,
    base: String,
}
impl Client {
    #[cfg(test)]
    pub(crate) fn testing(base: String) -> Self {
        let mut key = [0; 32];
        key[31] = 1;
        Self {
            account: "account".into(),
            signer: "signer".into(),
            vault: Some([0x11; 20]),
            key: SigningKey::from_slice(&key).unwrap(),
            http: reqwest::Client::new(),
            base,
        }
    }
    pub fn load(path: &Path) -> Result<Self> {
        let mut env = BTreeMap::new();
        for line in std::fs::read_to_string(path)
            .context("cannot read live credential file")?
            .lines()
        {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let (name, value) = line.split_once('=').context("malformed credential line")?;
            ensure!(
                ["wallet_address", "private_key", "is_vault", "exchange"].contains(&name.trim()),
                "unknown credential field"
            );
            ensure!(
                env.insert(
                    name.trim().to_owned(),
                    value.trim().trim_matches(['\'', '"']).to_owned()
                )
                .is_none(),
                "duplicate credential field"
            );
        }
        let field = |name: &str| env.get(name).context("missing credential field");
        let raw = hex::decode(field("private_key")?.trim_start_matches("0x"))
            .map_err(|_| anyhow::anyhow!("invalid private key encoding"))?;
        let key =
            SigningKey::from_slice(&raw).map_err(|_| anyhow::anyhow!("invalid signing key"))?;
        let address: [u8; 20] = hex::decode(field("wallet_address")?.trim_start_matches("0x"))
            .context("invalid account address")?
            .try_into()
            .map_err(|_| anyhow::anyhow!("account address must be 20 bytes"))?;
        let account = format!("0x{}", hex::encode(address));
        let public = key.verifying_key().to_encoded_point(false);
        let signer = format!(
            "0x{}",
            hex::encode(&hl_sign::keccak(&public.as_bytes()[1..])[12..])
        );
        let vault = match field("is_vault")?.as_str() {
            "true" => Some(address),
            "false" => None,
            _ => anyhow::bail!("is_vault must be true or false"),
        };
        ensure!(
            vault.is_some() || signer == account,
            "direct account signer does not match account"
        );
        Ok(Self {
            account,
            signer,
            vault,
            key,
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(5))
                .tcp_nodelay(true)
                .pool_idle_timeout(Duration::from_secs(120))
                .build()?,
            base: BASE.into(),
        })
    }
    pub async fn info(&self, body: Value) -> Result<Value> {
        Ok(self
            .http
            .post(format!("{}/info", self.base))
            .json(&body)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?)
    }
    pub async fn user(&self, kind: &str) -> Result<Value> {
        self.info(json!({"type":kind,"user":self.account})).await
    }
    pub async fn balances(&self, universe: &Universe) -> Result<Balances> {
        balances(&self.user("spotClearinghouseState").await?, universe)
    }
    pub async fn exchange<A: Serialize>(
        &self,
        action: &A,
        nonce: u64,
        expiry: Option<u64>,
    ) -> Value {
        let signature = match hl_sign::sign(&self.key, action, nonce, self.vault.as_ref(), expiry) {
            Ok(s) => s,
            Err(_) => return json!({"local_error":"signing failed before send"}),
        };
        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct Envelope<'a, A> {
            action: &'a A,
            nonce: u64,
            signature: hl_sign::Signature,
            #[serde(skip_serializing_if = "Option::is_none")]
            vault_address: Option<&'a str>,
            #[serde(skip_serializing_if = "Option::is_none")]
            expires_after: Option<u64>,
        }
        let body = Envelope {
            action,
            nonce,
            signature,
            vault_address: self.vault.as_ref().map(|_| self.account.as_str()),
            expires_after: expiry,
        };
        match self
            .http
            .post(format!("{}/exchange", self.base))
            .json(&body)
            .send()
            .await
        {
            Ok(reply) => {
                let status = reply.status().as_u16();
                match reply.json::<Value>().await {
                    Ok(v) if status == 200 => v,
                    Ok(v) => json!({"http_status":status,"body":v,"unknown":true}),
                    Err(_) => json!({"http_status":status,"unknown":true}),
                }
            }
            Err(_) => json!({"transport_error":"exchange reply unavailable","unknown":true}),
        }
    }
    pub async fn order_status(&self, cloid: &str) -> Result<Value> {
        self.info(json!({"type":"orderStatus","user":self.account,"oid":cloid}))
            .await
    }
    pub async fn fills(&self, since_ms: u64) -> Result<Vec<Value>> {
        let mut start = since_ms;
        let mut all = Vec::new();
        for _ in 0..8 {
            let v = self.info(json!({"type":"userFillsByTime","user":self.account,"startTime":start,"aggregateByTime":false})).await?;
            let rows = v.as_array().context("fills are not an array")?;
            all.extend(rows.iter().cloned());
            if rows.len() < 2000 {
                return Ok(all);
            }
            let next = rows
                .iter()
                .filter_map(|f| f["time"].as_u64())
                .max()
                .context("fill page has no time")?;
            ensure!(next > start, "ambiguous truncated fill page");
            start = next; // Inclusive boundary: dedup by tid; never skip same-ms fills.
        }
        anyhow::bail!("fill history exceeds bounded recovery window")
    }
}
pub fn balances(v: &Value, u: &Universe) -> Result<Balances> {
    let mut out = Balances::new();
    for row in v["balances"].as_array().context("missing spot balances")? {
        let token = u32::try_from(row["token"].as_u64().context("balance token identity")?)?;
        ensure!(u.tokens.contains_key(&token), "unknown balance token");
        let total = dec(row["total"].as_str().context("balance total")?)?;
        let hold = dec(row["hold"].as_str().context("balance hold")?)?;
        ensure!(
            total >= Decimal::ZERO && hold >= Decimal::ZERO && hold <= total,
            "invalid spot balance"
        );
        ensure!(hold == Decimal::ZERO, "unexpected held balance/open order");
        ensure!(
            out.insert(token, total).is_none(),
            "duplicate balance token"
        );
    }
    Ok(out)
}
#[derive(Serialize)]
pub struct Limit {
    pub tif: &'static str,
}
#[derive(Serialize)]
pub struct OrderType {
    pub limit: Limit,
}
#[derive(Serialize)]
pub struct OrderWire {
    a: u32,
    b: bool,
    p: String,
    s: String,
    r: bool,
    t: OrderType,
    c: String,
}
#[derive(Serialize)]
pub struct OrderAction {
    #[serde(rename = "type")]
    kind: &'static str,
    orders: [OrderWire; 1],
    grouping: &'static str,
}
pub fn order_action(order: &Order, u: &Universe, cloid: &str, alo: bool) -> Result<OrderAction> {
    let m = &u.markets[order.edge.market];
    let dp = u.tokens[&m.base].sz_decimals;
    ensure!(
        order.qty > Decimal::ZERO && quantity::floor(order.qty, dp) == order.qty,
        "invalid live lot"
    );
    ensure!(order.limit > Decimal::ZERO, "invalid live price");
    ensure!(
        quantity::limit_price(order.limit, dp, order.edge.buy) == order.limit,
        "invalid live tick"
    );
    ensure!(
        order.qty * order.limit >= Decimal::from(10),
        "minimum live notional"
    );
    let spent = if order.edge.buy {
        order.qty * order.limit
    } else {
        order.qty
    };
    ensure!(spent <= order.budget, "order exceeds reserved token budget");
    ensure!(
        cloid.len() == 34 && cloid.starts_with("0x") && hex::decode(&cloid[2..]).is_ok(),
        "invalid cloid"
    );
    Ok(OrderAction {
        kind: "order",
        orders: [OrderWire {
            a: m.index.checked_add(10000).context("spot asset overflow")?,
            b: order.edge.buy,
            p: order.limit.normalize().to_string(),
            s: order.qty.normalize().to_string(),
            r: false,
            t: OrderType {
                limit: Limit {
                    tif: if alo { "Alo" } else { "Ioc" },
                },
            },
            c: cloid.into(),
        }],
        grouping: "na",
    })
}
#[derive(Serialize)]
struct Cancel {
    asset: u32,
    cloid: String,
}
#[derive(Serialize)]
pub struct CancelAction {
    #[serde(rename = "type")]
    kind: &'static str,
    cancels: [Cancel; 1],
}
pub fn cancel_action(index: u32, cloid: &str) -> CancelAction {
    CancelAction {
        kind: "cancelByCloid",
        cancels: [Cancel {
            asset: 10000 + index,
            cloid: cloid.into(),
        }],
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Trade {
    pub tid: u64,
    pub oid: u64,
    pub qty: Decimal,
    pub px: Decimal,
    pub fee: Decimal,
    pub fee_token: u32,
    pub exchange_ms: u64,
}
pub fn trade(v: &Value, order: &Order, u: &Universe) -> Result<Trade> {
    let m = &u.markets[order.edge.market];
    ensure!(v["coin"].as_str() == Some(&m.coin), "fill market mismatch");
    ensure!(
        v["side"].as_str() == Some(if order.edge.buy { "B" } else { "A" }),
        "fill side mismatch"
    );
    let fee_name = v["feeToken"].as_str().context("missing fee token")?;
    let fee_tokens: Vec<_> = [m.base, m.quote]
        .into_iter()
        .filter(|t| u.tokens[t].name == fee_name)
        .collect();
    ensure!(fee_tokens.len() == 1, "ambiguous/unsupported fee token");
    let out = Trade {
        tid: v["tid"].as_u64().context("fill tid")?,
        oid: v["oid"].as_u64().context("fill oid")?,
        qty: dec(v["sz"].as_str().context("fill size")?)?,
        px: dec(v["px"].as_str().context("fill price")?)?,
        fee: dec(v["fee"].as_str().context("fill fee")?)?,
        fee_token: fee_tokens[0],
        exchange_ms: v["time"].as_u64().context("fill time")?,
    };
    ensure!(
        out.qty > Decimal::ZERO && out.px > Decimal::ZERO,
        "invalid live fill"
    );
    ensure!(
        if order.edge.buy {
            out.px <= order.limit
        } else {
            out.px >= order.limit
        },
        "fill violated fixed limit"
    );
    Ok(out)
}
pub fn apply_trades(
    b: &mut Balances,
    order: &Order,
    trades: &[Trade],
    u: &Universe,
) -> Result<Decimal> {
    let mut next = b.clone();
    let mut seen = std::collections::BTreeSet::new();
    let mut qty = Decimal::ZERO;
    let mut spent = Decimal::ZERO;
    for f in trades {
        ensure!(seen.insert((f.oid, f.tid)), "duplicate live trade");
        ensure!(
            f.qty > Decimal::ZERO
                && f.px > Decimal::ZERO
                && if order.edge.buy {
                    f.px <= order.limit
                } else {
                    f.px >= order.limit
                },
            "invalid live trade price/size"
        );
        qty += f.qty;
        spent += if order.edge.buy { f.qty * f.px } else { f.qty };
        if f.fee_token == order.edge.from(u) {
            spent += f.fee;
        }
        quantity::add(
            &mut next,
            order.edge.from(u),
            -if order.edge.buy { f.qty * f.px } else { f.qty },
        );
        quantity::add(
            &mut next,
            order.edge.to(u),
            if order.edge.buy { f.qty } else { f.qty * f.px },
        );
        quantity::add(&mut next, f.fee_token, -f.fee);
    }
    ensure!(
        qty <= order.qty && spent <= order.budget,
        "live execution exceeded quantity/budget"
    );
    ensure!(
        next.values().all(|v| *v >= Decimal::ZERO),
        "negative confirmed token ledger"
    );
    *b = next;
    Ok(qty)
}
#[cfg(test)]
mod tests {
    use super::*;
    fn universe() -> Universe {
        Universe::parse(
            &serde_json::from_str(include_str!("../review/hyperliquid_spot_snapshot.json"))
                .unwrap(),
            &crate::config::Config::default(),
        )
        .unwrap()
    }
    fn order(u: &Universe, buy: bool) -> Order {
        Order {
            edge: crate::market::Edge {
                market: u.markets.iter().position(|m| m.index == 107).unwrap(),
                buy,
            },
            qty: dec("0.12").unwrap(),
            limit: Decimal::from(100),
            budget: if buy {
                Decimal::from(12)
            } else {
                dec("0.12").unwrap()
            },
            source: None,
        }
    }
    #[test]
    fn spot_wire_keeps_exact_tick_asset_and_reservation() {
        let u = universe();
        let o = order(&u, true);
        let action = order_action(&o, &u, "0x000102030405060708090a0b0c0d0e0f", false).unwrap();
        let v = serde_json::to_value(&action).unwrap();
        assert_eq!(v["orders"][0]["a"], 10107);
        assert_eq!(v["orders"][0]["p"], "100");
        assert_eq!(v["orders"][0]["r"], false);
        assert_eq!(v["orders"][0]["t"]["limit"]["tif"], "Ioc");
        let mut bad = o.clone();
        bad.qty = dec("0.121").unwrap();
        assert!(order_action(&bad, &u, "0x000102030405060708090a0b0c0d0e0f", false).is_err());
        bad = o.clone();
        bad.limit = Decimal::ZERO;
        assert!(order_action(&bad, &u, "0x000102030405060708090a0b0c0d0e0f", false).is_err());
        bad = o;
        bad.budget = Decimal::from(11);
        assert!(order_action(&bad, &u, "0x000102030405060708090a0b0c0d0e0f", false).is_err());
    }
    #[test]
    fn actual_fee_tokens_and_partial_fills_conserve_inventory() {
        let u = universe();
        let buy = order(&u, true);
        let base = u.markets[buy.edge.market].base;
        let fill = Trade {
            tid: 1,
            oid: 2,
            qty: dec("0.06").unwrap(),
            px: Decimal::from(100),
            fee: dec("0.000042").unwrap(),
            fee_token: base,
            exchange_ms: 1,
        };
        let mut b = Balances::from([(u.usdc, Decimal::from(12))]);
        apply_trades(&mut b, &buy, &[fill.clone()], &u).unwrap();
        assert_eq!(quantity::amount(&b, u.usdc), Decimal::from(6));
        assert_eq!(quantity::amount(&b, base), dec("0.059958").unwrap());
        let mut sell = order(&u, false);
        sell.qty = dec("0.05").unwrap();
        let fill = Trade {
            tid: 3,
            oid: 4,
            qty: dec("0.05").unwrap(),
            px: Decimal::from(100),
            fee: dec("0.0035").unwrap(),
            fee_token: u.usdc,
            exchange_ms: 2,
        };
        apply_trades(&mut b, &sell, &[fill], &u).unwrap();
        assert_eq!(quantity::amount(&b, u.usdc), dec("10.9965").unwrap());
        assert_eq!(quantity::amount(&b, base), dec("0.009958").unwrap());
    }
    #[test]
    fn duplicate_invalid_and_overbudget_fills_do_not_mutate_ledger() {
        let u = universe();
        let o = order(&u, true);
        let base = u.markets[o.edge.market].base;
        let f = Trade {
            tid: 1,
            oid: 2,
            qty: o.qty,
            px: o.limit,
            fee: dec("0.000084").unwrap(),
            fee_token: base,
            exchange_ms: 1,
        };
        let original = Balances::from([(u.usdc, Decimal::from(12))]);
        let mut b = original.clone();
        assert!(apply_trades(&mut b, &o, &[f.clone(), f.clone()], &u).is_err());
        assert_eq!(b, original);
        let mut bad = f.clone();
        bad.qty = Decimal::ONE;
        assert!(apply_trades(&mut b, &o, &[bad], &u).is_err());
        assert_eq!(b, original);
        bad = f;
        bad.px = Decimal::from(101);
        assert!(apply_trades(&mut b, &o, &[bad], &u).is_err());
        assert_eq!(b, original);
    }
    #[test]
    fn balances_use_numeric_token_identity_and_reject_holds() {
        let u = universe();
        assert_eq!(balances(&json!({"balances":[{"token":0,"coin":"wrong display name","total":"75","hold":"0"}]}),&u).unwrap()[&0],Decimal::from(75));
        assert!(balances(
            &json!({"balances":[{"token":0,"total":"75","hold":"1"}]}),
            &u
        )
        .is_err());
        assert!(balances(
            &json!({"balances":[{"token":99999,"total":"75","hold":"0"}]}),
            &u
        )
        .is_err());
    }
    #[test]
    fn fill_direction_limits_and_fee_identity_are_validated() {
        let u = universe();
        let o = order(&u, true);
        let v = json!({"coin":"@107","side":"B","sz":"0.12","px":"100","fee":"0.000084","feeToken":"HYPE","oid":2,"tid":1,"time":1});
        assert!(trade(&v, &o, &u).is_ok());
        for (field, value) in [
            ("coin", "HYPE"),
            ("side", "A"),
            ("px", "101"),
            ("feeToken", "UNKNOWN"),
        ] {
            let mut bad = v.clone();
            bad[field] = value.into();
            assert!(trade(&bad, &o, &u).is_err());
        }
    }
    #[tokio::test]
    async fn transport_loss_remains_unknown_and_never_retries_exchange() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let path = std::env::temp_dir().join(format!("hl-env-{}", crate::hyperliquid::utc_ns()));
        std::fs::write(&path,"wallet_address=0x1111111111111111111111111111111111111111\nprivate_key=0000000000000000000000000000000000000000000000000000000000000001\nis_vault=true\n").unwrap();
        let mut client = Client::load(&path).unwrap();
        std::fs::remove_file(path).unwrap();
        client.base = endpoint;
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut bytes = [0; 4096];
            socket.read(&mut bytes).await.unwrap();
            socket.write_all(b"HTTP/1.1 500 Internal Server Error\r\nContent-Length: 7\r\nConnection: close\r\n\r\ninvalid").await.unwrap();
        });
        let reply = client
            .exchange(
                &cancel_action(107, "0x000102030405060708090a0b0c0d0e0f"),
                1,
                None,
            )
            .await;
        assert_eq!(reply["unknown"], true);
        server.await.unwrap();
    }
}
