//! Independent public-feed diagnostic; does not call the screener's collector or engine.
//! docker compose run --rm check cargo run --release --locked --example depth_probe -- RUN_DIR OUTPUT_DIR SECONDS_PER_MODE
use anyhow::{bail, ensure, Context, Result};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::BTreeSet,
    fs::{self, File},
    io::{BufRead, BufReader, BufWriter, Write},
    path::Path,
    time::{Instant, SystemTime, UNIX_EPOCH},
};
use tokio::time::{interval, sleep, timeout, Duration};
use tokio_tungstenite::{connect_async, tungstenite::Message};

const COIN: &str = "@107"; // HYPE/USDC, checked against the running manifest below.
#[derive(Serialize, Deserialize)]
struct Frame {
    receipt_ns: u64,
    utc_ns: u64,
    text: String,
}
fn utc() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos() as u64
}
fn write_json(path: &Path, value: &impl Serialize) -> Result<()> {
    let mut f = BufWriter::new(File::create(path)?);
    serde_json::to_writer_pretty(&mut f, value)?;
    f.write_all(b"\n")?;
    f.flush()?;
    f.get_ref().sync_all()?;
    Ok(())
}
fn percentiles(mut values: Vec<f64>) -> Value {
    if values.is_empty() {
        return Value::Null;
    }
    values.sort_by(f64::total_cmp);
    let q = |p: f64| values[((values.len() as f64 * p).ceil() as usize).saturating_sub(1)];
    json!({"n":values.len(),"p50":q(0.5),"p95":q(0.95),"p99":q(0.99),"max":values.last()})
}
fn observations(
    frames: &[Frame],
    channel: &str,
    lo: u64,
    hi: u64,
) -> Vec<(u64, u64, usize, usize)> {
    frames
        .iter()
        .filter_map(|r| {
            let f: Value = serde_json::from_str(&r.text).ok()?;
            let d = &f["data"];
            let ex = d["time"].as_u64()?;
            if f["channel"] != channel || d["coin"] != COIN || ex < lo || ex > hi {
                return None;
            }
            Some((
                r.receipt_ns,
                ex,
                d["levels"][0].as_array().map(Vec::len).unwrap_or(0),
                d["levels"][1].as_array().map(Vec::len).unwrap_or(0),
            ))
        })
        .collect()
}
fn stats(rows: &[(u64, u64, usize, usize)]) -> Value {
    json!({"frames":rows.len(),"receipt_gap_ms":percentiles(rows.windows(2).map(|w|(w[1].0 as f64-w[0].0 as f64)/1e6).collect()),
        "exchange_gap_ms":percentiles(rows.windows(2).map(|w|w[1].1 as f64-w[0].1 as f64).collect()),
        "max_levels_per_side":rows.iter().map(|r|r.2.max(r.3)).max(),
        "nonincreasing_exchange_times":rows.windows(2).filter(|w|w[1].1<=w[0].1).count()})
}
fn load_live(dir: &Path) -> Result<Vec<Frame>> {
    let mut files = fs::read_dir(dir)?
        .map(|e| e.map(|e| e.path()))
        .collect::<std::io::Result<Vec<_>>>()?;
    files.retain(|p| {
        p.file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("events-")
            && p.extension().is_some_and(|x| x == "jsonl")
    });
    files.sort();
    let mut frames = Vec::new();
    for p in files {
        let mut reader = BufReader::new(File::open(p)?);
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line)? == 0 {
                break;
            }
            if !line.ends_with('\n') {
                break;
            } // The running writer may have a partial final line.
            let r: Value = serde_json::from_str(&line)?;
            if let Some(text) = r["input"]["event"]["text"].as_str() {
                let f: Value = serde_json::from_str(text)?;
                if f["data"]["coin"] == COIN {
                    frames.push(Frame {
                        receipt_ns: r["input"]["receipt_ns"].as_u64().context("receipt")?,
                        utc_ns: r["input"]["receipt_utc_ns"].as_u64().context("utc")?,
                        text: text.into(),
                    });
                }
            }
        }
    }
    Ok(frames)
}
async fn capture(fast: bool, seconds: u64, output: &Path) -> Result<Vec<Frame>> {
    let (mut ws, _) = timeout(
        Duration::from_secs(15),
        connect_async("wss://api.hyperliquid.xyz/ws"),
    )
    .await??;
    let origin = Instant::now();
    let began_utc = utc();
    let mut subscriptions = vec![
        json!({"type":"bbo","coin":COIN}),
        json!({"type":"l2Book","coin":COIN}),
    ];
    if fast {
        subscriptions[1]["fast"] = true.into();
    }
    for s in &subscriptions {
        timeout(
            Duration::from_secs(5),
            ws.send(Message::Text(
                json!({"method":"subscribe","subscription":s}).to_string(),
            )),
        )
        .await??;
    }
    let mut frames = Vec::new();
    let mut ping = interval(Duration::from_secs(20));
    let mut progress = interval(Duration::from_secs(30));
    let deadline = sleep(Duration::from_secs(seconds));
    tokio::pin!(deadline);
    loop {
        tokio::select! {
            _=&mut deadline=>break,
            _=ping.tick()=>{timeout(Duration::from_secs(5),ws.send(Message::Text(r#"{"method":"ping"}"#.into()))).await??;},
            _=progress.tick()=>eprintln!("mode={} elapsed={}s frames={}",if fast{"fast"}else{"default"},origin.elapsed().as_secs(),frames.len()),
            next=ws.next()=>{let receipt_ns=origin.elapsed().as_nanos() as u64;let utc_ns=utc();match next.context("probe disconnected")?? {
                Message::Text(text)=>{let v:Value=serde_json::from_str(&text)?;if v["channel"]=="error" {bail!("subscription error: {text}");}frames.push(Frame{receipt_ns,utc_ns,text});},
                Message::Ping(p)=>{timeout(Duration::from_secs(5),ws.send(Message::Pong(p))).await??;},
                Message::Close(c)=>bail!("probe closed: {c:?}"),_=>{}
            }}
        }
    }
    let _ = timeout(Duration::from_secs(2), ws.close(None)).await;
    let mode = if fast { "fast" } else { "default" };
    write_json(
        &output.join(format!("{mode}-manifest.json")),
        &json!({"coin":COIN,"seconds":seconds,"started_utc_ns":began_utc,"ended_utc_ns":utc(),"subscriptions":subscriptions,"cpu_quota":fs::read_to_string("/sys/fs/cgroup/cpu.max").unwrap_or_default(),"note":"Independent direct tungstenite receiver; no engine, hot/cold queues, or per-frame disk writes."}),
    )?;
    let mut file = BufWriter::new(File::create(output.join(format!("{mode}-frames.jsonl")))?);
    for f in &frames {
        serde_json::to_writer(&mut file, f)?;
        file.write_all(b"\n")?;
    }
    file.flush()?;
    file.get_ref().sync_all()?;
    Ok(frames)
}
#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().collect();
    ensure!(
        args.len() == 4,
        "usage: depth_probe RUN_DIR OUTPUT_DIR SECONDS_PER_MODE"
    );
    let live = Path::new(&args[1]);
    let out = Path::new(&args[2]);
    let seconds: u64 = args[3].parse()?;
    ensure!(
        (30..=600).contains(&seconds),
        "duration must be 30..600 seconds"
    );
    fs::create_dir(out)?;
    let manifest: Value =
        serde_json::from_reader(BufReader::new(File::open(live.join("manifest.json"))?))?;
    let market = manifest["universe"]["markets"]
        .as_array()
        .context("markets")?
        .iter()
        .find(|m| m["coin"] == COIN)
        .context("spot coin missing")?;
    let base = market["base"].as_u64().context("base")?.to_string();
    ensure!(
        manifest["universe"]["tokens"][&base]["name"] == "HYPE" && market["quote"] == 0,
        "expected HYPE/USDC identity"
    );
    let mut reports = Vec::new();
    for fast in [false, true] {
        let frames = capture(fast, seconds, out).await?;
        // Exclude initial subscription snapshots. Compare equal exchange-time windows;
        // absolute UTC clocks across processes need not be synchronized.
        let all = observations(&frames, "bbo", 0, u64::MAX);
        ensure!(all.len() > 2, "insufficient BBO data");
        let lo = all[0].1 + 5000;
        let hi = all.last().unwrap().1;
        sleep(Duration::from_secs(2)).await; // Let the live recorder flush its same-window tail.
        let live_frames = load_live(live)?;
        let probe_depth = observations(&frames, "l2Book", lo, hi);
        let live_depth = observations(&live_frames, "l2Book", lo, hi);
        let pa: BTreeSet<_> = probe_depth.iter().map(|r| r.1).collect();
        let la: BTreeSet<_> = live_depth.iter().map(|r| r.1).collect();
        let pb: BTreeSet<_> = observations(&frames, "bbo", lo, hi)
            .iter()
            .map(|r| r.1)
            .collect();
        let lb: BTreeSet<_> = observations(&live_frames, "bbo", lo, hi)
            .iter()
            .map(|r| r.1)
            .collect();
        let r = json!({"mode":if fast{"fast"}else{"default"},"exchange_window_ms":[lo,hi],"probe_bbo":stats(&observations(&frames,"bbo",lo,hi)),"probe_depth":stats(&probe_depth),"live_bbo":stats(&observations(&live_frames,"bbo",lo,hi)),"live_depth":stats(&live_depth),"shared_depth_exchange_timestamps":pa.intersection(&la).count(),"probe_unique_depth_timestamps":pa.len(),"live_unique_depth_timestamps":la.len(),"shared_bbo_exchange_timestamps":pb.intersection(&lb).count(),"acks":frames.iter().filter_map(|f|serde_json::from_str::<Value>(&f.text).ok()).filter(|f|f["channel"]=="subscriptionResponse").collect::<Vec<_>>()});
        println!("{}", serde_json::to_string_pretty(&r)?);
        reports.push(r);
        write_json(&out.join("comparison.json"), &reports)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn independent_clocks_and_common_window() {
        let f = |n, e| Frame {
            receipt_ns: n,
            utc_ns: 0,
            text: json!({"channel":"l2Book","data":{"coin":COIN,"time":e,"levels":[[],[]]}})
                .to_string(),
        };
        let rows = observations(
            &[f(0, 10), f(5_400_000_000, 5410), f(10_800_000_000, 10810)],
            "l2Book",
            5000,
            20000,
        );
        let s = stats(&rows);
        assert_eq!(s["frames"], 2);
        assert_eq!(s["receipt_gap_ms"]["p50"], 5400.0);
        assert_eq!(s["exchange_gap_ms"]["p50"], 5400.0);
    }
}
