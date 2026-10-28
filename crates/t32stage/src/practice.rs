// SPDX-License-Identifier: Apache-2.0
//! The only module that formats PRACTICE command text. Inputs are typed (`StagePath`,
//! `VmRegion`, `Symbol`, `RegName`, `MemAddr`, `Macro`) and validated so that no builder needs
//! escaping.
//!
//! Syntax sources are the TRACE32 manuals (not in the repository): `general_ref_d`
//! (`Data.LOAD.Binary`, `Data.SAVE.Binary`, `Data.CLEARVM`, `Data.LOAD.Elf`), `ide_ref` (`MKDIR`,
//! `RM`, `RMDIR`, `TAR`, `ZIP`, `ERROR.RESet`, `STOre`), `practice_ref` (`DO`, `GLOBAL`, `END`).
//! Text the manuals do not settle carries `ASSUMED(<probe>)`: the probe that measures it.

use std::fmt;

use t32rcl::MemAddr;

use crate::{
    error::{ArgError, PathError, RegNameError, SymbolError},
    macros::Macro,
    vm::VmRegion,
};

const MAX_REL_LEN: usize = 120;
const MAX_ABS_LEN: usize = 200;
const REL_PUNCT: &[char] = &['_', '-', '.', '+', '@', '=', ',', '(', ')', ' ', '/'];
/// `"`, `&` (macro expansion), `%`, `;` (comment) and wildcards never appear in a quoted path.
const ABS_FORBIDDEN: &[char] = &['"', '&', '%', ';', '*', '?', '<', '>', '|', '`'];
const RESERVED: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8", "COM9", "LPT1", "LPT2",
    "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// Relative, `/`-separated, ASCII-safe path. One type for bundle members and stage paths.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RelPath(String);

impl RelPath {
    pub fn new(text: &str) -> Result<Self, PathError> {
        if text.is_empty() {
            return Err(PathError::Empty);
        }
        if text.starts_with('/') || text.contains('\\') {
            return Err(PathError::NotRelative(text.to_string()));
        }
        if text.len() > MAX_REL_LEN {
            return Err(PathError::TooLong { path: text.to_string(), len: text.len(), max: MAX_REL_LEN });
        }
        if let Some(bad) = text.chars().find(|c| !(c.is_ascii_alphanumeric() || REL_PUNCT.contains(c))) {
            return Err(PathError::Char { path: text.to_string(), bad });
        }
        for comp in text.split('/') {
            let ok = !comp.is_empty()
                && comp != "."
                && comp != ".."
                && !comp.ends_with('.')
                && !comp.ends_with(' ')
                && !comp.starts_with(' ');
            if !ok {
                return Err(PathError::Component { path: text.to_string(), component: comp.to_string() });
            }
            let stem = comp.split('.').next().unwrap_or("").to_ascii_uppercase();
            if RESERVED.contains(&stem.as_str()) {
                return Err(PathError::Reserved(text.to_string()));
            }
        }
        Ok(Self(text.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn join(&self, tail: &str) -> Result<RelPath, PathError> {
        RelPath::new(&format!("{}/{tail}", self.0))
    }

    /// Directory prefixes, shallowest first: `a/b/c.cmm` gives `a`, `a/b`.
    pub fn parents(&self) -> impl Iterator<Item = &str> {
        self.0.match_indices('/').map(|(i, _)| &self.0[..i])
    }

    /// Where PowerView may keep the old content when a SAVE overwrites this file: the last
    /// extension replaced by `.bak`, and the whole name plus `.bak`. Never the file itself.
    pub fn bak_candidates(&self) -> Vec<RelPath> {
        let name_at = self.0.rfind('/').map_or(0, |i| i + 1);
        let stem = match self.0[name_at..].rfind('.') {
            Some(dot) if dot > 0 => Some(&self.0[..name_at + dot]),
            _ => None,
        };
        stem.into_iter()
            .chain([self.0.as_str()])
            .filter_map(|base| RelPath::new(&format!("{base}.bak")).ok())
            .filter(|c| c != self)
            .collect()
    }

    /// ASCII case-folded form: two paths with the same fold collide on Windows.
    pub fn fold(&self) -> String {
        self.0.to_ascii_lowercase()
    }
}

impl fmt::Display for RelPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Absolute path on the PowerView host: `X:\...`, `\\server\share\...` or `/...`. Printable ASCII
/// without the characters PRACTICE would interpret inside a quoted string. No trailing separator.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct PvPath {
    text: String,
    sep: char,
}

impl PvPath {
    pub fn parse(text: &str) -> Result<Self, PathError> {
        let t = text.trim_end_matches(['\\', '/']);
        let drive = t.len() >= 2 && t.as_bytes()[0].is_ascii_alphabetic() && t.as_bytes()[1] == b':';
        if !(drive || t.starts_with("\\\\") || text.starts_with('/')) {
            return Err(PathError::NotAbsolute(text.to_string()));
        }
        if t.len() > MAX_ABS_LEN {
            return Err(PathError::TooLong { path: text.to_string(), len: t.len(), max: MAX_ABS_LEN });
        }
        if let Some(bad) = t.chars().find(|c| !(' '..='~').contains(c) || ABS_FORBIDDEN.contains(c)) {
            return Err(PathError::Char { path: text.to_string(), bad });
        }
        let sep = if t.contains('\\') { '\\' } else { '/' };
        Ok(Self { text: t.to_string(), sep })
    }

    pub fn as_str(&self) -> &str {
        &self.text
    }

    fn join(&self, rel: &str) -> String {
        format!("{}{}{}", self.text, self.sep, rel.replace('/', &self.sep.to_string()))
    }
}

impl fmt::Display for PvPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.text)
    }
}

/// Where staged files live: an ASCII-safe absolute path, written into client commands as is.
/// Resolved once per `Stage` and the single source of truth for how a client-issued command names
/// a staged file. Always spelled out: PowerView does not expand macros in commands sent over RCL
/// (practice_user, "PRACTICE Macros").
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct StageRoot(PvPath);

/// Name of the root directory below `OS.PTD()`.
pub const ROOT_DIR: &str = "t32ctl";

impl StageRoot {
    pub fn new(path: PvPath) -> Self {
        Self(path)
    }

    /// `<ptd>\t32ctl`. A temp directory that is not ASCII-safe cannot travel through the client's
    /// command bytes, so the caller needs an explicit root instead.
    pub fn from_ptd(ptd: &str) -> Result<Self, PathError> {
        PvPath::parse(ptd).and_then(|p| PvPath::parse(&p.join(ROOT_DIR))).map(Self)
    }

    pub fn as_path(&self) -> &PvPath {
        &self.0
    }

    /// `rel` with the root's separator, for a path PRACTICE appends to this root.
    pub fn native(&self, rel: &RelPath) -> String {
        rel.as_str().replace('/', &self.0.sep.to_string())
    }

    pub fn path(&self, rel: RelPath) -> StagePath {
        StagePath { root: self.clone(), rel }
    }
}

/// A location under a `StageRoot`. Only `StageRoot::path` builds one.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct StagePath {
    root: StageRoot,
    rel: RelPath,
}

impl StagePath {
    pub fn root(&self) -> &StageRoot {
        &self.root
    }

    pub fn rel(&self) -> &RelPath {
        &self.rel
    }

    pub fn join(&self, tail: &str) -> Result<StagePath, PathError> {
        Ok(StagePath { root: self.root.clone(), rel: self.rel.join(tail)? })
    }

    /// Path text for PRACTICE, unquoted.
    pub fn text(&self) -> String {
        self.root.0.join(self.rel.as_str())
    }

    /// `"<text>"`, ready for a command or a function argument, and what an operator types by hand.
    pub fn quoted(&self) -> String {
        format!("\"{}\"", self.text())
    }
}

impl fmt::Display for StagePath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.text())
    }
}

/// The function that tells whether a file exists. Which spellings DVD 02/2024 accepts is not in
/// the manuals' text. ASSUMED(tempdir): the default answers; the probe tries all three and the
/// `config` line reports the one that works.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum ExistsFn {
    #[default]
    OsFileReadable,
    OsFileExist,
    FileExist,
}

impl ExistsFn {
    pub const ALL: [ExistsFn; 3] = [ExistsFn::OsFileReadable, ExistsFn::OsFileExist, ExistsFn::FileExist];

    pub fn name(self) -> &'static str {
        match self {
            ExistsFn::OsFileReadable => "OS.FILE.readable",
            ExistsFn::OsFileExist => "OS.FILE.EXIST",
            ExistsFn::FileExist => "FILE.EXIST",
        }
    }

    pub fn call(self, p: &StagePath) -> String {
        format!("{}({})", self.name(), p.quoted())
    }
}

/// A target symbol for `Go` and `sYmbol.EXIST`: ASCII letters, digits, `_`, `.`, `\`, `:`.
/// No spaces, `&`, quotes or operators, so it is spliced into PRACTICE text without quoting.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Symbol(String);

impl Symbol {
    pub const MAX_LEN: usize = 128;

    pub fn parse(text: &str) -> Result<Self, SymbolError> {
        let symbol = || text.to_string();
        if text.is_empty() {
            return Err(SymbolError::Empty);
        }
        if text.len() > Self::MAX_LEN {
            return Err(SymbolError::TooLong { symbol: symbol(), len: text.len(), max: Self::MAX_LEN });
        }
        if text.starts_with(|c: char| c.is_ascii_digit()) {
            return Err(SymbolError::LeadingDigit(symbol()));
        }
        match text.chars().find(|c| !(c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '\\' | ':'))) {
            Some(bad) => Err(SymbolError::Char { symbol: symbol(), bad }),
            None => Ok(Self(symbol())),
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A register name as the caller typed it: `[A-Za-z][A-Za-z0-9_]{0,31}`. Builders upper-case it.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct RegName(String);

impl RegName {
    pub const MAX_LEN: usize = 32;

    pub fn new(text: &str) -> Result<Self, RegNameError> {
        let mut bytes = text.bytes();
        let ok = bytes.next().is_some_and(|b| b.is_ascii_alphabetic())
            && text.len() <= Self::MAX_LEN
            && bytes.all(|b| b.is_ascii_alphanumeric() || b == b'_');
        if ok { Ok(Self(text.to_string())) } else { Err(RegNameError(text.to_string())) }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for RegName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Characters a script argument may contain besides ASCII letters and digits. Arguments travel
/// unquoted in a macro value, are spliced into the runner's `DO` line and split by `ENTRY` at
/// blanks. ASSUMED(channel-in): none of them splits or changes an argument there (`,`
/// separates parameters in some command formats, `/` starts an option); the probe passes each
/// through `ENTRY` and compares. Widen only with that evidence.
const ARG_SAFE: &[char] = &['_', '-', '.', '/', ':', '+', '=', ',', '@'];
const MAX_ARGS_LEN: usize = 8 * 1024;

/// `DO` argument text: validated arguments joined by one space.
pub fn render_args(args: &[String]) -> Result<String, ArgError> {
    for (index, arg) in args.iter().enumerate() {
        if let Some(ch) = arg.chars().find(|c| !(c.is_ascii_alphanumeric() || ARG_SAFE.contains(c))) {
            return Err(ArgError::Unsafe { index, ch });
        }
        if arg.is_empty() {
            return Err(ArgError::Unsafe { index, ch: ' ' });
        }
    }
    let text = args.join(" ");
    if text.len() > MAX_ARGS_LEN {
        return Err(ArgError::TooLong { len: text.len(), max: MAX_ARGS_LEN });
    }
    Ok(text)
}

/// Command builders for `Session::cmd` and generated scripts.
pub mod cmd {
    use super::*;

    /// `GLOBAL &a &b ...`. ASSUMED(macro): redeclaring an existing global keeps its value.
    pub fn global(macros: &[Macro]) -> String {
        format!("GLOBAL {}", macros.iter().map(|m| m.amp()).collect::<Vec<_>>().join(" "))
    }

    pub fn mkdir(p: &StagePath) -> String {
        format!("MKDIR {}", p.quoted())
    }

    /// `StagePath` cannot hold a wildcard, which would open a file browser and hang the call.
    pub fn rm(p: &StagePath) -> String {
        format!("RM {}", p.quoted())
    }

    pub fn rmdir(p: &StagePath) -> String {
        format!("RMDIR {}", p.quoted())
    }

    /// `VM:0x........++0x........`: `++` covers `len` bytes as `len-1`. Fixed width, so generated
    /// script text has a length independent of the values.
    pub fn vm_range(r: &VmRegion) -> String {
        format!("VM:0x{:08X}++0x{:08X}", r.start(), r.len() - 1)
    }

    pub fn data_save_binary(dest: &StagePath, from: &VmRegion, zip: bool) -> String {
        let z = if zip { " /ZIP" } else { "" };
        format!("Data.SAVE.Binary {} {}{z}", dest.quoted(), vm_range(from))
    }

    /// `/UNZIP` accepts gzip from `/ZIP` or an external tool (general_ref_d).
    pub fn data_load_binary(src: &StagePath, at: u32, unzip: bool) -> String {
        let z = if unzip { " /UNZIP" } else { "" };
        format!("Data.LOAD.Binary {} VM:0x{at:08X}{z}", src.quoted())
    }

    /// ASSUMED(vm-rw): present on DVD 02/2024; without it an uninitialized unit stays residue.
    pub fn clear_vm(r: &VmRegion) -> String {
        format!("Data.CLEARVM {}", vm_range(r))
    }

    /// `TAR <archive> <dir>\*`: recursive by default (ide_ref). Build 168999 stores each name with
    /// the full path minus the drive; `tar::below` makes it relative to `<dir>`.
    pub fn tar(archive: &StagePath, dir: &StagePath) -> String {
        format!("TAR {} \"{}\\*\"", archive.quoted(), dir.text())
    }

    /// ASSUMED(tar-download): the output is plain gzip that `transfer::gunzip` reads.
    pub fn zip(src: &StagePath, dest: &StagePath) -> String {
        format!("ZIP {} {}", src.quoted(), dest.quoted())
    }

    pub fn do_script(script: &StagePath) -> String {
        format!("DO {}", script.quoted())
    }

    /// Ends every script on the PRACTICE stack. Callers send it only after they verified an empty
    /// stack before starting their own script.
    pub const END: &str = "END";
    pub const ERROR_RESET: &str = "ERROR.RESet";
    pub const BREAK: &str = "Break";

    /// `Data.LOAD.Elf <file> [/UNZIP] /NoCODE /NoReg`: symbols only, target memory and registers
    /// are never written.
    /// ASSUMED(elf-unzip): `/UNZIP` exists for `Data.LOAD.Elf` (release history 21-Oct-2024).
    pub fn load_elf_symbols(file: &StagePath, unzip: bool) -> String {
        let z = if unzip { " /UNZIP" } else { "" };
        format!("Data.LOAD.Elf {}{z} /NoCODE /NoReg", file.quoted())
    }

    /// The current breakpoint list as a script (ide_ref `STOre ~~~\my-store.cmm Break Analyzer`).
    /// ASSUMED(run-control): two lists differ only when breakpoints differ, comments aside.
    pub fn store_breakpoints(file: &StagePath) -> String {
        format!("STOre {} Break", file.quoted())
    }

    pub fn go(symbol: &Symbol) -> String {
        format!("Go {}", symbol.as_str())
    }
}

/// Expressions for `Session::eval` and `Session::eval_string`.
pub mod fnc {
    use super::*;

    /// Short form of `OS.PresentTemporaryDirectory()` (ide_ref). ASSUMED(tempdir): the short form
    /// answers on DVD 02/2024.
    pub const PTD: &str = "OS.PTD()";
    /// pyrcl `_functions.py` and t32mcp spell it this way; rcl.py uses `PRACTICE.SD()`.
    /// ASSUMED(tempdir): answers over ExecuteFunction (`depth_fn`).
    pub const STACK_DEPTH: &str = "PRACTICE.StackDepth()";
    pub const ERROR_MESSAGE: &str = "ERROR.MESSAGE()";
    pub const ERROR_CMDLINE: &str = "ERROR.CMDLINE()";
    /// Spellings tried in order (ide_func). The bare one is deprecated but answers from DVD
    /// 09/2022; the `api.` group starts at build 171257, so build 168999 refuses it.
    pub const NR_USED_CONS: [&str; 2] = ["RCL.TCP.NrUsedCons()", "api.RCL.TCP.NrUsedCons()"];

    pub fn file_size(p: &StagePath) -> String {
        format!("OS.FILE.SIZE({})", p.quoted())
    }

    pub fn dir_exists(p: &StagePath) -> String {
        format!("OS.DIR({})", p.quoted())
    }

    pub fn symbol_exists(s: &Symbol) -> String {
        format!("sYmbol.EXIST({})", s.as_str())
    }

    /// The selected core, 0-based (ide_func). Answers on build 168999 (`fn-table`).
    pub const CORE: &str = "CORE()";
    /// Number of cores in the SMP system. Answers while the target is down (`fn-table`).
    pub const CORE_NUMBER: &str = "CORE.NUMBER()";
    /// The CPU PowerView is configured for. Answers while the target is down (`fn-table`).
    pub const SYSTEM_CPU: &str = "SYStem.CPU()";

    /// A register of the selected core. Fails with "target system down" while the target is
    /// down (`fn-table`).
    pub fn register(r: &RegName) -> String {
        format!("Register({})", r.as_str().to_ascii_uppercase())
    }

    /// The physical address `a` translates to, as an address value (`A:0x...`) (general_func
    /// `TRANS.PHYSICAL`, alias `MMU.PHYSICAL`; available on build 168999, `fn-table`). The width
    /// does not take part. ASSUMED(target-read): the reply names the physical address after its
    /// last `:`, and an unmapped address is refused.
    pub fn physical(a: &MemAddr) -> String {
        format!("MMU.PHYSICAL({}:0x{:X})", a.access.as_str(), a.addr)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::num::NonZeroU32;

    fn rel(s: &str) -> RelPath {
        RelPath::new(s).unwrap()
    }

    #[test]
    fn bak_candidates_cover_both_naming_rules() {
        let names = |p: &str| rel(p).bak_candidates().iter().map(|c| c.as_str().to_string()).collect::<Vec<_>>();
        assert_eq!(names("main.cmm"), ["main.bak", "main.cmm.bak"]);
        assert_eq!(names(".t32ctl/runner.cmm"), [".t32ctl/runner.bak", ".t32ctl/runner.cmm.bak"]);
        assert_eq!(names("a.tar.gz"), ["a.tar.bak", "a.tar.gz.bak"]);
        assert_eq!(names(".t32ctl/complete"), [".t32ctl/complete.bak"]);
        assert_eq!(names("d.x/.hidden"), ["d.x/.hidden.bak"]);
        assert_eq!(names("old.bak"), ["old.bak.bak"]);
    }

    #[test]
    fn rel_path_accepts_bundle_shapes_and_refuses_the_rest() {
        for ok in ["main.cmm", "sub/b.cmm", "dir with space/x.cmm", ".t32ctl/runner.cmm", "a/b-c_d.e+f@g=h,(i)"] {
            assert!(RelPath::new(ok).is_ok(), "{ok}");
        }
        for bad in [
            "",
            "/abs",
            "a\\b",
            "a/../b",
            "a//b",
            "./a",
            "a/",
            "x.",
            "x ",
            " x",
            "con",
            "dir/NUL.txt",
            "q\"uote",
            "a&b",
            "100%",
            "한글.cmm",
            "a;b",
            "~x",
        ] {
            assert!(RelPath::new(bad).is_err(), "{bad}");
        }
        assert_eq!(rel("a/b/c.cmm").parents().collect::<Vec<_>>(), ["a", "a/b"]);
    }

    #[test]
    fn pv_path_keeps_the_root_separator_and_refuses_practice_metacharacters() {
        let p = PvPath::parse("C:\\Users\\op\\AppData\\Local\\Temp\\").unwrap();
        assert_eq!(p.as_str(), "C:\\Users\\op\\AppData\\Local\\Temp");
        assert!(PvPath::parse("/tmp/x").is_ok());
        assert!(PvPath::parse("\\\\srv\\share").is_ok());
        for bad in ["relative\\x", "C:\\R&D\\Temp", "C:\\사용자\\Temp", "C:\\a\"b", "C:\\50%"] {
            assert!(PvPath::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn stage_root_comes_only_from_an_ascii_safe_temp_dir() {
        let root = StageRoot::from_ptd("C:\\Users\\op\\AppData\\Local\\Temp").unwrap();
        assert_eq!(root.path(rel("k/a b.bin")).quoted(), "\"C:\\Users\\op\\AppData\\Local\\Temp\\t32ctl\\k\\a b.bin\"");
        assert!(matches!(
            StageRoot::from_ptd("C:\\Users\\김\\AppData\\Local\\Temp"),
            Err(PathError::Char { bad: '김', .. })
        ));
    }

    #[test]
    fn command_text_golden() {
        let root = StageRoot::from_ptd("C:\\T").unwrap();
        let p = root.path(rel("k/a.bin"));
        let r = VmRegion::new(0x0100_0000, NonZeroU32::new(0x40).unwrap()).unwrap();
        assert_eq!(
            cmd::data_save_binary(&p, &r, false),
            "Data.SAVE.Binary \"C:\\T\\t32ctl\\k\\a.bin\" VM:0x01000000++0x0000003F"
        );
        assert_eq!(
            cmd::data_save_binary(&p, &r, true),
            "Data.SAVE.Binary \"C:\\T\\t32ctl\\k\\a.bin\" VM:0x01000000++0x0000003F /ZIP"
        );
        assert_eq!(
            cmd::data_load_binary(&p, 0x0200_0000, true),
            "Data.LOAD.Binary \"C:\\T\\t32ctl\\k\\a.bin\" VM:0x02000000 /UNZIP"
        );
        assert_eq!(cmd::clear_vm(&r), "Data.CLEARVM VM:0x01000000++0x0000003F");
        let dir = root.path(rel("o"));
        assert_eq!(cmd::tar(&p, &dir), "TAR \"C:\\T\\t32ctl\\k\\a.bin\" \"C:\\T\\t32ctl\\o\\*\"");
        assert_eq!(cmd::load_elf_symbols(&p, true), "Data.LOAD.Elf \"C:\\T\\t32ctl\\k\\a.bin\" /UNZIP /NoCODE /NoReg");
        assert_eq!(ExistsFn::OsFileExist.call(&dir), "OS.FILE.EXIST(\"C:\\T\\t32ctl\\o\")");
    }

    #[test]
    fn arguments_and_symbols_are_validated_not_escaped() {
        assert_eq!(render_args(&["a".into(), "0x10".into(), "k=v,w".into()]), Ok("a 0x10 k=v,w".to_string()));
        assert_eq!(render_args(&["a b".into()]), Err(ArgError::Unsafe { index: 0, ch: ' ' }));
        assert_eq!(render_args(&["ok".into(), "x\"y".into()]), Err(ArgError::Unsafe { index: 1, ch: '"' }));
        assert!(Symbol::parse("main").is_ok());
        assert!(Symbol::parse("\\\\app\\mod\\func").is_ok());
        for bad in ["", "1abc", "a b", "a&b", "a\"b", "a+b"] {
            assert!(Symbol::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn register_names_are_kept_as_typed_and_upper_cased_in_practice_text() {
        for ok in ["x0", "sp", "pc", "cpsr", "ESR_EL1", "R", &"a".repeat(32)] {
            assert_eq!(RegName::new(ok).map(|r| r.as_str().to_string()), Ok(ok.to_string()), "{ok}");
        }
        for bad in ["", "0x", "_a", "a-b", "a b", "x0+8", "a&b", &"a".repeat(33)] {
            assert_eq!(RegName::new(bad), Err(RegNameError(bad.to_string())), "{bad}");
        }
        assert_eq!(fnc::register(&RegName::new("esr_el1").unwrap()), "Register(ESR_EL1)");
        assert_eq!(fnc::register(&RegName::new("pc").unwrap()), "Register(PC)");
        assert_eq!([fnc::CORE, fnc::CORE_NUMBER, fnc::SYSTEM_CPU], ["CORE()", "CORE.NUMBER()", "SYStem.CPU()"]);
        assert_eq!(
            RegName::new("a-b").unwrap_err().to_string(),
            "register name \"a-b\" is not a letter followed by at most 31 letters, digits or _"
        );
    }

    #[test]
    fn a_translation_names_the_address_with_its_class_and_ignores_the_width() {
        let d = |a| MemAddr::new(t32rcl::AccessClass::parse("D").unwrap(), a);
        assert_eq!(fnc::physical(&d(0x4010_7000)), "MMU.PHYSICAL(D:0x40107000)");
        assert_eq!(fnc::physical(&d(0xffff_0000_0000_0abc).with_width(4)), "MMU.PHYSICAL(D:0xFFFF000000000ABC)");
    }

    #[test]
    fn a_symbol_error_names_the_rule_it_breaks() {
        let msg = |s: &str| Symbol::parse(s).unwrap_err().to_string();
        assert_eq!(msg("a b"), "symbol \"a b\" contains ' '; use ASCII letters, digits, _ . \\ :");
        assert_eq!(msg("a+b"), "symbol \"a+b\" contains '+'; use ASCII letters, digits, _ . \\ :");
        assert_eq!(msg("1abc"), "symbol \"1abc\" starts with a digit");
        assert_eq!(msg(""), "empty symbol");
        assert_eq!(msg(&"a".repeat(129)), format!("symbol {:?} is 129 bytes, limit 128", "a".repeat(129)));
    }
}
