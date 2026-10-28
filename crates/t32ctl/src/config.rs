// SPDX-License-Identifier: Apache-2.0
//! The process environment, the connection config, the binding of a workspace to a build and a
//! profile, and where t32ctl keeps its state. Everything below `main` reads the environment
//! through `Env`, never through `std::env`.

use std::{
    fs, io,
    ops::Range,
    path::{Path, PathBuf},
};

use serde::Deserialize;
use t32rcl::{Endpoint, Tuning};
use t32stage::{PvPath, VmWindow, host, pv_state_dir};
use toml::Spanned;

use crate::{
    error::{CtlError, ErrorKind},
    notation::{Size, TransferTimeout},
};

/// What t32ctl reads from the process environment.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Env {
    pub cwd: PathBuf,
    pub home: Option<PathBuf>,
    pub xdg_config_home: Option<PathBuf>,
    pub xdg_state_home: Option<PathBuf>,
    /// `T32CTL_BUILD`.
    pub build: Option<PathBuf>,
    /// `T32CTL_PROFILE`.
    pub profile: Option<PathBuf>,
    /// The hosts `host = "auto"` tries, in order.
    pub auto_hosts: Vec<String>,
}

impl Env {
    /// An empty variable counts as unset.
    pub fn from_process() -> Self {
        let var = |name: &str| std::env::var_os(name).filter(|v| !v.is_empty()).map(PathBuf::from);
        Self {
            cwd: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            home: var("HOME"),
            xdg_config_home: var("XDG_CONFIG_HOME"),
            xdg_state_home: var("XDG_STATE_HOME"),
            build: var("T32CTL_BUILD"),
            profile: var("T32CTL_PROFILE"),
            auto_hosts: host::system_auto_candidates(),
        }
    }

    /// `p` below the current directory as a relative path, below `$HOME` with `~`, else as is.
    pub fn display(&self, p: &Path) -> String {
        if let Ok(rel) = p.strip_prefix(&self.cwd) {
            return if rel.as_os_str().is_empty() { ".".into() } else { rel.display().to_string() };
        }
        if let Some(rel) = self.home.as_deref().and_then(|h| p.strip_prefix(h).ok()) {
            return if rel.as_os_str().is_empty() { "~".into() } else { format!("~/{}", rel.display()) };
        }
        p.display().to_string()
    }

    /// An XDG base directory must be absolute; a relative one is ignored.
    fn xdg(&self, v: &Option<PathBuf>, fallback: &str) -> Option<PathBuf> {
        v.clone().filter(|p| p.is_absolute()).or_else(|| self.home.as_ref().map(|h| h.join(fallback)))
    }
}

/// Where t32ctl's own files live.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Paths {
    /// `$XDG_CONFIG_HOME/t32ctl/config.toml`, else `$HOME/.config/t32ctl/config.toml`.
    pub config_file: Option<PathBuf>,
    /// `$XDG_STATE_HOME/t32ctl`, else `$HOME/.local/state/t32ctl`.
    pub state_base: PathBuf,
}

impl Paths {
    pub fn new(env: &Env) -> Result<Self, CtlError> {
        let config_file = env.xdg(&env.xdg_config_home, ".config").map(|d| d.join("t32ctl/config.toml"));
        let state_base = env
            .xdg(&env.xdg_state_home, ".local/state")
            .map(|d| d.join("t32ctl"))
            .ok_or_else(|| CtlError::new(ErrorKind::StateDir, "no state directory: set XDG_STATE_HOME or HOME"))?;
        Ok(Self { config_file, state_base })
    }

    /// The t32stage state directory of one PowerView: journals and the `VM:` lease.
    pub fn pv_dir(&self, config: &Config) -> PathBuf {
        pv_state_dir(&self.state_base, &config.host, config.port)
    }

    /// Output files of scripts, procedures and `read mem --to`.
    pub fn out_base(&self) -> PathBuf {
        self.state_base.join("out")
    }
}

/// The operator's connection config. Every key is optional; a missing file means the defaults.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    /// The file the values came from; `None` for the defaults.
    pub file: Option<PathBuf>,
    /// `auto` or a host, as configured.
    pub host: String,
    pub port: u16,
    pub window: VmWindow,
    pub stage_root: Option<PvPath>,
    pub tuning: Tuning,
    pub transfer_timeout: TransferTimeout,
    pub transfer_max: Size,
}

/// Largest `[calls] transfer_max`.
const TRANSFER_MAX_LIMIT: u64 = 4 << 30;
/// Largest memory request the client sends; measured on build 168999 (`vm-rw`).
const CHUNK: usize = Tuning::MAX_CHUNK;

impl Default for Config {
    fn default() -> Self {
        Self {
            file: None,
            host: "auto".into(),
            port: Endpoint::DEFAULT_PORT,
            window: VmWindow::new(VmWindow::DEFAULT_BASE, VmWindow::DEFAULT_LEN).expect("the default window is valid"),
            stage_root: None,
            tuning: Tuning::new(CHUNK, CHUNK).expect("the measured chunk is valid"),
            transfer_timeout: TransferTimeout::from_ms(600_000).expect("10m is in range"),
            transfer_max: Size(1 << 30),
        }
    }
}

impl Config {
    /// The defaults when `file` is `None` or does not exist.
    pub fn load(file: Option<&Path>) -> Result<Self, CtlError> {
        let Some(file) = file else { return Ok(Self::default()) };
        match fs::read_to_string(file) {
            Ok(text) => Self::parse(&text, file),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(invalid(format!("{}: {e}", file.display()))),
        }
    }

    pub fn parse(text: &str, file: &Path) -> Result<Self, CtlError> {
        let raw: RawConfig = toml::from_str(text).map_err(|e| syntax(text, file, &e))?;
        let mut issues = Issues::default();
        issues.schema(&raw.schema);
        issues.planned(raw.lease.as_ref().map(Spanned::span), "[lease]");
        issues.planned(raw.tuning.go_leaves_breakpoint.as_ref().map(Spanned::span), "tuning.go_leaves_breakpoint");
        issues.planned(raw.calls.lock_wait.as_ref().map(Spanned::span), "calls.lock_wait");
        let mut c = Config { file: Some(file.to_path_buf()), ..Config::default() };
        if let Some(h) = raw.connection.host {
            let ok = !h.get_ref().is_empty() && h.get_ref().bytes().all(|b| b.is_ascii_graphic());
            issues.check(ok, h.span(), "connection.host must be \"auto\" or a host name or address");
            c.host = h.into_inner();
        }
        if let Some(p) = raw.connection.port {
            c.port = issues.int(&p, 1, u16::MAX.into(), "connection.port").map_or(c.port, |v| v as u16);
        }
        let base = raw.staging.vm_base.as_ref().map(|b| issues.int(b, 0, u32::MAX.into(), "staging.vm_base"));
        let len = raw.staging.vm_len.as_ref().map(|l| issues.int(l, 1, u32::MAX.into(), "staging.vm_len"));
        if base != Some(None) && len != Some(None) {
            let b = base.flatten().map_or(c.window.base(), |v| v as u32);
            let l = len.flatten().map_or(c.window.len(), |v| v as u32);
            match VmWindow::new(b, l) {
                Ok(w) => c.window = w,
                Err(e) => {
                    let span = raw.staging.vm_len.or(raw.staging.vm_base).map_or(0..0, |v| v.span());
                    issues.push(span, format!("staging.vm_base and staging.vm_len: {e}"));
                }
            }
        }
        if let Some(r) = raw.staging.root {
            match PvPath::parse(r.get_ref()) {
                Ok(p) => c.stage_root = Some(p),
                Err(e) => issues.push(r.span(), format!("staging.root: {e}")),
            }
        }
        let max = CHUNK as i64;
        let chunk = |issues: &mut Issues, v: Option<Spanned<i64>>, key| {
            v.map_or(Some(CHUNK as i64), |v| issues.int(&v, Tuning::MIN_CHUNK as i64, max, key))
        };
        let read = chunk(&mut issues, raw.tuning.read_chunk, "tuning.read_chunk");
        let write = chunk(&mut issues, raw.tuning.write_chunk, "tuning.write_chunk");
        if let (Some(r), Some(w)) = (read, write) {
            c.tuning = Tuning::new(r as usize, w as usize).expect("chunks were range-checked");
        }
        if let Some(t) = raw.calls.transfer_timeout {
            match t.get_ref().parse::<TransferTimeout>() {
                Ok(v) => c.transfer_timeout = v,
                Err(e) => issues.push(t.span(), format!("calls.transfer_timeout: {e}")),
            }
        }
        if let Some(m) = raw.calls.transfer_max {
            match m.get_ref().parse::<Size>() {
                Ok(v) if (1..=TRANSFER_MAX_LIMIT).contains(&v.bytes()) => c.transfer_max = v,
                Ok(v) => issues.push(m.span(), format!("calls.transfer_max {v} is outside 1..4G")),
                Err(e) => issues.push(m.span(), format!("calls.transfer_max: {e}")),
            }
        }
        issues.into_result(text, file).map(|()| c)
    }

    /// The hosts to try, in order.
    pub fn hosts(&self, env: &Env) -> Vec<String> {
        if self.host == "auto" { env.auto_hosts.clone() } else { vec![self.host.clone()] }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    schema: Spanned<i64>,
    #[serde(default)]
    connection: RawConnection,
    #[serde(default)]
    staging: RawStaging,
    #[serde(default)]
    tuning: RawTuning,
    #[serde(default)]
    calls: RawCalls,
    lease: Option<Spanned<toml::Value>>,
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConnection {
    host: Option<Spanned<String>>,
    port: Option<Spanned<i64>>,
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawStaging {
    vm_base: Option<Spanned<i64>>,
    vm_len: Option<Spanned<i64>>,
    root: Option<Spanned<String>>,
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawTuning {
    read_chunk: Option<Spanned<i64>>,
    write_chunk: Option<Spanned<i64>>,
    go_leaves_breakpoint: Option<Spanned<toml::Value>>,
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawCalls {
    transfer_timeout: Option<Spanned<String>>,
    transfer_max: Option<Spanned<String>>,
    lock_wait: Option<Spanned<toml::Value>>,
}

fn invalid(message: String) -> CtlError {
    CtlError::new(ErrorKind::ProfileInvalid, message)
}

/// `line:column` of a byte offset, both 1-based; the column counts characters.
pub fn line_col(text: &str, offset: usize) -> (usize, usize) {
    let before = &text[..offset.min(text.len())];
    let line = before.matches('\n').count() + 1;
    let col = before.rsplit('\n').next().map_or(0, |l| l.chars().count()) + 1;
    (line, col)
}

fn at(text: &str, file: &Path, span: &Range<usize>, message: &str) -> String {
    let (line, col) = line_col(text, span.start);
    format!("{}:{line}:{col}: {message}", file.display())
}

/// The first syntax or schema error: TOML parsing stops there.
fn syntax(text: &str, file: &Path, e: &toml::de::Error) -> CtlError {
    let message = e.message().trim_end();
    match e.span() {
        Some(span) => invalid(at(text, file, &span, message)),
        None => invalid(format!("{}: {message}", file.display())),
    }
}

/// Every semantic error of one file.
#[derive(Default)]
struct Issues(Vec<(Range<usize>, String)>);

impl Issues {
    fn push(&mut self, span: Range<usize>, message: String) {
        self.0.push((span, message));
    }

    fn check(&mut self, ok: bool, span: Range<usize>, message: &str) {
        if !ok {
            self.push(span, message.to_string());
        }
    }

    fn schema(&mut self, v: &Spanned<i64>) {
        self.check(*v.get_ref() == 1, v.span(), "schema must be 1");
    }

    fn planned(&mut self, span: Option<Range<usize>>, key: &str) {
        if let Some(span) = span {
            self.push(span, format!("{key} is planned, not in this t32ctl"));
        }
    }

    fn int(&mut self, v: &Spanned<i64>, min: i64, max: i64, key: &str) -> Option<i64> {
        let n = *v.get_ref();
        if (min..=max).contains(&n) {
            Some(n)
        } else {
            self.push(v.span(), format!("{key} = {n} is outside {min}..={max}"));
            None
        }
    }

    /// The first error in the file is the message; the others follow as details, each with its
    /// position.
    fn into_result(mut self, text: &str, file: &Path) -> Result<(), CtlError> {
        self.0.sort_by_key(|(span, _)| span.start);
        let mut all = self.0.iter().map(|(span, m)| at(text, file, span, m));
        match all.next() {
            None => Ok(()),
            Some(first) => Err(invalid(first).with_details(all.collect())),
        }
    }
}

/// Where a binding value came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    /// `--build DIR`.
    Flag,
    /// `T32CTL_BUILD` or `T32CTL_PROFILE`.
    Var(&'static str),
    /// `<repo>/.t32ctl.toml`.
    BindingFile,
    /// `./build`, because `./build/CMakeCache.txt` exists.
    Auto,
    /// `BOARD_DIR` in the build's `CMakeCache.txt`.
    BoardDir,
}

impl Source {
    pub fn text(self) -> &'static str {
        match self {
            Source::Flag => "--build",
            Source::Var(name) => name,
            Source::BindingFile => ".t32ctl.toml",
            Source::Auto => "auto-detection: ./build/CMakeCache.txt",
            Source::BoardDir => "BOARD_DIR",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Sourced<T> {
    pub value: T,
    pub source: Source,
}

/// Which build directory, profile directory and board a workspace uses.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Binding {
    pub build: Option<Sourced<PathBuf>>,
    pub profile: Option<Sourced<PathBuf>>,
    /// `BOARD` from the build's `CMakeCache.txt`: selects a profile variant.
    pub board: Option<String>,
    /// The profile sources tried, in order, with why each gave nothing.
    pub tried: Vec<String>,
}

pub const BINDING_FILE: &str = ".t32ctl.toml";
const CMAKE_CACHE: &str = "CMakeCache.txt";

impl Binding {
    /// Per key, highest first: `--build`, the environment, `<repo>/.t32ctl.toml`, then the build's
    /// `CMakeCache.txt`. Relative paths resolve against the current directory, and those of the
    /// binding file against the repository root.
    pub fn resolve(env: &Env, build_flag: Option<&Path>) -> Result<Self, CtlError> {
        let root = repo_root(&env.cwd);
        let file = match &root {
            Some(r) => BindingFile::load(&r.join(BINDING_FILE))?,
            None => None,
        };
        let from_file = |pick: fn(&BindingFile) -> Option<&PathBuf>| {
            file.as_ref().and_then(pick).zip(root.as_ref()).map(|(p, r)| below(r, p))
        };
        let auto = env.cwd.join("build");
        let build = build_flag
            .map(|p| Sourced { value: below(&env.cwd, p), source: Source::Flag })
            .or_else(|| {
                env.build.as_ref().map(|p| Sourced { value: below(&env.cwd, p), source: Source::Var("T32CTL_BUILD") })
            })
            .or_else(|| from_file(|f| f.build.as_ref()).map(|value| Sourced { value, source: Source::BindingFile }))
            .or_else(|| auto.join(CMAKE_CACHE).is_file().then_some(Sourced { value: auto, source: Source::Auto }));
        let cache = match &build {
            Some(b) => read_cache(&b.value.join(CMAKE_CACHE))?,
            None => Cache::default(),
        };
        let mut tried = Vec::new();
        if env.profile.is_none() {
            tried.push("T32CTL_PROFILE: unset".to_string());
        }
        let profile = if let Some(p) = &env.profile {
            Some(Sourced { value: below(&env.cwd, p), source: Source::Var("T32CTL_PROFILE") })
        } else if let Some(value) = from_file(|f| f.profile.as_ref()) {
            Some(Sourced { value, source: Source::BindingFile })
        } else {
            tried.push(match &root {
                Some(r) if file.is_some() => format!("{}: no profile key", env.display(&r.join(BINDING_FILE))),
                Some(r) => format!("{}: none", env.display(&r.join(BINDING_FILE))),
                None => format!("{BINDING_FILE}: not in a git repository"),
            });
            match (&build, &cache.board_dir) {
                (Some(b), Some(d)) => {
                    Some(Sourced { value: below(&b.value, d).join("support/t32ctl"), source: Source::BoardDir })
                }
                (Some(b), None) => {
                    tried.push(format!("BOARD_DIR: not in {}", env.display(&b.value.join(CMAKE_CACHE))));
                    None
                }
                (None, _) => {
                    tried.push("BOARD_DIR: no build directory (--build, T32CTL_BUILD, .t32ctl.toml, ./build)".into());
                    None
                }
            }
        };
        if let Some(p) = &profile
            && !p.value.ends_with("support/t32ctl")
        {
            return Err(invalid(format!(
                "{}: profile directory {} is not a board's support/t32ctl directory",
                p.source.text(),
                env.display(&p.value)
            )));
        }
        Ok(Self { build, profile, board: cache.board, tried })
    }
}

/// `p` resolved against `dir`, without trailing or doubled separators.
fn below(dir: &Path, p: &Path) -> PathBuf {
    dir.join(p).components().collect()
}

/// The nearest ancestor of `cwd`, itself included, that holds `.git` (a directory or a file).
fn repo_root(cwd: &Path) -> Option<PathBuf> {
    cwd.ancestors().find(|d| d.join(".git").symlink_metadata().is_ok()).map(Path::to_path_buf)
}

#[derive(Debug, Default)]
struct BindingFile {
    build: Option<PathBuf>,
    profile: Option<PathBuf>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawBindingFile {
    schema: Spanned<i64>,
    build: Option<String>,
    profile: Option<String>,
}

impl BindingFile {
    fn load(path: &Path) -> Result<Option<Self>, CtlError> {
        let text = match fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(invalid(format!("{}: {e}", path.display()))),
        };
        let raw: RawBindingFile = toml::from_str(&text).map_err(|e| syntax(&text, path, &e))?;
        let mut issues = Issues::default();
        issues.schema(&raw.schema);
        issues.into_result(&text, path)?;
        Ok(Some(Self { build: raw.build.map(PathBuf::from), profile: raw.profile.map(PathBuf::from) }))
    }
}

/// The two `CMakeCache.txt` entries the binding reads.
#[derive(Debug, Default)]
struct Cache {
    board_dir: Option<PathBuf>,
    board: Option<String>,
}

/// Lines `KEY:TYPE=VALUE`; a missing file is an empty cache.
fn read_cache(path: &Path) -> Result<Cache, CtlError> {
    let bytes = match fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Cache::default()),
        Err(e) => return Err(invalid(format!("{}: {e}", path.display()))),
    };
    let mut cache = Cache::default();
    for line in String::from_utf8_lossy(&bytes).lines() {
        let Some((key, value)) = line.split_once('=') else { continue };
        match key.split(':').next() {
            Some("BOARD_DIR") if !value.is_empty() => cache.board_dir = Some(PathBuf::from(value)),
            Some("BOARD") if !value.is_empty() => cache.board = Some(value.to_string()),
            _ => {}
        }
    }
    Ok(cache)
}

#[cfg(test)]
#[path = "config_tests.rs"]
mod tests;
