// SPDX-License-Identifier: Apache-2.0
//! ux-lab: what an agent needs to use PowerView the way a person does, measured without the
//! target. The message line after a command (`T32_GetMessageString`), the last lines of the
//! default message area (`AREA.LINE`), a window's text without printing it
//! (`T32_GetWindowContent`), the names PowerView gives windows nobody named, and which spelling
//! of the system log this build has. Leaves one `PRINT` line in AREA `A000`; opens one window and
//! closes it.

use t32rcl::{MessageLine, PrintFormat};
use t32stage::Undo;

use super::{
    functions::{command, func, reply, shown},
    optin::revert,
};
use crate::{
    ctx::{ProbeCtx, ProbeFail, ProbeResult},
    outcome::Value,
};

/// Windows asked for with `T32_GetWindowContent`. None of them reads the target.
pub(crate) const WINDOWS: &[&str] =
    &["VERSION.SOFTWARE", "AREA.view", "SYStem.state", "WinPAGE.List", "Break.List", "SYStem.LOG.List"];

/// Most bytes kept of one window's text.
pub(crate) const WINDOW_MAX: usize = 64 * 1024;

/// Every call answers or is recorded as refused; only a connection-fatal error fails it.
pub fn ux_lab(ctx: &mut ProbeCtx<'_>) -> ProbeResult {
    let mark = format!("t32probe-ux-{}", ctx.env.cfg.run_id.to_ascii_lowercase());
    let before = window_names(ctx)?;

    message(ctx, "msg.initial")?;
    let printed = command(ctx, &format!("PRINT \"{mark}\""))?;
    ctx.measure("cmd.print", Value::Text(printed.map_or_else(|e| format!("err{}", e.code), |()| "ok".into())));
    let after_print = message(ctx, "msg.print")?;
    let follows = after_print.as_ref().is_some_and(|m| m.text.contains(&mark));
    ctx.text(
        "msg",
        if follows {
            "follows"
        } else if after_print.is_none() {
            "refused"
        } else {
            "no"
        },
    );

    let mut area = "none";
    for (label, line) in [("0", 0), ("-1", -1)] {
        let r = func(ctx, &format!("AREA.LINE(A000,{line})"))?;
        let v = shown(&r);
        if area == "none" && v.contains(&mark) {
            area = label;
        }
        ctx.measure(&format!("area.{label}"), Value::Text(v));
    }
    ctx.text("area", area);

    let refused = command(ctx, "T32PROBE_NOPE")?;
    ctx.measure("cmd.refused", Value::Text(refused.map_or_else(|e| format!("err{}", e.code), |()| "ok".into())));
    let after_refused = message(ctx, "msg.refused")?;
    let _ = command(ctx, "ERROR.RESet")?;
    let after_silent = message(ctx, "msg.silent")?;
    let silent = match (&after_refused, &after_silent) {
        (_, None) => "refused",
        (_, Some(m)) if m.kind == 0 || m.text.is_empty() => "cleared",
        (Some(r), Some(m)) if r == m => "kept",
        _ => "changed",
    };
    ctx.text("msg_silent", silent);

    for w in WINDOWS {
        let v = match reply(ctx.session.window_content(w, PrintFormat::Ascii, WINDOW_MAX))? {
            Ok((bytes, cut)) => {
                let text = String::from_utf8_lossy(&bytes);
                let first = text.lines().next().unwrap_or_default();
                let cut = if cut { " cut" } else { "" };
                format!("lines={} bytes={} first={first:?}{cut}", text.lines().count(), bytes.len())
            }
            Err(e) => format!("err{}", e.code),
        };
        if *w == WINDOWS[0] {
            ctx.text("wincontent", if v.starts_with("err") { format!("no ({v})") } else { "yes".into() });
        }
        ctx.measure(&format!("win.{w}"), Value::Text(v));
    }
    let after = window_names(ctx)?;
    ctx.text("win_left", listed(&added(&before, &after)));

    let _ = command(ctx, "VERSION.SOFTWARE")?;
    let opened = window_names(ctx)?;
    match added(&after, &opened).first() {
        Some(name) => {
            ctx.text("winname", name.clone());
            let cmd = shown(&func(ctx, &format!("WINdow.COMMAND({name})"))?);
            ctx.measure("winname.command", Value::Text(cmd));
            ctx.push(Undo::Revert { what: "window".into(), commands: vec![format!("WinCLEAR {name}")] })?;
            let mut clean = true;
            revert(ctx, "window", &mut clean);
        }
        None => ctx.text("winname", "none (no new name: reused or unnamed)"),
    }

    let mut spellings = Vec::new();
    for name in ["SYStem.LOG.List", "SystemLOG.List"] {
        let v = shown(&func(ctx, &format!("PRACTICE.CoMmanD.AVAILable({name})"))?);
        if v == "TRUE" {
            spellings.push(name.trim_end_matches(".List"));
        }
        ctx.measure(&format!("avail.{name}"), Value::Text(v));
    }
    ctx.text("syslog", if spellings.is_empty() { "none".to_string() } else { spellings.join("+") });
    Ok(())
}

/// `T32_GetMessageString`, recorded as `kind=<bits> "<text>"`; `None` when refused.
fn message(ctx: &mut ProbeCtx<'_>, key: &str) -> Result<Option<MessageLine>, ProbeFail> {
    let r = reply(ctx.session.message())?;
    let v = r.as_ref().map_or_else(|e| format!("err{}", e.code), |m| format!("kind={} {:?}", m.kind, m.text));
    ctx.measure(key, Value::Text(v));
    Ok(r.ok())
}

/// `WINdow.LIST()` split at commas; empty when refused.
fn window_names(ctx: &mut ProbeCtx<'_>) -> Result<Vec<String>, ProbeFail> {
    let r = func(ctx, "WINdow.LIST()")?;
    let v = shown(&r);
    ctx.measure("win.list", Value::Text(v.clone()));
    Ok(if r.is_ok() {
        v.split(',').map(str::trim).filter(|n| !n.is_empty()).map(String::from).collect()
    } else {
        Vec::new()
    })
}

/// Names in `now` not in `then`.
fn added(then: &[String], now: &[String]) -> Vec<String> {
    now.iter().filter(|n| !then.contains(n)).cloned().collect()
}

fn listed(names: &[String]) -> String {
    if names.is_empty() { "none".into() } else { names.join(",") }
}

#[cfg(test)]
#[path = "ux_tests.rs"]
mod tests;
