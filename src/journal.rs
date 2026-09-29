use crate::{
    config::Config,
    engine::{Diagnostic, Engine, Input, InputKind},
    market::Universe,
    paper::Account,
};
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    fs::{self, File, OpenOptions},
    io::{BufRead, BufReader, BufWriter, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc,
    },
    thread,
    time::{Duration, Instant},
};

#[derive(Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub format: u32,
    pub run_id: String,
    pub created_utc_ns: u64,
    pub config: Config,
    pub universe: Universe,
    pub initial_accounts: Option<Vec<Account>>,
    pub source_version: String,
    pub cpu_quota: String,
    #[serde(default)]
    pub raw_metadata: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paper_epoch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub predecessor_run_id: Option<String>,
}
#[derive(Serialize, Deserialize)]
pub struct Record {
    pub input: Input,
    pub completed_ns: u64,
    pub events: Vec<Value>,
}
enum WriteItem {
    Record(Record),
    Summary(Value),
    Finish(Value),
}
pub struct Journal {
    tx: mpsc::SyncSender<WriteItem>,
    failed: Arc<AtomicBool>,
    handle: Option<thread::JoinHandle<Result<()>>>,
    pub dir: PathBuf,
}
pub fn size(path: &Path) -> Result<u64> {
    let mut n = 0;
    for e in fs::read_dir(path)? {
        let e = e?;
        let t = e.file_type()?;
        if t.is_dir() {
            n += size(&e.path())?
        } else if t.is_file() {
            n += e.metadata()?.len();
        }
    }
    Ok(n)
}
pub fn write_json(path: &Path, value: &impl Serialize) -> Result<()> {
    let tmp = path.with_extension("tmp");
    let mut f = BufWriter::new(File::create(&tmp)?);
    serde_json::to_writer_pretty(&mut f, value)?;
    f.write_all(b"\n")?;
    f.flush()?;
    f.get_ref().sync_all()?;
    fs::rename(&tmp, path)?;
    Ok(())
}
impl Journal {
    pub fn open(root: &Path, manifest: Manifest) -> Result<Self> {
        fs::create_dir_all(root)?;
        let used = size(root)?;
        if used + serde_json::to_vec_pretty(&manifest)?.len() as u64 + 1025
            >= manifest.config.record_limit_bytes
        {
            fs::write(
                root.join("RECORDING_LIMIT_REACHED"),
                "Insufficient recording space for a new segment; existing evidence preserved.\n",
            )?;
            anyhow::bail!("recording limit reached before manifest");
        }
        let dir = root.join(&manifest.run_id);
        fs::create_dir(&dir)?;
        write_json(&dir.join("manifest.json"), &manifest)?;
        // UTC can step backwards. Recovery follows a durable pointer, never a
        // timestamp-sorted directory after migration from older recordings.
        let current = root.join("current.json");
        let old_pointer = fs::metadata(&current).map(|m| m.len()).unwrap_or(0);
        write_json(&current, &manifest.run_id)?;
        let used =
            used + fs::metadata(dir.join("manifest.json"))?.len() + fs::metadata(&current)?.len()
                - old_pointer;
        let (tx, rx) = mpsc::sync_channel(manifest.config.channel_capacity);
        let failed = Arc::new(AtomicBool::new(false));
        let flag = failed.clone();
        let destination = dir.clone();
        let handle = thread::spawn(move || {
            let result = writer(&destination, &manifest, used, rx);
            if let Err(e) = &result {
                eprintln!("recording failed: {e:#}");
                if e.to_string().contains("recording limit") {
                    let _=fs::write(destination.parent().unwrap().join("RECORDING_LIMIT_REACHED"),"Recording limit reached; preserve evidence. Resolve storage before restarting.\n");
                }
                flag.store(true, Ordering::Release);
            }
            result
        });
        Ok(Self {
            tx,
            failed,
            handle: Some(handle),
            dir,
        })
    }
    fn send(&self, item: WriteItem) -> Result<()> {
        ensure!(
            !self.failed.load(Ordering::Acquire),
            "recording worker failed"
        );
        self.tx
            .try_send(item)
            .context("recording overflow/disconnected: incomplete segment")
    }
    pub fn record(&self, r: Record) -> Result<()> {
        self.send(WriteItem::Record(r))
    }
    pub fn summary(&self, v: Value) -> Result<()> {
        self.send(WriteItem::Summary(v))
    }
    pub fn finish(mut self, v: Value) -> Result<()> {
        let sent = self
            .tx
            .send(WriteItem::Finish(v))
            .context("writer failed before shutdown");
        let joined = self
            .handle
            .take()
            .unwrap()
            .join()
            .map_err(|_| anyhow::anyhow!("writer panicked"))?;
        sent?;
        joined
    }
}
fn writer(
    dir: &Path,
    manifest: &Manifest,
    mut total: u64,
    rx: mpsc::Receiver<WriteItem>,
) -> Result<()> {
    let cfg = &manifest.config;
    let mut terminal = None;
    let mut part = 0;
    let mut bytes = 0;
    let mut first_ns = 0;
    let mut flushed = Instant::now();
    let open = |n| -> Result<BufWriter<File>> {
        Ok(BufWriter::new(
            OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(dir.join(format!("events-{n:05}.jsonl")))?,
        ))
    };
    let mut out = open(part)?;
    loop {
        match rx.recv_timeout(Duration::from_secs(1)) {
            Ok(WriteItem::Record(r)) => {
                let mut line = serde_json::to_vec(&r)?;
                line.push(b'\n');
                ensure!(
                    total + line.len() as u64 + 1024 <= cfg.record_limit_bytes,
                    "recording limit reached; segment incomplete"
                );
                if bytes > 0
                    && (bytes + line.len() as u64 > cfg.rotate_bytes
                        || r.input.process_ns.saturating_sub(first_ns)
                            >= cfg.rotate_secs * 1_000_000_000)
                {
                    out.flush()?;
                    out.get_ref().sync_all()?;
                    part += 1;
                    out = open(part)?;
                    bytes = 0;
                    first_ns = r.input.process_ns;
                }
                out.write_all(&line)?;
                let has_events = !r.events.is_empty();
                terminal = if matches!(r.input.event, InputKind::Stop { .. }) {
                    Some(r)
                } else {
                    None
                };
                bytes += line.len() as u64;
                total += line.len() as u64;
                if has_events || flushed.elapsed() >= Duration::from_secs(1) {
                    out.flush()?;
                    out.get_ref().sync_data()?;
                    flushed = Instant::now();
                }
            }
            Ok(WriteItem::Summary(mut v)) => {
                v["recording_total_bytes"] = total.into();
                v["recording_parts"] = (part + 1).into();
                out.flush()?;
                out.get_ref().sync_data()?;
                let p = dir.join("status.json");
                let old = fs::metadata(&p).map(|m| m.len()).unwrap_or(0);
                let bytes = serde_json::to_vec_pretty(&v)?.len() as u64 + 1;
                ensure!(
                    total + bytes + 1024 < cfg.record_limit_bytes,
                    "recording limit reached before summary"
                );
                write_json(&p, &v)?;
                total = total - old + bytes;
            }
            Ok(WriteItem::Finish(v)) => {
                out.flush()?;
                out.get_ref().sync_all()?;
                if manifest.format >= 3 && v["recording_complete"] == true {
                    validate_checkpoint(
                        manifest,
                        &v,
                        terminal
                            .as_ref()
                            .context("clean shutdown lacks Stop record")?,
                    )?;
                }
                ensure!(
                    total + serde_json::to_vec_pretty(&v)?.len() as u64 + 1025
                        < cfg.record_limit_bytes,
                    "recording limit reached before final report"
                );
                write_json(&dir.join("final.json"), &v)?;
                return Ok(());
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                out.flush()?;
                out.get_ref().sync_data()?;
                flushed = Instant::now();
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                out.flush()?;
                out.get_ref().sync_all()?;
                anyhow::bail!("owner exited without clean journal close")
            }
        }
    }
}
pub fn replay(dir: &Path, latencies: Option<Vec<u64>>, verify: bool) -> Result<Engine> {
    replay_with_ages(dir, latencies, verify, None, None)
}
pub fn replay_with_ages(
    dir: &Path,
    latencies: Option<Vec<u64>>,
    verify: bool,
    quote_ms: Option<u64>,
    depth_ms: Option<u64>,
) -> Result<Engine> {
    replay_options(dir, latencies, verify, quote_ms, depth_ms, None)
}
pub fn replay_options(
    dir: &Path,
    latencies: Option<Vec<u64>>,
    verify: bool,
    quote_ms: Option<u64>,
    depth_ms: Option<u64>,
    diagnostic: Option<Diagnostic>,
) -> Result<Engine> {
    replay_model(dir, latencies, verify, quote_ms, depth_ms, diagnostic, None)
}
pub fn replay_model(
    dir: &Path,
    latencies: Option<Vec<u64>>,
    verify: bool,
    quote_ms: Option<u64>,
    depth_ms: Option<u64>,
    diagnostic: Option<Diagnostic>,
    model: Option<u32>,
) -> Result<Engine> {
    ensure!(
        model.is_none_or(|m| matches!(m, 3 | 4)),
        "execution model must be 3 or 4"
    );
    let alternative = latencies.is_some()
        || quote_ms.is_some()
        || depth_ms.is_some()
        || diagnostic.is_some()
        || model.is_some();
    ensure!(
        !verify || !alternative,
        "verification requires recorded assumptions and no forced entry"
    );
    let m: Manifest =
        serde_json::from_reader(BufReader::new(File::open(dir.join("manifest.json"))?))?;
    ensure!(matches!(m.format, 1..=4), "unsupported recording format");
    let mut cfg = m.config;
    if let Some(ms) = quote_ms {
        cfg.quote_age_ms = ms;
    }
    if let Some(ms) = depth_ms {
        cfg.depth_age_ms = ms;
    }
    if let Some(ls) = latencies {
        cfg.latency_ms = ls;
    }
    let initial = if alternative {
        None
    } else {
        m.initial_accounts
    };
    let mut engine = Engine::new(cfg, m.universe, initial)?;
    engine.legacy_shadow = m.format == 1 && !alternative;
    engine.model_version = if alternative {
        model.unwrap_or(4)
    } else {
        m.format
    };
    if !alternative {
        engine.paper_epoch = m.paper_epoch;
        engine.predecessor_run_id = m.predecessor_run_id;
    }
    if let Some(d) = &diagnostic {
        ensure!(
            engine.routes.iter().any(|r| r.id == d.route),
            "unknown diagnostic route"
        );
        ensure!(
            d.amount > rust_decimal::Decimal::ZERO && d.amount <= engine.config.starting_usdc,
            "invalid diagnostic amount"
        );
    }
    engine.diagnostic = diagnostic;
    let mut hot = crate::hot::Core::new(&engine.universe, &engine.routes, &engine.config);
    let mut rates = Vec::with_capacity(engine.routes.len());
    let files = event_files(dir)?;
    let mut available = 0;
    for (fi, path) in files.iter().enumerate() {
        let mut reader = BufReader::new(File::open(path)?);
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line)? == 0 {
                break;
            }
            if !line.ends_with('\n') {
                ensure!(fi + 1 == files.len(), "truncated middle segment");
                eprintln!("incomplete final journal line ignored; recovery uses durable prefix");
                break;
            }
            let mut rec: Record = serde_json::from_str(&line).context("corrupt journal record")?;
            if alternative {
                while let Some(at) = engine
                    .deadline()
                    .filter(|d| (*d).max(available) <= rec.input.process_ns)
                {
                    let now = at.max(available);
                    let tick = Input {
                        sequence: engine.last_sequence + 1,
                        generation: engine.generation,
                        receipt_ns: now,
                        receipt_utc_ns: 0,
                        process_ns: now,
                        hot_started_ns: 0,
                        hot_done_ns: 0,
                        event: InputKind::Clock,
                    };
                    let mut events = engine.step(&tick)?;
                    engine.complete_step(&tick, now, &mut events)?;
                    available = now;
                }
                rec.input.sequence = engine.last_sequence + 1;
            }
            let mut events = if rec.input.hot_done_ns > 0 {
                let signal = crate::hot::normalize(
                    &rec.input.event,
                    &engine.universe,
                    rec.input.generation,
                    rec.input.receipt_ns,
                );
                hot.evaluate(signal, rec.input.hot_started_ns, &mut rates);
                engine.step_hot(&rec.input, &rates)?
            } else {
                engine.step(&rec.input)?
            };
            engine.complete_step(&rec.input, rec.completed_ns, &mut events)?;
            available = rec.completed_ns;
            if verify {
                if let Some(c) = rec
                    .events
                    .iter()
                    .find(|v| v["type"] == "account_checkpoint")
                {
                    ensure!(
                        *c == checkpoint(&m.run_id, &engine),
                        "terminal account checkpoint differs from replay"
                    );
                    events.push(c.clone());
                }
                ensure!(
                    events == rec.events,
                    "replay divergence at sequence {}",
                    rec.input.sequence
                );
            }
            if matches!(rec.input.event, crate::engine::InputKind::Frame { .. }) {
                engine
                    .stats
                    .receipt_to_decision
                    .record(rec.completed_ns.saturating_sub(rec.input.receipt_ns));
                engine
                    .stats
                    .processing
                    .record(rec.completed_ns.saturating_sub(rec.input.process_ns));
            }
        }
    }
    Ok(engine)
}

fn event_files(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    for e in fs::read_dir(dir)? {
        let p = e?.path();
        if p.file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("events-")
            && p.extension().is_some_and(|e| e == "jsonl")
        {
            files.push(p);
        }
    }
    files.sort();
    for (n, p) in files.iter().enumerate() {
        ensure!(
            p.file_name().unwrap() == format!("events-{n:05}.jsonl").as_str(),
            "missing journal part"
        );
    }
    Ok(files)
}

pub fn checkpoint(run_id: &str, e: &Engine) -> Value {
    let mut value = serde_json::json!({"type":"account_checkpoint","run_id":run_id,"accounting_version":e.model_version,"sequence":e.last_sequence,"universe":e.universe,"accounts":e.accounts});
    if e.model_version >= 4 {
        value["paper_epoch"] = serde_json::json!(e.paper_epoch);
        value["predecessor_run_id"] = serde_json::json!(e.predecessor_run_id);
    }
    value
}
fn validate_checkpoint(m: &Manifest, report: &Value, terminal: &Record) -> Result<Vec<Account>> {
    let c = &report["recovery"];
    ensure!(
        report["recording_complete"] == true
            && matches!(terminal.input.event, InputKind::Stop { .. }),
        "unclean checkpoint"
    );
    ensure!(
        c["run_id"] == m.run_id
            && c["accounting_version"] == m.format
            && c["sequence"].as_u64() == Some(terminal.input.sequence),
        "checkpoint identity mismatch"
    );
    ensure!(
        c["universe"] == serde_json::to_value(&m.universe)?
            && c["accounts"] == report["accounts"]
            && terminal.events.last() == Some(c),
        "checkpoint account/metadata mismatch"
    );
    if m.format >= 4 {
        ensure!(
            c["paper_epoch"] == serde_json::json!(m.paper_epoch)
                && c["predecessor_run_id"] == serde_json::json!(m.predecessor_run_id)
                && c["paper_epoch"] == report["paper_epoch"]
                && c["predecessor_run_id"] == report["predecessor_run_id"],
            "checkpoint epoch mismatch"
        );
    }
    Ok(serde_json::from_value(c["accounts"].clone())?)
}
fn last_record(path: &Path) -> Result<Record> {
    let mut f = File::open(path)?;
    let mut pos = f.metadata()?.len();
    let mut tail = Vec::new();
    loop {
        let n = pos.min(65536) as usize;
        pos -= n as u64;
        f.seek(SeekFrom::Start(pos))?;
        let mut buf = vec![0; n];
        f.read_exact(&mut buf)?;
        buf.extend(tail);
        tail = buf;
        ensure!(tail.last() == Some(&b'\n'), "incomplete terminal record");
        if let Some(i) = tail[..tail.len() - 1].iter().rposition(|b| *b == b'\n') {
            return Ok(serde_json::from_slice(&tail[i + 1..])?);
        }
        if pos == 0 {
            return Ok(serde_json::from_slice(&tail)?);
        }
        ensure!(
            tail.len() <= 64 * 1024 * 1024,
            "terminal checkpoint exceeds 64 MiB; use explicit recovery"
        );
    }
}
pub fn recover(dir: &Path) -> Result<(Universe, Vec<Account>)> {
    let m: Manifest =
        serde_json::from_reader(BufReader::new(File::open(dir.join("manifest.json"))?))?;
    ensure!(matches!(m.format, 1..=4), "unsupported recovery format");
    if m.format >= 3 && dir.join("final.json").exists() {
        let report: Value =
            serde_json::from_reader(BufReader::new(File::open(dir.join("final.json"))?))?;
        if report["recording_complete"] == true {
            let files = event_files(dir)?;
            let last = last_record(files.last().context("missing checkpoint journal")?)?;
            return Ok((m.universe.clone(), validate_checkpoint(&m, &report, &last)?));
        }
    }
    let prior = replay(dir, None, true)?;
    Ok((prior.universe, prior.accounts))
}
/// Explicit experiment funding only. Same-ID restarts recover rather than refund.
pub fn paper_epoch(
    root: &Path,
    prior: Option<&Path>,
    requested: Option<&str>,
) -> Result<(Option<String>, Option<String>, bool)> {
    let previous: Option<Manifest> = prior
        .map(|p| -> Result<_> {
            Ok(serde_json::from_reader(BufReader::new(File::open(
                p.join("manifest.json"),
            )?))?)
        })
        .transpose()?;
    let Some(id) = requested else {
        return Ok((
            previous.as_ref().and_then(|m| m.paper_epoch.clone()),
            previous.and_then(|m| m.predecessor_run_id),
            false,
        ));
    };
    ensure!(
        !id.is_empty()
            && id.len() <= 64
            && id
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_'),
        "invalid paper epoch ID"
    );
    if previous
        .as_ref()
        .is_some_and(|m| m.paper_epoch.as_deref() == Some(id))
    {
        return Ok((Some(id.into()), previous.unwrap().predecessor_run_id, false));
    }
    if root.exists() {
        for entry in fs::read_dir(root)? {
            let path = entry?.path().join("manifest.json");
            if path.is_file() {
                let m: Manifest = serde_json::from_reader(BufReader::new(File::open(path)?))?;
                ensure!(
                    m.paper_epoch.as_deref() != Some(id),
                    "paper epoch ID already used; refusing new funding"
                );
            }
        }
    }
    if let Some(p) = prior {
        let final_report: Value = serde_json::from_reader(BufReader::new(
            File::open(p.join("final.json"))
                .context("new epoch requires a durable predecessor checkpoint")?,
        ))?;
        ensure!(
            previous.as_ref().unwrap().format >= 3 && final_report["recording_complete"] == true,
            "new epoch requires a clean predecessor checkpoint"
        );
        recover(p)?;
    }
    Ok((Some(id.into()), previous.map(|m| m.run_id), true))
}
pub fn latest(root: &Path) -> Result<Option<PathBuf>> {
    if !root.exists() {
        return Ok(None);
    }
    let current = root.join("current.json");
    if current.exists() {
        let id: String = serde_json::from_str(&fs::read_to_string(current)?)?;
        ensure!(
            id.starts_with("run-") && !id.contains(['/', '\\']),
            "invalid current run pointer"
        );
        let dir = root.join(id);
        ensure!(
            dir.join("manifest.json").is_file(),
            "current run manifest missing; refusing older balances"
        );
        return Ok(Some(dir));
    }
    let mut paths = Vec::new();
    for e in fs::read_dir(root)? {
        let e = e?;
        if e.file_type()?.is_dir()
            && e.file_name().to_string_lossy().starts_with("run-")
            && e.path().join("manifest.json").exists()
        {
            paths.push(e.path())
        }
    }
    paths.sort();
    Ok(paths.pop())
}
