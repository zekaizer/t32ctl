// SPDX-License-Identifier: Apache-2.0
use std::fs;

use super::*;
use crate::config::{Source, Sourced};

const FILE: &str = "p/profile.toml";

/// The board of the contract's example, without the planned keys.
const ACME: &str = r#"schema = 1

[target]
name = "acme_evb"
cpu = "CortexA53"
cores = 4
word = 8

[[memory]]
name = "ddr"
start = 0x40000000
size = 0x40000000
kind = "ram"
running = true

[[memory]]
name = "uart0"
start = 0x09000000
size = 0x00001000
kind = "mmio"
width = 4
write = true

[[memory]]
name = "uart0-fifo"
start = 0x09000000
size = 0x00000004
kind = "deny"

[variant."acme_evb/a53/up"]
target = { cores = 1 }

[variant."acme_evb/a53/sram"]
target = { classes = ["A", "D"], default_class = "A" }
memory = [{ name = "sram", start = 0x00100000, size = 0x00040000, kind = "ram" }]
"#;

fn parse(text: &str) -> Result<ProfileDoc, CtlError> {
    ProfileDoc::parse(text, Path::new(FILE))
}

fn class(c: &str) -> AccessClass {
    AccessClass::parse(c).unwrap()
}

/// The error of a profile that differs from `ACME` by one replacement.
fn error_of(from: &str, to: &str) -> CtlError {
    assert!(ACME.contains(from), "{from}");
    parse(&ACME.replacen(from, to, 1)).unwrap_err()
}

#[test]
fn a_profile_parses_with_the_default_classes() {
    let doc = parse(ACME).unwrap();
    let (variant, t, memory) = doc.select(None);
    assert_eq!(variant, None);
    assert_eq!(
        t,
        Target {
            name: "acme_evb".into(),
            cpu: "CortexA53".into(),
            cores: 4,
            word: 8,
            classes: vec![class("A"), class("D"), class("P")],
            default_class: class("D"),
        }
    );
    let region = |name: &str, start, size, kind, width, running, write| Region {
        name: name.into(),
        start,
        size,
        kind,
        width,
        read: None,
        write,
        running,
    };
    assert_eq!(
        memory,
        [
            region("ddr", 0x4000_0000, 0x4000_0000, RegionKind::Ram, None, true, None),
            region("uart0", 0x0900_0000, 0x1000, RegionKind::Mmio, Some(4), false, Some(true)),
            region("uart0-fifo", 0x0900_0000, 4, RegionKind::Deny, None, false, None),
        ]
    );
}

#[test]
fn the_build_board_selects_a_variant_exactly_and_anything_else_the_base() {
    let doc = parse(ACME).unwrap();
    let (variant, t, memory) = doc.select(Some("acme_evb/a53/up"));
    assert_eq!((variant.as_deref(), t.cores, t.word, memory.len()), (Some("acme_evb/a53/up"), 1, 8, 3));
    let (variant, t, memory) = doc.select(Some("acme_evb/a53/sram"));
    assert_eq!(variant.as_deref(), Some("acme_evb/a53/sram"));
    assert_eq!((t.cores, t.classes, t.default_class), (4, vec![class("A"), class("D")], class("A")));
    let names: Vec<&str> = memory.iter().map(|r| r.name.as_str()).collect();
    assert_eq!(names, ["sram"], "a variant's memory replaces the whole list");
    for other in [Some("acme_evb/a53"), Some("ACME_EVB/A53/UP"), None] {
        let (variant, t, _) = doc.select(other);
        assert_eq!((variant, t.cores), (None, 4), "{other:?}");
    }
}

#[test]
fn every_semantic_error_is_reported_in_file_order_with_its_line_and_column() {
    let text = ACME
        .replacen("cores = 4", "cores = 0", 1)
        .replacen("word = 8", "word = 3\nclasses = [\"A\", \"D-1\"]\ndefault_class = \"P\"", 1)
        .replacen("width = 4\n", "", 1)
        .replacen("start = 0x09000000\nsize = 0x00000004", "start = 0x09000800\nsize = 0x00001000", 1);
    let e = parse(&text).unwrap_err();
    assert_eq!(e.kind, ErrorKind::ProfileInvalid);
    assert_eq!(e.message, "p/profile.toml:6:9: target.cores = 0 is outside 1..=4294967295");
    assert_eq!(
        e.details,
        [
            "p/profile.toml:7:8: target.word = 3 must be 1, 2, 4 or 8",
            "p/profile.toml:8:17: target.classes: \"D-1\" is not 1 to 8 letters or digits",
            "p/profile.toml:9:17: target.default_class \"P\" is not one of target.classes",
            "p/profile.toml:18:1: memory \"uart0\": an mmio region needs width = 1, 2, 4 or 8",
            "p/profile.toml:25:1: memory \"uart0-fifo\" partially overlaps memory \"uart0\"",
        ]
    );
}

#[test]
fn region_rules() {
    let cases = [
        (
            "kind = \"ram\"\nrunning",
            "kind = \"flash\"\nrunning",
            "13:8: memory.kind \"flash\" is not ram, rom, mmio or deny",
        ),
        ("size = 0x40000000", "size = 0", "12:8: memory \"ddr\" size = 0 is outside 1..=9223372036854775807"),
        ("start = 0x40000000", "start = -1", "11:9: memory \"ddr\" start = -1 is outside 0..=9223372036854775807"),
        ("width = 4", "width = 3", "21:9: memory \"uart0\" width = 3 must be 1, 2, 4 or 8"),
        ("running = true", "width = 8", "14:9: memory \"ddr\": width applies only to mmio regions"),
        ("name = \"uart0-fifo\"", "name = \"ddr\"", "25:8: memory \"ddr\" is declared twice"),
        ("name = \"ddr\"", "name = \"\"", "10:8: memory.name must be non-empty text on one line without \""),
        (
            "size = 0x00000004",
            "size = 0x00001000",
            "24:1: memory \"uart0-fifo\" covers the same range as memory \"uart0\"",
        ),
    ];
    for (from, to, want) in cases {
        let e = error_of(from, to);
        assert_eq!((e.message.strip_prefix("p/profile.toml:"), e.details.len()), (Some(want), 0), "{to}");
    }
    let nested = ACME.replacen("start = 0x09000000\nsize = 0x00000004", "start = 0x09000ffc\nsize = 4", 1);
    assert!(parse(&nested).is_ok(), "a hole at the end of a block nests");
    let disjoint = ACME.replacen("start = 0x09000000\nsize = 0x00000004", "start = 0x09001000\nsize = 4", 1);
    assert!(parse(&disjoint).is_ok(), "adjacent regions are disjoint");
}

#[test]
fn target_rules() {
    let cases = [
        ("name = \"acme_evb\"\n", "", "3:1: target.name is required"),
        ("cpu = \"CortexA53\"", "cpu = \"\"", "5:7: target.cpu must be non-empty text on one line"),
        (
            "word = 8",
            "word = 8\nclasses = [\"D\", \"TOOLONGCL\"]",
            "8:17: target.classes: \"TOOLONGCL\" is not 1 to 8 letters or digits",
        ),
        (
            "word = 8",
            "word = 8\ndefault_class = \"E\"",
            "8:17: target.default_class \"E\" is not one of target.classes",
        ),
        ("target = { cores = 1 }", "target = { cores = -1 }", "31:20: target.cores = -1 is outside 1..=4294967295"),
        (
            "target = { cores = 1 }",
            "target = { default_class = \"P\", classes = [\"A\"] }",
            "31:28: target.default_class \"P\" is not one of target.classes",
        ),
    ];
    for (from, to, want) in cases {
        let e = error_of(from, to);
        assert_eq!((e.message.strip_prefix("p/profile.toml:"), e.details.len()), (Some(want), 0), "{to}");
    }
    let e = parse("schema = 2\n").unwrap_err();
    assert_eq!(e.message, "p/profile.toml:1:1: [target] is required");
    assert_eq!(e.details, ["p/profile.toml:1:10: schema must be 1"]);
    let lower = ACME.replacen("word = 8", "word = 8\nclasses = [\"a\", \"d\"]\ndefault_class = \"d\"", 1);
    let (_, t, _) = parse(&lower).unwrap().select(None);
    assert_eq!((t.classes, t.default_class), (vec![class("A"), class("D")], class("D")));
}

#[test]
fn planned_keys_say_they_are_planned_and_unknown_keys_stop_at_the_first() {
    let text = ACME.replacen("word = 8", "word = 8\nelf = \"zephyr/zephyr.elf\"", 1)
        + "\n[timing]\ngo_wait = \"30s\"\n\n[stop]\nfatal = []\n\n[per.clocks]\nsection = \"Clock Controller\"\n";
    let e = parse(&text).unwrap_err();
    assert_eq!(e.message, "p/profile.toml:8:7: target.elf is planned, not in this t32ctl");
    assert_eq!(
        e.details,
        [
            "p/profile.toml:38:1: [timing] is planned, not in this t32ctl",
            "p/profile.toml:41:1: [stop] is planned, not in this t32ctl",
            "p/profile.toml:44:2: [per.*] is planned, not in this t32ctl",
        ]
    );
    let e = parse(&(ACME.to_string() + "\n[proc.selftest]\nabout = \"x\"\n")).unwrap_err();
    assert!(e.message.starts_with("p/profile.toml:37:2: unknown field `proc`, expected one of"), "{}", e.message);
    let e = error_of("target = { cores = 1 }", "target = { cores = 1 }\ntiming = { go_wait = \"1s\" }");
    assert_eq!(e.message, "p/profile.toml:32:10: [timing] is planned, not in this t32ctl");
    let e = error_of("cores = 4", "cores = 4\nlease = \"x\"");
    assert!(e.message.starts_with("p/profile.toml:7:1: unknown field `lease`"), "{}", e.message);
}

/// A fresh directory under the system temp dir, removed when dropped.
struct Temp(PathBuf);

impl Temp {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("t32ctl-profile-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("ws/b/support/t32ctl")).unwrap();
        Self(dir)
    }

    fn profile_dir(&self) -> PathBuf {
        self.0.join("ws/b/support/t32ctl")
    }

    fn write(&self, rel: &str, text: &str) {
        let p = self.profile_dir().join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, text).unwrap();
    }

    fn env(&self) -> Env {
        Env { cwd: self.0.join("ws"), home: Some(self.0.join("home")), ..Env::default() }
    }
}

impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn the_hash_covers_paths_and_contents_and_skips_hidden_files_and_editor_leftovers() {
    let t = Temp::new("hash");
    t.write("profile.toml", "schema = 1\n");
    t.write("sub/a.cmm", "x\n");
    let files = walk(&t.profile_dir()).unwrap();
    let rels: Vec<&str> = files.iter().map(|f| f.rel.as_str()).collect();
    assert_eq!(rels, ["profile.toml", "sub/a.cmm"]);
    let h = hash(&files);
    assert_eq!(h.to_string(), "fc130c2d65db7b882eaed5626a8c67cbc8ab0198ba2231e6180d69aa0bcb6697");
    assert_eq!(h.short(), "fc130c2d");
    for leftover in [".hidden", ".git/config", "sub/a.cmm~", "#a.cmm#", "sub/.a.cmm.swp", "a.cmm.swp"] {
        t.write(leftover, "ignored");
    }
    assert_eq!(hash(&walk(&t.profile_dir()).unwrap()), h);
    t.write("sub/a.cmm", "y\n");
    assert_ne!(hash(&walk(&t.profile_dir()).unwrap()), h);
}

#[test]
fn a_symlink_or_a_name_practice_cannot_carry_is_refused_by_name() {
    let t = Temp::new("symlink");
    t.write("profile.toml", ACME);
    std::os::unix::fs::symlink("profile.toml", t.profile_dir().join("link.cmm")).unwrap();
    let e = Profile::load(&t.env(), &t.profile_dir(), None).unwrap_err();
    assert_eq!(e.kind, ErrorKind::ProfileInvalid);
    assert_eq!(e.message, "b/support/t32ctl/link.cmm: a symlink; a profile holds only regular files and directories");
    fs::remove_file(t.profile_dir().join("link.cmm")).unwrap();
    t.write("a&b.cmm", "x");
    let e = Profile::load(&t.env(), &t.profile_dir(), None).unwrap_err();
    assert_eq!(e.message, "b/support/t32ctl/a&b.cmm: path \"a&b.cmm\" contains '&', which PRACTICE text cannot carry");
}

#[test]
fn a_loaded_profile_has_its_variant_files_and_hash_and_reports_errors_relative_to_the_cwd() {
    let t = Temp::new("load");
    t.write("profile.toml", ACME);
    t.write("attach.cmm", "ENDDO\n");
    let p = Profile::load(&t.env(), &t.profile_dir(), Some("acme_evb/a53/up")).unwrap();
    assert_eq!((p.variant.as_deref(), p.target.cores, p.files.len()), (Some("acme_evb/a53/up"), 1, 2));
    assert_eq!(p.hash, hash(&walk(&t.profile_dir()).unwrap()));
    assert_eq!(p.dir, t.profile_dir());
    t.write("profile.toml", "schema = 2\n[target]\n");
    let e = Profile::load(&t.env(), &t.profile_dir(), None).unwrap_err();
    assert_eq!(e.message, "b/support/t32ctl/profile.toml:1:10: schema must be 1");
}

#[test]
fn without_a_profile_directory_or_its_profile_toml_there_is_no_profile() {
    let t = Temp::new("none");
    let env = t.env();
    let tried = vec!["T32CTL_PROFILE: unset".to_string(), ".t32ctl.toml: not in a git repository".to_string()];
    let unbound = Binding { build: None, profile: None, board: None, tried: tried.clone() };
    let e = Profile::from_binding(&env, &unbound).unwrap_err();
    assert_eq!((e.kind, e.message.as_str()), (ErrorKind::NoProfile, "no target profile for this build"));
    assert_eq!(e.details, tried);
    let bound =
        Binding { profile: Some(Sourced { value: t.profile_dir(), source: Source::Var("T32CTL_PROFILE") }), ..unbound };
    let e = Profile::from_binding(&env, &bound).unwrap_err();
    assert_eq!(
        (e.kind, e.message.as_str()),
        (ErrorKind::NoProfile, "no profile.toml in b/support/t32ctl/ (from T32CTL_PROFILE)")
    );
    t.write("profile.toml", ACME);
    assert_eq!(Profile::from_binding(&env, &bound).unwrap().target.name, "acme_evb");
}
