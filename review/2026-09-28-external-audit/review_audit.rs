// Archived 28 September 2026 diagnostic for that revision's behavior;
// compiled in a throwaway Docker copy, not part of the maintained test suite.
include!("/app/tests/research.rs");

fn audit_ready() -> Engine {
    let cfg=Config{latency_ms:vec![250],amounts_usdc:vec![Decimal::from(100)],..Config::default()};
    let mut e=Engine::new(cfg.clone(),universe(&cfg),None).unwrap();
    step(&mut e,0,InputKind::Open);
    for (m,b,a) in [(0,"9.99","10"),(1,"2.2","2.201"),(2,"5","5.001")] {
        step(&mut e,m as u64+1,InputKind::Frame{text:raw(m,b,a,"1000",1)});
    }
    assert!(e.accounts[0].attempt.is_some()); e
}

#[test]
fn audit_real_fee_unwind_dust_pauses_account() {
    let mut e=audit_ready();
    let p=e.accounts[0].attempt.as_ref().unwrap().pending.as_ref().unwrap().clone();
    step(&mut e,p.arrival_ns-1,InputKind::Frame{text:raw(0,"9.99","10","2",2)});
    for ms in [250,500,750,1000] {step(&mut e,p.submitted_ns+ms*1_000_000,InputKind::Clock);}
    let a=&e.accounts[0];
    assert!(a.paused.is_some());assert_eq!(a.unobservable,0);assert_eq!(a.failed,1);
    assert!(quantity::amount(&a.balances,1)>Decimal::ZERO && quantity::amount(&a.balances,1)<dec("0.001").unwrap());
    println!("AUDIT_DUST {}",json!({"fee_bps":e.config.taker_fee_bps,"slippage_bps":e.config.slippage_bps,"freshness_ms":e.config.depth_age_ms,"paused":a.paused,"balances":a.balances,"cash_change":a.realized_usdc}));
}

#[test]
fn audit_cash_return_and_residual_value_are_distinct() {
    let cfg=Config::default();let mut u=universe(&cfg);u.tokens.get_mut(&1).unwrap().sz_decimals=5;
    let mut books=vec![Book::default();3];
    for (m,b,a) in [(0,"84999","85000"),(1,"85340","85341"),(2,"1","1.0001")] {
        books[m].apply(&book::parse(&raw(m,b,a,"1000000",1),&u,1,1).unwrap().unwrap());
    }
    let route=u.routes(&cfg).unwrap().into_iter().find(|r| {let x=r.funded_edges(&u).unwrap();x[0].market==0 && x[0].buy}).unwrap();
    for size in [25,100,250,1000] {
        let est=quantity::estimate(&route,Decimal::from(size),&books,&u,&cfg,1).unwrap();
        let mark=quantity::amount(&est.residual,1)*Decimal::from(84999)+quantity::amount(&est.residual,2);
        println!("AUDIT_RESIDUAL {}",json!({"start":size,"cash_profit":est.profit,"cash_bps":est.bps,"residual":est.residual,"indicative_bid_value":mark,"cash_plus_mark_bps":(est.profit+mark)/Decimal::from(size)*Decimal::from(10000),"note":"Synthetic gross-positive case; marks ignore minimum notional and are not spendable cash"}));
        assert!(mark>Decimal::ZERO);
    }
}

#[test]
fn audit_timer_dispatch_lag_and_confirmation_lookahead() {
    let mut e=audit_ready();let p=e.accounts[0].attempt.as_ref().unwrap().pending.as_ref().unwrap().clone();
    step(&mut e,250_000_000,InputKind::Clock);
    assert!(!e.accounts[0].attempt.as_ref().unwrap().pending.as_ref().unwrap().arrived);
    let arrived=step(&mut e,300_000_000,InputKind::Clock);
    let lag=arrived.iter().find(|x|x["type"]=="arrived").unwrap()["data"]["processing_lag_ns"].as_u64().unwrap();
    assert!(lag>49_000_000);
    // Price moves after simulated arrival but before confirmation. The stored
    // arrival fill must not be replaced by this later book.
    step(&mut e,400_000_000,InputKind::Frame{text:raw(0,"10.9","11","1000",2)});
    assert!(e.accounts[0].attempt.as_ref().unwrap().pending.as_ref().unwrap().fill.as_ref().unwrap().qty>Decimal::ZERO);
    let later=quantity::execute(&p.order,&e.books[0],&e.universe,&e.config,500_000_000,None).unwrap();
    assert_eq!(later.qty,Decimal::ZERO);
    step(&mut e,550_000_000,InputKind::Clock);
    let next=e.accounts[0].attempt.as_ref().unwrap().pending.as_ref().unwrap().submitted_ns;
    assert_eq!(next,550_000_000);
    println!("AUDIT_TIMING {}",json!({"arrival_lateness_ns":lag,"confirmation_deadline_ns":p.confirmation_ns,"next_submission_ns":next,"later_book_would_change_fill":true}));
}

#[test]
fn audit_all_routes_fee_floor() {
    let cfg=Config::default();let v:Value=serde_json::from_str(include_str!("/app/review/hyperliquid_spot_snapshot.json")).unwrap();let u=Universe::parse(&v,&cfg).unwrap();
    let routes=u.routes(&cfg).unwrap();let mut by_length=BTreeMap::new();let mut min=1.0_f64;
    for r in routes {
        let fee_only=1.0-r.edges.iter().map(|e|1.0-bellman_arb::market::fee_f64(&u.markets[e.market])).product::<f64>();min=min.min(fee_only);
        by_length.entry(r.edges.len()).or_insert_with(Vec::new).push(fee_only*10000.0);
    }
    let summary:Vec<_>=by_length.into_iter().map(|(len,v)|json!({"legs":len,"routes":v.len(),"min_fee_drag_bps":v.iter().copied().fold(f64::INFINITY,f64::min),"max_fee_drag_bps":v.iter().copied().fold(0.0,f64::max)})).collect();
    println!("AUDIT_FEES {}",json!({"minimum_fee_drag_bps":min*10000.0,"by_length":summary}));
}
