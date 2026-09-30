use anyhow::{bail, Context, Result};
use bellman_arb::{
    config::Config,
    engine::{Diagnostic, Engine, Input, InputKind},
    hyperliquid::{self, Clock},
    journal::{self, Journal, Manifest, Record},
};
use serde_json::json;
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};
use tokio::sync::mpsc;

fn option(args: &[String], key: &str) -> Option<String> {
    args.windows(2).find(|x| x[0] == key).map(|x| x[1].clone())
}
async fn shutdown() {
    #[cfg(unix)]
    {
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("install SIGTERM handler");
        tokio::select! {_=tokio::signal::ctrl_c()=>{},_=term.recv()=>{}}
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}
#[tokio::main(worker_threads = 2)]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let continuous = args.first().map(String::as_str).unwrap_or("run") == "run"
        && option(&args, "--duration").is_none();
    match execute(&args).await {
        Err(error) if continuous => {
            let reason = format!("{error:#}");
            eprintln!("BLOCKED: {reason}. Preserve recordings, resolve the cause, then restart this service.");
            let root = PathBuf::from(option(&args, "--runs").unwrap_or_else(|| "runs".into()));
            let _ = std::fs::create_dir_all(&root);
            if let Err(e) = journal::write_json(
                &root.join("blocked.json"),
                &json!({"state":"blocked","reason":reason,"utc_ns":hyperliquid::utc_ns()}),
            ) {
                eprintln!("Cannot persist blocked status: {e:#}");
            }
            shutdown().await;
            Ok(())
        }
        result => result,
    }
}
async fn execute(args: &[String]) -> Result<()> {
    let command = args.first().map(String::as_str).unwrap_or("run");
    if command == "live" {
        #[cfg(feature = "live")]
        return bellman_arb::live::run(&args[1..]).await;
        #[cfg(not(feature = "live"))]
        bail!("live commands require an explicit build with --features live");
    }
    if command == "replay" {
        let dir = args.get(1).context(
            "usage: bellman-arb replay RUN_DIRECTORY [--latency-ms 100,250,500] [--verify]",
        )?;
        let lat = option(&args, "--latency-ms")
            .map(|s| {
                s.split(',')
                    .map(str::parse)
                    .collect::<std::result::Result<Vec<u64>, _>>()
            })
            .transpose()?;
        let verify = args.iter().any(|s| s == "--verify");
        let execution_model = option(args, "--execution-model")
            .map(|s| s.parse::<u32>())
            .transpose()?;
        anyhow::ensure!(
            !args.iter().any(|a| a == "--execution-model") || execution_model.is_some(),
            "execution model requires 3 or 4"
        );
        anyhow::ensure!(
            !args.iter().any(|a| a == "--new-paper-epoch"),
            "new epoch is run-only"
        );
        anyhow::ensure!(
            !verify || lat.is_none(),
            "verification requires recorded latencies"
        );
        let quote_age = option(&args, "--quote-age-ms")
            .map(|s| s.parse())
            .transpose()?;
        let depth_age = option(&args, "--depth-age-ms")
            .map(|s| s.parse())
            .transpose()?;
        let diagnostic = option(args, "--force-route")
            .map(|route| -> Result<Diagnostic> {
                Ok(Diagnostic {
                    route,
                    amount: bellman_arb::config::dec(
                        &option(args, "--force-amount-usdc")
                            .context("forced route requires --force-amount-usdc")?,
                    )?,
                    after_ns: option(args, "--force-after-ns")
                        .map(|v| v.parse())
                        .transpose()?
                        .unwrap_or(0),
                })
            })
            .transpose()?;
        anyhow::ensure!(
            diagnostic.is_some()
                || (option(args, "--force-amount-usdc").is_none()
                    && option(args, "--force-after-ns").is_none()),
            "force options require a route"
        );
        let alternative = lat.is_some()
            || quote_age.is_some()
            || depth_age.is_some()
            || diagnostic.is_some()
            || execution_model.is_some();
        let engine = journal::replay_model(
            Path::new(dir),
            lat,
            verify,
            quote_age,
            depth_age,
            diagnostic,
            execution_model,
        )?;
        let mut report = engine.report();
        if alternative {
            report["additional_timer_processing_assumption_ns"] = 0.into();
            report["counterfactual"] = true.into();
            report["execution_performance_included"] = false.into();
        }
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }
    anyhow::ensure!(
        matches!(command, "run" | "discover"),
        "commands: discover, run, replay, live (requires the live build feature)"
    );
    anyhow::ensure!(
        !args.iter().any(|a| a.starts_with("--force-")),
        "forced entry is replay-only"
    );
    let root = PathBuf::from(option(args, "--runs").unwrap_or_else(|| "runs".into()));
    let new_epoch = option(args, "--new-paper-epoch");
    anyhow::ensure!(
        !args.iter().any(|a| a == "--new-paper-epoch") || new_epoch.is_some(),
        "new epoch requires an ID"
    );
    anyhow::ensure!(
        !args.iter().any(|a| a == "--execution-model") && (command == "run" || new_epoch.is_none()),
        "execution model override is replay-only; new epoch is run-only"
    );
    if command == "run" && root.join("RECORDING_LIMIT_REACHED").exists() {
        bail!("recording limit marker present; resolve storage explicitly");
    }
    let cfg = Config::load(Path::new(
        &option(&args, "--config").unwrap_or_else(|| "config.toml".into()),
    ))?;
    let force = option(args, "--reconnect-after")
        .map(|s| s.parse::<u64>())
        .transpose()?;
    let duration = option(args, "--duration")
        .map(|s| s.parse::<u64>())
        .transpose()?;
    let mut reconcile = option(args, "--reconcile-dust")
        .map(|s| s.parse::<u64>())
        .transpose()?;
    anyhow::ensure!(
        reconcile
            .is_none_or(|l| cfg.latency_ms.contains(&l)
                && cfg.dust_limit_usdc > rust_decimal::Decimal::ZERO),
        "reconciliation requires a configured scenario and enabled dust policy"
    );
    let prior_dir = if command == "run" {
        journal::latest(&root)?
    } else {
        None
    };
    let prior = prior_dir
        .as_ref()
        .map(|p| journal::recover(p))
        .transpose()?;
    let (epoch, predecessor, fresh_epoch) =
        journal::paper_epoch(&root, prior_dir.as_deref(), new_epoch.as_deref())?;
    anyhow::ensure!(
        !fresh_epoch || reconcile.is_none(),
        "new epoch cannot reconcile a predecessor account"
    );
    // A reboot may bring Docker up before DNS/networking. Transient public-API
    // failures retry; malformed metadata and recovery failures remain blocked.
    let mut discovery_backoff = 1;
    let (u, raw) = loop {
        match hyperliquid::discover(&cfg).await {
            Ok(found) => break found,
            Err(e)
                if command == "run"
                    && e.downcast_ref::<reqwest::Error>().is_some_and(|r| {
                        r.is_timeout()
                            || r.is_connect()
                            || r.status()
                                .is_some_and(|s| s.is_server_error() || s.as_u16() == 429)
                    }) =>
            {
                eprintln!("Public metadata unavailable; retry in {discovery_backoff}s: {e}");
                tokio::select! { _=shutdown()=>return Ok(()), _=tokio::time::sleep(std::time::Duration::from_secs(discovery_backoff))=>{} }
                discovery_backoff = (discovery_backoff * 2).min(30);
            }
            Err(e) => return Err(e),
        }
    };
    if command == "discover" {
        let e = Engine::new(cfg, u, None)?;
        let coins = e.selected_coins();
        println!(
            "{}",
            serde_json::to_string_pretty(
                &json!({"tokens":e.universe.tokens.len(),"listed_markets":e.universe.markets.len(),"selected_coins":coins,"cycle_count":e.routes.len(),"cycles":e.routes.iter().map(|r|json!({"id":r.id,"name":r.name})).collect::<Vec<_>>(),"market_fees":e.universe.markets.iter().filter(|m|coins.contains(&m.coin)).collect::<Vec<_>>()})
            )?
        );
        return Ok(());
    }
    if command != "run" {
        bail!("commands: discover, run, replay, live (requires the live build feature)")
    }
    let mut accounts = None;
    if let Some((old, mut saved)) = prior.filter(|_| !fresh_epoch) {
        for a in &mut saved {
            a.remap(&old, &u)?;
            a.interrupt("restart recording gap");
        }
        accounts = Some(saved);
    }
    let created = hyperliquid::utc_ns();
    let mut engine = Engine::new(cfg.clone(), u.clone(), accounts)?;
    engine.model_version = 4;
    engine.paper_epoch = epoch.clone();
    engine.predecessor_run_id = predecessor.clone();
    let manifest = Manifest {
        format: 4,
        observer_only: false,
        run_id: format!("run-{created}"),
        created_utc_ns: created,
        config: cfg.clone(),
        universe: u.clone(),
        initial_accounts: Some(engine.accounts.clone()),
        paper_epoch: epoch,
        predecessor_run_id: predecessor,
        source_version: std::fs::read_to_string("SOURCE_SHA256")
            .unwrap_or_else(|_| env!("CARGO_PKG_VERSION").into()),
        cpu_quota: std::fs::read_to_string("/sys/fs/cgroup/cpu.max")
            .unwrap_or_else(|_| "unavailable".into()),
        raw_metadata: Some(raw),
    };
    let run_id = manifest.run_id.clone();
    let journal = Journal::open(&root, manifest)?;
    if root.join("blocked.json").exists() {
        std::fs::remove_file(root.join("blocked.json"))?;
    }
    println!(
        "Public spot screener: {} markets, {} cycles; depth {} levels; paper latencies {:?} ms EACH way; {}",
        engine.selected_coins().len(),
        engine.routes.len(),
        if cfg.l2_fast { 5 } else { 20 },
        cfg.latency_ms,
        journal.dir.display()
    );
    let clock = Arc::new(Clock::new());
    let (reconnect, reconnect_rx) = mpsc::channel(1);
    let (tx, wire_rx) = mpsc::channel(cfg.channel_capacity);
    let (mut rx, mut pipeline) = bellman_arb::hot::launch(
        engine.universe.clone(),
        engine.routes.clone(),
        cfg.clone(),
        wire_rx,
        clock.clone(),
    );
    let mut task = tokio::spawn(hyperliquid::follow(
        engine.selected_coins(),
        cfg.clone(),
        tx,
        clock.clone(),
        reconnect_rx,
        force,
    ));
    let mut seq = 0;
    let mut last_summary = 0;
    let mut stop_reason = "shutdown".to_string();
    let mut failure = None;
    let signal = shutdown();
    tokio::pin!(signal);
    loop {
        let deadline = engine.deadline();
        let batch = tokio::select! {
            biased;
            _=&mut signal=>{stop_reason="operator shutdown".into();break},
            result=&mut task=>{failure=Some(match result{Ok(Err(e))=>e,other=>anyhow::anyhow!("collector terminated: {other:?}")});break},
            result=&mut pipeline=>{failure=Some(match result{Ok(Err(e))=>e,other=>anyhow::anyhow!("hot pipeline terminated: {other:?}")});break},
            _=tokio::time::sleep_until(clock.deadline(deadline.unwrap_or(0))), if deadline.is_some()=>None,
            w=rx.recv()=>match w{Some(w)=>Some(w),None=>{failure=Some(anyhow::anyhow!("hot pipeline closed"));break}},
        };
        let now = clock.ns();
        if duration.is_some_and(|s| now >= s * 1_000_000_000) {
            stop_reason = "bounded run complete".into();
            break;
        }
        seq += 1;
        let (input, rates) = if let Some(batch) = batch {
            let w = batch.wire;
            (
                Input {
                    sequence: seq,
                    generation: w.generation,
                    receipt_ns: w.receipt_ns,
                    receipt_utc_ns: w.utc_ns,
                    process_ns: now,
                    hot_started_ns: batch.started_ns,
                    hot_done_ns: batch.done_ns,
                    event: w.event,
                },
                Some(batch.rates),
            )
        } else {
            (
                Input {
                    sequence: seq,
                    generation: engine.generation,
                    receipt_ns: now,
                    receipt_utc_ns: 0,
                    process_ns: now,
                    hot_started_ns: 0,
                    hot_done_ns: 0,
                    event: InputKind::Clock,
                },
                None,
            )
        };
        let was_connected = engine.connected;
        let mut events = match if let Some(rates) = rates {
            engine.step_hot(&input, &rates)
        } else {
            engine.step(&input)
        } {
            Ok(e) => e,
            Err(e) => {
                failure = Some(e);
                break;
            }
        };
        let completed = clock.ns();
        if let Err(e) = engine.complete_step(&input, completed, &mut events) {
            failure = Some(e);
            break;
        }
        if matches!(input.event, InputKind::Frame { .. }) {
            engine
                .stats
                .receipt_to_decision
                .record(completed - input.receipt_ns);
            engine.stats.processing.record(completed - input.process_ns);
            if was_connected && !engine.connected {
                if let Err(e) = reconnect.try_send(input.generation) {
                    failure = Some(anyhow::anyhow!("reconnect control overflow/closed: {e}"));
                    break;
                }
            }
        }
        if let Err(e) = journal.record(Record {
            input,
            completed_ns: completed,
            events,
        }) {
            failure = Some(e);
            break;
        }
        if let Some(latency_ms) = reconcile.take() {
            seq += 1;
            let at = clock.ns();
            let request = Input {
                sequence: seq,
                generation: engine.generation,
                receipt_ns: at,
                receipt_utc_ns: 0,
                process_ns: at,
                hot_started_ns: 0,
                hot_done_ns: 0,
                event: InputKind::ReconcileDust { latency_ms },
            };
            let result = (|| -> Result<()> {
                let mut events = engine.step(&request)?;
                let completed_ns = clock.ns();
                engine.complete_step(&request, completed_ns, &mut events)?;
                journal.record(Record {
                    input: request,
                    completed_ns,
                    events,
                })
            })();
            if let Err(e) = result {
                failure = Some(e);
                break;
            }
        }
        if now - last_summary >= 60_000_000_000 {
            let report = engine.report();
            println!(
                "{}",
                json!({"elapsed_s":now/1_000_000_000,"books":engine.stats.accepted_books,"coverage":report["coverage"],"deadline_dispatch_lag":report["deadline_dispatch_lag"],"observable_routes":report["observable_routes"],"eligible_routes":report["size_eligible_routes"],"positive_episodes":report["positive_episodes"],"receipt_to_decision":report["receipt_to_decision"],"accounts":engine.accounts.iter().map(|a|json!({"latency_each_way_ms":a.latency_ms,"balances":a.balances,"reserved":a.attempt.as_ref().map(|x|x.start),"completed":a.completed,"paused":a.paused,"entry_guard":a.entry_guard})).collect::<Vec<_>>()})
            );
            if let Err(e) = journal.summary(report) {
                failure = Some(e);
                break;
            }
            last_summary = now;
        }
    }
    task.abort();
    pipeline.abort();
    if let Some(e) = &failure {
        stop_reason = format!("incomplete: {e:#}");
    }
    seq += 1;
    let now = clock.ns();
    let stop = Input {
        sequence: seq,
        generation: engine.generation,
        receipt_ns: now,
        receipt_utc_ns: hyperliquid::utc_ns(),
        process_ns: now,
        hot_started_ns: 0,
        hot_done_ns: 0,
        event: InputKind::Stop {
            reason: stop_reason.clone(),
        },
    };
    let terminal = (|| -> Result<()> {
        let mut events = engine.step(&stop)?;
        let completed_ns = clock.ns();
        engine.complete_step(&stop, completed_ns, &mut events)?;
        events.push(journal::checkpoint(&run_id, &engine));
        journal.record(Record {
            input: stop,
            completed_ns,
            events,
        })
    })();
    if let Err(e) = terminal {
        failure.get_or_insert(e);
    }
    let mut report = engine.report();
    report["stop_reason"] = stop_reason.into();
    report["recording_complete"] = failure.is_none().into();
    report["recovery"] = journal::checkpoint(&run_id, &engine);
    let dir = journal.dir.clone();
    journal.finish(report)?;
    println!("Run saved: {}", dir.display());
    if let Some(e) = failure {
        return Err(e);
    }
    Ok(())
}
