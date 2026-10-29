// SPDX-License-Identifier: Apache-2.0
use clap::Command;

use super::*;
use crate::describe;

/// A POSIX shell command line split into words: blanks separate, single quotes group, and a
/// backslash outside quotes takes the next character literally.
pub(crate) fn words(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut word: Option<String> = None;
    let mut quoted = false;
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        match c {
            '\'' => {
                quoted = !quoted;
                word.get_or_insert_with(String::new);
            }
            '\\' if !quoted => word.get_or_insert_with(String::new).extend(chars.next()),
            ' ' if !quoted => out.extend(word.take()),
            c => word.get_or_insert_with(String::new).push(c),
        }
    }
    assert!(!quoted, "unbalanced quote in {line:?}");
    out.extend(word);
    out
}

pub(crate) fn parse_line(line: &str) -> Result<Cli, clap::Error> {
    command().try_get_matches_from(words(line)).and_then(|m| Cli::from_arg_matches(&m))
}

#[test]
fn every_example_parses_to_its_own_leaf() {
    for spec in SPECS {
        assert!(!spec.examples.is_empty(), "{:?} has no example", spec.path);
        for example in spec.examples {
            let cli = parse_line(example).unwrap_or_else(|e| panic!("{example}: {e}"));
            assert_eq!(cli.spec(), *spec, "{example}");
        }
    }
}

#[test]
fn every_clap_leaf_has_a_spec_under_its_grade_and_every_spec_a_leaf() {
    let clap: Vec<Vec<String>> = describe::leaves().into_iter().map(|l| l.path).collect();
    let specs: Vec<Vec<String>> = SPECS.iter().map(|s| s.path.iter().map(|w| w.to_string()).collect()).collect();
    assert_eq!(clap, specs);
    let mut all = Vec::new();
    fn walk(cmd: &Command, all: &mut Vec<String>) {
        for sub in cmd.get_subcommands() {
            if sub.has_subcommands() {
                walk(sub, all);
            } else {
                all.push(sub.get_name().to_string());
            }
        }
    }
    walk(&Cli::command(), &mut all);
    assert_eq!(all.len(), SPECS.len(), "a leaf without a spec: {all:?}");
    for spec in SPECS {
        assert_eq!(spec.grade.name(), spec.path[0], "{:?}", spec.path);
    }
}

#[test]
fn flags_parse_only_after_the_grade_word() {
    for bad in ["t32ctl --json read status", "t32ctl --build b read status", "t32ctl --full read status"] {
        assert!(parse_line(bad).is_err(), "{bad}");
    }
    let GradeCmd::Read(g) = parse_line("t32ctl read --json status --build b").unwrap().grade else { panic!() };
    assert!(g.globals.json);
    assert_eq!(g.globals.build.as_deref(), Some(std::path::Path::new("b")));
    let GradeCmd::Read(g) = parse_line("t32ctl read status --full").unwrap().grade else { panic!() };
    assert!(matches!(g.leaf, ReadLeaf::Status(ReadStatus { full: true })));
    assert!(!g.globals.json);
    let GradeCmd::Raw(g) = parse_line("t32ctl raw eval 'OS.PTD()' --json").unwrap().grade else { panic!() };
    assert!(g.globals.json);
    assert!(matches!(g.leaf, RawLeaf::Eval(RawEval { ref text }) if text.as_str() == "OS.PTD()"));
    assert!(parse_line("t32ctl --json raw eval 'OS.PTD()'").is_err());
}

#[test]
fn there_are_no_aliases_abbreviations_or_help_subcommands() {
    for bad in [
        "t32ctl r status",
        "t32ctl read stat",
        "t32ctl read status --ful",
        "t32ctl read describe --format perm",
        "t32ctl help",
        "t32ctl read help",
        "t32ctl read help status",
        "t32ctl",
        "t32ctl read",
    ] {
        assert!(parse_line(bad).is_err(), "{bad}");
    }
}

#[test]
fn no_flag_or_alias_is_named_like_a_bypass() {
    fn bypass(name: &str) -> bool {
        ["force", "yes", "steal", "unsafe", "override"].contains(&name)
            || ["ignore", "no-", "skip"].iter().any(|p| name.starts_with(p))
            || name.contains("verify")
    }
    fn walk(cmd: &Command, seen: &mut Vec<String>) {
        for a in cmd.get_arguments() {
            seen.extend(a.get_long().map(String::from));
            seen.extend(a.get_all_aliases().unwrap_or_default().into_iter().map(String::from));
        }
        for sub in cmd.get_subcommands() {
            seen.extend(sub.get_all_aliases().map(String::from));
            walk(sub, seen);
        }
    }
    let mut seen = Vec::new();
    walk(&command(), &mut seen);
    assert!(seen.contains(&"full".to_string()), "{seen:?}");
    let bad: Vec<&String> = seen.iter().filter(|n| bypass(n)).collect();
    assert!(bad.is_empty(), "{bad:?}");
    assert!(bypass("no-verify") && bypass("skip-checks") && bypass("force"));
}

#[test]
fn help_and_version_print_and_everything_else_is_a_usage_error() {
    let argv = |line: &str| words(line).into_iter().map(OsString::from).collect::<Vec<_>>();
    match parse(&argv("t32ctl read status --help")) {
        Parsed::Print(text) => assert!(text.contains("Examples:\n  t32ctl read status\n"), "{text}"),
        other => panic!("{other:?}"),
    }
    assert!(matches!(parse(&argv("t32ctl --version")), Parsed::Print(t) if t.starts_with("t32ctl ")));
    let Parsed::Usage(e) = parse(&argv("t32ctl r status")) else { panic!("parses") };
    assert_eq!(e.kind, ErrorKind::Usage);
    assert_eq!(e.message, "unrecognized subcommand 'r'");
    assert_eq!(
        e.details,
        [
            "tip: some similar subcommands exist: 'write', 'read', 'run', 'raw'",
            "Usage: t32ctl <COMMAND>",
            "For more information, try '--help'."
        ]
    );
}

#[test]
fn raw_text_is_one_line_of_at_most_2037_bytes_or_a_notation_error() {
    let argv = |text: &str| vec![OsString::from("t32ctl"), "raw".into(), "cmd".into(), text.into()];
    assert!(matches!(parse(&argv(&"A".repeat(2037))), Parsed::Cli(_)));
    let Parsed::Usage(e) = parse(&argv(&"A".repeat(2038))) else { panic!("parses") };
    assert_eq!(
        (e.kind, e.message.as_str()),
        (ErrorKind::Notation, "PRACTICE text is 2038 bytes; it must be 1 to 2037 bytes")
    );
    let Parsed::Usage(e) = parse(&argv("PRINT 1\nPRINT 2")) else { panic!("parses") };
    assert_eq!(e.kind, ErrorKind::Notation);
}

#[derive(Debug, Parser)]
#[command(name = "probe")]
struct Typed {
    #[arg(long)]
    len: crate::notation::Size,
}

#[test]
fn a_value_a_notation_type_refuses_is_a_notation_error() {
    let e = Typed::try_parse_from(["probe", "--len", "4k"]).unwrap_err();
    let ctl = usage_error(&e);
    assert_eq!(ctl.kind, ErrorKind::Notation);
    assert_eq!(ctl.message, "size \"4k\": use bytes (24), 0x hex (0x40), or a K, M, G suffix (4K, 64M, 1G)");
    let e = Typed::try_parse_from(["probe", "--len"]).unwrap_err();
    assert_eq!(usage_error(&e).kind, ErrorKind::Usage);
}

#[test]
fn usage_lists_positionals_then_options_then_trailing_arguments() {
    let cmd = Command::new("x")
        .arg(clap::Arg::new("loc").required(true).value_name("LOC"))
        .arg(clap::Arg::new("names").num_args(1..).action(clap::ArgAction::Append).value_name("NAME"))
        .arg(clap::Arg::new("len").long("len").value_name("SIZE"))
        .arg(clap::Arg::new("width").long("width").value_parser(["1", "2", "4", "8"]))
        .arg(clap::Arg::new("root").long("root").required(true).value_name("DIR"))
        .arg(clap::Arg::new("all").long("all").action(clap::ArgAction::SetTrue))
        .arg(clap::Arg::new("args").last(true).num_args(0..).value_name("ARG"));
    let path = ["read".to_string(), "x".to_string()];
    assert_eq!(
        describe::usage(&path, &cmd, None, "\\|"),
        "t32ctl read x LOC [NAME...] [--len SIZE] [--width 1\\|2\\|4\\|8] --root DIR [--all] [-- ARG...]"
    );
    assert_eq!(
        describe::usage(&path, &cmd, Some("NAME [--PARAM VALUE...]"), "|"),
        "t32ctl read x [--len SIZE] [--width 1|2|4|8] --root DIR [--all] NAME [--PARAM VALUE...]",
        "a tail replaces the positionals"
    );
}

#[test]
fn a_hint_parses_back_to_the_arguments_it_was_built_from() {
    let hint = crate::report::Hint(["read", "status", "--full", "--build", "my build's"].map(String::from).to_vec());
    let cli = parse_line(&hint.render()).unwrap();
    assert_eq!(cli.spec().path, ["read", "status"]);
    let GradeCmd::Read(g) = cli.grade else { panic!() };
    assert_eq!(g.globals.build.as_deref(), Some(std::path::Path::new("my build's")));
    assert!(matches!(g.leaf, ReadLeaf::Status(ReadStatus { full: true })));
}

#[test]
fn every_token_after_a_procedure_name_reaches_the_second_parser() {
    let cases = [
        ("t32ctl burn proc flash --part user", false, vec!["--part", "user"]),
        (
            "t32ctl burn proc flash --json --part=user --build b --help",
            true,
            vec!["--part=user", "--build", "b", "--help"],
        ),
        ("t32ctl burn --json proc flash --part user", true, vec!["--part", "user"]),
        ("t32ctl read proc selftest x --y", false, vec!["x", "--y"]),
        ("t32ctl run proc attach", false, vec![]),
    ];
    for (line, json, want) in cases {
        let cli = parse_line(line).unwrap_or_else(|e| panic!("{line}: {e}"));
        let (globals, args) = match cli.grade {
            GradeCmd::Burn(g) => {
                let BurnLeaf::Proc(a) = g.leaf;
                (g.globals, a)
            }
            GradeCmd::Read(g) => match g.leaf {
                ReadLeaf::Proc(a) => (g.globals, a),
                other => panic!("{other:?}"),
            },
            GradeCmd::Run(g) => {
                let RunLeaf::Proc(a) = g.leaf;
                (g.globals, a)
            }
            other => panic!("{other:?}"),
        };
        assert_eq!((globals.json, args.tokens.iter().map(String::as_str).collect::<Vec<_>>()), (json, want), "{line}");
    }
    assert!(parse_line("t32ctl raw proc x").is_err(), "raw has no proc leaf");
    assert!(parse_line("t32ctl burn proc").is_err(), "a procedure needs a name");
}

#[test]
fn help_after_a_procedure_name_is_a_token_and_without_a_name_clap_help() {
    let argv = |line: &str| words(line).into_iter().map(OsString::from).collect::<Vec<_>>();
    let Parsed::Cli(cli) = parse(&argv("t32ctl burn proc flash --help")) else { panic!("not a command") };
    let GradeCmd::Burn(g) = cli.grade else { panic!("not burn") };
    let BurnLeaf::Proc(a) = g.leaf;
    assert_eq!((a.name.as_str(), a.tokens.as_slice()), ("flash", &["--help".to_string()][..]));
    assert!(matches!(parse(&argv("t32ctl burn proc --help")), Parsed::Print(t) if t.contains("Usage:")));
    assert!(matches!(parse(&argv("t32ctl read status --help")), Parsed::Print(t) if t.contains("Examples:")));
}

#[test]
fn raw_text_help_says_to_quote_the_whole_text() {
    let argv = |line: &str| words(line).into_iter().map(OsString::from).collect::<Vec<_>>();
    for line in ["t32ctl raw cmd --help", "t32ctl raw eval --help"] {
        let Parsed::Print(text) = parse(&argv(line)) else { panic!("{line}: no help") };
        assert!(text.contains("Quote it whole in single quotes"), "{line}:\n{text}");
    }
}
