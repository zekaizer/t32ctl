// SPDX-License-Identifier: Apache-2.0
//! Probes about the connection: connect, version, latency, clients, keepalive, reconnect.

use std::time::{Duration, Instant};

use t32rcl::{
    CallBudget, Endpoint, Error, Feature, Health, Op, PoisonCause, Session,
    version::{BASE_FUNCTIONS, BUILD_FUNCTIONS, FEATURES},
};
use t32stage::practice::fnc;

use super::{hex, ok};
use crate::{
    ctx::{ProbeCtx, ProbeFail, ProbeResult},
    outcome::Value,
    stats::Stats,
};

/// Each candidate host in turn; the first that attaches replaces the placeholder session.
/// On failure the class of every attempt is reported (`class=refused`, `tried=...`).
pub fn connect(ctx: &mut ProbeCtx<'_>) -> ProbeResult {
    let mut tried = Vec::new();
    let mut first_error = None;
    for host in &ctx.env.cfg.hosts {
        match Session::connect(Endpoint::new(host, ctx.env.cfg.port), ctx.env.connect_opts()) {
            Ok(s) => {
                let r = s.connect_report();
                *ctx.session = s;
                ctx.headline("tcp", Value::Seconds(r.tcp.as_secs_f64()));
                ctx.headline("attach", Value::Seconds(r.attach.as_secs_f64()));
                let t = Instant::now();
                ctx.session.eval("TRUE()")?;
                ctx.headline("first", Value::Seconds(t.elapsed().as_secs_f64()));
                ctx.text("host", host.clone());
                if !tried.is_empty() {
                    ctx.measure("tried", Value::Text(tried.join(" ")));
                }
                return Ok(());
            }
            Err(e) => {
                tried.push(format!("{host}:{}", e.tag()));
                first_error.get_or_insert(e);
            }
        }
    }
    let e = first_error.expect("at least one host");
    ctx.text("class", e.tag());
    if tried.len() > 1 {
        ctx.text("tried", tried.join(","));
    }
    Err(ProbeFail::Rcl(e.into()))
}

/// Build numbers, which spellings answer, the feature table. Never FAIL from a build number.
pub fn version(ctx: &mut ProbeCtx<'_>) -> ProbeResult {
    for expr in BUILD_FUNCTIONS.iter().chain(&BASE_FUNCTIONS) {
        let answer = match ctx.session.eval(expr) {
            Ok(v) => v.to_string(),
            Err(e) if e.remote().is_some() => format!("refused: {e}"),
            Err(e) => return Err(e.into()),
        };
        ctx.measure(expr, Value::Text(answer));
    }
    let b = ctx.session.build()?;
    ctx.learned.build = Some(b);
    ctx.headline("build", Value::Count(b.build.into()));
    if let Some(base) = b.base {
        ctx.headline("base", Value::Count(base.into()));
    }
    ctx.text("dvd", b.release_hint().map_or("?".into(), |h| h.trim_start_matches("~DVD ").to_string()));
    ctx.text("min", b.min_support().tag());
    for f in FEATURES {
        ctx.text(f.id, b.support(f.feature).tag());
    }
    Ok(())
}

/// `latency_n` round trips of GetState and of `TRUE()`; the first of each is recorded apart.
pub fn latency(ctx: &mut ProbeCtx<'_>) -> ProbeResult {
    let n = ctx.env.cfg.latency_n as usize;
    let time =
        |ctx: &mut ProbeCtx<'_>, f: &dyn Fn(&mut Session) -> Result<(), Error>| -> Result<Vec<Duration>, ProbeFail> {
            let mut v = Vec::with_capacity(n + 1);
            for _ in 0..=n {
                let t = Instant::now();
                f(ctx.session)?;
                v.push(t.elapsed());
            }
            Ok(v)
        };
    let mut state = time(ctx, &|s| s.target_state().map(drop))?;
    let mut func = time(ctx, &|s| s.eval("TRUE()").map(drop))?;
    ctx.measure("state_first", Value::Seconds(state[0].as_secs_f64()));
    ctx.measure("fn_first", Value::Seconds(func[0].as_secs_f64()));
    let (s, f) = (Stats::from_samples(&mut state[1..]), Stats::from_samples(&mut func[1..]));
    let (s, f) = (s.expect("n >= 1"), f.expect("n >= 1"));
    for (k, st) in [("state", s), ("fn", f)] {
        ctx.measure(&format!("{k}_p50"), Value::Seconds(st.p50.as_secs_f64()));
        ctx.measure(&format!("{k}_p95"), Value::Seconds(st.p95.as_secs_f64()));
        ctx.measure(&format!("{k}_max"), Value::Seconds(st.max.as_secs_f64()));
        ctx.text(k, st.compact());
    }
    ctx.headline("n", Value::Count(n as u64));
    Ok(())
}

pub fn clients(ctx: &mut ProbeCtx<'_>) -> ProbeResult {
    match ctx.session.eval(fnc::NR_USED_CONS).and_then(|v| v.into_int(fnc::NR_USED_CONS)) {
        Ok(n) => {
            ctx.headline("NrUsedCons", Value::Count(n.max(0) as u64));
            ctx.check(n >= 1, || format!("NrUsedCons is {n} while this client is connected"))
        }
        Err(e) => Err(ctx.feature_failure(Feature::NrUsedCons, e)),
    }
}

/// `WAIT 3.s` with a generous idle budget. Zero keepalives is a result, not a failure: it
/// decides whether Phase 1 idle timeouts must exceed the longest blocking command.
pub fn keepalive(ctx: &mut ProbeCtx<'_>) -> ProbeResult {
    ctx.session.take_trace();
    let t = Instant::now();
    ctx.session
        .with_budget(CallBudget::idle(Duration::from_secs(10)).with_total(Duration::from_secs(15)))
        .cmd("WAIT 3.s")?;
    let elapsed = t.elapsed();
    let trace = ctx.session.take_trace();
    let at: Vec<Duration> = trace.keepalives.iter().filter(|k| k.op == Op::Command).map(|k| k.since_request).collect();
    ctx.headline("wait", Value::Seconds(elapsed.as_secs_f64()));
    ctx.headline("n", Value::Count(at.len() as u64));
    let mut gaps: Vec<Duration> = at.windows(2).map(|w| w[1] - w[0]).collect();
    match (at.first(), Stats::from_samples(&mut gaps)) {
        (Some(first), Some(st)) => {
            ctx.measure("first", Value::Seconds(first.as_secs_f64()));
            ctx.headline("interval", Value::Seconds(st.p50.as_secs_f64()));
        }
        (Some(first), None) => ctx.headline("interval", Value::Text(format!("once@{:.2}s", first.as_secs_f64()))),
        (None, _) => ctx.headline("interval", Value::Text("none".into())),
    }
    let raw: Vec<String> = trace.keepalive_raw.iter().map(|b| hex(b)).collect();
    ctx.measure("raw", Value::Text(raw.join(" ")));
    ctx.measure("stale", Value::Count(trace.stale.into()));
    Ok(())
}

/// Forces a read timeout shorter than a blocking command and proves the session is poisoned
/// without sending. It does not reconnect: the executor revives once and records `revive=` and
/// `first_call=`, measured while PowerView may still be inside the `WAIT`.
pub fn reconnect(ctx: &mut ProbeCtx<'_>) -> ProbeResult {
    let budget = CallBudget::idle(Duration::from_millis(300)).with_total(Duration::from_secs(1));
    let forced = ctx.session.with_budget(budget).cmd("WAIT 5.s");
    match forced {
        Err(Error::Timeout { kind, waited, .. }) => {
            ctx.headline("timeout", Value::Seconds(waited.as_secs_f64()));
            ctx.text("kind", kind.to_string());
        }
        Ok(()) => return Err(ProbeFail::Check("WAIT 5.s returned inside a 1 s budget".into())),
        Err(e) => return Err(e.into()),
    }
    let poisoned = ctx.session.health() == Health::Poisoned(PoisonCause::Timeout);
    ctx.session.take_trace();
    let refused = matches!(ctx.session.eval("TRUE()"), Err(Error::Poisoned(_)));
    let sent = ctx.session.take_trace().requests;
    ctx.text("poisoned", ok(poisoned && refused && sent == 0));
    ctx.check(poisoned && refused && sent == 0, || "a timed-out session accepted another request".into())
}
