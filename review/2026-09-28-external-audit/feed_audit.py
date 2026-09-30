"""Read-only audit of recorded Hyperliquid bbo/l2Book frames (bellman_ford runs).
Archived 28 September 2026 diagnostic: reads only events-00000.jsonl.
Not a complete-run or current execution-model audit.
Usage: python feed_audit.py <run_dir> [max_bytes]
Time axis = input.process_ns (engine 'now'); freshness stamp = receipt_ns (as engine).
"""
import json, sys, math, os, statistics as st
from decimal import Decimal
from collections import defaultdict, Counter

run = sys.argv[1]
path = os.path.join(run, 'events-00000.jsonl')
limit = int(sys.argv[2]) if len(sys.argv) > 2 else os.path.getsize(path)
man = json.load(open(os.path.join(run, 'manifest.json'), encoding='utf-8'))
mk = {m['index']: m for m in man['universe']['markets']}
disc = json.load(open(os.path.join(run, '..', 'discovery-validation.json'), encoding='utf-8')) \
    if os.path.exists(os.path.join(run, '..', 'discovery-validation.json')) else \
    json.load(open(os.path.join(run, '..', '..', 'discovery-validation.json'), encoding='utf-8'))
routes = [c['id'] for c in disc['cycles']]
rname = {c['id']: c['name'] for c in disc['cycles']}
coins = disc['selected_coins']
cidx = {c: i for i, c in enumerate(coins)}
def coin_of(idx): return 'PURR/USDC' if idx == 0 else f'@{idx}'
R = []  # route -> list of (coin_i, buy)
for r in routes:
    legs = []
    for p in r.split('>'):
        legs.append((cidx[coin_of(int(p[:-1]))], p[-1] == 'B'))
    tok=0; ordered=[]; rest=legs[:]
    while rest:
        for L in rest:
            m_=mk[0 if coins[L[0]]=='PURR/USDC' else int(coins[L[0]][1:])]
            if (m_['quote'] if L[1] else m_['base'])==tok:
                ordered.append(L); rest.remove(L); tok=m_['base'] if L[1] else m_['quote']; break
        else: raise SystemExit('route not chainable '+r)
    assert tok==0, r
    R.append(ordered)
fee_log = [math.log1p(-float(mk[0 if c == 'PURR/USDC' else int(c[1:])]['fee'])) for c in coins]
by_coin = defaultdict(list)
for ri, legs in enumerate(R):
    for ci, _ in legs: by_coin[ci].append(ri)
AGE = 1_000_000_000
LOG5 = math.log1p(5e-4)

def D(x): return Decimal(x).normalize()
def lvl(l): return (D(l['px']), D(l['sz'])) if l else None

# per coin/channel stats
S = {ch: [dict(n=0, rx=[], ex=[], same=0, same_n=0, same_top=0, lat=[], prev=None, prevn=None, prevtop=None,
               prev_rx=None, prev_ex=None, crossed=0, locked=0, empty=0, badsz=0, ex_back=0, same_ex=0)
          for _ in coins] for ch in ('bbo', 'l2Book')}
seq_prev = None; seq_gaps = 0; seq_dups = 0; gen_back = 0; gen_prev = 0; rx_back = 0; proc_back = 0; rx_prev = 0; proc_prev = 0
l2times = Counter(); arr = Counter(); utc_back = []; utc_prev = 0; rx_back_mag = []
kinds = Counter(); stale_gen_frames = 0; closes = []; opens = []
cross_older = Counter()  # frames whose exchange time < latest other-channel time for coin (engine would reject)
# timeline state
cur_gen = None; connected = False
bbo = [None] * len(coins)   # (rx, ex, top(bid,ask), logbid, logask, px/sz floats)
dep = [None] * len(coins)
conn_time = 0
T = [defaultdict(int) for _ in coins]
dis_start = [None] * len(coins); dis_eps = [[] for _ in coins]; dis_end_by = [Counter() for _ in coins]
# l2Book silent while bbo changes
bbo_changes_since_dep = [0] * len(coins); silent_eps = [[] for _ in coins]
# route economics
rv = {v: [None] * len(routes) for v in ('A', 'B')}  # net log per route (A=bbo channel last known, B=engine latest-of-both)
rmax = {v: [(-1e9, None)] * len(routes) for v in ('A', 'Af', 'B', 'Bf')}
tpos = Counter()
first_t = None; last_t = None

def top_of(v, ci):
    if v == 'A':
        return bbo[ci]
    a, b = bbo[ci], dep[ci]
    if a and b: return a if (a['ex'], a['av']) > (b['ex'], b['av']) else b
    return a or b

def route_eval(v, ri):
    net = 0.0; vu = 1 << 62
    for ci, buy in R[ri]:
        q = top_of(v, ci)
        if q is None or q['lb'] is None or q['la'] is None: return None, 0
        net += (-q['la'] if buy else q['lb']) + fee_log[ci]
        vu = min(vu, q['rx'] + AGE)
    return net, vu

def snapshot(v, ri, t):
    out = []; amt = 1.0; cap = 1e18
    for ci, buy in R[ri]:
        q = top_of(v, ci)
        (bp, bs), (ap, as_) = q['top']
        if buy:
            c_in = float(ap) * float(as_); amt_next = amt / float(ap)
        else:
            c_in = float(bs); amt_next = amt * float(bp)
        cap = min(cap, c_in / amt)
        out.append(f"{coins[ci]}{'B' if buy else 'S'} bid {bp}x{bs} ask {ap}x{as_} age {(t - q['rx'])/1e6:.0f}ms")
        amt = amt_next
    return dict(t=t, legs=out, bottleneck_usdc=cap)

def upd_routes(ci, t):
    for v in ('A', 'B'):
        for ri in by_coin[ci]:
            net, vu = route_eval(v, ri)
            rv[v][ri] = (net, vu) if net is not None else None
            if net is None: continue
            if net > rmax[v][ri][0]: rmax[v][ri] = (net, snapshot(v, ri, t))
            if t <= vu and net > rmax[v + 'f'][ri][0]: rmax[v + 'f'][ri] = (net, snapshot(v, ri, t))

def integrate(t0, t1):
    global conn_time
    dt = t1 - t0
    if dt <= 0 or not connected: return
    conn_time += dt
    def ov(until): return max(0, min(t1, until) - t0)
    for ci in range(len(coins)):
        b, d = bbo[ci], dep[ci]
        Tc = T[ci]
        if b: Tc['bbo_fresh'] += ov(b['rx'] + AGE)
        if d: Tc['dep_fresh'] += ov(d['rx'] + AGE)
        eq = b is not None and d is not None and b['top'] == d['top']
        if eq: Tc['eq_lastknown'] += dt
        if b and d and eq: Tc['both_fresh_eq'] += ov(min(b['rx'], d['rx']) + AGE)
        lt = top_of('B', ci)
        if lt: Tc['latest_fresh'] += ov(lt['rx'] + AGE)
        if lt and d and lt['top'] == d['top']:
            Tc['engine_depth_ok'] += ov(min(lt['rx'], d['rx']) + AGE)
    for v in ('A', 'B'):
        vals = [x for x in rv[v] if x is not None]
        if any(x[0] > 0 for x in vals): tpos[v + '>0'] += dt
        if any(x[0] > LOG5 for x in vals): tpos[v + '>5'] += dt
        u0 = max([x[1] for x in vals if x[0] > 0], default=0); tpos[v + 'f>0'] += ov(u0)
        u5 = max([x[1] for x in vals if x[0] > LOG5], default=0); tpos[v + 'f>5'] += ov(u5)
        tpos[v + '_allobs'] += dt if len(vals) == len(routes) else 0

def reset_books(t):
    for ci in range(len(coins)):
        if dis_start[ci] is not None:
            dis_eps[ci].append(t - dis_start[ci]); dis_end_by[ci]['session_end'] += 1; dis_start[ci] = None
        bbo[ci] = None; dep[ci] = None; bbo_changes_since_dep[ci] = 0
    for v in ('A', 'B'): rv[v] = [None] * len(routes)

with open(path, 'rb') as f:
    pos = 0
    for line in f:
        pos += len(line)
        if pos > limit or not line.endswith(b'\n'): break
        r = json.loads(line); inp = r['input']; ev = inp['event']; k = ev['kind']; kinds[k] += 1
        seq = inp['sequence']; gen = inp['generation']; rx = inp['receipt_ns']; t = inp['process_ns']
        if seq_prev is not None:
            if seq == seq_prev: seq_dups += 1
            elif seq != seq_prev + 1: seq_gaps += 1
        seq_prev = seq
        if gen < gen_prev: gen_back += 1
        gen_prev = max(gen_prev, gen)
        if rx < rx_prev: rx_back += 1; rx_back_mag.append((rx_prev - rx) / 1e6)
        u_ = inp['receipt_utc_ns']
        if u_ > 0:
            if u_ < utc_prev: utc_back.append((seq, (utc_prev - u_) / 1e6))
            utc_prev = u_
        if t < proc_prev: proc_back += 1
        rx_prev = max(rx, rx_prev)
        if first_t is None: first_t = t
        if last_t is not None: integrate(last_t, t)
        last_t = max(t, last_t or t); proc_prev = last_t
        if k == 'Open':
            opens.append((seq, gen, t)); cur_gen = gen; connected = True; reset_books(t); continue
        if k in ('Close', 'Stop'):
            closes.append((seq, gen, t, ev.get('reason'))); connected = False; reset_books(t); continue
        if k != 'Frame': continue
        m = json.loads(ev['text']); ch = m.get('channel')
        if ch not in ('bbo', 'l2Book'): continue
        if gen != cur_gen or not connected: stale_gen_frames += 1; continue
        d = m['data']; ci = cidx.get(d['coin'])
        if ci is None: continue
        s = S[ch][ci]; s['n'] += 1
        ex = d['time']
        if ch == 'bbo':
            sides = [[x] if x else [] for x in d['bbo']]
        else:
            sides = d['levels']
        full = tuple(tuple((D(l['px']), D(l['sz']), l.get('n')) for l in side) for side in sides)
        pxsz = tuple(tuple(x[:2] for x in side) for side in full)
        top = tuple((side[0][0], side[0][1]) if side else None for side in full)
        for side in full:
            for x in side:
                if x[1] <= 0 or x[0] <= 0: s['badsz'] += 1
        if top[0] is None or top[1] is None: s['empty'] += 1
        elif top[0][0] > top[1][0]: s['crossed'] += 1
        elif top[0][0] == top[1][0]: s['locked'] += 1
        if s['prev'] is not None:
            s['rx'].append(rx - s['prev_rx']); s['ex'].append(ex - s['prev_ex'])
            if ex < s['prev_ex']: s['ex_back'] += 1
            if ex == s['prev_ex']: s['same_ex'] += 1
            if pxsz == s['prev']: s['same'] += 1
            if full == s['prevn']: s['same_n'] += 1
            if top == s['prevtop']: s['same_top'] += 1
        s['prev'] = pxsz; s['prevn'] = full; s['prevtop'] = top; s['prev_rx'] = rx; s['prev_ex'] = ex
        s['lat'].append(inp['receipt_utc_ns'] / 1e6 - ex)
        if ch == 'l2Book':
            l2times[ex] += 1
            if bbo[ci] is not None:
                arr['n'] += 1; arr['eq'] += top == bbo[ci]['top']; arr['l2_newer'] += ex >= bbo[ci]['ex']
                if top != bbo[ci]['top'] and ex >= bbo[ci]['ex']: arr['neq_newer'] += 1
        other = dep[ci] if ch == 'bbo' else bbo[ci]
        if other and ex < other['ex']: cross_older[ch] += 1
        obs = dict(rx=rx, ex=ex, av=t, top=top,
                   lb=math.log(float(top[0][0])) if top[0] else None,
                   la=math.log(float(top[1][0])) if top[1] else None)
        if ch == 'bbo':
            if bbo[ci] is not None and bbo[ci]['top'] != top: bbo_changes_since_dep[ci] += 1
            bbo[ci] = obs
        else:
            if dep[ci] is not None and bbo_changes_since_dep[ci] > 0:
                silent_eps[ci].append((rx - dep[ci]['rx'], bbo_changes_since_dep[ci]))
            bbo_changes_since_dep[ci] = 0
            dep[ci] = obs
        b, dd = bbo[ci], dep[ci]
        neq = b is not None and dd is not None and b['top'] != dd['top']
        if neq and dis_start[ci] is None: dis_start[ci] = t
        elif not neq and dis_start[ci] is not None:
            dis_eps[ci].append(t - dis_start[ci]); dis_end_by[ci][ch] += 1; dis_start[ci] = None
        upd_routes(ci, t)
reset_books(last_t)

def q(xs, p):
    if not xs: return float('nan')
    xs = sorted(xs); return xs[min(len(xs) - 1, int(p * (len(xs) - 1) + 0.5))]
span = (last_t - first_t) / 1e9
print(f'# run {run}\n# bytes read {pos} / limit {limit}; records {sum(kinds.values())} {dict(kinds)}; span {span:.1f}s; connected {conn_time/1e9:.1f}s')
print(f'# seq gaps {seq_gaps} dups {seq_dups}; generation backsteps {gen_back}; receipt_ns backsteps {rx_back}; process_ns backsteps {proc_back}; stale-gen frames {stale_gen_frames}')
print(f'# opens {opens}\n# closes {closes}\n# frames older than other channel (engine would drop): {dict(cross_older)}')
print(f'# receipt_ns backstep magnitudes ms (file order): max {max(rx_back_mag, default=0):.3f}; utc backsteps (seq, ms): {utc_back[:10]}')
print(f'# l2Book distinct exchange times {len(l2times)}; frames per distinct time: {Counter(l2times.values()).most_common(5)}')
print(f'# l2Book arrivals with a prior bbo: {dict(arr)}')
print('coin      pair         | bbo: n  rx_med rx_p90 rx_max  ex_med  same%  sameN% lat_med | l2: n  rx_med rx_p90 rx_max ex_med same% sameTop% lat_med | crossed/locked/empty/badsz b ; l2')
for ci, c in enumerate(coins):
    m = mk[0 if c == 'PURR/USDC' else int(c[1:])]
    b = S['bbo'][ci]; l = S['l2Book'][ci]
    def row(s, top=False):
        n = s['n']; k = max(1, n - 1)
        return (f"{n:6d} {q(s['rx'],.5)/1e6:7.0f} {q(s['rx'],.9)/1e6:7.0f} {max(s['rx'],default=0)/1e6:7.0f} {q(s['ex'],.5):7.0f} "
                f"{100*s['same']/k:6.2f} {100*(s['same_top'] if top else s['same_n'])/k:6.2f} {q(s['lat'],.5):6.0f}")
    print(f"{c:9s} {man['universe']['tokens'][str(m['base'])]['name']+'/'+man['universe']['tokens'][str(m['quote'])]['name']:12s} | {row(b)} | {row(l, True)} | "
          f"{b['crossed']}/{b['locked']}/{b['empty']}/{b['badsz']} ; {l['crossed']}/{l['locked']}/{l['empty']}/{l['badsz']} exback {b['ex_back']}/{l['ex_back']} sameex {b['same_ex']}/{l['same_ex']}")
print('\ncoin      | %conn: bbo<=1s dep<=1s eq(lastknown) bothfresh&eq latest<=1s engine_depth_ok | disagree eps n med p90 max(s) end_by | l2 gaps w/ bbo changes: n, med gap s, max gap s, max bbo chg')
for ci, c in enumerate(coins):
    Tc = T[ci]; ct = conn_time or 1
    e = dis_eps[ci]; se = silent_eps[ci]
    print(f"{c:9s} | {100*Tc['bbo_fresh']/ct:6.1f} {100*Tc['dep_fresh']/ct:6.1f} {100*Tc['eq_lastknown']/ct:6.1f} {100*Tc['both_fresh_eq']/ct:6.1f} {100*Tc['latest_fresh']/ct:6.1f} {100*Tc['engine_depth_ok']/ct:6.1f} | "
          f"{len(e):5d} {q(e,.5)/1e9:6.2f} {q(e,.9)/1e9:6.2f} {max(e,default=0)/1e9:7.1f} {dict(dis_end_by[ci])} | {len(se)} {q([x[0] for x in se],.5)/1e9:.2f} {max([x[0] for x in se],default=0)/1e9:.1f} {max([x[1] for x in se],default=0)}")
print('\n# route-time fractions of connected time (A=bbo channel last-known, B=engine latest-of-bbo/l2 top; f = all legs age<=1s)')
ct = conn_time or 1
print({k: round(100 * v / ct, 4) for k, v in sorted(tpos.items())})
for v in ('A', 'Af', 'B', 'Bf'):
    order = sorted(range(len(routes)), key=lambda i: -rmax[v][i][0])
    npos = sum(1 for i in range(len(routes)) if rmax[v][i][0] > 0)
    nobs = sum(1 for i in range(len(routes)) if rmax[v][i][1] is not None)
    print(f'\n## variant {v}: routes observed {nobs}, routes ever >0 bps: {npos}, >5bps: {sum(1 for i in range(len(routes)) if rmax[v][i][0] > LOG5)}')
    for i in order[:10]:
        x, snap = rmax[v][i]
        if snap is None: continue
        print(f"{routes[i]:24s} {rname[i] if isinstance(rname, list) else rname[routes[i]]:45s} max {math.expm1(x)*1e4:9.2f} bps  t={(snap['t']-first_t)/1e9:8.1f}s bottleneck~{snap['bottleneck_usdc']:.2f} USDC")
        for leg in snap['legs']: print('     ', leg)
json.dump({v: {routes[i]: (math.expm1(rmax[v][i][0]) * 1e4 if rmax[v][i][1] else None) for i in range(len(routes))} for v in rmax},
          open(os.path.join(os.path.dirname(__file__), 'rmax_' + os.path.basename(run.rstrip('/\\')) + '.json'), 'w'))
