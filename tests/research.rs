use bellman_arb::{
    book::{self, Book},
    config::{dec, Config},
    engine::{Engine, Input, InputKind},
    journal::{Journal, Manifest, Record},
    market::{negative_cycle, Universe},
    quantity::{self, Shadow},
};
use rust_decimal::Decimal;
use serde_json::{json, Value};
use std::collections::BTreeMap;

fn config() -> Config {
    Config {
        taker_fee_bps: Decimal::ZERO,
        slippage_bps: Decimal::ZERO,
        amounts_usdc: vec![Decimal::from(100)],
        latency_ms: vec![250],
        quote_age_ms: 10_000,
        depth_age_ms: 10_000,
        ..Config::default()
    }
}

fn bbo(m: usize, bid: &str, bid_size: &str, ask: &str, ask_size: &str, time: u64) -> String {
    json!({"channel":"bbo","data":{"coin":format!("@{}",m+1),"time":time,
        "bbo":[{"px":bid,"sz":bid_size},{"px":ask,"sz":ask_size}]}})
    .to_string()
}

#[test]
fn recorded_bbo_failures_have_bounded_v4_outcomes() {
    let cfg = Config::default();
    let u = universe(&cfg);
    let buy = bellman_arb::market::Edge {
        market: 0,
        buy: true,
    };
    let mut book = Book::default();
    let mut shadow = Shadow::default();
    for (text, at) in [
        (raw(0, "88.299", "88.3", "11.33", 1790675378175), 1),
        (
            bbo(0, "88.299", "12.67", "88.483", "0.17", 1790675378503),
            286_000_000,
        ),
    ] {
        let up = book::parse(&text, &u, at, at).unwrap().unwrap();
        assert!(book.apply(&up));
        shadow.update(&up);
    }
    let order = quantity::Order {
        edge: buy,
        qty: dec("11.32").unwrap(),
        limit: dec("88.317").unwrap(),
        budget: Decimal::from(1000),
        source: None,
    };
    assert!(quantity::execute_model(&order, &book, &u, &cfg, 301_000_000, None, 3).is_err());
    let zero = quantity::execute_model(&order, &book, &u, &cfg, 301_000_000, Some(&mut shadow), 4)
        .unwrap();
    assert_eq!(zero.qty, Decimal::ZERO);
    assert_eq!(zero.source.unwrap().scope, "zero");

    let mut book = Book::default();
    let mut shadow = Shadow::default();
    for (text, at) in [
        (raw(0, "0.9994", "0.9999", "123084.58", 1790698552126), 1),
        (
            bbo(
                0,
                "0.9994",
                "39616.74",
                "0.9999",
                "122081.42",
                1790698552264,
            ),
            69_000_000,
        ),
    ] {
        let up = book::parse(&text, &u, at, at).unwrap().unwrap();
        assert!(book.apply(&up));
        shadow.update(&up);
    }
    let order = quantity::prepare_model(
        buy,
        Decimal::from(1000),
        &book,
        &u,
        &cfg,
        250_000_000,
        cfg.slippage_bps,
        4,
    )
    .unwrap();
    let fill = quantity::execute_model(&order, &book, &u, &cfg, 250_000_000, Some(&mut shadow), 4)
        .unwrap();
    assert_eq!(fill.qty, order.qty);
    assert_eq!(fill.gross, fill.received + fill.fee);
    assert_eq!(fill.source.as_ref().unwrap().scope, "top");
    let level = &book.bbo.as_ref().unwrap().levels[1][0];
    assert_eq!(shadow.available(buy, level), level.sz - fill.qty);
    assert_eq!(fill.source.unwrap().quantity_age_ns, 181_000_000);
}

#[test]
fn v4_side_validity_unknown_depth_and_zero_fill_are_conservative() {
    let cfg = Config::default();
    let u = universe(&cfg);
    for buy in [true, false] {
        let edge = bellman_arb::market::Edge { market: 0, buy };
        let mut book = Book::default();
        book.apply(
            &book::parse(&raw(0, "9.99", "10", "1000", 1), &u, 1, 1)
                .unwrap()
                .unwrap(),
        );
        let text = if buy {
            bbo(0, "9.98", "20", "10", "1000", 2)
        } else {
            bbo(0, "9.99", "1000", "10.01", "20", 2)
        };
        book.apply(&book::parse(&text, &u, 2, 2).unwrap().unwrap());
        assert!(book.depth(2, &cfg).is_err());
        assert_eq!(
            book.execution_side(buy, 2, &cfg, None, 4).unwrap().scope,
            "l2"
        );
        let changed = bbo(0, "9.99", "1", "10", "1", 3);
        book.apply(&book::parse(&changed, &u, 3, 3).unwrap().unwrap());
        assert_eq!(
            book.execution_side(buy, 3, &cfg, None, 4).unwrap().scope,
            "top"
        );
        let mut order = quantity::Order {
            edge,
            qty: Decimal::from(2),
            limit: if buy {
                dec("10.01").unwrap()
            } else {
                dec("9.98").unwrap()
            },
            budget: Decimal::from(100),
            source: None,
        };
        assert!(quantity::execute_model(&order, &book, &u, &cfg, 3, None, 4)
            .unwrap_err()
            .to_string()
            .contains("unknown_deeper"));
        assert!(quantity::prepare_model(
            edge,
            Decimal::from(100),
            &book,
            &u,
            &cfg,
            3,
            cfg.slippage_bps,
            4
        )
        .is_err());
        order.limit = if buy {
            Decimal::from(10)
        } else {
            dec("9.99").unwrap()
        };
        assert_eq!(
            quantity::execute_model(&order, &book, &u, &cfg, 3, None, 4)
                .unwrap()
                .qty,
            Decimal::ONE
        );
        order.limit = if buy {
            dec("9.99").unwrap()
        } else {
            Decimal::from(10)
        };
        assert_eq!(
            quantity::execute_model(&order, &book, &u, &cfg, 3, None, 4)
                .unwrap()
                .qty,
            Decimal::ZERO
        );
        assert!(quantity::execute_model(&order, &book, &u, &cfg, 1_000_000_004, None, 4).is_err());
        book.invalidate("disconnect");
        assert!(book.execution_side(buy, 4, &cfg, None, 4).is_err());
    }
}

#[test]
fn v4_shadow_removal_and_replenishment_require_evidence() {
    let cfg = config();
    let u = universe(&cfg);
    for buy in [true, false] {
        let edge = bellman_arb::market::Edge { market: 0, buy };
        let original = book::parse(&bbo(0, "9.99", "10", "10", "10", 1), &u, 1, 1)
            .unwrap()
            .unwrap();
        let level = original.observation.levels[usize::from(buy)][0].clone();
        let mut shadow = Shadow::default();
        shadow.update_model(&original, Some(5), 4);
        shadow.consume(edge, level.px, Decimal::from(7));
        shadow.update_model(&original, Some(5), 4);
        assert_eq!(shadow.available(edge, &level), Decimal::from(3));
        let better = if buy {
            bbo(0, "9.98", "10", "9.99", "10", 2)
        } else {
            bbo(0, "10", "10", "10.01", "10", 2)
        };
        shadow.update_model(
            &book::parse(&better, &u, 2, 2).unwrap().unwrap(),
            Some(5),
            4,
        );
        shadow.update_model(&original, Some(5), 4);
        assert_eq!(shadow.available(edge, &level), Decimal::from(3));
        let worse = if buy {
            bbo(0, "9.99", "10", "10.01", "10", 3)
        } else {
            bbo(0, "9.98", "10", "10", "10", 3)
        };
        shadow.update_model(&book::parse(&worse, &u, 3, 3).unwrap().unwrap(), Some(5), 4);
        assert_eq!(shadow.available(edge, &level), Decimal::ZERO);
        shadow.update_model(&original, Some(5), 4);
        assert_eq!(shadow.available(edge, &level), Decimal::from(10));
    }
}

#[test]
fn v4_observed_zero_fill_waits_for_confirmation_and_excludes_later_quotes() {
    let mut e = ready();
    e.model_version = 4;
    let order = e.accounts[0]
        .attempt
        .as_ref()
        .unwrap()
        .pending
        .as_ref()
        .unwrap()
        .clone();
    step(
        &mut e,
        order.arrival_ns - 1,
        InputKind::Frame {
            text: bbo(0, "10", "1000", "10.1", "1000", 2),
        },
    );
    assert!(
        !e.accounts[0]
            .attempt
            .as_ref()
            .unwrap()
            .pending
            .as_ref()
            .unwrap()
            .arrived
    );
    let events = step(
        &mut e,
        order.arrival_ns + 1,
        InputKind::Frame {
            text: raw(0, "9.99", "10", "1000", 3),
        },
    );
    assert!(events
        .iter()
        .any(|v| v["type"] == "arrived" && v["data"]["fill"]["qty"] == "0"));
    assert_eq!(e.accounts[0].balances[&0], Decimal::from(9900));
    assert!(e.accounts[0]
        .attempt
        .as_ref()
        .unwrap()
        .holdings
        .contains_key(&0));
    e.config.min_profit_bps = Decimal::from(10000);
    step(&mut e, order.confirmation_ns, InputKind::Clock);
    assert!(e.accounts[0].attempt.is_none() && e.accounts[0].paused.is_none());
    assert_eq!(e.accounts[0].balances[&0], Decimal::from(10000));
    assert_eq!(e.accounts[0].unobservable, 0);
}

#[test]
fn production_fee_dust_continues_without_restoring_cash() {
    let cfg = Config {
        dust_limit_usdc: Decimal::from(10),
        latency_ms: vec![250],
        amounts_usdc: vec![Decimal::from(100)],
        ..Config::default()
    };
    let mut e = Engine::new(cfg.clone(), universe(&cfg), None).unwrap();
    step(&mut e, 0, InputKind::Open);
    for (m, b, a) in [(0, "9.99", "10"), (1, "2.2", "2.201"), (2, "5", "5.001")] {
        step(
            &mut e,
            m as u64 + 1,
            InputKind::Frame {
                text: raw(m, b, a, "1000", 1),
            },
        );
    }
    let p = e.accounts[0]
        .attempt
        .as_ref()
        .unwrap()
        .pending
        .as_ref()
        .unwrap()
        .clone();
    step(
        &mut e,
        p.arrival_ns - 1,
        InputKind::Frame {
            text: raw(0, "9.99", "10", "2", 2),
        },
    );
    for ms in [250, 500, 750] {
        step(&mut e, p.submitted_ns + ms * 1_000_000, InputKind::Clock);
    }
    for (m, b, a) in [(0, "9.99", "10"), (1, "2.2", "2.201"), (2, "5", "5.001")] {
        step(
            &mut e,
            800_000_000 + m as u64,
            InputKind::Frame {
                text: raw(m, b, a, "1000", 3),
            },
        );
    }
    // Isolate this unwind from a new eligibility episode after depth replenishes.
    e.config.min_profit_bps = Decimal::from(10000);
    step(&mut e, p.submitted_ns + 1_000_000_000, InputKind::Clock);
    let a = &e.accounts[0];
    assert!(a.paused.is_none() && a.entry_guard.is_none());
    assert_eq!(a.failed, 1);
    assert_eq!(quantity::amount(&a.balances, 1), dec("0.0006").unwrap());
    assert_eq!(a.balances[&0], dec("9999.94604798").unwrap());
    assert_eq!(a.realized_usdc, a.balances[&0] - Decimal::from(10000));
    let before = a.balances.clone();
    e.config.dust_limit_usdc = dec("0.001").unwrap();
    step(&mut e, 1_010_000_000, InputKind::Clock);
    assert_eq!(
        e.accounts[0].entry_guard.as_deref(),
        Some("dust_limit_exceeded")
    );
    assert_eq!(e.accounts[0].balances, before);
}

#[test]
fn marked_inventory_is_not_cash_and_unknown_exposure_never_resumes() {
    let mut cfg = Config::default();
    cfg.dust_limit_usdc = Decimal::from(10);
    let mut u = universe(&cfg);
    u.tokens.get_mut(&1).unwrap().sz_decimals = 5;
    let mut books = vec![Book::default(); 3];
    for (m, b, a) in [
        (0, "84999", "85000"),
        (1, "85340", "85341"),
        (2, "1", "1.0001"),
    ] {
        books[m].apply(
            &book::parse(&raw(m, b, a, "1000000", 1), &u, 1, 1)
                .unwrap()
                .unwrap(),
        );
    }
    let route = u
        .routes(&cfg)
        .unwrap()
        .into_iter()
        .find(|r| r.funded_edges(&u).unwrap()[0].market == 0)
        .unwrap();
    let est = quantity::estimate(&route, Decimal::from(25), &books, &u, &cfg, 1).unwrap();
    let mark = quantity::mark_inventory(&est.residual, &books, &u, &cfg, 1);
    assert!(est.bps < Decimal::ZERO);
    assert!(est.profit + mark.indicative_usdc.unwrap() > Decimal::ZERO);
    books[1].invalidate("quiet cross");
    // Sub-lot size proves a sell impossible even without that cross's quote.
    assert!(quantity::mark_inventory(&est.residual, &books, &u, &cfg, 1).all_dust);
    let mut a = bellman_arb::paper::Account::new(250, &cfg, 0);
    a.balances.extend(est.residual);
    a.paused = Some("unresolved: missing fill".into());
    let balances = a.balances.clone();
    let mut events = vec![];
    a.check_dust(&books, &u, &cfg, 1, true, &mut events);
    assert!(a.paused.is_some());
    assert_eq!(a.balances, balances);
    assert_eq!(events.last().unwrap()["accepted"], Value::Null);
    assert_eq!(events.last().unwrap()["data"]["accepted"], false);
    books[0].invalidate("gap");
    assert!(quantity::mark_inventory(&a.balances, &books, &u, &cfg, 2)
        .indicative_usdc
        .is_none());
}

#[test]
fn metadata_remap_preserves_pending_orders_and_depletion() {
    let mut e = ready();
    let arrival = e.deadline().unwrap();
    step(&mut e, arrival, InputKind::Clock);
    let old = e.universe.clone();
    let mut new = old.clone();
    new.markets.reverse();
    let mut account = e.accounts[0].clone();
    let before = account.clone();
    account.remap(&old, &new).unwrap();
    let p = account.attempt.as_ref().unwrap().pending.as_ref().unwrap();
    assert_eq!(new.markets[p.order.edge.market].index, 1);
    assert_eq!(account.balances, before.balances);
    assert_eq!(
        account.attempt.as_ref().unwrap().holdings,
        before.attempt.as_ref().unwrap().holdings
    );
    let level = &e.books[0].depth.as_ref().unwrap().levels[1][0];
    assert_eq!(
        account.shadow.available(p.order.edge, level),
        before.shadow.available(
            before
                .attempt
                .as_ref()
                .unwrap()
                .pending
                .as_ref()
                .unwrap()
                .order
                .edge,
            level
        )
    );
    let mut missing = new.clone();
    missing.tokens.remove(&1);
    assert!(e.accounts[0].clone().remap(&old, &missing).is_err());
    let mut changed = new;
    changed.tokens.get_mut(&1).unwrap().sz_decimals = 4;
    assert!(e.accounts[0].clone().remap(&old, &changed).is_err());
    let empty = bellman_arb::paper::Account::new(250, &e.config, old.usdc);
    let mut fewer = old.clone();
    fewer.markets.pop();
    fewer.tokens.remove(&2);
    assert!(empty.clone().remap(&old, &fewer).is_ok());
    let mut legacy_unknown = empty;
    legacy_unknown.unobservable = 1;
    legacy_unknown.remap(&old, &old).unwrap();
    assert!(legacy_unknown
        .paused
        .as_ref()
        .unwrap()
        .starts_with("unresolved"));
    assert_eq!(legacy_unknown.balances[&0], Decimal::from(10000));
}

fn recorded_v3() -> (std::path::PathBuf, std::path::PathBuf, Engine) {
    let cfg = Config {
        taker_fee_bps: Decimal::from(7),
        dust_limit_usdc: Decimal::from(10),
        ..config()
    };
    let u = universe(&cfg);
    let root =
        std::env::temp_dir().join(format!("bellman-v3-{}", bellman_arb::hyperliquid::utc_ns()));
    let journal = Journal::open(
        &root,
        Manifest {
            format: 3,
            run_id: "run-1".into(),
            created_utc_ns: 0,
            config: cfg.clone(),
            universe: u.clone(),
            initial_accounts: None,
            source_version: "test".into(),
            cpu_quota: "test".into(),
            raw_metadata: None,
            paper_epoch: None,
            predecessor_run_id: None,
        },
    )
    .unwrap();
    let dir = journal.dir.clone();
    let mut e = Engine::new(cfg, u, None).unwrap();
    let mut kinds = vec![(0, InputKind::Open)];
    for (m, b, a) in [(0, "9.99", "10"), (1, "2", "2.01"), (2, "5", "5.001")] {
        kinds.push((
            m as u64 + 1,
            InputKind::Frame {
                text: raw(m, b, a, "1000", 1),
            },
        ));
    }
    kinds.push((
        2_000_000_000,
        InputKind::Stop {
            reason: "test complete".into(),
        },
    ));
    for (now, kind) in kinds {
        let x = input(&e, now, kind);
        let mut events = e.step(&x).unwrap();
        e.complete_step(&x, now, &mut events).unwrap();
        if matches!(x.event, InputKind::Frame { .. }) {
            e.stats.receipt_to_decision.record(0);
            e.stats.processing.record(0);
        }
        if matches!(x.event, InputKind::Stop { .. }) {
            events.push(bellman_arb::journal::checkpoint("run-1", &e));
        }
        journal
            .record(Record {
                input: x,
                completed_ns: now,
                events,
            })
            .unwrap();
    }
    let mut report = e.report();
    report["recording_complete"] = true.into();
    report["recovery"] = bellman_arb::journal::checkpoint("run-1", &e);
    journal.finish(report).unwrap();
    (root, dir, e)
}

#[test]
fn v4_epochs_preserve_predecessor_and_never_repeat_funding() {
    let (root, prior, baseline) = recorded_v3();
    let original = std::fs::read(prior.join("final.json")).unwrap();
    let selection = bellman_arb::journal::paper_epoch(&root, Some(&prior), Some("epoch4")).unwrap();
    assert_eq!(
        selection,
        (Some("epoch4".into()), Some("run-1".into()), true)
    );
    let mut previous = prior.clone();
    for id in ["epoch4", "epoch5"] {
        let mut e = Engine::new(baseline.config.clone(), baseline.universe.clone(), None).unwrap();
        e.model_version = 4;
        e.paper_epoch = Some(id.into());
        e.predecessor_run_id = Some(previous.file_name().unwrap().to_str().unwrap().into());
        // A restored account below initial funding must stay below initial funding.
        e.accounts[0].balances.insert(0, Decimal::from(9500));
        let run_id = format!("run-{id}");
        let j = Journal::open(
            &root,
            Manifest {
                format: 4,
                run_id: run_id.clone(),
                created_utc_ns: 0,
                config: e.config.clone(),
                universe: e.universe.clone(),
                initial_accounts: Some(e.accounts.clone()),
                source_version: "test".into(),
                cpu_quota: "test".into(),
                raw_metadata: None,
                paper_epoch: e.paper_epoch.clone(),
                predecessor_run_id: e.predecessor_run_id.clone(),
            },
        )
        .unwrap();
        let dir = j.dir.clone();
        let x = input(
            &e,
            1,
            InputKind::Stop {
                reason: "test".into(),
            },
        );
        let mut events = e.step(&x).unwrap();
        e.complete_step(&x, 1, &mut events).unwrap();
        events.push(bellman_arb::journal::checkpoint(&run_id, &e));
        j.record(Record {
            input: x,
            completed_ns: 1,
            events,
        })
        .unwrap();
        let mut report = e.report();
        report["recording_complete"] = true.into();
        report["recovery"] = bellman_arb::journal::checkpoint(&run_id, &e);
        j.finish(report).unwrap();
        assert_eq!(
            bellman_arb::journal::replay(&dir, None, true)
                .unwrap()
                .report(),
            e.report()
        );
        assert_eq!(
            bellman_arb::journal::recover(&dir).unwrap().1[0].balances[&0],
            Decimal::from(9500)
        );
        assert!(
            !bellman_arb::journal::paper_epoch(&root, Some(&dir), Some(id))
                .unwrap()
                .2
        );
        assert_eq!(std::fs::read(prior.join("final.json")).unwrap(), original);
        assert!(
            bellman_arb::journal::replay_model(&dir, None, true, None, None, None, Some(4))
                .is_err()
        );
        previous = dir;
    }
    assert!(bellman_arb::journal::paper_epoch(&root, Some(&previous), Some("epoch4")).is_err());
    assert_eq!(
        bellman_arb::journal::paper_epoch(&root, Some(&previous), None)
            .unwrap()
            .0
            .as_deref(),
        Some("epoch5")
    );
    let final_path = previous.join("final.json");
    let mut damaged: Value = serde_json::from_slice(&std::fs::read(&final_path).unwrap()).unwrap();
    damaged["accounts"][0]["balances"]["0"] = "10000".into();
    std::fs::write(final_path, serde_json::to_vec(&damaged).unwrap()).unwrap();
    assert!(bellman_arb::journal::paper_epoch(&root, Some(&previous), Some("epoch6")).is_err());
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn clean_checkpoint_and_forced_negative_replay_use_real_accounting() {
    let (root, dir, e) = recorded_v3();
    let replay = bellman_arb::journal::replay(&dir, None, true).unwrap();
    assert_eq!(e.report(), replay.report());
    let (_, restored) = bellman_arb::journal::recover(&dir).unwrap();
    assert_eq!(
        serde_json::to_value(restored).unwrap(),
        serde_json::to_value(&e.accounts).unwrap()
    );
    let force = bellman_arb::engine::Diagnostic {
        route: "1B>2S>3S".into(),
        amount: Decimal::from(100),
        after_ns: 0,
    };
    let diagnostic =
        bellman_arb::journal::replay_options(&dir, None, false, None, None, Some(force.clone()))
            .unwrap();
    assert_eq!(diagnostic.accounts[0].completed, 1);
    assert!(diagnostic.accounts[0].realized_usdc < Decimal::ZERO);
    assert_eq!(diagnostic.accounts[0].tried.len(), 1);
    assert_eq!(diagnostic.report()["execution_performance_included"], false);
    assert!(diagnostic
        .diagnostic_journal
        .iter()
        .any(|v| v["type"] == "confirmed"));
    assert!(
        bellman_arb::journal::replay_options(&dir, None, true, None, None, Some(force)).is_err()
    );
    let path = dir.join("final.json");
    let mut corrupt: Value = serde_json::from_reader(std::fs::File::open(&path).unwrap()).unwrap();
    corrupt["accounts"][0]["balances"]["0"] = "999999".into();
    bellman_arb::journal::write_json(&path, &corrupt).unwrap();
    assert!(bellman_arb::journal::recover(&dir).is_err());
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn v3_clean_report_requires_durable_terminal_checkpoint() {
    let cfg = config();
    let root = std::env::temp_dir().join(format!(
        "bellman-no-stop-{}",
        bellman_arb::hyperliquid::utc_ns()
    ));
    let u = universe(&cfg);
    let e = Engine::new(cfg.clone(), u.clone(), None).unwrap();
    let j = Journal::open(
        &root,
        Manifest {
            format: 3,
            run_id: "run-1".into(),
            created_utc_ns: 0,
            config: cfg,
            universe: u,
            initial_accounts: None,
            source_version: "test".into(),
            cpu_quota: "test".into(),
            raw_metadata: None,
            paper_epoch: None,
            predecessor_run_id: None,
        },
    )
    .unwrap();
    let dir = j.dir.clone();
    let mut report = e.report();
    report["recording_complete"] = true.into();
    report["recovery"] = bellman_arb::journal::checkpoint("run-1", &e);
    assert!(j.finish(report).is_err());
    assert!(!dir.join("final.json").exists());
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn terminal_checkpoint_retains_reservation_and_refuses_reconciliation() {
    let original = ready();
    let mut cfg = original.config.clone();
    cfg.dust_limit_usdc = Decimal::from(10);
    let mut e = Engine::new(
        cfg.clone(),
        original.universe.clone(),
        Some(original.accounts.clone()),
    )
    .unwrap();
    let root = std::env::temp_dir().join(format!(
        "bellman-pending-checkpoint-{}",
        bellman_arb::hyperliquid::utc_ns()
    ));
    let j = Journal::open(
        &root,
        Manifest {
            format: 3,
            run_id: "run-1".into(),
            created_utc_ns: 0,
            config: cfg,
            universe: e.universe.clone(),
            initial_accounts: Some(e.accounts.clone()),
            source_version: "test".into(),
            cpu_quota: "test".into(),
            raw_metadata: None,
            paper_epoch: None,
            predecessor_run_id: None,
        },
    )
    .unwrap();
    let dir = j.dir.clone();
    let x = input(
        &e,
        10,
        InputKind::Stop {
            reason: "operator stop".into(),
        },
    );
    let mut events = e.step(&x).unwrap();
    e.complete_step(&x, 10, &mut events).unwrap();
    events.push(bellman_arb::journal::checkpoint("run-1", &e));
    j.record(Record {
        input: x,
        completed_ns: 10,
        events,
    })
    .unwrap();
    let mut report = e.report();
    report["recording_complete"] = true.into();
    report["recovery"] = bellman_arb::journal::checkpoint("run-1", &e);
    j.finish(report).unwrap();
    let (_, accounts) = bellman_arb::journal::recover(&dir).unwrap();
    assert_eq!(
        serde_json::to_value(&accounts).unwrap(),
        serde_json::to_value(&e.accounts).unwrap()
    );
    assert_eq!(accounts[0].balances[&0], Decimal::from(9900));
    assert!(accounts[0].attempt.is_some());
    bellman_arb::journal::replay(&dir, None, true).unwrap();
    let events = step(&mut e, 11, InputKind::ReconcileDust { latency_ms: 250 });
    assert!(events
        .iter()
        .any(|v| v["type"] == "dust_reconciliation" && v["data"]["accepted"] == false));
    assert!(e.accounts[0].paused.is_some());
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn depth_coverage_expires_independently_and_empty_route_reactivates() {
    let mut e = ready();
    e.config.depth_age_ms = 100;
    step(
        &mut e,
        4,
        InputKind::Frame {
            text: raw(0, "9.99", "10", "1000", 2),
        },
    );
    step(&mut e, 101_000_000, InputKind::Clock);
    assert_eq!(e.report()["coverage"]["executable_depth_routes"], 0);
    assert_eq!(e.report()["coverage"]["fresh_price_routes"], 2);
    let before = e.report()["coverage"]["depth_route_ns"].as_u64().unwrap();
    step(&mut e, 110_000_000, InputKind::Clock);
    assert_eq!(
        e.report()["coverage"]["depth_route_ns"].as_u64().unwrap(),
        before
    );
    step(
        &mut e,
        120_000_000,
        InputKind::Frame {
            text: json!({"channel":"l2Book","data":{"coin":"@1","time":2,"levels":[[],[]]}})
                .to_string(),
        },
    );
    assert!(e.report()["coverage"]["dormant_routes"].as_u64().unwrap() > 0);
    for m in 0..3 {
        step(
            &mut e,
            130_000_000 + m as u64,
            InputKind::Frame {
                text: raw(m, "1", "1.01", "1000", 3),
            },
        );
    }
    assert_eq!(e.report()["coverage"]["dormant_routes"], 0);
    assert_eq!(e.report()["coverage"]["executable_depth_routes"], 2);
}

#[tokio::test]
async fn quiet_feed_order_deadline_uses_one_shot_timer() {
    let clock = bellman_arb::hyperliquid::Clock::new();
    let mut e = ready();
    let due = e.deadline().unwrap();
    tokio::time::sleep_until(clock.deadline(due)).await;
    let now = clock.ns();
    assert!(now >= due);
    let events = step(&mut e, now, InputKind::Clock);
    assert!(events
        .iter()
        .any(|v| v["type"] == "arrived" && v["scheduled_ns"] == due));
    assert!(
        e.accounts[0]
            .attempt
            .as_ref()
            .unwrap()
            .pending
            .as_ref()
            .unwrap()
            .arrived
    );
    println!("ONE_SHOT_TIMER dispatch_lag_ns={}", now - due);
}
fn universe(cfg: &Config) -> Universe {
    Universe::parse(&json!({"tokens":[
    {"index":0,"name":"USDC","tokenId":"usd","szDecimals":8,"weiDecimals":8},
    {"index":1,"name":"A","tokenId":"a","szDecimals":3,"weiDecimals":8},
    {"index":2,"name":"B","tokenId":"b","szDecimals":3,"weiDecimals":8}],
    "universe":[{"index":1,"name":"@1","tokens":[1,0]},{"index":2,"name":"@2","tokens":[1,2]},{"index":3,"name":"@3","tokens":[2,0]}]}),cfg).unwrap()
}
fn raw(m: usize, bid: &str, ask: &str, size: &str, time: u64) -> String {
    json!({"channel":"l2Book","data":{"coin":format!("@{}",m+1),"time":time,"levels":[[{"px":bid,"sz":size}],[{"px":ask,"sz":size}]]}}).to_string()
}
fn input(e: &Engine, now: u64, kind: InputKind) -> Input {
    Input {
        sequence: e.last_sequence + 1,
        generation: 1,
        receipt_ns: now,
        receipt_utc_ns: now + 1_000_000_000,
        process_ns: now,
        hot_started_ns: 0,
        hot_done_ns: 0,
        event: kind,
    }
}
fn step(e: &mut Engine, now: u64, kind: InputKind) -> Vec<Value> {
    e.step(&input(e, now, kind)).unwrap()
}
fn ready() -> Engine {
    let cfg = config();
    let mut e = Engine::new(cfg.clone(), universe(&cfg), None).unwrap();
    step(&mut e, 0, InputKind::Open);
    for (m, b, a) in [(0, "9.99", "10"), (1, "2.02", "2.021"), (2, "5", "5.001")] {
        step(
            &mut e,
            m as u64 + 1,
            InputKind::Frame {
                text: raw(m, b, a, "1000", 1),
            },
        );
    }
    e
}

#[test]
fn saved_metadata_identity_and_complete_cycles() {
    let raw: Value = serde_json::from_str(
        include_str!("../review/hyperliquid_spot_snapshot.json").trim_start_matches('\u{feff}'),
    )
    .unwrap();
    let cfg = Config::default();
    let u = Universe::parse(&raw, &cfg).unwrap();
    let routes = u.routes(&cfg).unwrap();
    assert_eq!(u.markets.len(), 330);
    assert_eq!(raw[1].as_array().unwrap().len(), 885);
    assert_eq!(routes.len(), 210);
    let mut count = BTreeMap::new();
    for r in &routes {
        *count.entry(r.edges.len()).or_insert(0) += 1;
        for i in 0..r.edges.len() {
            assert_eq!(r.edges[i].to(&u), r.edges[(i + 1) % r.edges.len()].from(&u));
        }
    }
    assert_eq!(count, BTreeMap::from([(3, 28), (4, 90), (5, 44), (6, 48)]));
    assert_eq!(
        Engine::new(cfg, u, None).unwrap().selected_coins().len(),
        29
    );
}
#[test]
fn duplicate_display_names_do_not_merge_assets() {
    let cfg = config();
    let mut u = universe(&cfg);
    u.tokens.get_mut(&2).unwrap().name = "A".into();
    assert_eq!(u.routes(&cfg).unwrap().len(), 2);
}
#[test]
fn all_210_route_identities_remain_observable() {
    let cfg = Config::default();
    let raw: Value =
        serde_json::from_str(include_str!("../review/hyperliquid_spot_snapshot.json")).unwrap();
    let u = Universe::parse(&raw, &cfg).unwrap();
    let mut e = Engine::new(cfg, u, None).unwrap();
    step(&mut e, 0, InputKind::Open);
    for (i, coin) in e.selected_coins().iter().enumerate() {
        let text = json!({"channel":"l2Book","data":{"coin":coin,"time":1,"levels":[[{"px":"1","sz":"100000"}],[{"px":"1","sz":"100000"}]]}}).to_string();
        step(&mut e, i as u64 + 1, InputKind::Frame { text });
    }
    assert_eq!(e.states.iter().filter(|s| s.net_bps.is_some()).count(), 210);
    assert_eq!(
        e.routes
            .iter()
            .map(|r| &r.id)
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        210
    );
    assert!(e.states.iter().all(|s| s.net_bps.unwrap() < 0.0));
}
#[test]
fn bad_metadata_reference_is_rejected() {
    let cfg = config();
    let mut v = serde_json::to_value(universe(&cfg)).unwrap();
    v["tokens"] = json!([]);
    assert!(Universe::parse(&v, &cfg).is_err());
}
#[test]
fn fractional_fee_and_threshold_are_unrounded() {
    let cfg = config();
    let u = universe(&cfg);
    let mut e = Engine::new(cfg, u, None).unwrap();
    step(&mut e, 0, InputKind::Open);
    for (m, b, a) in [(0, "1", "1"), (1, "1", "1"), (2, "1.00046", "1.00046")] {
        step(
            &mut e,
            m as u64 + 1,
            InputKind::Frame {
                text: raw(m, b, a, "100000", 1),
            },
        );
    }
    assert!(e
        .states
        .iter()
        .any(|s| s.net_bps.is_some_and(|b| b > 4.5 && b < 5.0)));
    assert!(e.accounts[0].attempt.is_none());
}
#[test]
fn latency_arrival_confirmation_and_conservation() {
    let mut e = ready();
    let start = e.accounts[0]
        .attempt
        .as_ref()
        .unwrap()
        .pending
        .as_ref()
        .unwrap()
        .submitted_ns;
    step(&mut e, start + 249_000_000, InputKind::Clock);
    assert!(
        !e.accounts[0]
            .attempt
            .as_ref()
            .unwrap()
            .pending
            .as_ref()
            .unwrap()
            .arrived
    );
    step(&mut e, start + 250_000_000, InputKind::Clock);
    let a = e.accounts[0].attempt.as_ref().unwrap();
    assert!(a.pending.as_ref().unwrap().arrived);
    assert_eq!(quantity::amount(&a.holdings, 1), Decimal::ZERO);
    step(&mut e, start + 499_000_000, InputKind::Clock);
    assert_eq!(e.accounts[0].attempt.as_ref().unwrap().next, 0);
    step(&mut e, start + 500_000_000, InputKind::Clock);
    let a = e.accounts[0].attempt.as_ref().unwrap();
    assert_eq!(a.next, 1);
    assert_eq!(
        a.pending.as_ref().unwrap().submitted_ns,
        start + 500_000_000
    );
    assert_eq!(quantity::amount(&a.holdings, 1), Decimal::from(10));
    for ms in [750, 1000, 1250, 1500] {
        step(&mut e, start + ms * 1_000_000, InputKind::Clock);
    }
    assert!(e.accounts[0].attempt.is_none());
    assert_eq!(e.accounts[0].completed, 1);
    assert_eq!(e.accounts[0].balances[&0], Decimal::from(10001));
    step(&mut e, start + 1_600_000_000, InputKind::Clock);
    assert!(
        e.accounts[0].attempt.is_none(),
        "same eligibility episode must not reenter"
    );
}
#[test]
fn future_quote_cannot_rewrite_arrival() {
    let mut e = ready();
    let p = e.accounts[0]
        .attempt
        .as_ref()
        .unwrap()
        .pending
        .as_ref()
        .unwrap()
        .clone();
    step(
        &mut e,
        p.arrival_ns + 1,
        InputKind::Frame {
            text: raw(0, "19", "20", "1000", 2),
        },
    );
    let f = e.accounts[0]
        .attempt
        .as_ref()
        .unwrap()
        .pending
        .as_ref()
        .unwrap()
        .fill
        .as_ref()
        .unwrap();
    assert_eq!(f.qty, Decimal::from(10));
    assert_eq!(f.spent, Decimal::from(100));
}
#[test]
fn partial_fill_is_unwound_not_magically_restored() {
    let mut e = ready();
    let p = e.accounts[0]
        .attempt
        .as_ref()
        .unwrap()
        .pending
        .as_ref()
        .unwrap()
        .clone();
    step(
        &mut e,
        p.arrival_ns - 1,
        InputKind::Frame {
            text: raw(0, "9.99", "10", "2", 2),
        },
    );
    step(&mut e, p.arrival_ns, InputKind::Clock);
    assert_eq!(
        e.accounts[0]
            .attempt
            .as_ref()
            .unwrap()
            .pending
            .as_ref()
            .unwrap()
            .fill
            .as_ref()
            .unwrap()
            .qty,
        Decimal::from(2)
    );
    for ns in [
        p.confirmation_ns,
        p.confirmation_ns + 250_000_000,
        p.confirmation_ns + 500_000_000,
    ] {
        step(&mut e, ns, InputKind::Clock);
    }
    assert!(e.accounts[0].attempt.is_none());
    assert_eq!(e.accounts[0].failed, 1);
    assert_eq!(e.accounts[0].balances[&0], dec("9999.98").unwrap());
}
#[test]
fn unavailable_arrival_is_censored() {
    let mut e = ready();
    let p = e.accounts[0]
        .attempt
        .as_ref()
        .unwrap()
        .pending
        .as_ref()
        .unwrap()
        .clone();
    step(
        &mut e,
        p.arrival_ns - 1,
        InputKind::Close {
            reason: "test disconnect".into(),
        },
    );
    step(&mut e, p.confirmation_ns, InputKind::Clock);
    assert_eq!(e.accounts[0].unobservable, 1);
    assert_eq!(e.accounts[0].realized_usdc, Decimal::ZERO);
    assert_eq!(e.accounts[0].balances[&0], Decimal::from(9900));
    assert!(e.accounts[0].attempt.is_some());
    assert!(e.accounts[0]
        .paused
        .as_ref()
        .unwrap()
        .starts_with("unresolved"));
}
#[test]
fn unchanged_snapshot_does_not_refill_shadow() {
    let cfg = config();
    let u = universe(&cfg);
    let up = book::parse(&raw(0, "9.99", "10", "20", 1), &u, 1, 1)
        .unwrap()
        .unwrap();
    let mut shadow = Shadow::default();
    shadow.update(&up);
    let mut b = Book::default();
    b.apply(&up);
    let edge = bellman_arb::market::Edge {
        market: 0,
        buy: true,
    };
    let o = quantity::prepare(edge, Decimal::from(100), &b, &u, &cfg, 1, Decimal::ZERO).unwrap();
    quantity::execute(&o, &b, &u, &cfg, 1, Some(&mut shadow)).unwrap();
    shadow.update(&up);
    assert_eq!(
        shadow.available(edge, &up.observation.levels[1][0]),
        Decimal::from(10)
    );
    let newer = book::parse(&raw(0, "9.99", "10", "25", 2), &u, 2, 2)
        .unwrap()
        .unwrap();
    shadow.update(&newer);
    assert_eq!(
        shadow.available(edge, &newer.observation.levels[1][0]),
        Decimal::from(15)
    );
}
#[test]
fn empty_book_is_known_zero_fill_and_reappearance_replenishes() {
    let mut e = ready();
    let p = e.accounts[0]
        .attempt
        .as_ref()
        .unwrap()
        .pending
        .as_ref()
        .unwrap()
        .clone();
    let text =
        json!({"channel":"l2Book","data":{"coin":"@1","time":2,"levels":[[],[]]}}).to_string();
    let ev = step(&mut e, p.arrival_ns - 1, InputKind::Frame { text });
    assert!(ev
        .iter()
        .any(|x| x["type"] == "episode_ended" && x["censored"] == false));
    step(&mut e, p.confirmation_ns, InputKind::Clock);
    assert_eq!(e.accounts[0].unobservable, 0);
    assert_eq!(e.accounts[0].failed, 1);
    assert_eq!(e.accounts[0].balances[&0], Decimal::from(10000));
    let up = book::parse(&raw(0, "9.99", "10", "20", 3), &e.universe, 1, 1)
        .unwrap()
        .unwrap();
    e.accounts[0].shadow.update(&up);
    let edge = bellman_arb::market::Edge {
        market: 0,
        buy: true,
    };
    assert_eq!(
        e.accounts[0]
            .shadow
            .available(edge, &up.observation.levels[1][0]),
        Decimal::from(20)
    );
    let null = book::parse(
        r#"{"channel":"bbo","data":{"coin":"@1","time":4,"bbo":[null,null]}}"#,
        &e.universe,
        2,
        2,
    )
    .unwrap()
    .unwrap();
    e.books[0].apply(&null);
    assert!(e.books[0].known_empty(true, 2, &e.config));
}
#[test]
fn restart_preserves_consumed_shadow_liquidity() {
    let mut e = ready();
    let up = book::parse(&raw(0, "9.99", "10", "20", 2), &e.universe, 1, 1)
        .unwrap()
        .unwrap();
    let edge = bellman_arb::market::Edge {
        market: 0,
        buy: true,
    };
    let a = &mut e.accounts[0];
    a.shadow.update(&up);
    a.shadow.consume(edge, Decimal::from(10), Decimal::from(7));
    a.interrupt("restart");
    let mut saved: bellman_arb::paper::Account =
        serde_json::from_str(&serde_json::to_string(a).unwrap()).unwrap();
    saved.shadow.update(&up);
    assert_eq!(
        saved.shadow.available(edge, &up.observation.levels[1][0]),
        Decimal::from(13)
    );
}
#[test]
fn restart_pointer_survives_a_backward_wall_clock() {
    let root = std::env::temp_dir().join(format!(
        "bellman-clock-{}",
        bellman_arb::hyperliquid::utc_ns()
    ));
    let cfg = config();
    for id in ["run-200", "run-100"] {
        let j = Journal::open(
            &root,
            Manifest {
                format: 2,
                run_id: id.into(),
                created_utc_ns: 0,
                config: cfg.clone(),
                universe: universe(&cfg),
                initial_accounts: None,
                source_version: "test".into(),
                cpu_quota: "test".into(),
                raw_metadata: None,
                paper_epoch: None,
                predecessor_run_id: None,
            },
        )
        .unwrap();
        j.finish(json!({})).unwrap();
    }
    assert_eq!(
        bellman_arb::journal::latest(&root).unwrap().unwrap(),
        root.join("run-100")
    );
    std::fs::remove_file(root.join("run-100/manifest.json")).unwrap();
    assert!(bellman_arb::journal::latest(&root).is_err());
    std::fs::remove_dir_all(root).unwrap();
}
#[test]
fn decimal_fees_lots_and_minimum_notional() {
    let mut cfg = config();
    cfg.taker_fee_bps = dec("7.125").unwrap();
    let u = universe(&cfg);
    let up = book::parse(&raw(0, "9.99", "10", "1000", 1), &u, 1, 1)
        .unwrap()
        .unwrap();
    let mut b = Book::default();
    b.apply(&up);
    let edge = bellman_arb::market::Edge {
        market: 0,
        buy: true,
    };
    assert!(quantity::prepare(edge, dec("9.99").unwrap(), &b, &u, &cfg, 1, Decimal::ZERO).is_err());
    let o = quantity::prepare(
        edge,
        dec("100.009").unwrap(),
        &b,
        &u,
        &cfg,
        1,
        Decimal::ZERO,
    )
    .unwrap();
    let f = quantity::execute(&o, &b, &u, &cfg, 1, None).unwrap();
    assert_eq!(f.spent, Decimal::from(100));
    assert_eq!(f.qty, Decimal::from(10));
    assert_eq!(f.received + f.fee, f.gross);
    assert_eq!(f.fee, dec("0.007125").unwrap());
}
#[test]
fn bbo_does_not_refresh_stale_depth_or_pongs_refresh_books() {
    let mut cfg = config();
    cfg.depth_age_ms = 1;
    let u = universe(&cfg);
    let mut b = Book::default();
    b.apply(
        &book::parse(&raw(0, "9.99", "10", "1000", 10), &u, 1, 1)
            .unwrap()
            .unwrap(),
    );
    let bbo=json!({"channel":"bbo","data":{"coin":"@1","time":11,"bbo":[{"px":"9.99","sz":"1000"},{"px":"10","sz":"1000"}]}}).to_string();
    b.apply(
        &book::parse(&bbo, &u, 2_000_000, 2_000_000)
            .unwrap()
            .unwrap(),
    );
    assert!(b.top(2_000_000, &cfg).is_some());
    assert!(b.depth(2_000_000, &cfg).is_err());
    assert!(book::parse(r#"{"channel":"pong"}"#, &u, 3, 3)
        .unwrap()
        .is_none());
    assert!(!b.apply(
        &book::parse(&raw(0, "9", "10", "1000", 9), &u, 3_000_000, 3_000_000)
            .unwrap()
            .unwrap()
    ));
}
#[test]
fn invalid_prices_sizes_and_crosses_rejected() {
    let cfg = config();
    let u = universe(&cfg);
    for (b, a, s) in [
        ("NaN", "10", "1"),
        ("11", "10", "1"),
        ("9", "10", "0"),
        ("0", "10", "1"),
    ] {
        assert!(book::parse(&raw(0, b, a, s, 1), &u, 1, 1).is_err());
    }
    assert!(book::parse(
        r#"{"channel":"bbo","data":{"coin":"@1","time":1,"bbo":[null]}}"#,
        &u,
        1,
        1
    )
    .is_err());
    assert!(book::parse(
        r#"{"channel":"subscriptionResponse","data":{"method":"subscribe"}}"#,
        &u,
        1,
        1
    )
    .unwrap()
    .is_none());
}
#[test]
fn no_arbitrage_moving_single_market() {
    let cfg = config();
    let mut u = universe(&cfg);
    u.markets.truncate(1);
    assert!(u.routes(&cfg).unwrap().is_empty());
    for i in 0..100 {
        let p = 10.0 + i as f64;
        let es = [
            (
                bellman_arb::market::Edge {
                    market: 0,
                    buy: false,
                },
                -p.ln(),
            ),
            (
                bellman_arb::market::Edge {
                    market: 0,
                    buy: true,
                },
                (p + 1.0).ln(),
            ),
        ];
        assert!(negative_cycle(&u, &es).unwrap().is_none());
    }
}
#[test]
fn bf_finds_forward_witness() {
    let e = ready();
    let mut weights = Vec::new();
    for m in 0..e.universe.markets.len() {
        for buy in [false, true] {
            let edge = bellman_arb::market::Edge { market: m, buy };
            use rust_decimal::prelude::ToPrimitive;
            let p = e.books[m]
                .price(edge, e.now, &e.config)
                .unwrap()
                .to_f64()
                .unwrap();
            weights.push((edge, if buy { p.ln() } else { -p.ln() }));
        }
    }
    let c = negative_cycle(&e.universe, &weights).unwrap().unwrap();
    for i in 0..c.len() {
        assert_eq!(c[i].to(&e.universe), c[(i + 1) % c.len()].from(&e.universe));
    }
}
#[test]
fn clock_jump_does_not_change_monotonic_latency() {
    let mut e = ready();
    let at = e.now + 10;
    let mut x = input(&e, at, InputKind::Clock);
    x.receipt_utc_ns = 1;
    e.step(&x).unwrap();
    assert_eq!(e.stats.utc_backsteps, 1);
    assert!(
        !e.accounts[0]
            .attempt
            .as_ref()
            .unwrap()
            .pending
            .as_ref()
            .unwrap()
            .arrived
    );
}
#[test]
fn restart_keeps_reserved_funds_and_marks_pending_unresolved() {
    let mut e = ready();
    let before = e.accounts[0].balances.clone();
    e.accounts[0].interrupt("gap");
    assert!(e.accounts[0]
        .paused
        .as_ref()
        .unwrap()
        .starts_with("unresolved"));
    assert_eq!(before, e.accounts[0].balances);
    assert!(e.accounts[0].attempt.is_some());
}
#[test]
fn journal_roundtrip_replays_actual_path() {
    let cfg = config();
    let u = universe(&cfg);
    let temp = std::env::temp_dir().join(format!(
        "bellman-test-{}",
        bellman_arb::hyperliquid::utc_ns()
    ));
    let m = Manifest {
        format: 2,
        run_id: "run-1".into(),
        created_utc_ns: 0,
        config: cfg.clone(),
        universe: u.clone(),
        initial_accounts: None,
        source_version: "test".into(),
        cpu_quota: "test".into(),
        raw_metadata: None,
        paper_epoch: None,
        predecessor_run_id: None,
    };
    let j = Journal::open(&temp, m).unwrap();
    let dir = j.dir.clone();
    let mut e = Engine::new(cfg, u, None).unwrap();
    e.model_version = 2;
    let mut kinds = vec![(0, InputKind::Open)];
    for (m, b, a) in [(0, "9.99", "10"), (1, "2.02", "2.021"), (2, "5", "5.001")] {
        kinds.push((
            m as u64 + 1,
            InputKind::Frame {
                text: raw(m, b, a, "1000", 1),
            },
        ));
    }
    kinds.push((1_500_000_003, InputKind::Clock));
    for (now, kind) in kinds {
        let x = input(&e, now, kind);
        let events = e.step(&x).unwrap();
        j.record(Record {
            input: x,
            completed_ns: now,
            events,
        })
        .unwrap();
    }
    j.finish(e.report()).unwrap();
    let replay = bellman_arb::journal::replay(&dir, None, true).unwrap();
    assert_eq!(
        serde_json::to_value(e.accounts).unwrap(),
        serde_json::to_value(replay.accounts).unwrap()
    );
    std::fs::remove_dir_all(temp).unwrap();
}

#[test]
fn overlapping_cycles_are_all_evaluated() {
    let cfg = config();
    let mut u = universe(&cfg);
    let mut t = u.tokens[&2].clone();
    t.index = 3;
    t.name = "C".into();
    t.token_id = "c".into();
    u.tokens.insert(3, t);
    let mut m = u.markets[1].clone();
    m.index = 4;
    m.coin = "@4".into();
    m.quote = 3;
    u.markets.push(m);
    let mut m = u.markets[2].clone();
    m.index = 5;
    m.coin = "@5".into();
    m.base = 3;
    u.markets.push(m);
    let mut e = Engine::new(cfg, u, None).unwrap();
    step(&mut e, 0, InputKind::Open);
    for (m, b, a) in [
        (0, "9.99", "10"),
        (1, "2.02", "2.021"),
        (2, "5", "5.001"),
        (3, "2.04", "2.041"),
        (4, "5", "5.001"),
    ] {
        step(
            &mut e,
            m as u64 + 1,
            InputKind::Frame {
                text: raw(m, b, a, "1000", 1),
            },
        );
    }
    for id in ["1B>2S>3S", "1B>4S>5S"] {
        let i = e.routes.iter().position(|r| r.id == id).unwrap();
        assert!(e.states[i].net_bps.unwrap() > 5.0);
    }
}
#[test]
fn cycle_bound_and_capacity_are_explicit() {
    let raw: Value =
        serde_json::from_str(include_str!("../review/hyperliquid_spot_snapshot.json")).unwrap();
    let mut cfg = Config::default();
    cfg.max_cycle_len = 3;
    let u = Universe::parse(&raw, &cfg).unwrap();
    assert_eq!(u.routes(&cfg).unwrap().len(), 28);
    cfg.max_cycles = 1;
    assert!(u.routes(&cfg).is_err());
}
#[test]
fn older_cross_channel_update_cannot_refresh_liquidity() {
    let cfg = config();
    let u = universe(&cfg);
    let mut b = Book::default();
    b.apply(
        &book::parse(&raw(0, "9.99", "10", "5", 20), &u, 1, 1)
            .unwrap()
            .unwrap(),
    );
    let older=json!({"channel":"bbo","data":{"coin":"@1","time":19,"bbo":[{"px":"9.99","sz":"5000"},{"px":"10","sz":"5000"}]}}).to_string();
    assert!(!b.apply(&book::parse(&older, &u, 2, 2).unwrap().unwrap()));
}
#[test]
fn subminimum_residual_is_preserved_as_inventory() {
    let mut e = ready();
    let p = e.accounts[0]
        .attempt
        .as_ref()
        .unwrap()
        .pending
        .as_ref()
        .unwrap()
        .clone();
    step(
        &mut e,
        p.arrival_ns - 1,
        InputKind::Frame {
            text: raw(0, "9.99", "10", "1", 2),
        },
    );
    step(&mut e, p.confirmation_ns + 500_000_000, InputKind::Clock);
    assert_eq!(e.accounts[0].balances[&0], Decimal::from(9990));
    assert_eq!(e.accounts[0].balances[&1], Decimal::ONE);
    assert!(e.accounts[0].paused.is_some());
}
#[test]
fn failed_unwind_retains_exposure_and_pauses_account() {
    let mut e = ready();
    let p = e.accounts[0]
        .attempt
        .as_ref()
        .unwrap()
        .pending
        .as_ref()
        .unwrap()
        .clone();
    step(
        &mut e,
        p.arrival_ns - 1,
        InputKind::Frame {
            text: raw(0, "9.99", "10", "2", 2),
        },
    );
    step(&mut e, p.confirmation_ns, InputKind::Clock);
    let unwind = e.accounts[0]
        .attempt
        .as_ref()
        .unwrap()
        .pending
        .as_ref()
        .unwrap()
        .clone();
    step(
        &mut e,
        unwind.arrival_ns - 1,
        InputKind::Frame {
            text: raw(0, "8", "10", "1000", 3),
        },
    );
    step(&mut e, unwind.confirmation_ns, InputKind::Clock);
    assert!(e.accounts[0].paused.is_some());
    assert_eq!(e.accounts[0].balances[&1], Decimal::from(2));
    assert_eq!(e.accounts[0].balances[&0], Decimal::from(9980));
}
#[test]
fn episode_end_has_no_grace_inflation() {
    let mut e = ready();
    let start = e.states.iter().find_map(|s| s.positive_since).unwrap();
    let ev = step(
        &mut e,
        1003,
        InputKind::Frame {
            text: raw(1, "1.9", "2.021", "1000", 2),
        },
    );
    let end = ev.iter().find(|x| x["type"] == "episode_ended").unwrap();
    assert_eq!(end["first_positive_ns"], start);
    assert_eq!(end["first_failure_ns"], 1003);
    assert_eq!(end["censored"], false);
    assert!(end["observed_span_ns"].as_u64().unwrap() < 1003);
}
#[test]
fn journal_limits_and_disk_errors_surface() {
    let root =
        std::env::temp_dir().join(format!("bellman-io-{}", bellman_arb::hyperliquid::utc_ns()));
    let mut cfg = config();
    cfg.record_limit_bytes = 64_000;
    cfg.rotate_bytes = 1000;
    let make = |name: &str| Manifest {
        format: 2,
        run_id: name.into(),
        created_utc_ns: 0,
        config: cfg.clone(),
        universe: universe(&cfg),
        initial_accounts: None,
        source_version: "test".into(),
        cpu_quota: "test".into(),
        raw_metadata: None,
        paper_epoch: None,
        predecessor_run_id: None,
    };
    let j = Journal::open(&root, make("run-1")).unwrap();
    let mut e = ready();
    let x = input(
        &e,
        10,
        InputKind::Frame {
            text: "x".repeat(64_000),
        },
    );
    let ev = e.step(&x).unwrap();
    let _ = j.record(Record {
        input: x,
        completed_ns: 10,
        events: ev,
    });
    assert!(j.finish(json!({})).is_err());
    assert!(root.join("RECORDING_LIMIT_REACHED").exists());
    let j = Journal::open(&root, make("run-2")).unwrap();
    std::fs::create_dir(j.dir.join("final.json")).unwrap();
    assert!(j.finish(json!({})).is_err());
    let mut too_small = make("run-3");
    too_small.config.record_limit_bytes = 100;
    assert!(Journal::open(&root, too_small).is_err());
    assert!(root.join("RECORDING_LIMIT_REACHED").exists());
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn processing_delay_is_added_to_submission_and_replayed() {
    let cfg = config();
    let mut e = Engine::new(cfg.clone(), universe(&cfg), None).unwrap();
    step(&mut e, 0, InputKind::Open);
    step(
        &mut e,
        1,
        InputKind::Frame {
            text: raw(0, "9.99", "10", "1000", 1),
        },
    );
    step(
        &mut e,
        2,
        InputKind::Frame {
            text: raw(1, "2.02", "2.021", "1000", 1),
        },
    );
    let x = input(
        &e,
        3,
        InputKind::Frame {
            text: raw(2, "5", "5.001", "1000", 1),
        },
    );
    let mut events = e.step(&x).unwrap();
    e.complete_step(&x, 1_000_003, &mut events).unwrap();
    let p = e.accounts[0]
        .attempt
        .as_ref()
        .unwrap()
        .pending
        .as_ref()
        .unwrap();
    assert_eq!(p.submitted_ns, 1_000_003);
    assert_eq!(p.arrival_ns, 251_000_003);
    step(&mut e, 251_000_003, InputKind::Clock);
    step(&mut e, 551_000_003, InputKind::Clock);
    let p = e.accounts[0]
        .attempt
        .as_ref()
        .unwrap()
        .pending
        .as_ref()
        .unwrap();
    assert_eq!(
        p.submitted_ns, 551_000_003,
        "late confirmation must not backdate next submission"
    );
}

#[test]
fn hot_pipeline_matches_reference_and_replays() {
    for model in [2, 4] {
        let cfg = config();
        let u = universe(&cfg);
        let mut live = Engine::new(cfg.clone(), u.clone(), None).unwrap();
        let mut reference = Engine::new(cfg.clone(), u.clone(), None).unwrap();
        live.model_version = model;
        reference.model_version = model;
        let mut hot = bellman_arb::hot::Core::new(&u, &live.routes, &cfg);
        let mut rates = Vec::with_capacity(live.routes.len());
        let root = std::env::temp_dir().join(format!(
            "bellman-hot-replay-{}",
            bellman_arb::hyperliquid::utc_ns()
        ));
        let j = Journal::open(
            &root,
            Manifest {
                format: model,
                run_id: "run-1".into(),
                created_utc_ns: 0,
                config: cfg,
                universe: u.clone(),
                initial_accounts: None,
                source_version: "test".into(),
                cpu_quota: "test".into(),
                raw_metadata: None,
                paper_epoch: None,
                predecessor_run_id: None,
            },
        )
        .unwrap();
        let dir = j.dir.clone();
        let mut stream = vec![(10, InputKind::Open)];
        for (m, b, a) in [(0, "9.99", "10"), (1, "2.02", "2.021"), (2, "5", "5.001")] {
            stream.push((
                20 + m as u64 * 10,
                InputKind::Frame {
                    text: raw(m, b, a, "1000", 1),
                },
            ));
        }
        for ms in [250, 500, 750, 1000, 1250, 1500, 1750] {
            stream.push((ms * 1_000_000, InputKind::Clock));
        }
        stream.push((
            2_000_000_000,
            InputKind::Close {
                reason: "gap".into(),
            },
        ));
        for (now, kind) in stream {
            let mut x = input(&live, now, kind);
            x.hot_started_ns = now + 1;
            x.hot_done_ns = now + 2;
            x.process_ns = now + 3;
            hot.evaluate(
                bellman_arb::hot::normalize(&x.event, &u, x.generation, x.receipt_ns),
                x.hot_started_ns,
                &mut rates,
            );
            let mut a = live.step_hot(&x, &rates).unwrap();
            let mut b = reference.step(&x).unwrap();
            live.complete_step(&x, now + 4, &mut a).unwrap();
            reference.complete_step(&x, now + 4, &mut b).unwrap();
            assert_eq!(a, b);
            assert_eq!(
                serde_json::to_value(&live.states).unwrap(),
                serde_json::to_value(&reference.states).unwrap()
            );
            j.record(Record {
                input: x,
                completed_ns: now + 4,
                events: a,
            })
            .unwrap();
        }
        j.finish(live.report()).unwrap();
        let restored = bellman_arb::journal::replay(&dir, None, true).unwrap();
        assert_eq!(
            serde_json::to_value(live.accounts).unwrap(),
            serde_json::to_value(restored.accounts).unwrap()
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn v4_quantity_expiry_and_truncated_remainders_do_not_fabricate_fills() {
    let mut cfg = Config {
        l2_fast: true,
        ..Config::default()
    };
    let u = universe(&cfg);
    for buy in [true, false] {
        let edge = bellman_arb::market::Edge { market: 0, buy };
        let up = book::parse(&ladder(99, 100, 5, 1), &u, 1, 1)
            .unwrap()
            .unwrap();
        let mut book = Book::default();
        book.apply(&up);
        let mut shadow = Shadow::default();
        shadow.update_model(&up, Some(5), 4);
        let mut order = quantity::Order {
            edge,
            qty: Decimal::from(60),
            limit: Decimal::from(if buy { 105 } else { 94 }),
            budget: Decimal::from(10000),
            source: None,
        };
        assert!(quantity::execute_model(&order, &book, &u, &cfg, 1, Some(&mut shadow), 4).is_err());
        assert_eq!(
            shadow.available(edge, &up.observation.levels[usize::from(buy)][0]),
            Decimal::from(10)
        );
        order.limit = Decimal::from(if buy { 104 } else { 95 });
        assert_eq!(
            quantity::execute_model(&order, &book, &u, &cfg, 1, None, 4)
                .unwrap()
                .qty,
            Decimal::from(50)
        );
        let quote = bbo(0, "99", "2", "100", "2", 2);
        book.apply(&book::parse(&quote, &u, 2, 2).unwrap().unwrap());
        cfg.depth_age_ms = 10;
        assert!(book.execution_side(buy, 11_000_002, &cfg, None, 4).is_err());
        order.limit = Decimal::from(if buy { 99 } else { 100 });
        assert_eq!(
            quantity::execute_model(&order, &book, &u, &cfg, 11_000_002, None, 4)
                .unwrap()
                .qty,
            Decimal::ZERO
        );
        cfg.depth_age_ms = 1000;
    }
}

#[test]
fn ioc_precision_never_exceeds_adverse_bound() {
    let p = dec("1.23456").unwrap();
    assert!(quantity::limit_price(p, 3, true) <= p);
    assert!(quantity::limit_price(p, 3, false) >= p);
}

fn ladder(bid: i64, ask: i64, count: usize, time: u64) -> String {
    let rows = |buy: bool| {
        (0..count)
            .map(|i| json!({"px":(if buy {ask+i as i64}else{bid-i as i64}).to_string(),"sz":"10"}))
            .collect::<Vec<_>>()
    };
    json!({"channel":"l2Book","data":{"coin":"@1","time":time,"levels":[rows(false),rows(true)]}})
        .to_string()
}

#[test]
fn truncated_snapshots_preserve_consumed_liquidity_on_both_sides() {
    for count in [5, 20] {
        for buy in [false, true] {
            let cfg = Config {
                l2_fast: count == 5,
                ..config()
            };
            let mut e = Engine::new(cfg.clone(), universe(&cfg), None).unwrap();
            step(&mut e, 0, InputKind::Open);
            step(
                &mut e,
                1,
                InputKind::Frame {
                    text: ladder(99, 100, count, 1),
                },
            );
            let edge = bellman_arb::market::Edge { market: 0, buy };
            let price = Decimal::from(if buy {
                100 + count as i64 - 1
            } else {
                99 - count as i64 + 1
            });
            let level = book::Level {
                px: price,
                sz: Decimal::from(10),
            };
            e.accounts[0].shadow.consume(edge, price, Decimal::from(7));
            // The consumed worst level moves just outside the visible window.
            let hidden = if buy {
                ladder(98, 99, count, 2)
            } else {
                ladder(100, 101, count, 2)
            };
            step(&mut e, 2, InputKind::Frame { text: hidden });
            step(
                &mut e,
                3,
                InputKind::Frame {
                    text: ladder(99, 100, count, 3),
                },
            );
            assert_eq!(
                e.accounts[0].shadow.available(edge, &level),
                Decimal::from(3)
            );
            // A gap within the observed range is evidence of actual removal.
            let mut removed: Value = serde_json::from_str(&ladder(99, 100, count, 4)).unwrap();
            removed["data"]["levels"][usize::from(buy)][count - 1]["px"] = (price
                + if buy { Decimal::ONE } else { -Decimal::ONE })
            .to_string()
            .into();
            step(
                &mut e,
                4,
                InputKind::Frame {
                    text: removed.to_string(),
                },
            );
            step(
                &mut e,
                5,
                InputKind::Frame {
                    text: ladder(99, 100, count, 5),
                },
            );
            assert_eq!(
                e.accounts[0].shadow.available(edge, &level),
                Decimal::from(10)
            );
        }
    }
}

#[test]
fn fast_snapshot_replaces_old_deep_liquidity() {
    let cfg = Config {
        l2_fast: true,
        ..config()
    };
    let u = universe(&cfg);
    let mut b = Book::default();
    b.apply(
        &book::parse(&ladder(99, 100, 20, 1), &u, 1, 1)
            .unwrap()
            .unwrap(),
    );
    let edge = bellman_arb::market::Edge {
        market: 0,
        buy: true,
    };
    let order =
        quantity::prepare(edge, Decimal::from(6000), &b, &u, &cfg, 1, Decimal::ZERO).unwrap();
    b.apply(
        &book::parse(&ladder(99, 100, 5, 2), &u, 2, 2)
            .unwrap()
            .unwrap(),
    );
    assert_eq!(b.depth(2, &cfg).unwrap().levels[1].len(), 5);
    assert!(quantity::prepare(edge, Decimal::from(6000), &b, &u, &cfg, 2, Decimal::ZERO).is_err());
    let fill = quantity::execute(&order, &b, &u, &cfg, 2, None).unwrap();
    assert_eq!(fill.qty, Decimal::from(50));
    assert!(fill.qty < order.qty);
    assert_eq!(fill.spent, Decimal::from(5100));
}

#[test]
fn replay_preserves_legacy_shadow_model_and_uses_new_model_for_format_two() {
    let root = std::env::temp_dir().join(format!(
        "bellman-depth-model-{}",
        bellman_arb::hyperliquid::utc_ns()
    ));
    let cfg = config();
    let u = universe(&cfg);
    let mut initial = bellman_arb::paper::Account::new(250, &cfg, 0);
    let first = book::parse(&ladder(99, 100, 20, 1), &u, 1, 1)
        .unwrap()
        .unwrap();
    initial.shadow.update(&first);
    let edge = bellman_arb::market::Edge {
        market: 0,
        buy: true,
    };
    initial
        .shadow
        .consume(edge, Decimal::from(119), Decimal::from(7));
    for format in [1, 2] {
        let j = Journal::open(
            &root,
            Manifest {
                format,
                run_id: format!("run-{format}"),
                created_utc_ns: 0,
                config: cfg.clone(),
                universe: u.clone(),
                initial_accounts: Some(vec![initial.clone()]),
                source_version: "test".into(),
                cpu_quota: "test".into(),
                raw_metadata: None,
                paper_epoch: None,
                predecessor_run_id: None,
            },
        )
        .unwrap();
        let dir = j.dir.clone();
        for (i, event) in [
            InputKind::Open,
            InputKind::Frame {
                text: ladder(98, 99, 20, 2),
            },
            InputKind::Frame {
                text: ladder(99, 100, 20, 3),
            },
        ]
        .into_iter()
        .enumerate()
        {
            let n = i as u64 + 1;
            j.record(Record {
                input: Input {
                    sequence: n,
                    generation: 1,
                    receipt_ns: n,
                    receipt_utc_ns: n,
                    process_ns: n,
                    hot_started_ns: 0,
                    hot_done_ns: 0,
                    event,
                },
                completed_ns: n,
                events: vec![],
            })
            .unwrap();
        }
        j.finish(json!({})).unwrap();
        let e = bellman_arb::journal::replay(&dir, None, true).unwrap();
        let visible = e.accounts[0].shadow.available(
            edge,
            &book::Level {
                px: Decimal::from(119),
                sz: Decimal::from(10),
            },
        );
        assert_eq!(visible, Decimal::from(if format == 1 { 10 } else { 3 }));
    }
    std::fs::remove_dir_all(root).unwrap();
}
