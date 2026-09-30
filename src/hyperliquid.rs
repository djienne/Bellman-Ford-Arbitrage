//! Public discovery and WebSocket transport adapted from XEMM SCREENER at
//! 30b3132e5e3c1c5239f84ce349f49ace63ec1951. The live owner can add account
//! subscriptions; signing and order submission are handled by live_client.
use crate::{config::Config, engine::InputKind, market::Universe};
use anyhow::{bail, Context, Result};
use futures_util::{SinkExt, StreamExt};
use serde_json::json;
use std::{
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tokio::{
    sync::mpsc,
    time::{interval, sleep, timeout},
};
use tokio_tungstenite::{connect_async, tungstenite::Message};
pub const INFO: &str = "https://api.hyperliquid.xyz/info";
pub const WS: &str = "wss://api.hyperliquid.xyz/ws";

fn subscription(coin: &str, kind: &str, fast: bool) -> String {
    let mut sub = json!({"type":kind,"coin":coin});
    if kind == "l2Book" {
        sub["fast"] = fast.into();
    }
    json!({"method":"subscribe","subscription":sub}).to_string()
}

pub struct Clock(Instant);
impl Clock {
    pub fn new() -> Self {
        Self(Instant::now())
    }
    pub fn ns(&self) -> u64 {
        self.0.elapsed().as_nanos() as u64
    }
    pub fn deadline(&self, ns: u64) -> tokio::time::Instant {
        (self.0 + Duration::from_nanos(ns)).into()
    }
}
impl Default for Clock {
    fn default() -> Self {
        Self::new()
    }
}
pub fn utc_ns() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as u64
}
pub struct Wire {
    pub generation: u64,
    pub receipt_ns: u64,
    pub utc_ns: u64,
    pub event: InputKind,
}
pub async fn discover(cfg: &Config) -> Result<(Universe, serde_json::Value)> {
    let raw = reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .build()?
        .post(INFO)
        .json(&json!({"type":"spotMetaAndAssetCtxs"}))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    Ok((Universe::parse(&raw, cfg)?, raw))
}
fn send(tx: &mpsc::Sender<Wire>, clock: &Clock, generation: u64, event: InputKind) -> Result<()> {
    tx.try_send(Wire {
        generation,
        receipt_ns: clock.ns(),
        utc_ns: utc_ns(),
        event,
    })
    .context("ingestion overflow/closed: recording segment incomplete")
}
async fn reconnect_requested(rx: &mut mpsc::Receiver<u64>, generation: u64) -> Result<()> {
    loop {
        let request = rx.recv().await.context("reconnect control closed")?;
        if request == generation {
            return Ok(());
        }
    }
}
pub async fn follow(
    coins: Vec<String>,
    cfg: Config,
    tx: mpsc::Sender<Wire>,
    clock: Arc<Clock>,
    reconnect: mpsc::Receiver<u64>,
    force_after: Option<u64>,
) -> Result<()> {
    follow_extra(coins, cfg, tx, clock, reconnect, force_after, vec![]).await
}
pub async fn follow_extra(
    coins: Vec<String>,
    cfg: Config,
    tx: mpsc::Sender<Wire>,
    clock: Arc<Clock>,
    mut reconnect: mpsc::Receiver<u64>,
    force_after: Option<u64>,
    extra: Vec<String>,
) -> Result<()> {
    let mut generation = 0;
    let mut backoff = 1;
    let mut forced = false;
    loop {
        generation += 1;
        let began = Instant::now();
        let socket = timeout(Duration::from_secs(10), connect_async(WS)).await;
        let result: Result<()> = match socket {
            Ok(Ok((ws, _))) => {
                send(&tx, &clock, generation, InputKind::Open)?;
                let (mut write, mut read) = ws.split();
                let fast = cfg.l2_fast;
                let mut messages = coins.iter().flat_map(|coin| {
                    ["bbo", "l2Book"].map(move |kind| subscription(coin, kind, fast))
                }).chain(extra.iter().cloned());
                let mut next = messages.next();
                let mut pace = interval(Duration::from_millis(100));
                let mut ping = interval(Duration::from_secs(20));
                let mut watchdog = interval(Duration::from_secs(1));
                let mut last = Instant::now();
                loop {
                    tokio::select! {
                        msg=read.next()=>{
                            // Receipt is stamped immediately after the application receives the frame.
                            let received=clock.ns();let utc=utc_ns();last=Instant::now();
                            match msg {
                                Some(Ok(Message::Text(text)))=>{tx.try_send(Wire{generation,receipt_ns:received,utc_ns:utc,event:InputKind::Frame{text}}).context("ingestion overflow: incomplete segment")?;},
                                Some(Ok(Message::Ping(p)))=>{match timeout(Duration::from_secs(5),write.send(Message::Pong(p))).await{Ok(Ok(()))=>{},other=>break Err(anyhow::anyhow!("pong write failed: {other:?}"))}},
                                Some(Ok(Message::Close(_)))|None=>break Err(anyhow::anyhow!("server closed stream")),
                                Some(Err(e))=>break Err(e.into()),_=>{}
                            }
                        },
                        _=pace.tick(),if next.is_some()=>{let m=next.take().unwrap();match timeout(Duration::from_secs(5),write.send(Message::Text(m))).await{Ok(Ok(()))=>{},other=>break Err(anyhow::anyhow!("subscription write failed: {other:?}"))}next=messages.next();},
                        _=ping.tick()=>{match timeout(Duration::from_secs(5),write.send(Message::Text(r#"{"method":"ping"}"#.into()))).await{Ok(Ok(()))=>{},other=>break Err(anyhow::anyhow!("ping write failed: {other:?}"))}},
                        _=watchdog.tick()=>{if last.elapsed()>Duration::from_secs(cfg.socket_timeout_secs){break Err(anyhow::anyhow!("socket silence timeout"))}if !forced && force_after.is_some_and(|s|clock.ns()>=s*1_000_000_000){forced=true;break Err(anyhow::anyhow!("controlled smoke reconnect"));}},
                        request=reconnect_requested(&mut reconnect,generation)=>{request?;break Err(anyhow::anyhow!("invalid frame forced reconnect"));},
                    }
                }
            }
            Ok(Err(e)) => Err(e.into()),
            Err(e) => Err(e.into()),
        };
        let reason = result
            .err()
            .map(|e| e.to_string())
            .unwrap_or_else(|| "disconnected".into());
        eprintln!("Hyperliquid connection {generation}: {reason}");
        send(&tx, &clock, generation, InputKind::Close { reason })?;
        if tx.is_closed() {
            bail!("evaluator closed")
        }
        if began.elapsed() > Duration::from_secs(60) {
            backoff = 1;
        }
        sleep(Duration::from_secs(backoff)).await;
        backoff = (backoff * 2).min(30);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn obsolete_reconnect_request_cannot_close_new_generation() {
        let (tx, mut rx) = mpsc::channel(1);
        tx.try_send(1).unwrap();
        assert!(
            timeout(Duration::from_millis(5), reconnect_requested(&mut rx, 2))
                .await
                .is_err()
        );
        tx.try_send(2).unwrap();
        reconnect_requested(&mut rx, 2).await.unwrap();
        assert!(
            timeout(Duration::from_millis(5), reconnect_requested(&mut rx, 3))
                .await
                .is_err()
        );
    }
    #[test]
    fn depth_mode_is_explicit_and_legacy_config_remains_slow() {
        let legacy: Config = serde_json::from_str("{}").unwrap();
        assert!(!legacy.l2_fast);
        for fast in [true, false] {
            let d: serde_json::Value =
                serde_json::from_str(&subscription("@107", "l2Book", fast)).unwrap();
            assert_eq!(
                d["subscription"],
                json!({"coin":"@107","type":"l2Book","fast":fast})
            );
            let b: serde_json::Value =
                serde_json::from_str(&subscription("@107", "bbo", fast)).unwrap();
            assert!(b["subscription"].get("fast").is_none());
        }
    }
    #[test]
    fn overflow_returns_error_instead_of_silently_dropping_market_data() {
        let (tx, _rx) = mpsc::channel(1);
        let clock = Clock::new();
        send(&tx, &clock, 1, InputKind::Open).unwrap();
        assert!(send(
            &tx,
            &clock,
            1,
            InputKind::Frame {
                text: "book".into()
            }
        )
        .unwrap_err()
        .to_string()
        .contains("overflow"));
    }
}
