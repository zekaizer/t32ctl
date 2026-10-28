// SPDX-License-Identifier: Apache-2.0
//! The board profile: `profile.toml` in a board's `support/t32ctl` directory, the variant the
//! build's `BOARD` selects, and the profile hash over the directory's files.
//!
//! Validation reports the first TOML syntax or schema error (parsing stops there), then every
//! semantic error, each with file, line and column. Keys of planned features say so. Procedures
//! and their parameters are in `procedure`.

use std::{
    collections::{BTreeMap, HashSet},
    fmt, fs,
    ops::Range,
    path::{Path, PathBuf},
};

use serde::Deserialize;
use sha2::{Digest, Sha256};
use t32rcl::AccessClass;
use t32stage::RelPath;
use toml::Spanned;

use crate::{
    config::{Binding, Env, Issues, invalid, syntax},
    error::{CtlError, ErrorKind},
};

pub mod policy;
pub mod procedure;

pub use procedure::{Effect, ParamCx, ParamDecl, ParamType, ParamValue, ProcDecl};

pub const PROFILE_FILE: &str = "profile.toml";

/// A loaded profile with the variant applied.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Profile {
    pub dir: PathBuf,
    pub target: Target,
    /// The memory map: physical regions that nest or are disjoint.
    pub memory: Vec<Region>,
    /// The `[variant."<board>"]` the build's `BOARD` selected; `None` for the base profile.
    pub variant: Option<String>,
    /// Board procedures in declaration order; every variant shares them.
    pub procs: Vec<ProcDecl>,
    /// The directory's files, sorted by relative path: what the hash covers.
    pub files: Vec<Walked>,
    pub hash: ProfileHash,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    pub name: String,
    /// Compared ASCII case-insensitively with `SYStem.CPU()`.
    pub cpu: String,
    pub cores: u32,
    /// The default cell width of `ram` and `rom`, in bytes.
    pub word: u8,
    /// The access classes a place may name.
    pub classes: Vec<AccessClass>,
    /// The class of an address typed without one.
    pub default_class: AccessClass,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RegionKind {
    Ram,
    Rom,
    Mmio,
    Deny,
}

impl RegionKind {
    pub fn name(self) -> &'static str {
        match self {
            RegionKind::Ram => "ram",
            RegionKind::Rom => "rom",
            RegionKind::Mmio => "mmio",
            RegionKind::Deny => "deny",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        [RegionKind::Ram, RegionKind::Rom, RegionKind::Mmio, RegionKind::Deny].into_iter().find(|k| k.name() == s)
    }
}

/// One `[[memory]]` region.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Region {
    pub name: String,
    pub start: u64,
    pub size: u64,
    pub kind: RegionKind,
    /// The access width of an `mmio` region; `None` elsewhere.
    pub width: Option<u8>,
    /// Overrides of the kind's read and write rules.
    pub read: Option<bool>,
    pub write: Option<bool>,
    /// Readable while the cores run.
    pub running: bool,
}

impl Region {
    /// One past the last byte. TOML integers are signed 64-bit, so `start + size` fits.
    pub fn end(&self) -> u64 {
        self.start + self.size
    }

    fn contains(&self, other: &Region) -> bool {
        self.start <= other.start && other.end() <= self.end()
    }

    fn overlaps(&self, other: &Region) -> bool {
        self.start < other.end() && other.start < self.end()
    }
}

/// SHA-256 over the profile directory's files.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProfileHash(pub [u8; 32]);

impl ProfileHash {
    /// The first 8 hex digits, as text reports print it.
    pub fn short(&self) -> String {
        self.to_string()[..8].to_string()
    }
}

impl fmt::Display for ProfileHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.iter().try_for_each(|b| write!(f, "{b:02x}"))
    }
}

/// One file of a walked directory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Walked {
    pub rel: RelPath,
    pub bytes: Vec<u8>,
}

/// A file a walk refuses: a symlink, a special file, a name PowerView cannot carry, or an I/O
/// error.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WalkError {
    pub path: PathBuf,
    pub what: String,
}

/// Hidden files and directories and editor leftovers: `.*`, `*~`, `#*#`, `*.swp`.
fn ignored(name: &str) -> bool {
    name.starts_with('.')
        || name.ends_with('~')
        || (name.starts_with('#') && name.ends_with('#'))
        || name.ends_with(".swp")
}

/// Every regular file below `root` except ignored names, sorted by relative path with `/`
/// separators. Anything that is neither a regular file nor a directory is refused.
pub fn walk(root: &Path) -> Result<Vec<Walked>, WalkError> {
    fn visit(root: &Path, dir: &Path, out: &mut Vec<Walked>) -> Result<(), WalkError> {
        let io = |path: &Path, e: std::io::Error| WalkError { path: path.to_path_buf(), what: e.to_string() };
        for entry in fs::read_dir(dir).map_err(|e| io(dir, e))? {
            let path = entry.map_err(|e| io(dir, e))?.path();
            let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            if ignored(&name) {
                continue;
            }
            let meta = fs::symlink_metadata(&path).map_err(|e| io(&path, e))?;
            let refuse = |what: &str| WalkError {
                path: path.clone(),
                what: format!("{what}; a profile holds only regular files and directories"),
            };
            if meta.file_type().is_symlink() {
                return Err(refuse("a symlink"));
            }
            if meta.is_dir() {
                visit(root, &path, out)?;
            } else if meta.is_file() {
                let rel_text = path.strip_prefix(root).unwrap_or(&path).to_string_lossy().into_owned();
                let rel = RelPath::new(&rel_text).map_err(|e| WalkError { path: path.clone(), what: e.to_string() })?;
                let bytes = fs::read(&path).map_err(|e| io(&path, e))?;
                out.push(Walked { rel, bytes });
            } else {
                return Err(refuse("a special file"));
            }
        }
        Ok(())
    }
    let mut out = Vec::new();
    visit(root, root, &mut out)?;
    out.sort_by(|a, b| a.rel.as_str().cmp(b.rel.as_str()));
    Ok(out)
}

/// `b"t32ctl-profile\0"`, `1u32` LE, then per file: path length (u32 LE), path, content length
/// (u64 LE), content.
pub fn hash(files: &[Walked]) -> ProfileHash {
    let mut h = Sha256::new();
    h.update(b"t32ctl-profile\0");
    h.update(1u32.to_le_bytes());
    for f in files {
        let path = f.rel.as_str().as_bytes();
        h.update((path.len() as u32).to_le_bytes());
        h.update(path);
        h.update((f.bytes.len() as u64).to_le_bytes());
        h.update(&f.bytes);
    }
    ProfileHash(h.finalize().into())
}

impl Profile {
    /// The profile the binding names. No binding, no directory, or no `profile.toml` is
    /// `no-profile`, with the sources tried.
    pub fn from_binding(env: &Env, binding: &Binding) -> Result<Self, CtlError> {
        let Some(src) = &binding.profile else {
            return Err(CtlError::new(ErrorKind::NoProfile, "no target profile for this build")
                .with_details(binding.tried.clone()));
        };
        if !src.value.join(PROFILE_FILE).is_file() {
            let message = format!("no {PROFILE_FILE} in {}/ (from {})", env.display(&src.value), src.source.text());
            return Err(CtlError::new(ErrorKind::NoProfile, message).with_details(binding.tried.clone()));
        }
        Self::load(env, &src.value, binding.board.as_deref())
    }

    /// Walk and hash `dir`, parse its `profile.toml`, and apply the variant `board` selects.
    pub fn load(env: &Env, dir: &Path, board: Option<&str>) -> Result<Self, CtlError> {
        let files = walk(dir).map_err(|e| invalid(format!("{}: {}", env.display(&e.path), e.what)))?;
        let file = dir.join(PROFILE_FILE);
        let shown = PathBuf::from(env.display(&file));
        let Some(toml) = files.iter().find(|f| f.rel.as_str() == PROFILE_FILE) else {
            return Err(invalid(format!("{}: not a regular file", shown.display())));
        };
        let text =
            std::str::from_utf8(&toml.bytes).map_err(|_| invalid(format!("{}: not UTF-8 text", shown.display())))?;
        let doc = ProfileDoc::parse(text, &shown, &procedure::names(&files))?;
        let (variant, target, memory) = doc.select(board);
        let hash = hash(&files);
        Ok(Self { dir: dir.to_path_buf(), target, memory, variant, procs: doc.procs, files, hash })
    }

    /// The procedure declared under `name`.
    pub fn proc(&self, name: &str) -> Option<&ProcDecl> {
        self.procs.iter().find(|p| p.name == name)
    }
}

/// A validated `profile.toml`: the base and every variant, each already merged.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProfileDoc {
    pub target: Target,
    pub memory: Vec<Region>,
    pub variants: BTreeMap<String, (Target, Vec<Region>)>,
    pub procs: Vec<ProcDecl>,
}

impl ProfileDoc {
    /// `file` names the file in messages; `files` are the profile directory's files, relative,
    /// which a procedure's `script` must name.
    pub fn parse(text: &str, file: &Path, files: &[&str]) -> Result<Self, CtlError> {
        let raw: RawProfile = toml::from_str(text).map_err(|e| syntax(text, file, &e))?;
        let mut issues = Issues::default();
        issues.schema(&raw.schema);
        issues.planned(raw.timing.as_ref().map(Spanned::span), "[timing]");
        issues.planned(raw.stop.as_ref().map(Spanned::span), "[stop]");
        issues.planned(raw.per.as_ref().map(Spanned::span), "[per.*]");
        let base = match &raw.target {
            Some(t) => target(&mut issues, None, t.get_ref(), t.span()),
            None => {
                issues.push(0..0, "[target] is required".into());
                None
            }
        };
        let memory = regions(&mut issues, &raw.memory);
        let procs = procedure::procedures(&mut issues, &raw.proc, files, base.as_ref());
        let mut variants = BTreeMap::new();
        for (name, v) in &raw.variant {
            let v = v.get_ref();
            issues.planned(v.timing.as_ref().map(Spanned::span), "[timing]");
            let t = match (&base, &v.target) {
                (Some(base), Some(patch)) => target(&mut issues, Some(base), patch.get_ref(), patch.span()),
                (base, _) => base.clone(),
            };
            let m = match &v.memory {
                Some(list) => regions(&mut issues, list),
                None => memory.clone(),
            };
            if let Some(t) = t {
                variants.insert(name.clone(), (t, m));
            }
        }
        issues.into_result(text, file)?;
        let target = base.expect("a profile without errors has a target");
        Ok(Self { target, memory, variants, procs })
    }

    /// The variant named exactly `board`, else the base: its name, target and memory map.
    pub fn select(&self, board: Option<&str>) -> (Option<String>, Target, Vec<Region>) {
        match board.and_then(|b| self.variants.get_key_value(b)) {
            Some((name, (t, m))) => (Some(name.clone()), t.clone(), m.clone()),
            None => (None, self.target.clone(), self.memory.clone()),
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawProfile {
    schema: Spanned<i64>,
    target: Option<Spanned<RawTarget>>,
    #[serde(default)]
    memory: Vec<Spanned<RawRegion>>,
    #[serde(default)]
    variant: BTreeMap<String, Spanned<RawVariant>>,
    #[serde(default)]
    proc: BTreeMap<String, Spanned<procedure::RawProc>>,
    timing: Option<Spanned<toml::Value>>,
    stop: Option<Spanned<toml::Value>>,
    per: Option<Spanned<toml::Value>>,
}

/// `[target]`, or a variant's patch of it: every key is optional here and required keys are
/// checked on the base.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawTarget {
    name: Option<Spanned<String>>,
    cpu: Option<Spanned<String>>,
    cores: Option<Spanned<i64>>,
    word: Option<Spanned<i64>>,
    classes: Option<Vec<Spanned<String>>>,
    default_class: Option<Spanned<String>>,
    elf: Option<Spanned<toml::Value>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRegion {
    name: Spanned<String>,
    start: Spanned<i64>,
    size: Spanned<i64>,
    kind: Spanned<String>,
    width: Option<Spanned<i64>>,
    read: Option<bool>,
    write: Option<bool>,
    running: Option<bool>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawVariant {
    target: Option<Spanned<RawTarget>>,
    memory: Option<Vec<Spanned<RawRegion>>>,
    timing: Option<Spanned<toml::Value>>,
}

const DEFAULT_CLASSES: [&str; 3] = ["A", "D", "P"];
const DEFAULT_CLASS: &str = "D";
const WIDTHS: [i64; 4] = [1, 2, 4, 8];

fn text(issues: &mut Issues, v: &Spanned<String>, key: &str, quote_ok: bool) -> Option<String> {
    let s = v.get_ref();
    let ok = !s.is_empty() && !s.chars().any(char::is_control) && (quote_ok || !s.contains('"'));
    if !ok {
        let rule = if quote_ok { "" } else { " without \"" };
        issues.push(v.span(), format!("{key} must be non-empty text on one line{rule}"));
    }
    ok.then(|| s.clone())
}

fn width(issues: &mut Issues, v: &Spanned<i64>, key: &str) -> Option<u8> {
    let n = *v.get_ref();
    let ok = WIDTHS.contains(&n);
    if !ok {
        issues.push(v.span(), format!("{key} = {n} must be 1, 2, 4 or 8"));
    }
    ok.then_some(n as u8)
}

/// 1 to 8 ASCII letters or digits, upper-cased.
fn class(s: &str) -> Option<AccessClass> {
    let ok = (1..=8).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_alphanumeric());
    ok.then(|| AccessClass::parse(s).ok()).flatten()
}

/// A key's value: as given (`None` inside when it was wrong), else the base's, else an error at
/// `span` because the key is required.
fn pick<T>(
    issues: &mut Issues,
    given: Option<Option<T>>,
    base: Option<T>,
    span: &Range<usize>,
    key: &str,
) -> Option<T> {
    match (given, base) {
        (Some(v), _) => v,
        (None, Some(b)) => Some(b),
        (None, None) => {
            issues.push(span.clone(), format!("{key} is required"));
            None
        }
    }
}

/// The target `raw` describes over `base` (a variant's patch), or the base target itself.
/// `None` when a value is wrong or a required key is missing; the reasons are in `issues`.
fn target(issues: &mut Issues, base: Option<&Target>, raw: &RawTarget, span: Range<usize>) -> Option<Target> {
    issues.planned(raw.elf.as_ref().map(Spanned::span), "target.elf");
    let given = raw.name.as_ref().map(|v| text(issues, v, "target.name", true));
    let name = pick(issues, given, base.map(|b| b.name.clone()), &span, "target.name");
    let given = raw.cpu.as_ref().map(|v| text(issues, v, "target.cpu", true));
    let cpu = pick(issues, given, base.map(|b| b.cpu.clone()), &span, "target.cpu");
    let given = raw.cores.as_ref().map(|v| issues.int(v, 1, u32::MAX.into(), "target.cores").map(|n| n as u32));
    let cores = pick(issues, given, base.map(|b| b.cores), &span, "target.cores");
    let given = raw.word.as_ref().map(|v| width(issues, v, "target.word"));
    let word = pick(issues, given, base.map(|b| b.word), &span, "target.word");
    let classes: Vec<AccessClass> = match &raw.classes {
        Some(list) => list
            .iter()
            .filter_map(|v| {
                let c = class(v.get_ref());
                if c.is_none() {
                    issues.push(v.span(), format!("target.classes: {:?} is not 1 to 8 letters or digits", v.get_ref()));
                }
                c
            })
            .collect(),
        None => base.map_or_else(|| DEFAULT_CLASSES.iter().filter_map(|c| class(c)).collect(), |b| b.classes.clone()),
    };
    let (given, shown, at) = match &raw.default_class {
        Some(v) => (class(v.get_ref()), v.get_ref().clone(), v.span()),
        None => {
            let c = base.map_or_else(|| class(DEFAULT_CLASS), |b| Some(b.default_class.clone()));
            let shown = c.as_ref().map_or_else(String::new, |c| c.as_str().to_string());
            (c, shown, span.clone())
        }
    };
    let default_class = match given {
        Some(c) if classes.contains(&c) => Some(c),
        _ => {
            issues.push(at, format!("target.default_class {shown:?} is not one of target.classes"));
            None
        }
    };
    Some(Target { name: name?, cpu: cpu?, cores: cores?, word: word?, classes, default_class: default_class? })
}

/// The regions of one `memory` list. A region with a wrong value is left out; the reasons are in
/// `issues`.
fn regions(issues: &mut Issues, raw: &[Spanned<RawRegion>]) -> Vec<Region> {
    let mut out = Vec::new();
    // Every region with a valid range, also one with another error, so overlaps are reported too.
    let mut ranges: Vec<Region> = Vec::new();
    let mut names = HashSet::new();
    for r in raw {
        let span = r.span();
        let r = r.get_ref();
        let Some(name) = text(issues, &r.name, "memory.name", false) else { continue };
        if !names.insert(name.clone()) {
            issues.push(r.name.span(), format!("memory {name:?} is declared twice"));
            continue;
        }
        let start = issues.int(&r.start, 0, i64::MAX, &format!("memory {name:?} start"));
        let size = issues.int(&r.size, 1, i64::MAX, &format!("memory {name:?} size"));
        let kind = RegionKind::parse(r.kind.get_ref());
        if kind.is_none() {
            issues.push(r.kind.span(), format!("memory.kind {:?} is not ram, rom, mmio or deny", r.kind.get_ref()));
        }
        let w = match (&r.width, kind) {
            (Some(v), Some(RegionKind::Mmio)) => width(issues, v, &format!("memory {name:?} width")).map(Some),
            (None, Some(RegionKind::Mmio)) => {
                issues.push(span.clone(), format!("memory {name:?}: an mmio region needs width = 1, 2, 4 or 8"));
                None
            }
            (Some(v), Some(_)) => {
                issues.push(v.span(), format!("memory {name:?}: width applies only to mmio regions"));
                None
            }
            (_, _) => Some(None),
        };
        let (Some(start), Some(size), Some(kind)) = (start, size, kind) else { continue };
        let region = Region {
            name,
            start: start as u64,
            size: size as u64,
            kind,
            width: w.flatten(),
            read: r.read,
            write: r.write,
            running: r.running.unwrap_or(false),
        };
        for prior in ranges.iter().filter(|p| region.overlaps(p)) {
            if region.start == prior.start && region.size == prior.size {
                let m = format!("memory {:?} covers the same range as memory {:?}", region.name, prior.name);
                issues.push(span.clone(), m);
            } else if !region.contains(prior) && !prior.contains(&region) {
                issues
                    .push(span.clone(), format!("memory {:?} partially overlaps memory {:?}", region.name, prior.name));
            }
        }
        ranges.push(region.clone());
        if w.is_some() {
            out.push(region);
        }
    }
    out
}

#[cfg(test)]
#[path = "profile_tests.rs"]
mod tests;
