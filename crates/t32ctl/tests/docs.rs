// SPDX-License-Identifier: Apache-2.0
//! The documentation against the binary: the generated blocks, every documented command line, the
//! hand-written planned table, the example config, binding file and profile, and the transcripts
//! marked `transcript: mvp`.

use std::{
    collections::BTreeSet,
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};

use t32ctl::{
    Deps,
    cli::{Parsed, parse},
    config::{Binding, Config, Env, Source},
    describe,
    error::ErrorKind,
    notation::Place,
    profile::{Profile, hash, walk},
    report::{
        At, BindingView, Body, Bundled, ConfigView, CpuMatch, EvalBody, Header, Hex, MemBody, MemData, OutputFile,
        OutputsView, ParamShown, Phys, Practice, ProfileView, PvStatus, Reg64, RegValue, RegionRef, RegsBody, Report,
        RunBody, RunResult, Sourced, StatusBody, Subject, TargetView, Upload, cells,
    },
};
use t32rcl::{
    Cancel, FuncValue,
    testkit::{ManualClock, ScriptedConnector},
};
use t32stage::{PollSchedule, ScriptRc};

/// The documents whose command lines must parse or be planned.
const DOCS: [&str; 4] = ["docs/cli.md", "docs/profiles.md", "docs/cli-sessions.md", "skills/t32ctl/SKILL.md"];

/// The profile hash the documents print for the example profile; the real one depends on the
/// script contents, which the documents leave out.
const DOC_HASH: &str = "1c07e9b2";

/// The example board's profile directory, below the workspace.
const PROFILE_DIR: &str = "boards/acme/acme_evb/support/t32ctl";

/// The scripts of the example profile directory, beside `profile.toml` (`profiles.md` lists them).
const SCRIPTS: [&str; 5] = ["attach.cmm", "reset.cmm", "selftest.cmm", "flash_emmc.cmm", "emmc_setup.cmm"];

fn doc(rel: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").join(rel);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// The text from `<!-- name:start` through `<!-- name:end -->` and its newline.
fn marked<'a>(text: &'a str, name: &str) -> Option<&'a str> {
    let start = text.find(&format!("<!-- {name}:start"))?;
    let end = format!("<!-- {name}:end -->\n");
    let len = text[start..].find(&end)? + end.len();
    Some(&text[start..start + len])
}

/// The body of the fenced block of `lang` whose first line is `first`.
fn fenced(text: &str, lang: &str, first: &str) -> String {
    let open = format!("```{lang}\n{first}\n");
    let start = text.find(&open).unwrap_or_else(|| panic!("no ```{lang} block starting {first:?}")) + 4 + lang.len();
    let len = text[start..].find("\n```\n").expect("a closed block") + 1;
    text[start..start + len].to_string()
}

/// Shell words: single quotes keep everything, double quotes everything but `\"` and `\\`.
fn words(line: &str) -> Vec<String> {
    let (mut out, mut cur, mut quoted) = (Vec::new(), String::new(), false);
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        match c {
            '\'' => {
                quoted = true;
                cur.extend(chars.by_ref().take_while(|c| *c != '\''));
            }
            '"' => {
                quoted = true;
                while let Some(c) = chars.next() {
                    match c {
                        '"' => break,
                        '\\' => match chars.next() {
                            Some(n @ ('"' | '\\')) => cur.push(n),
                            Some(n) => cur.extend(['\\', n]),
                            None => cur.push('\\'),
                        },
                        c => cur.push(c),
                    }
                }
            }
            '\\' => cur.extend(chars.next()),
            c if c.is_whitespace() => {
                if quoted || !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                    quoted = false;
                }
            }
            c => cur.push(c),
        }
    }
    if quoted || !cur.is_empty() {
        out.push(cur);
    }
    out
}

fn argv(line: &str) -> Vec<OsString> {
    words(line).into_iter().map(OsString::from).collect()
}

/// A documented command line: after `$ `, `A$ `, `B$ ` or `next: `, starting with `t32ctl `.
fn command_line(line: &str) -> Option<&str> {
    let s = line.trim_start();
    ["$ ", "A$ ", "B$ ", "next: "].iter().find_map(|p| s.strip_prefix(p)).filter(|c| c.starts_with("t32ctl "))
}

/// The cells of a Markdown table row; `\|` stays inside its cell.
fn cells_of(row: &str) -> Vec<String> {
    let row = row.trim().trim_start_matches('|').trim_end_matches('|').replace("\\|", "\u{1}");
    row.split('|').map(|c| c.trim().replace('\u{1}', "|")).collect()
}

/// The code spans of a cell.
fn ticks(cell: &str) -> Vec<String> {
    cell.split('`').skip(1).step_by(2).map(String::from).collect()
}

/// The command words of a usage: the leading lowercase words after `t32ctl`.
fn path_of(usage: &str) -> Vec<String> {
    usage
        .split_whitespace()
        .skip(1)
        .take_while(|w| w.bytes().all(|b| b.is_ascii_lowercase()))
        .map(String::from)
        .collect()
}

/// The rows of each table in `text`, keyed by the first cell of its header.
fn tables(text: &str) -> Vec<(String, Vec<Vec<String>>)> {
    let mut out: Vec<(String, Vec<Vec<String>>)> = Vec::new();
    let mut lines = text.lines().peekable();
    while let Some(l) = lines.next() {
        if !l.starts_with('|') || !lines.peek().is_some_and(|n| n.starts_with("|---")) {
            continue;
        }
        lines.next();
        let mut rows = Vec::new();
        while let Some(r) = lines.next_if(|r| r.starts_with('|')) {
            rows.push(cells_of(r));
        }
        out.push((cells_of(l)[0].clone(), rows));
    }
    out
}

/// What the planned table of `cli.md` lists.
#[derive(Debug, Default)]
struct Planned {
    commands: Vec<Vec<String>>,
    /// A flag and the command paths it is planned on; `None` for every command.
    flags: Vec<(String, Option<Vec<Vec<String>>>)>,
    places: Vec<String>,
    tags: Vec<String>,
}

fn planned() -> Planned {
    let cli = doc("docs/cli.md");
    let block = marked(&cli, "planned").expect("docs/cli.md has the planned table");
    let mut p = Planned::default();
    for (head, rows) in tables(block) {
        for row in rows {
            let first = ticks(&row[0]);
            match head.as_str() {
                "Planned command" => p.commands.push(path_of(&first[0])),
                "Planned flag or place form" if first[0].starts_with("--") => {
                    let flag = first[0].split_whitespace().next().unwrap_or_default().to_string();
                    let on = ticks(&row[1]);
                    let on = (!on.is_empty()).then(|| on.iter().map(|c| words(c)).collect());
                    p.flags.push((flag, on));
                }
                "Planned flag or place form" => p.places.extend(first),
                "Planned tag" => p.tags.push(first[0].clone()),
                other => panic!("unknown table in the planned block: {other}"),
            }
        }
    }
    p
}

fn generated() -> Vec<Vec<String>> {
    describe::leaves().into_iter().map(|l| l.path).collect()
}

#[test]
fn the_generated_blocks_equal_the_binary() {
    let skill = describe::skill();
    let cli = doc("docs/cli.md");
    let skill_md = doc("skills/t32ctl/SKILL.md");
    for name in ["commands", "exits", "errors"] {
        assert_eq!(marked(&cli, name), marked(&skill, name), "docs/cli.md, block {name}");
    }
    for name in ["commands", "exits"] {
        assert_eq!(marked(&skill_md, name), marked(&skill, name), "skills/t32ctl/SKILL.md, block {name}");
    }
    let permissions = format!(
        "<!-- permissions:start (generated by `t32ctl read describe --format permissions`; do not edit) -->\n\
         ```json\n{}```\n<!-- permissions:end -->\n",
        describe::permissions()
    );
    assert_eq!(marked(&cli, "permissions"), Some(permissions.as_str()), "docs/cli.md, block permissions");
}

#[test]
fn the_planned_table_is_disjoint_from_the_binary() {
    let p = planned();
    let generated = generated();
    assert!(p.commands.len() >= 19 && p.tags.len() >= 13 && !p.flags.is_empty(), "{p:?}");
    for c in &p.commands {
        assert!(!c.is_empty() && !generated.contains(c), "planned command {c:?} is in the binary");
        let line: Vec<OsString> =
            std::iter::once("t32ctl").chain(c.iter().map(String::as_str)).map(Into::into).collect();
        assert!(matches!(parse(&line), Parsed::Usage(_)), "planned command {c:?} parses");
    }
    let leaves = describe::leaves();
    let root = t32ctl::cli::command();
    for (flag, on) in &p.flags {
        let long = flag.trim_start_matches('-');
        let has = |cmd: &clap::Command| cmd.get_arguments().any(|a| a.get_long() == Some(long));
        match on {
            None => {
                assert!(root.get_subcommands().all(|g| !has(g)), "planned {flag} is a flag of a grade");
                assert!(leaves.iter().all(|l| !has(&l.cmd)), "planned {flag} is a flag of a leaf");
            }
            Some(paths) => {
                for path in paths {
                    let leaf = leaves.iter().find(|l| l.path == *path);
                    let leaf = leaf.unwrap_or_else(|| panic!("planned {flag} is on {path:?}, which the binary lacks"));
                    assert!(!has(&leaf.cmd), "planned {flag} is a flag of {path:?}");
                }
            }
        }
    }
    assert!(!p.places.is_empty());
    for place in &p.places {
        assert!(place.parse::<Place>().is_err(), "planned place form {place} parses");
    }
    let tags: Vec<&str> = ErrorKind::ALL.iter().map(|k| k.tag()).collect();
    for tag in &p.tags {
        assert!(!tags.contains(&tag.as_str()), "planned tag {tag} is in the binary");
    }
}

#[test]
fn documented_command_lines_parse_or_are_planned() {
    let p = planned();
    let mut known = generated();
    known.extend(p.commands.iter().cloned());
    let (mut checked, mut failures) = (0, Vec::new());
    for rel in DOCS {
        for (n, line) in doc(rel).lines().enumerate() {
            let Some(cmd) = command_line(line) else { continue };
            checked += 1;
            let argv = argv(cmd);
            let Parsed::Usage(e) = parse(&argv) else { continue };
            let words: Vec<String> = argv[1..].iter().map(|w| w.to_string_lossy().into_owned()).collect();
            let path = known.iter().filter(|k| words.starts_with(k)).max_by_key(|k| k.len());
            if !path.is_some_and(|path| p.commands.contains(path)) {
                failures.push(format!("{rel}:{}: {cmd}: error[{}]: {}", n + 1, e.kind.tag(), e.message));
            }
        }
    }
    assert!(checked >= 40, "only {checked} documented command lines found");
    assert!(
        failures.is_empty(),
        "documented command lines that neither parse nor are planned:\n{}",
        failures.join("\n")
    );
}

/// A home and a workspace with the example profile of `profiles.md` in a fresh temp directory,
/// removed when dropped.
struct Board {
    root: PathBuf,
}

impl Board {
    fn new(tag: &str, profile: &str) -> Self {
        let root = std::env::temp_dir().join(format!("t32ctl-docs-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let dir = root.join("ws").join(PROFILE_DIR);
        fs::create_dir_all(&dir).expect("temp profile directory");
        fs::create_dir_all(root.join("home")).expect("temp home");
        fs::write(dir.join("profile.toml"), profile).expect("profile");
        for s in SCRIPTS {
            fs::write(dir.join(s), format!("; {s}\nENDDO\n")).expect("script");
        }
        Self { root }
    }

    fn dir(&self) -> PathBuf {
        self.root.join("ws").join(PROFILE_DIR)
    }

    fn env(&self) -> Env {
        Env {
            cwd: self.root.join("ws"),
            home: Some(self.root.join("home")),
            profile: Some(PROFILE_DIR.into()),
            auto_hosts: vec!["127.0.0.1".into()],
            ..Env::default()
        }
    }

    /// A whole call that never connects: the connector has no conversation. The real profile hash
    /// prints as the documents' example hash.
    fn call(&self, line: &str) -> String {
        let clock = ManualClock::new();
        let deps = Deps {
            env: self.env(),
            connector: Arc::new(ScriptedConnector::new(clock.clone())),
            clock: Arc::new(clock),
            cancel: Cancel::new(),
            poll: PollSchedule::DEFAULT,
        };
        let out = t32ctl::run(&argv(line), &deps);
        let real = walk(&self.dir()).map(|files| hash(&files).short()).unwrap_or_default();
        format!("{}[exit {}]\n", out.text.replace(&real, DOC_HASH), out.code)
    }
}

impl Drop for Board {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn example_profile() -> String {
    fenced(&doc("docs/profiles.md"), "toml", "# boards/acme/acme_evb/support/t32ctl/profile.toml")
}

#[test]
fn the_example_config_binding_and_profile_load() {
    let profiles = doc("docs/profiles.md");
    let config = fenced(
        &profiles,
        "toml",
        "# ~/.config/t32ctl/config.toml: personal, not in git. Every key but schema is optional.",
    );
    let c = Config::parse(&config, Path::new("config.toml")).unwrap_or_else(|e| panic!("{e}: {:?}", e.details));
    assert_eq!(
        (c.host.as_str(), c.port, c.transfer_timeout.as_ms(), c.transfer_max.bytes()),
        ("auto", 20000, 600_000, 1 << 30)
    );

    let b = Board::new("examples", &example_profile());
    fs::create_dir_all(b.root.join("ws/.git")).expect("temp repository");
    let binding =
        fenced(&profiles, "toml", "# <repo>/.t32ctl.toml: only when auto-detection picks the wrong build or profile.");
    fs::write(b.root.join("ws/.t32ctl.toml"), binding).expect("binding file");
    let env = Env { profile: None, ..b.env() };
    let bound = Binding::resolve(&env, None).unwrap_or_else(|e| panic!("{e}"));
    let profile = bound.profile.as_ref().expect("the binding file names the profile");
    assert_eq!((profile.value.clone(), profile.source), (b.dir(), Source::BindingFile));
    assert_eq!(bound.build.map(|s| s.value), Some(b.root.join("ws/build-evb")));

    let base = Profile::load(&env, &b.dir(), Some("acme_evb/a53")).unwrap_or_else(|e| panic!("{e}: {:?}", e.details));
    assert_eq!((base.variant.as_deref(), base.target.cores, base.files.len()), (None, 4, 6));
    assert!(profiles.contains("six files"), "profiles.md counts the example's files");
    let names: Vec<&str> = base.procs.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, ["attach", "reset", "selftest", "flash"]);
    let up = Profile::load(&env, &b.dir(), Some("acme_evb/a53/up")).expect("the variant loads");
    assert_eq!((up.variant.as_deref(), up.target.cores), (Some("acme_evb/a53/up"), 1));
}

/// One block marked `transcript: mvp`: the command after `$ `, and its output through `[exit N]`.
#[derive(Debug)]
struct Transcript {
    doc: &'static str,
    line: usize,
    command: String,
    output: String,
}

fn transcripts() -> Vec<Transcript> {
    let mut out = Vec::new();
    for rel in DOCS {
        let text = doc(rel);
        let lines: Vec<&str> = text.lines().collect();
        for (i, _) in lines.iter().enumerate().filter(|(_, l)| l.trim() == "<!-- transcript: mvp -->") {
            assert_eq!(lines.get(i + 1), Some(&"```console"), "{rel}:{}: a transcript is a console block", i + 2);
            let len = lines[i + 2..].iter().position(|l| *l == "```").expect("a closed console block");
            let block = &lines[i + 2..i + 2 + len];
            let command = block[0].strip_prefix("$ ").unwrap_or_else(|| panic!("{rel}:{}: no `$ ` line", i + 3));
            let output = block[1..].iter().map(|l| format!("{l}\n")).collect();
            out.push(Transcript { doc: rel, line: i + 3, command: command.to_string(), output });
        }
    }
    out
}

/// Every transcript the documents must mark, by document and command line.
const MARKED: [(&str, &str); 15] = [
    ("docs/cli.md", "t32ctl read status"),
    ("docs/cli.md", "t32ctl read mem @sp --len 64"),
    ("docs/cli.md", "t32ctl read reg x0 sp"),
    ("docs/cli.md", "t32ctl raw eval 'OS.PTD()'"),
    ("docs/profiles.md", "t32ctl read status --full"),
    ("docs/profiles.md", "t32ctl read procs"),
    ("docs/profiles.md", "t32ctl read procs flash"),
    ("docs/profiles.md", "t32ctl read proc selftest"),
    ("docs/profiles.md", "t32ctl read status"),
    ("docs/cli-sessions.md", "t32ctl raw script tools/t32/ddr_stress.cmm -- 0x40000000 0x100000"),
    ("docs/cli-sessions.md", "t32ctl run proc reset"),
    ("docs/cli-sessions.md", "t32ctl write proc reset"),
    ("docs/cli-sessions.md", "t32ctl read procs flash"),
    ("docs/cli-sessions.md", "t32ctl read status"),
    ("docs/cli-sessions.md", "t32ctl burn proc flash --part user"),
];

fn stopped(core: i64, pc: u64) -> Option<Header> {
    Some(Header::Stopped { core: Some(core), at: Some(At { pc: Hex(pc) }) })
}

/// The documents' example profile hash, all 64 digits.
fn doc_hash() -> String {
    format!("{DOC_HASH}{}", "0".repeat(56))
}

fn status(header: Option<Header>) -> Report {
    let body = StatusBody {
        target: TargetView {
            name: "acme_evb".into(),
            cpu: "CortexA53".into(),
            profile_cpu: "CortexA53".into(),
            cpu_check: CpuMatch::Match,
            cores: 4,
            profile_cores: 4,
        },
        profile: ProfileView { dir: format!("{PROFILE_DIR}/"), hash: None, files: None },
        pv: PvStatus { build: 168_999, base: Some(166_414), clients: Some(1), practice: Practice::Idle, depth: 0 },
        binding: None,
        config: None,
        state_dir: None,
    };
    report("read status", header, Body::Status(Box::new(body)))
}

fn report(command: &str, header: Option<Header>, body: Body) -> Report {
    let mut r = Report::new(command);
    r.header = header;
    r.body = Some(body);
    r
}

fn proc_run(name: &str, grade: &'static str, params: Vec<ParamShown>, result: RunResult, outputs: OutputsView) -> Body {
    let profile = Bundled { hash: Some(doc_hash()), root: None, files: 6, cached: true };
    Body::Run(Box::new(RunBody {
        subject: Subject::Proc { name: name.into(), grade, profile, params },
        result: Some(result),
        outputs,
    }))
}

fn finished(rc: ScriptRc, message: &str, after_ms: u64, burn: bool) -> RunResult {
    RunResult {
        outcome: t32ctl::report::Outcome::Finished,
        rc,
        message: message.into(),
        error: None,
        line: None,
        after_ms,
        burn,
    }
}

fn outputs(dir: &str, files: &[(&str, u64)]) -> OutputsView {
    OutputsView {
        dir: Some(dir.into()),
        files: files.iter().map(|(name, bytes)| OutputFile { name: (*name).into(), bytes: *bytes }).collect(),
        missing: Vec::new(),
    }
}

fn no_outputs() -> OutputsView {
    OutputsView { dir: None, files: Vec::new(), missing: Vec::new() }
}

fn param(name: &str, value: &str) -> ParamShown {
    ParamShown { name: name.into(), value: value.into(), file: None }
}

/// The report a marked transcript shows, rendered with its exit line; `None` for a transcript
/// this test does not know.
fn expected(doc: &str, command: &str, board: &Board, broken: &Board) -> Option<String> {
    let r = match (doc, command) {
        (_, "t32ctl read procs" | "t32ctl read procs flash" | "t32ctl run proc reset") => {
            return Some(board.call(command));
        }
        ("docs/profiles.md", "t32ctl read status") => return Some(broken.call(command)),
        ("docs/cli.md", "t32ctl read status") => status(stopped(0, 0x4008_3e10)),
        ("docs/cli-sessions.md", "t32ctl read status") => status(Some(Header::Running { cores: Some(4) })),
        ("docs/profiles.md", "t32ctl read status --full") => {
            let mut r = status(stopped(0, 0x4008_3e10));
            let Some(Body::Status(s)) = r.body.as_mut() else { unreachable!() };
            s.profile = ProfileView { dir: format!("{PROFILE_DIR}/"), hash: Some(doc_hash()), files: Some(6) };
            s.binding = Some(BindingView {
                build: Some(Sourced { path: "build".into(), source: Source::Auto.text().into() }),
                profile_from: Source::BoardDir.text().into(),
                variant: None,
                board: Some("acme_evb/a53".into()),
            });
            s.config = Some(ConfigView {
                file: Some("~/.config/t32ctl/config.toml".into()),
                host: "auto".into(),
                attached: Some("127.0.0.1".into()),
                port: 20000,
            });
            s.state_dir = Some("~/.local/state/t32ctl/pv/auto-20000/".into());
            r
        }
        ("docs/cli.md", "t32ctl read mem @sp --len 64") => {
            let values: [u64; 8] =
                [0x4010_7e60, 0x4008_1fa4, 0, 0x4008_f2d8, 0x4008_0b1c, 0x6000_0005, 0x4010_4000, 0x8];
            let bytes: Vec<u8> = values.iter().flat_map(|v| v.to_le_bytes()).collect();
            let mem = MemBody {
                place: "D:0x40107e40".into(),
                shown: "0x40107e40".into(),
                class: "D".into(),
                addr: 0x4010_7e40,
                phys: Some(Phys(0x4010_7e40)),
                len: 64,
                width: 8,
                regions: vec![RegionRef { name: "ddr".into(), kind: "ram" }],
                data: MemData::Cells(cells(&bytes, 8)),
            };
            report("read mem", stopped(2, 0x4008_1f10), Body::Mem(mem))
        }
        ("docs/cli.md", "t32ctl read reg x0 sp") => {
            let regs = vec![
                RegValue { name: "x0".into(), value: Reg64(0) },
                RegValue { name: "sp".into(), value: Reg64(0x4010_7e40) },
            ];
            report("read reg", stopped(0, 0x4008_3e10), Body::Regs(RegsBody { core: Some(0), regs }))
        }
        ("docs/cli.md", "t32ctl raw eval 'OS.PTD()'") => {
            let value = FuncValue::Text(r"C:\Users\op\AppData\Local\Temp".into());
            report("raw eval", stopped(0, 0x4008_3e10), Body::Eval(EvalBody::new(&value)))
        }
        ("docs/profiles.md", "t32ctl read proc selftest") => {
            let result = finished(ScriptRc::Int(0), "clocks locked, pmic ok", 2400, false);
            let out = outputs("~/.local/state/t32ctl/out/p-1f2c/", &[("selftest.txt", 922)]);
            report("read proc", stopped(0, 0x4008_3e10), proc_run("selftest", "read", Vec::new(), result, out))
        }
        ("docs/cli-sessions.md", "t32ctl write proc reset") => {
            let result = finished(ScriptRc::Int(0), "", 1900, false);
            report("write proc", stopped(0, 0x0), proc_run("reset", "write", Vec::new(), result, no_outputs()))
        }
        ("docs/cli-sessions.md", "t32ctl burn proc flash --part user") => {
            let upload = Upload { path: r"C:\T\t32ctl\in-5d1e\zephyr.bin".into(), bytes: 2_411_724, slices: 1 };
            let params = vec![
                ParamShown { file: Some(upload), ..param("image", "build/zephyr/zephyr.bin") },
                param("part", "user"),
                param("offset", "0"),
            ];
            let result = finished(ScriptRc::Int(0), "wrote 2411724 bytes to user at 0x0, verified", 38_400, true);
            let out = outputs("~/.local/state/t32ctl/out/p-3e07/", &[("verify.log", 1229)]);
            report("burn proc", stopped(0, 0x0010_0840), proc_run("flash", "burn", params, result, out))
        }
        ("docs/cli-sessions.md", "t32ctl raw script tools/t32/ddr_stress.cmm -- 0x40000000 0x100000") => {
            let result = RunResult {
                outcome: t32ctl::report::Outcome::Halted,
                rc: ScriptRc::Unset,
                message: String::new(),
                error: Some("memory access failed at A:0x40000400".into()),
                line: Some("Data.Test A:0x40000000++0xfffff /Prime".into()),
                after_ms: 3200,
                burn: false,
            };
            let bundle = Bundled { hash: None, root: Some("tools/t32".into()), files: 6, cached: true };
            let run = RunBody {
                subject: Subject::Script { entry: "tools/t32/ddr_stress.cmm".into(), bundle },
                result: Some(result),
                outputs: outputs("~/.local/state/t32ctl/out/s-0b91/", &[("stress.log", 2150)]),
            };
            report("raw script", stopped(0, 0x4008_5c40), Body::Run(Box::new(run)))
        }
        _ => return None,
    };
    let Parsed::Cli(cli) = parse(&argv(command)) else { panic!("{command} does not parse") };
    assert_eq!(cli.spec().path.join(" "), r.command, "{command}: the report is another leaf's");
    Some(format!("{}[exit {}]\n", r.text(), r.exit().code()))
}

#[test]
fn mvp_transcripts_equal_rendered_reports() {
    let profile = example_profile();
    let board = Board::new("transcripts", &profile);
    let reset = profile.find("[proc.reset]").expect("the example declares reset");
    let at = reset + profile[reset..].find("timeout = ").expect("reset has a timeout");
    let broken = Board::new("broken", &format!("{}timout{}", &profile[..at], &profile[at + "timeout".len()..]));
    let found = transcripts();
    let keys: BTreeSet<(&str, &str)> = found.iter().map(|t| (t.doc, t.command.as_str())).collect();
    assert_eq!(keys, MARKED.into_iter().collect::<BTreeSet<_>>(), "the marked transcripts");
    for t in &found {
        let want = expected(t.doc, &t.command, &board, &broken)
            .unwrap_or_else(|| panic!("{}:{}: no report for {}", t.doc, t.line, t.command));
        assert_eq!(t.output, want, "{}:{}: {}", t.doc, t.line, t.command);
    }
}
