//! Separate test process: allocation counting covers only the numeric hot kernel.
use bellman_arb::{
    config::Config,
    hot::{Core, Signal},
    market::Universe,
};
use std::{
    alloc::{GlobalAlloc, Layout, System},
    cell::Cell,
    collections::BTreeSet,
    hint::black_box,
    time::Instant,
};
thread_local! {static TRACK:Cell<bool>=const{Cell::new(false)};static ALLOCS:Cell<usize>=const{Cell::new(0)};}
struct Counting;
fn count() {
    let _ = TRACK.try_with(|t| {
        if t.get() {
            ALLOCS.with(|n| n.set(n.get() + 1));
        }
    });
}
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        count();
        unsafe { System.alloc(l) }
    }
    unsafe fn alloc_zeroed(&self, l: Layout) -> *mut u8 {
        count();
        unsafe { System.alloc_zeroed(l) }
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 {
        count();
        unsafe { System.realloc(p, l, n) }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        unsafe { System.dealloc(p, l) }
    }
}
#[global_allocator]
static ALLOCATOR: Counting = Counting;
#[test]
fn all_affected_cycles_without_hot_allocations() {
    let data =
        serde_json::from_str(include_str!("../review/hyperliquid_spot_snapshot.json")).unwrap();
    let cfg = Config::default();
    let u = Universe::parse(&data, &cfg).unwrap();
    let routes = u.routes(&cfg).unwrap();
    let mut core = Core::new(&u, &routes, &cfg);
    let mut out = Vec::with_capacity(routes.len());
    let selected: Vec<_> = routes
        .iter()
        .flat_map(|r| r.edges.iter().map(|e| e.market))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let rates: Vec<_> = selected
        .iter()
        .map(|&m| {
            let a = &u.markets[m];
            let p = (1.0 + a.base as f64 * 0.01) / (1.0 + a.quote as f64 * 0.01);
            (m, (p * 0.9999).ln(), (p * 1.0001).ln())
        })
        .collect();
    core.evaluate(Signal::Open(1), 0, &mut out);
    for &(market, b, a) in &rates {
        core.evaluate(
            Signal::Quote {
                generation: 1,
                market,
                exchange_ms: 1,
                receipt_ns: 0,
                log_bid: b,
                log_ask: a,
            },
            0,
            &mut out,
        );
    }
    let n = 200_000;
    let mut times = Vec::with_capacity(n);
    ALLOCS.with(|c| c.set(0));
    TRACK.with(|t| t.set(true));
    for i in 0..n {
        let (m, b, a) = rates[i % rates.len()];
        let shock = if (i / 500) % 2 == 0 { 0.004 } else { 0.0 };
        let now = i as u64 * 1000;
        let t = Instant::now();
        core.evaluate(
            black_box(Signal::Quote {
                generation: 1,
                market: m,
                exchange_ms: i as u64 + 2,
                receipt_ns: now,
                log_bid: b + shock,
                log_ask: a + shock,
            }),
            now,
            &mut out,
        );
        black_box(&out);
        times.push(t.elapsed().as_nanos() as u64);
    }
    TRACK.with(|t| t.set(false));
    let allocations = ALLOCS.with(Cell::get);
    assert_eq!(allocations, 0);
    times.sort_unstable();
    println!(
        "{}",
        serde_json::json!({"iterations":n,"markets":selected.len(),"routes":routes.len(),"hot_allocations":allocations,"p50_ns":times[n/2],"p95_ns":times[n*95/100],"p99_ns":times[n*99/100],"max_ns":times[n-1],"cpu_quota":std::fs::read_to_string("/sys/fs/cgroup/cpu.max").unwrap_or_default()})
    );
}
