// SPDX-License-Identifier: Apache-2.0
//! The outcome model. One `Outcome` per probe, built once and rendered twice (text, JSON), so
//! the paste-back line and the JSON cannot disagree.

use std::time::Duration;

use serde::Serialize;
use t32rcl::Trace;
use t32stage::{CleanupItem, CleanupReport};

use crate::registry::ProbeId;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Verdict {
    Pass,
    /// Ran, and the result contradicts a requirement or an error stopped it.
    Fail,
    /// PowerView lacks the feature. Not a failure.
    Unsupported,
    Skipped,
    /// Ctrl-C. Measurements so far are kept.
    Interrupted,
}

impl Verdict {
    /// The fixed 5-column tag of the text report.
    pub fn tag(self) -> &'static str {
        match self {
            Verdict::Pass => "PASS",
            Verdict::Fail => "FAIL",
            Verdict::Unsupported => "UNSUP",
            Verdict::Skipped => "SKIP",
            Verdict::Interrupted => "INTR",
        }
    }
}

/// A measured quantity with a unit. Formatting happens once, in `report`.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "unit", content = "v", rename_all = "snake_case")]
pub enum Value {
    Count(u64),
    Bytes(u64),
    Seconds(f64),
    /// 10^6 bytes per second.
    MbPerSec(f64),
    Flag(bool),
    Text(String),
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Measurement {
    pub key: String,
    pub value: Value,
    /// On the one-line report, in insertion order. Everything is in the JSON.
    pub headline: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SkipReason {
    /// A required probe did not pass.
    Prereq {
        probe: String,
        verdict: Verdict,
    },
    TargetNotUp {
        state: String,
    },
    /// A PRACTICE script is on the stack; probes never end scripts they did not start.
    PowerViewBusy {
        depth: i64,
    },
    /// The one reconnect failed or was already spent.
    ConnectionLost {
        detail: String,
    },
    VmLeaseBusy {
        holder: Option<String>,
    },
    NotApplicable {
        why: String,
    },
}

impl SkipReason {
    pub fn text(&self) -> String {
        match self {
            SkipReason::Prereq { probe, verdict } => format!("prereq {probe} {}", verdict.tag()),
            SkipReason::TargetNotUp { state } => format!("target {state}"),
            SkipReason::PowerViewBusy { depth } => format!("PowerView busy: PRACTICE stack depth {depth}"),
            SkipReason::ConnectionLost { detail } => format!("connection lost: {detail}"),
            SkipReason::VmLeaseBusy { holder } => format!("VM: lease held by {}", holder.as_deref().unwrap_or("?")),
            SkipReason::NotApplicable { why } => why.clone(),
        }
    }
}

/// Error as data: a stable tag for machines, message and chain for humans, a hint for the operator.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ErrorRecord {
    pub tag: String,
    pub message: String,
    pub chain: Vec<String>,
    pub hint: Option<String>,
}

impl ErrorRecord {
    pub fn new(tag: &str, message: impl Into<String>) -> Self {
        Self { tag: tag.to_string(), message: message.into(), chain: Vec::new(), hint: None }
    }

    pub fn from_error(e: &(dyn std::error::Error + 'static), tag: &str, hint: Option<&str>) -> Self {
        let mut chain = Vec::new();
        let mut cur = e.source();
        while let Some(s) = cur {
            chain.push(s.to_string());
            cur = s.source();
        }
        Self { tag: tag.to_string(), message: e.to_string(), chain, hint: hint.map(str::to_string) }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct CleanupRecord {
    pub what: String,
    pub result: String,
    pub clean: bool,
    /// What to type into PowerView by hand when not clean.
    pub manual: Option<String>,
}

impl From<&CleanupItem> for CleanupRecord {
    fn from(i: &CleanupItem) -> Self {
        Self { what: i.what.clone(), result: i.outcome.label(), clean: i.outcome.is_clean(), manual: i.manual.clone() }
    }
}

pub fn cleanup_records(r: &CleanupReport) -> Vec<CleanupRecord> {
    r.items.iter().map(CleanupRecord::from).collect()
}

/// Transport counters of one probe, from `Session::take_trace`.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct TraceSummary {
    pub requests: u32,
    pub bytes_tx: u64,
    pub bytes_rx: u64,
    pub keepalives: u32,
    /// First keepalive payloads as hex: status 254 and keepalive share the first byte `0xFE`.
    pub keepalive_raw: Vec<String>,
    pub notifies: u32,
    pub stale: u32,
    pub lossy_text: u32,
}

impl From<&Trace> for TraceSummary {
    fn from(t: &Trace) -> Self {
        Self {
            requests: t.requests,
            bytes_tx: t.bytes_tx,
            bytes_rx: t.bytes_rx,
            keepalives: t.keepalives_total,
            keepalive_raw: t.keepalive_raw.iter().map(|b| b.iter().map(|x| format!("{x:02x}")).collect()).collect(),
            notifies: t.notifies,
            stale: t.stale,
            lossy_text: t.lossy_text,
        }
    }
}

/// The executor's single reconnect after a probe left the connection dead.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Revive {
    pub ok: bool,
    #[serde(serialize_with = "ser_ms")]
    pub connect: Duration,
    /// First call on the new connection, `None` if reconnect failed.
    #[serde(serialize_with = "ser_opt_ms")]
    pub first_call: Option<Duration>,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Outcome {
    pub probe: ProbeId,
    pub verdict: Verdict,
    pub measurements: Vec<Measurement>,
    pub error: Option<ErrorRecord>,
    pub skip: Option<SkipReason>,
    pub cleanup: Vec<CleanupRecord>,
    pub revive: Option<Revive>,
    pub trace: TraceSummary,
    #[serde(rename = "elapsed_ms", serialize_with = "ser_ms")]
    pub elapsed: Duration,
}

fn ser_ms<S: serde::Serializer>(d: &Duration, s: S) -> Result<S::Ok, S::Error> {
    s.serialize_f64((d.as_secs_f64() * 1e4).round() / 10.0)
}

fn ser_opt_ms<S: serde::Serializer>(d: &Option<Duration>, s: S) -> Result<S::Ok, S::Error> {
    match d {
        Some(d) => ser_ms(d, s),
        None => s.serialize_none(),
    }
}

impl Outcome {
    pub fn new(probe: ProbeId, verdict: Verdict) -> Self {
        Self {
            probe,
            verdict,
            measurements: Vec::new(),
            error: None,
            skip: None,
            cleanup: Vec::new(),
            revive: None,
            trace: TraceSummary::default(),
            elapsed: Duration::ZERO,
        }
    }

    pub fn skipped(probe: ProbeId, reason: SkipReason) -> Self {
        Self { skip: Some(reason), ..Self::new(probe, Verdict::Skipped) }
    }

    pub fn residue(&self) -> impl Iterator<Item = &CleanupRecord> {
        self.cleanup.iter().filter(|c| !c.clean)
    }

    pub fn lookup(&self, key: &str) -> Option<&Value> {
        self.measurements.iter().find(|m| m.key == key).map(|m| &m.value)
    }
}

/// 0 when every selected probe passed, was unsupported, or was skipped for a reason other than a
/// failure; 1 when one failed or the connection was lost for the rest; 130 when interrupted.
pub fn exit_code(outcomes: &[Outcome], interrupted: bool) -> u8 {
    if interrupted {
        130
    } else if outcomes
        .iter()
        .any(|o| o.verdict == Verdict::Fail || matches!(o.skip, Some(SkipReason::ConnectionLost { .. })))
    {
        1
    } else {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outcome_json_shape_is_stable() {
        let mut o = Outcome::new(ProbeId::VmRw, Verdict::Pass);
        o.measurements = vec![
            Measurement { key: "max".into(), value: Value::Bytes(1 << 20), headline: true },
            Measurement { key: "w".into(), value: Value::MbPerSec(8.1), headline: true },
        ];
        o.cleanup =
            vec![CleanupRecord { what: "restore".into(), result: "restored".into(), clean: true, manual: None }];
        o.trace = TraceSummary { requests: 2, keepalive_raw: vec!["fe00".into()], ..TraceSummary::default() };
        o.elapsed = Duration::from_millis(1234);
        assert_eq!(
            serde_json::to_string(&o).unwrap(),
            r#"{"probe":"vm-rw","verdict":"pass","measurements":[{"key":"max","value":{"unit":"bytes","v":1048576},"headline":true},{"key":"w","value":{"unit":"mb_per_sec","v":8.1},"headline":true}],"error":null,"skip":null,"cleanup":[{"what":"restore","result":"restored","clean":true,"manual":null}],"revive":null,"trace":{"requests":2,"bytes_tx":0,"bytes_rx":0,"keepalives":0,"keepalive_raw":["fe00"],"notifies":0,"stale":0,"lossy_text":0},"elapsed_ms":1234.0}"#
        );
    }

    #[test]
    fn exit_codes_follow_failures_not_residue() {
        let pass = Outcome::new(ProbeId::Connect, Verdict::Pass);
        let unsup = Outcome::new(ProbeId::Clients, Verdict::Unsupported);
        assert_eq!(exit_code(&[pass.clone(), unsup], false), 0);
        let mut residue = Outcome::new(ProbeId::VmRw, Verdict::Pass);
        residue.cleanup.push(CleanupRecord { what: "rm".into(), result: "failed".into(), clean: false, manual: None });
        assert_eq!(exit_code(&[residue], false), 0);
        assert_eq!(exit_code(&[pass.clone(), Outcome::new(ProbeId::Version, Verdict::Fail)], false), 1);
        let lost = Outcome::skipped(ProbeId::Gzip, SkipReason::ConnectionLost { detail: "x".into() });
        assert_eq!(exit_code(&[pass, lost], false), 1);
        assert_eq!(exit_code(&[], true), 130);
    }
}
