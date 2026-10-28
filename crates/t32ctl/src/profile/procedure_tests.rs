// SPDX-License-Identifier: Apache-2.0
use std::path::Path;

use super::*;
use crate::{
    error::{CtlError, ErrorKind},
    profile::ProfileDoc,
};

const FILE: &str = "p/profile.toml";
const FILES: [&str; 5] = ["profile.toml", "attach.cmm", "selftest.cmm", "flash_emmc.cmm", "peek.cmm"];

/// The contract's procedures without `lease`, and one with the remaining parameter types.
const PROCS: &str = r#"schema = 1

[target]
name = "acme_evb"
cpu = "CortexA53"
cores = 4
word = 8

[proc.attach]
about = "Connect to the A53 cluster without resetting it"
script = "attach.cmm"
grade = "run"
timeout = "30s"
effects = ["debugger-config"]

[proc.selftest]
about = "Read clock and PMIC status into selftest.txt"
script = "selftest.cmm"
grade = "read"
timeout = "30s"
effects = []
outputs = ["selftest.txt"]

[proc.flash]
about = "Write an image into eMMC and verify it by readback"
script = "flash_emmc.cmm"
grade = "burn"
timeout = "45m"
effects = ["flash", "target-reset", "target-memory", "target-registers"]
outputs = ["verify.log"]

[proc.flash.params.image]
type = "file"
about = "image to write"
default = "{build}/zephyr/zephyr.bin"

[proc.flash.params.part]
type = "enum"
values = ["boot0", "boot1", "user"]
about = "eMMC hardware partition"
default = "user"

[proc.flash.params.offset]
type = "integer"
about = "byte offset inside the partition"
min = 0
default = 0

[proc.peek]
about = "Dump a block"
script = "peek.cmm"
grade = "read"
timeout = "1m"
effects = []

[proc.peek.params.at]
type = "address"
about = "where"

[proc.peek.params.label]
type = "string"
about = "a title"
default = "dump"

[proc.peek.params.long_form]
type = "bool"
about = "every line"
default = false
"#;

fn parse(text: &str) -> Result<ProfileDoc, CtlError> {
    ProfileDoc::parse(text, Path::new(FILE), &FILES)
}

fn error_of(from: &str, to: &str) -> CtlError {
    assert!(PROCS.contains(from), "{from}");
    parse(&PROCS.replacen(from, to, 1)).unwrap_err()
}

#[test]
fn procedures_keep_their_declaration_order_and_parameters() {
    let doc = parse(PROCS).unwrap();
    let names: Vec<&str> = doc.procs.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, ["attach", "selftest", "flash", "peek"]);
    let flash = &doc.procs[2];
    assert_eq!(
        (flash.grade, flash.timeout.to_string(), flash.script.as_str()),
        (GradeId::Burn, "45m".into(), "flash_emmc.cmm")
    );
    assert_eq!(flash.effects, [Effect::Flash, Effect::TargetReset, Effect::TargetMemory, Effect::TargetRegisters]);
    assert_eq!(flash.outputs.iter().map(OutName::as_str).collect::<Vec<_>>(), ["verify.log"]);
    let params: Vec<(&str, &str, Option<&str>, bool)> =
        flash.params.iter().map(|p| (p.name.as_str(), p.ty.name(), p.default.as_deref(), p.required())).collect();
    assert_eq!(
        params,
        [
            ("image", "file", Some("{build}/zephyr/zephyr.bin"), false),
            ("part", "enum", Some("user"), false),
            ("offset", "integer", Some("0"), false),
        ]
    );
    assert_eq!(flash.params[2].ty, ParamType::Integer { min: Some(0), max: None });
    assert!(doc.procs[0].declares(Effect::DebuggerConfig) && !flash.declares(Effect::DebuggerConfig));
    let peek = &doc.procs[3];
    let flags: Vec<String> = peek.params.iter().map(ParamDecl::flag).collect();
    assert_eq!(flags, ["--at", "--label", "--long-form"]);
    assert!(peek.params[0].required() && !peek.params[2].required());
    assert_eq!(peek.param_by_flag("--long-form").map(|p| p.name.as_str()), Some("long_form"));
    assert_eq!(peek.params[2].default.as_deref(), Some("false"));
}

#[test]
fn every_procedure_rule_is_an_error_at_its_line_and_column() {
    let cases = [
        ("[proc.attach]", "[proc.Attach]", "9:1: proc.Attach: a procedure name is [a-z][a-z0-9_]{0,23}"),
        ("about = \"Connect to the A53 cluster without resetting it\"\n", "", "9:1: proc.attach.about is required"),
        (
            "script = \"attach.cmm\"",
            "script = \"missing.cmm\"",
            "11:10: proc.attach.script missing.cmm is not a file of the profile directory",
        ),
        (
            "script = \"attach.cmm\"",
            "script = \"../attach.cmm\"",
            "11:10: proc.attach.script: path \"../attach.cmm\": component \"..\" is `.`/`..`, empty, or ends in a dot or space",
        ),
        (
            "grade = \"run\"",
            "grade = \"raw\"",
            "12:9: proc.attach.grade: raw cannot be declared: raw is PRACTICE outside every policy",
        ),
        (
            "grade = \"run\"",
            "grade = \"read\"",
            "12:9: proc.attach.grade read is below run, the floor of the effect debugger-config",
        ),
        (
            "grade = \"burn\"",
            "grade = \"sys\"",
            "27:9: proc.flash.grade sys is below burn, the floor of the effect flash",
        ),
        ("timeout = \"30s\"", "timeout = \"3h\"", "13:11: proc.attach.timeout: duration 3h is outside 1s..2h"),
        (
            "effects = [\"debugger-config\"]",
            "effects = [\"reboot\"]",
            "14:12: proc.attach.effects: \"reboot\" is not one of debugger-config, target-run, target-reset, target-memory, target-registers, flash",
        ),
        (
            "outputs = [\"selftest.txt\"]",
            "outputs = [\"selftest.txt\", \"SELFTEST.TXT\"]",
            "22:28: proc.selftest.outputs: selftest.txt and SELFTEST.TXT differ only in case",
        ),
        (
            "outputs = [\"selftest.txt\"]",
            "outputs = [\"sub/x.txt\"]",
            "22:12: proc.selftest.outputs: output name \"sub/x.txt\": a plain file name without / that does not start with .",
        ),
        (
            "[proc.flash.params.part]",
            "[proc.flash.params.json]",
            "37:1: proc.flash.params.json: json is reserved for t32ctl's own flags",
        ),
        (
            "[proc.flash.params.part]",
            "[proc.flash.params.no_wait]",
            "37:1: proc.flash.params.no_wait: the name matches the bypass pattern (force, yes, steal, unsafe, override, ignore*, no_*, skip*, a bool with verify); a check the procedure performs belongs in its script",
        ),
        (
            "[proc.peek.params.long_form]",
            "[proc.peek.params.skip_verify]",
            "65:1: proc.peek.params.skip_verify: the name matches the bypass pattern (force, yes, steal, unsafe, override, ignore*, no_*, skip*, a bool with verify); a check the procedure performs belongs in its script",
        ),
        (
            "[proc.peek.params.long_form]",
            "[proc.peek.params.verify_all]",
            "65:1: proc.peek.params.verify_all: the name matches the bypass pattern (force, yes, steal, unsafe, override, ignore*, no_*, skip*, a bool with verify); a check the procedure performs belongs in its script",
        ),
        (
            "[proc.peek.params.label]\ntype = \"string\"",
            "[proc.peek.params.label]\ntype = \"file\"",
            "61:8: proc.peek.params.label: a file parameter belongs to a write, sys or burn procedure: it moves a local file into PowerView",
        ),
        (
            "default = \"user\"",
            "default = \"emmc\"",
            "41:11: proc.flash.params.part: default emmc: not one of boot0, boot1, user",
        ),
        (
            "default = 0\n",
            "default = \"0\"\n",
            "47:11: proc.flash.params.offset: default is a string; the parameter's type integer takes an integer",
        ),
        ("min = 0\n", "min = 1\n", "47:11: proc.flash.params.offset: default 0: 0 is below the minimum 1"),
        ("min = 0\ndefault = 0\n", "min = 5\nmax = 4\n", "47:7: proc.flash.params.offset: min 5 is above max 4"),
        (
            "default = \"dump\"",
            "default = \"{build}\"",
            "63:11: proc.peek.params.label: default: {build} belongs only in a file default",
        ),
        (
            "default = \"dump\"",
            "default = \"a&b\"",
            "63:11: proc.peek.params.label: default 'a&b': contains & before a letter, which PRACTICE expands as a macro",
        ),
        (
            "type = \"address\"",
            "type = \"address\"\ndefault = \"C15:0x0\"",
            "58:11: proc.peek.params.at: default C15:0x0: access class C15 is not one of the profile's classes A, D, P",
        ),
        (
            "type = \"address\"",
            "type = \"pointer\"",
            "57:8: proc.peek.params.at.type \"pointer\" is not string, integer, address, enum, bool or file",
        ),
        (
            "values = [\"boot0\", \"boot1\", \"user\"]",
            "values = [\"boot0\", \"boot0\", \"user\"]",
            "39:20: proc.flash.params.part: value \"boot0\" is listed twice",
        ),
        (
            "values = [\"boot0\", \"boot1\", \"user\"]\nabout = \"eMMC hardware partition\"\ndefault = \"user\"\n",
            "about = \"eMMC hardware partition\"\n",
            "37:1: proc.flash.params.part: an enum parameter needs values",
        ),
        (
            "about = \"where\"",
            "about = \"where\"\nvalues = [\"x\"]",
            "59:11: proc.peek.params.at: values applies only to enum parameters",
        ),
        (
            "about = \"a title\"",
            "about = \"a title\"\nmin = 1",
            "63:7: proc.peek.params.label: min and max apply only to integer parameters",
        ),
    ];
    for (from, to, want) in cases {
        let e = error_of(from, to);
        assert_eq!(e.kind, ErrorKind::ProfileInvalid);
        assert_eq!((e.message.strip_prefix("p/profile.toml:"), e.details.as_slice()), (Some(want), &[][..]), "{to}");
    }
    let e = error_of("effects = [\"debugger-config\"]", "effects = [\"debugger-config\"]\nlease = \"required\"");
    assert_eq!(e.message, "p/profile.toml:15:9: proc.attach.lease is planned, not in this t32ctl");
    let e = error_of("timeout = \"30s\"", "timout = \"30s\"");
    assert!(e.message.starts_with("p/profile.toml:13:1: unknown field `timout`, expected one of"), "{}", e.message);
}

fn target() -> Target {
    ProfileDoc::parse(PROCS, Path::new(FILE), &FILES).unwrap().target
}

fn decl(ty: ParamType) -> ParamDecl {
    ParamDecl { name: "x_y".into(), about: "a".into(), ty, default: None }
}

#[test]
fn values_parse_per_type_into_practice_form_and_show_in_the_callers_notation() {
    let t = target();
    let cx = ParamCx { target: &t, cwd: Path::new("/ws"), build: Some(Path::new("/ws/build")) };
    let env = Env { cwd: "/ws".into(), ..Env::default() };
    let int = decl(ParamType::Integer { min: Some(-1), max: Some(0x40) });
    let cases: [(&ParamDecl, &str, &str, &str); 9] = [
        (&decl(ParamType::String), "chip a", "chip a", "chip a"),
        (&int, "0x2a", "42.", "42"),
        (&int, "-1", "-1.", "-1"),
        (&decl(ParamType::Address), "0x40100010", "D:0x40100010", "D:0x40100010"),
        (&decl(ParamType::Address), "A:0x9000000", "A:0x9000000", "A:0x9000000"),
        (&decl(ParamType::Enum(vec!["boot0".into(), "user".into()])), "user", "user", "user"),
        (&decl(ParamType::Bool), "true", "TRUE()", "true"),
        (&decl(ParamType::Bool), "false", "FALSE()", "false"),
        (&decl(ParamType::String), "1&2 & 3", "1&2 & 3", "1&2 & 3"),
    ];
    for (d, text, practice, shown) in cases {
        let v = d.parse(text, &cx).unwrap_or_else(|e| panic!("{text}: {e}"));
        assert_eq!((v.macro_text().as_deref(), v.shown(&env).as_str()), (Some(practice), shown), "{text}");
    }
    let file = decl(ParamType::File).parse("img/a.bin", &cx).unwrap();
    assert_eq!((file.macro_text(), file.shown(&env)), (None, "img/a.bin".to_string()));
    assert_eq!(file, ParamValue::File("/ws/img/a.bin".into()));
}

#[test]
fn a_value_outside_its_type_names_the_flag_and_the_rule() {
    let t = target();
    let cx = ParamCx { target: &t, cwd: Path::new("/ws"), build: None };
    let int = decl(ParamType::Integer { min: Some(0), max: Some(16) });
    let cases: [(&ParamDecl, &str, &str); 9] = [
        (&int, "17", "--x-y 17: 17 is above the maximum 16"),
        (&int, "-1", "--x-y -1: -1 is below the minimum 0"),
        (&int, "4K", "--x-y 4K: integer \"4K\": use 42, 0x2a or -1, within 64 bits"),
        (&decl(ParamType::String), "say \"hi\"", "--x-y 'say \"hi\"': contains \""),
        (&decl(ParamType::String), &"a".repeat(257), &format!("--x-y {}: 257 bytes; at most 256", "a".repeat(257))),
        (&decl(ParamType::Address), "@sp", "--x-y @sp: an address parameter is [CLASS:]0x<hex>, not a register place"),
        (
            &decl(ParamType::Address),
            "main",
            "--x-y main: place \"main\": symbol places are planned, not in this t32ctl; use [CLASS:]0x<hex> or @<reg>[+0x<hex>|-0x<hex>]",
        ),
        (&decl(ParamType::Enum(vec!["a".into(), "b".into()])), "c", "--x-y c: not one of a, b"),
        (&decl(ParamType::Bool), "yes", "--x-y yes: use true or false"),
    ];
    for (d, text, want) in cases {
        assert_eq!(d.parse(text, &cx).unwrap_err(), want, "{text}");
    }
}

#[test]
fn a_file_default_names_the_build_directory_and_needs_one() {
    let t = target();
    let image = ParamDecl {
        name: "image".into(),
        about: "a".into(),
        ty: ParamType::File,
        default: Some("{build}/zephyr/zephyr.bin".into()),
    };
    let cx = ParamCx { target: &t, cwd: Path::new("/ws"), build: Some(Path::new("/ws/build")) };
    assert_eq!(image.default_value(&cx), Some(Ok(ParamValue::File("/ws/build/zephyr/zephyr.bin".into()))));
    let cx = ParamCx { build: None, ..cx };
    assert_eq!(
        image.default_value(&cx),
        Some(Err(
            "--image: the default {build}/zephyr/zephyr.bin needs a build directory; pass --image FILE or --build DIR"
                .into()
        ))
    );
    let offset = ParamDecl { default: Some("0".into()), ..decl(ParamType::Integer { min: None, max: None }) };
    assert_eq!(offset.default_value(&cx), Some(Ok(ParamValue::Int(0))));
    assert_eq!(decl(ParamType::Bool).default_value(&cx), None);
}
