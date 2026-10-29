// SPDX-License-Identifier: Apache-2.0
//! debugger-info: what PowerView, the debug probe and the debug port report about themselves,
//! as opposed to the target. Reads only; it never changes the debugger mode, so the operator runs
//! it once in each mode (Down, Prepare, Attach, Up) to see what each one answers.

use t32rcl::FuncValue;

use super::functions::{func, reply, shown};
use crate::{
    ctx::{ProbeCtx, ProbeResult},
    outcome::Value,
};

/// Function calls read, in order, each after `PRACTICE.FUNCtion.AVAILable(<name>)`. Every one is
/// documented without side effects (Lauterbach function references, Release 02.2026). `{i}` is
/// the licence index `LICENSE.getINDEX()` returned, as a decimal.
///
/// Left out because reading them acts: `SYStem.DETECT` and `IDCODE()` (forces Down, may reset),
/// `IFCONFIG.TEST` and `IFTEST.*` (bandwidth test), `CONNECTION.FindDEVices` (device scan),
/// `JTAG.PIN()` latches (cleared by the read), `Trace.STATE()` (blocks in STREAM mode).
pub(crate) const INFO: &[&str] = &[
    // Software and host.
    "VERSION.SOFTWARE()",
    "VERSION.SOFTWARE.TYPE()",
    "VERSION.BUILD()",
    "VERSION.BUILD.BASE()",
    "VERSION.DATE()",
    "VERSION.ENVironment(\"CONFIG\")",
    "SOFTWARE.64BIT()",
    "PowerView.StartMode()",
    "OS.NAME()",
    "OS.PresentExecutableFile()",
    // PowerView instance and remote interfaces.
    "TITLE()",
    "OS.PresentConfigurationFile()",
    "RCL.TCP.PORT()",
    "RCL.TCP.NrUsedCons()",
    "InterCom.NAME()",
    "InterCom.PORT()",
    "SYStem.INSTANCE()",
    "SYStem.INSTANCECOUNT()",
    "SYStem.USECORE()",
    "PBI()",
    "INTERFACE.NAME()",
    "CONNECTION.STATE.Interactive()",
    // Licence.
    "LICENSE.getINDEX()",
    "LICENSE.DATE({i})",
    "LICENSE.SERIAL({i})",
    "LICENSE.MSERIAL({i})",
    "LICENSE.FAMILY({i})",
    "LICENSE.FEATURES({i})",
    "LICENSE.MULTICORE()",
    "LICENSE.RequiredForCPU()",
    // Probe hardware and cable.
    "hardware.POWERDEBUG()",
    "hardware.COMBIPROBE()",
    "hardware.QUADPROBE()",
    "hardware.POWERTRACE2()",
    "hardware.POWERTRACE3()",
    "VERSION.SERIAL()",
    "VERSION.SERIAL.DEBUG()",
    "VERSION.FirmWare.DEBUG()",
    "CABLE.NAME()",
    "CABLE.SERIAL()",
    "CABLE.TWOWIRE()",
    "VERSION.CABLE()",
    "ID.CABLE()",
    // Host link to the probe.
    "IFCONFIG.CONFIGURATION()",
    "IFCONFIG.DEVICENAME()",
    "IFCONFIG.IPADDRESS()",
    "IFCONFIG.ETHernetADDRESS()",
    "NODENAME()",
    "IFCONFIG.ERRORS()",
    "IFCONFIG.RETRIES()",
    "IFCONFIG.RESYNCS()",
    // Debug port and mode.
    "SYStem.Mode()",
    "SYStem.Up()",
    "SYStem.JtagClock()",
    "SYStem.CONFIG.DEBUGPORTTYPE()",
    "SYStem.CONFIG.DEBUGPORT()",
    "SYStem.CONFIG.TAPState()",
    "SYStem.CONFIG.Slave()",
    "SYStem.Option.EnReset()",
    "SYStem.Option.ResBreak()",
    "DAP.Available()",
    "STATE.TARGET()",
    // Power and reset sense.
    "STATE.POWER()",
    "STATE.RESET()",
    // CPU selection and cores.
    "SYStem.CPU()",
    "CPUFAMILY()",
    "CORENAME()",
    "CPUIS64BIT()",
    "ARMARCHVERSION()",
    "CONFIGNUMBER()",
    "CORE.NUMBER()",
    "CORE()",
];

/// `SYStem.Mode()` values (general_func). Attach is not in the table.
fn mode_name(v: u64) -> &'static str {
    match v {
        0 => "Down",
        1 => "StandBy",
        2 => "NoDebug",
        4 => "Prepare",
        11 => "Up",
        12 => "Up(StandBy)",
        13 => "Prepare(StandBy)",
        _ => "unlisted",
    }
}

/// Every `INFO` call PowerView offers, as `info.<call>`: the value, `err<status>` for a refused
/// call, `absent (<answer>)` when the availability check did not answer `TRUE`. Headlines carry
/// the values a status line would use. Only a connection-fatal error fails it.
pub fn debugger_info(ctx: &mut ProbeCtx<'_>) -> ProbeResult {
    let state = reply(ctx.session.target_state())?.map_or_else(|e| format!("err{}", e.code), |s| s.name().into());
    ctx.text("state", state);
    let mut index = 0;
    let mut values: Vec<(&str, String)> = Vec::with_capacity(INFO.len());
    let mut yes = 0;
    for &call in INFO {
        let name = call.split('(').next().expect("a call has a name");
        let avail = shown(&func(ctx, &format!("PRACTICE.FUNCtion.AVAILable({name})"))?);
        let value = if avail == "TRUE" {
            yes += 1;
            let r = func(ctx, &call.replace("{i}", &format!("{index}.")))?;
            match (&r, call) {
                (Ok(FuncValue::Int(n)), "LICENSE.getINDEX()") => index = *n,
                (Ok(FuncValue::Uint(m)), "SYStem.Mode()") => ctx.text("mode", format!("{m:#x} {}", mode_name(*m))),
                _ => {}
            }
            shown(&r)
        } else {
            format!("absent ({avail})")
        };
        ctx.measure(&format!("info.{call}"), Value::Text(value.clone()));
        values.push((call, value));
    }
    ctx.text("avail", format!("{yes}/{}", INFO.len()));
    for (key, call) in [
        ("software", "VERSION.SOFTWARE()"),
        ("jtag_clock", "SYStem.JtagClock()"),
        ("port", "SYStem.CONFIG.DEBUGPORTTYPE()"),
        ("link", "IFCONFIG.CONFIGURATION()"),
        ("cable", "CABLE.NAME()"),
        ("power", "STATE.POWER()"),
        ("licence_date", "LICENSE.DATE({i})"),
    ] {
        let v = values.iter().find(|(c, _)| *c == call).map(|(_, v)| v.clone()).unwrap_or_default();
        ctx.text(key, v);
    }
    Ok(())
}

#[cfg(test)]
#[path = "debugger_tests.rs"]
mod tests;
