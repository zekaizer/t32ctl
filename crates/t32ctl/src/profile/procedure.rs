// SPDX-License-Identifier: Apache-2.0
//! Board procedures: `[proc.<name>]` and its typed parameters, validated when the profile loads,
//! and the one parser of parameter values, for the caller's text and for declared defaults alike.

use std::{
    collections::BTreeMap,
    fmt,
    ops::Range,
    path::{Path, PathBuf},
};

use serde::Deserialize;
use t32rcl::AccessClass;
use t32stage::RelPath;
use toml::Spanned;

use super::Target;
use crate::{
    config::{Env, Issues},
    grade::GradeId,
    notation::{IntValue, OutName, Place, ScriptTimeout},
};

/// What a procedure changes. The declared grade must be at least each effect's floor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Effect {
    /// Symbols, breakpoints, debugger settings. A procedure with it skips the CPU check, so an
    /// `attach` procedure can select the CPU.
    DebuggerConfig,
    TargetRun,
    TargetReset,
    TargetMemory,
    TargetRegisters,
    /// Persistent storage.
    Flash,
}

impl Effect {
    pub const ALL: [Effect; 6] = [
        Effect::DebuggerConfig,
        Effect::TargetRun,
        Effect::TargetReset,
        Effect::TargetMemory,
        Effect::TargetRegisters,
        Effect::Flash,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Effect::DebuggerConfig => "debugger-config",
            Effect::TargetRun => "target-run",
            Effect::TargetReset => "target-reset",
            Effect::TargetMemory => "target-memory",
            Effect::TargetRegisters => "target-registers",
            Effect::Flash => "flash",
        }
    }

    /// The lowest grade a procedure with this effect may declare.
    pub fn floor(self) -> GradeId {
        match self {
            Effect::DebuggerConfig | Effect::TargetRun => GradeId::Run,
            Effect::TargetReset | Effect::TargetMemory | Effect::TargetRegisters => GradeId::Write,
            Effect::Flash => GradeId::Burn,
        }
    }

    fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|e| e.name() == s)
    }
}

/// One `[proc.<name>]`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcDecl {
    pub name: String,
    pub about: String,
    /// The entry script, relative to the profile directory.
    pub script: RelPath,
    pub grade: GradeId,
    pub timeout: ScriptTimeout,
    pub effects: Vec<Effect>,
    /// Files the script writes into `&t32ctl_out`.
    pub outputs: Vec<OutName>,
    /// In declaration order.
    pub params: Vec<ParamDecl>,
}

impl ProcDecl {
    pub fn declares(&self, effect: Effect) -> bool {
        self.effects.contains(&effect)
    }

    /// The parameter a flag names, `--` included.
    pub fn param_by_flag(&self, flag: &str) -> Option<&ParamDecl> {
        self.params.iter().find(|p| p.flag() == flag)
    }

    /// `t32ctl <grade> proc <name>` and each parameter, optional ones in brackets:
    /// `[--part boot0|boot1|user]`.
    pub fn usage(&self, grade: GradeId) -> String {
        let mut words = vec![format!("t32ctl {} proc {}", grade.name(), self.name)];
        for p in &self.params {
            let word = format!("{} {}", p.flag(), p.ty.placeholder());
            words.push(if p.required() { word } else { format!("[{word}]") });
        }
        words.join(" ")
    }
}

/// One `[proc.<name>.params.<param>]`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParamDecl {
    pub name: String,
    pub about: String,
    pub ty: ParamType,
    /// In the caller's notation; a `file` default may hold `{build}`. `None`: required.
    pub default: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ParamType {
    String,
    Integer { min: Option<i64>, max: Option<i64> },
    Address,
    Enum(Vec<String>),
    Bool,
    File,
}

impl ParamType {
    /// The `type` key's value.
    pub fn name(&self) -> &'static str {
        match self {
            ParamType::String => "string",
            ParamType::Integer { .. } => "integer",
            ParamType::Address => "address",
            ParamType::Enum(_) => "enum",
            ParamType::Bool => "bool",
            ParamType::File => "file",
        }
    }

    /// The value placeholder in a usage line: `FILE`, `boot0|boot1|user`, `N`.
    pub fn placeholder(&self) -> String {
        match self {
            ParamType::String => "TEXT".into(),
            ParamType::Integer { .. } => "N".into(),
            ParamType::Address => "LOC".into(),
            ParamType::Enum(values) => values.join("|"),
            ParamType::Bool => "true|false".into(),
            ParamType::File => "FILE".into(),
        }
    }
}

/// What a value is resolved against: the selected target (address classes), the current
/// directory (file paths), and the build directory (`{build}` in a file default).
#[derive(Clone, Copy, Debug)]
pub struct ParamCx<'a> {
    pub target: &'a Target,
    pub cwd: &'a Path,
    pub build: Option<&'a Path>,
}

/// A parameter value, parsed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ParamValue {
    Text(String),
    Int(i64),
    Addr {
        class: AccessClass,
        addr: u64,
    },
    Bool(bool),
    /// A local file; the macro holds the PowerView path of its uploaded copy.
    File(PathBuf),
}

impl ParamValue {
    /// What `&t32ctl_p_<name>` holds, in PRACTICE form. `None` for a file, whose macro is the
    /// path of the uploaded copy.
    pub fn macro_text(&self) -> Option<String> {
        match self {
            ParamValue::Text(t) => Some(t.clone()),
            ParamValue::Int(n) => Some(format!("{n}.")),
            ParamValue::Addr { class, addr } => Some(format!("{}:{addr:#x}", class.as_str())),
            ParamValue::Bool(b) => Some(if *b { "TRUE()" } else { "FALSE()" }.into()),
            ParamValue::File(_) => None,
        }
    }

    /// The value in the caller's notation; a file as `env` displays its path.
    pub fn shown(&self, env: &Env) -> String {
        match self {
            ParamValue::Text(t) => t.clone(),
            ParamValue::Int(n) => n.to_string(),
            ParamValue::Addr { class, addr } => format!("{}:{addr:#x}", class.as_str()),
            ParamValue::Bool(b) => b.to_string(),
            ParamValue::File(p) => env.display(p),
        }
    }
}

/// The longest `string` value: far below the 4 KiB macro limit.
const MAX_TEXT: usize = 256;
const BUILD: &str = "{build}";

/// A `string` value: no `"`, no `&` before a letter (a macro reference), no control character.
fn plain_text(s: &str) -> Result<(), String> {
    if s.len() > MAX_TEXT {
        return Err(format!("{} bytes; at most {MAX_TEXT}", s.len()));
    }
    if s.contains('"') {
        return Err("contains \"".into());
    }
    if let Some(c) = s.chars().find(|c| c.is_control()) {
        return Err(format!("contains the control character {c:?}"));
    }
    if s.as_bytes().windows(2).any(|w| w[0] == b'&' && w[1].is_ascii_alphabetic()) {
        return Err("contains & before a letter, which PRACTICE expands as a macro".into());
    }
    Ok(())
}

impl ParamDecl {
    /// `--<name>` with `_` as `-`.
    pub fn flag(&self) -> String {
        format!("--{}", self.name.replace('_', "-"))
    }

    pub fn required(&self) -> bool {
        self.default.is_none()
    }

    /// The caller's text as this parameter's value. The message names the flag.
    pub fn parse(&self, text: &str, cx: &ParamCx<'_>) -> Result<ParamValue, String> {
        self.value(text, cx).map_err(|why| format!("{} {}: {why}", self.flag(), crate::notation::shell_word(text)))
    }

    /// The declared default as a value; `{build}` in a file default is the build directory.
    /// `None` for a required parameter.
    pub fn default_value(&self, cx: &ParamCx<'_>) -> Option<Result<ParamValue, String>> {
        let text = self.default.as_deref()?;
        if self.ty != ParamType::File || !text.contains(BUILD) {
            return Some(self.parse(text, cx));
        }
        Some(match cx.build {
            Some(b) => {
                let path = PathBuf::from(text.replace(BUILD, &b.to_string_lossy()));
                Ok(ParamValue::File(cx.cwd.join(path)))
            }
            None => Err(format!(
                "{}: the default {text} needs a build directory; pass {} FILE or --build DIR",
                self.flag(),
                self.flag()
            )),
        })
    }

    fn value(&self, text: &str, cx: &ParamCx<'_>) -> Result<ParamValue, String> {
        match &self.ty {
            ParamType::String => plain_text(text).map(|()| ParamValue::Text(text.to_string())),
            ParamType::Integer { min, max } => {
                let n = text.parse::<IntValue>().map_err(|e| e.0)?.0;
                if let Some(m) = min.filter(|m| n < *m) {
                    return Err(format!("{n} is below the minimum {m}"));
                }
                if let Some(m) = max.filter(|m| n > *m) {
                    return Err(format!("{n} is above the maximum {m}"));
                }
                Ok(ParamValue::Int(n))
            }
            ParamType::Address => match text.parse::<Place>().map_err(|e| e.0)? {
                Place::Addr { class, addr } => {
                    let class = class.unwrap_or_else(|| cx.target.default_class.clone());
                    if !cx.target.classes.contains(&class) {
                        let listed: Vec<&str> = cx.target.classes.iter().map(AccessClass::as_str).collect();
                        return Err(format!(
                            "access class {} is not one of the profile's classes {}",
                            class.as_str(),
                            listed.join(", ")
                        ));
                    }
                    Ok(ParamValue::Addr { class, addr })
                }
                Place::Reg { .. } => Err("an address parameter is [CLASS:]0x<hex>, not a register place".into()),
            },
            ParamType::Enum(values) => match values.iter().find(|v| *v == text) {
                Some(v) => Ok(ParamValue::Text(v.clone())),
                None => Err(format!("not one of {}", values.join(", "))),
            },
            ParamType::Bool => match text {
                "true" => Ok(ParamValue::Bool(true)),
                "false" => Ok(ParamValue::Bool(false)),
                _ => Err("use true or false".into()),
            },
            ParamType::File => {
                if text.is_empty() {
                    return Err("an empty path".into());
                }
                Ok(ParamValue::File(cx.cwd.join(text)))
            }
        }
    }
}

impl fmt::Display for ParamType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RawProc {
    about: Option<Spanned<String>>,
    script: Option<Spanned<String>>,
    grade: Option<Spanned<String>>,
    timeout: Option<Spanned<String>>,
    effects: Option<Vec<Spanned<String>>>,
    #[serde(default)]
    outputs: Vec<Spanned<String>>,
    #[serde(default)]
    params: BTreeMap<String, Spanned<RawParam>>,
    lease: Option<Spanned<toml::Value>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RawParam {
    #[serde(rename = "type")]
    ty: Option<Spanned<String>>,
    about: Option<Spanned<String>>,
    default: Option<Spanned<toml::Value>>,
    values: Option<Vec<Spanned<String>>>,
    min: Option<Spanned<i64>>,
    max: Option<Spanned<i64>>,
}

/// Names of procedures and parameters: `[a-z][a-z0-9_]{0,23}`.
fn valid_name(s: &str) -> bool {
    let mut b = s.bytes();
    b.next().is_some_and(|c| c.is_ascii_lowercase())
        && s.len() <= 24
        && b.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_')
}

/// t32ctl's own flags; a parameter flag may not shadow one.
const RESERVED: [&str; 6] = ["json", "lease", "build", "help", "timeout", "out"];

/// The bypass pattern the clap test applies to t32ctl's own flags.
fn bypass(name: &str, ty: &ParamType) -> bool {
    ["force", "yes", "steal", "unsafe", "override"].contains(&name)
        || ["ignore", "no_", "skip"].iter().any(|p| name.starts_with(p))
        || (*ty == ParamType::Bool && name.contains("verify"))
}

fn grade(s: &str) -> Result<GradeId, String> {
    match s {
        "read" => Ok(GradeId::Read),
        "run" => Ok(GradeId::Run),
        "write" => Ok(GradeId::Write),
        "sys" => Ok(GradeId::Sys),
        "burn" => Ok(GradeId::Burn),
        "raw" => Err("raw cannot be declared: raw is PRACTICE outside every policy".into()),
        other => Err(format!("{other:?} is not read, run, write, sys or burn")),
    }
}

/// The procedures of a profile in declaration order, each validated; one with an error is left
/// out and the reasons are in `issues`. `files` are the profile directory's files; `target` is the
/// base target when it is valid.
pub(super) fn procedures(
    issues: &mut Issues,
    raw: &BTreeMap<String, Spanned<RawProc>>,
    files: &[&str],
    target: Option<&Target>,
) -> Vec<ProcDecl> {
    let mut ordered: Vec<(&String, &Spanned<RawProc>)> = raw.iter().collect();
    ordered.sort_by_key(|(_, p)| p.span().start);
    ordered.into_iter().filter_map(|(name, p)| procedure(issues, name, p.get_ref(), p.span(), files, target)).collect()
}

fn required<'a, T>(issues: &mut Issues, v: &'a Option<T>, span: &Range<usize>, key: &str) -> Option<&'a T> {
    if v.is_none() {
        issues.push(span.clone(), format!("{key} is required"));
    }
    v.as_ref()
}

fn procedure(
    issues: &mut Issues,
    name: &str,
    raw: &RawProc,
    span: Range<usize>,
    files: &[&str],
    target: Option<&Target>,
) -> Option<ProcDecl> {
    let key = |k: &str| format!("proc.{name}.{k}");
    let mut ok = true;
    if !valid_name(name) {
        issues.push(span.clone(), format!("proc.{name}: a procedure name is [a-z][a-z0-9_]{{0,23}}"));
        ok = false;
    }
    issues.planned(raw.lease.as_ref().map(Spanned::span), &key("lease"));
    let about = required(issues, &raw.about, &span, &key("about")).and_then(|v| {
        let t = v.get_ref();
        let fine = !t.is_empty() && !t.chars().any(char::is_control);
        if !fine {
            issues.push(v.span(), format!("{} must be non-empty text on one line", key("about")));
        }
        fine.then(|| t.clone())
    });
    let script = required(issues, &raw.script, &span, &key("script")).and_then(|v| {
        let rel = match RelPath::new(v.get_ref()) {
            Ok(r) => r,
            Err(e) => {
                issues.push(v.span(), format!("{}: {e}", key("script")));
                return None;
            }
        };
        if !files.contains(&rel.as_str()) {
            issues.push(v.span(), format!("{} {rel} is not a file of the profile directory", key("script")));
            return None;
        }
        Some(rel)
    });
    let grade = required(issues, &raw.grade, &span, &key("grade")).and_then(|v| match grade(v.get_ref()) {
        Ok(g) => Some((g, v.span())),
        Err(why) => {
            issues.push(v.span(), format!("{}: {why}", key("grade")));
            None
        }
    });
    let timeout = required(issues, &raw.timeout, &span, &key("timeout")).and_then(|v| {
        match v.get_ref().parse::<ScriptTimeout>() {
            Ok(t) => Some(t),
            Err(e) => {
                issues.push(v.span(), format!("{}: {e}", key("timeout")));
                None
            }
        }
    });
    let mut effects = Vec::new();
    for v in required(issues, &raw.effects, &span, &key("effects")).into_iter().flatten() {
        match Effect::parse(v.get_ref()) {
            Some(e) => {
                if let Some((g, at)) = &grade
                    && g.order() < e.floor().order()
                {
                    issues.push(
                        at.clone(),
                        format!(
                            "{} {} is below {}, the floor of the effect {}",
                            key("grade"),
                            g.name(),
                            e.floor().name(),
                            e.name()
                        ),
                    );
                    ok = false;
                }
                effects.push(e);
            }
            None => {
                let all: Vec<&str> = Effect::ALL.iter().map(|e| e.name()).collect();
                issues
                    .push(v.span(), format!("{}: {:?} is not one of {}", key("effects"), v.get_ref(), all.join(", ")));
                ok = false;
            }
        }
    }
    let mut outputs: Vec<OutName> = Vec::new();
    for v in &raw.outputs {
        match v.get_ref().parse::<OutName>() {
            Ok(o) => match outputs.iter().find(|p| p.as_str().eq_ignore_ascii_case(o.as_str())) {
                Some(p) => {
                    issues.push(v.span(), format!("{}: {p} and {o} differ only in case", key("outputs")));
                    ok = false;
                }
                None => outputs.push(o),
            },
            Err(e) => {
                issues.push(v.span(), format!("{}: {e}", key("outputs")));
                ok = false;
            }
        }
    }
    let mut ordered: Vec<(&String, &Spanned<RawParam>)> = raw.params.iter().collect();
    ordered.sort_by_key(|(_, p)| p.span().start);
    let mut params = Vec::new();
    for (pname, p) in ordered {
        let g = grade.as_ref().map(|(g, _)| *g);
        match param(issues, &format!("{}.{pname}", key("params")), pname, p.get_ref(), p.span(), g, target) {
            Some(d) => params.push(d),
            None => ok = false,
        }
    }
    let (about, script, (grade, _), timeout) = (about?, script?, grade?, timeout?);
    ok.then(|| ProcDecl { name: name.to_string(), about, script, grade, timeout, effects, outputs, params })
}

fn param(
    issues: &mut Issues,
    key: &str,
    name: &str,
    raw: &RawParam,
    span: Range<usize>,
    grade: Option<GradeId>,
    target: Option<&Target>,
) -> Option<ParamDecl> {
    let mut ok = true;
    let mut refuse = |issues: &mut Issues, at: Range<usize>, why: String| {
        issues.push(at, format!("{key}: {why}"));
        ok = false;
    };
    if !valid_name(name) {
        refuse(issues, span.clone(), "a parameter name is [a-z][a-z0-9_]{0,23}".into());
    } else if RESERVED.contains(&name) {
        refuse(issues, span.clone(), format!("{name} is reserved for t32ctl's own flags"));
    }
    let about = required(issues, &raw.about, &span, &format!("{key}.about")).and_then(|v| {
        let t = v.get_ref();
        let fine = !t.is_empty() && !t.chars().any(char::is_control);
        if !fine {
            issues.push(v.span(), format!("{key}.about must be non-empty text on one line"));
        }
        fine.then(|| t.clone())
    });
    let ty_text = required(issues, &raw.ty, &span, &format!("{key}.type"));
    let ty = ty_text.and_then(|v| match v.get_ref().as_str() {
        "string" => Some(ParamType::String),
        "integer" => Some(ParamType::Integer {
            min: raw.min.as_ref().map(|m| *m.get_ref()),
            max: raw.max.as_ref().map(|m| *m.get_ref()),
        }),
        "address" => Some(ParamType::Address),
        "enum" => Some(ParamType::Enum(raw.values.iter().flatten().map(|v| v.get_ref().clone()).collect())),
        "bool" => Some(ParamType::Bool),
        "file" => Some(ParamType::File),
        other => {
            issues.push(v.span(), format!("{key}.type {other:?} is not string, integer, address, enum, bool or file"));
            None
        }
    });
    let ty = ty?;
    if bypass(name, &ty) {
        refuse(
            issues,
            span.clone(),
            "the name matches the bypass pattern (force, yes, steal, unsafe, override, ignore*, no_*, skip*, a \
             bool with verify); a check the procedure performs belongs in its script"
                .into(),
        );
    }
    if ty == ParamType::File && grade.is_some_and(|g| g.order() < GradeId::Write.order()) {
        let at = ty_text.map_or(span.clone(), Spanned::span);
        refuse(
            issues,
            at,
            "a file parameter belongs to a write, sys or burn procedure: it moves a local file into PowerView".into(),
        );
    }
    match (&ty, &raw.values) {
        (ParamType::Enum(values), _) => {
            if values.is_empty() {
                refuse(issues, span.clone(), "an enum parameter needs values".into());
            }
            for (i, v) in raw.values.iter().flatten().enumerate() {
                if let Err(why) = plain_text(v.get_ref())
                    .and_then(|()| if v.get_ref().is_empty() { Err("is empty".into()) } else { Ok(()) })
                {
                    refuse(issues, v.span(), format!("value {:?} {why}", v.get_ref()));
                } else if values[..i].contains(v.get_ref()) {
                    refuse(issues, v.span(), format!("value {:?} is listed twice", v.get_ref()));
                }
            }
        }
        (_, Some(v)) => {
            let at = v.first().map_or(span.clone(), Spanned::span);
            refuse(issues, at, "values applies only to enum parameters".into());
        }
        _ => {}
    }
    match &ty {
        ParamType::Integer { min: Some(lo), max: Some(hi) } if lo > hi => {
            let at = raw.max.as_ref().map_or(span.clone(), Spanned::span);
            refuse(issues, at, format!("min {lo} is above max {hi}"));
        }
        ParamType::Integer { .. } => {}
        _ => {
            for m in [&raw.min, &raw.max].into_iter().flatten() {
                refuse(issues, m.span(), "min and max apply only to integer parameters".into());
            }
        }
    }
    let default = match &raw.default {
        None => None,
        Some(v) => match default_text(&ty, v.get_ref()) {
            Err(why) => {
                refuse(issues, v.span(), format!("default {why}"));
                None
            }
            Ok(text) => {
                if text.contains(BUILD) && ty != ParamType::File {
                    refuse(issues, v.span(), "default: {build} belongs only in a file default".into());
                }
                // A file default is checked when it is used; without a valid base target the
                // target's errors come first.
                let decl = ParamDecl { name: name.into(), about: String::new(), ty: ty.clone(), default: None };
                let checked = match target.filter(|_| ty != ParamType::File) {
                    Some(t) => decl.value(&text, &ParamCx { target: t, cwd: Path::new(""), build: None }).map(|_| ()),
                    None => Ok(()),
                };
                if let Err(why) = checked {
                    refuse(issues, v.span(), format!("default {}: {why}", crate::notation::shell_word(&text)));
                }
                Some(text)
            }
        },
    };
    let about = about?;
    ok.then(|| ParamDecl { name: name.to_string(), about, ty, default })
}

/// A TOML default in the caller's notation, if its TOML type fits the parameter type.
fn default_text(ty: &ParamType, v: &toml::Value) -> Result<String, String> {
    match (ty, v) {
        (ParamType::Integer { .. }, toml::Value::Integer(n)) => Ok(n.to_string()),
        (ParamType::Bool, toml::Value::Boolean(b)) => Ok(b.to_string()),
        (ParamType::String | ParamType::Address | ParamType::Enum(_) | ParamType::File, toml::Value::String(s)) => {
            Ok(s.clone())
        }
        (ty, v) => {
            let want = match ty {
                ParamType::Integer { .. } => "an integer",
                ParamType::Bool => "a boolean",
                _ => "a string",
            };
            Err(format!("is a {}; the parameter's type {ty} takes {want}", v.type_str()))
        }
    }
}

/// The names of `files`, for `procedures`.
pub(super) fn names(files: &[super::Walked]) -> Vec<&str> {
    files.iter().map(|f| f.rel.as_str()).collect()
}

#[cfg(test)]
#[path = "procedure_tests.rs"]
mod tests;
