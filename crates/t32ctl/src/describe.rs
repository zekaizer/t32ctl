// SPDX-License-Identifier: Apache-2.0
//! `read describe`: the static schema, the generated Markdown blocks and the permission snippet,
//! all walked from the clap definition, the leaf specs, `GradeId`, `Exit` and `ErrorKind`.
//! Planned commands are absent: these list only what the binary does.

use clap::{Arg, ArgAction, Command, CommandFactory};
use serde::Serialize;

use crate::{
    cli::{Cli, DescribeFormat, LeafSpec, SPECS},
    error::{ErrorKind, Exit},
    grade::{GradeId, Rule},
    notation::{FORMS, Form},
};

pub const SCHEMA: &str = "t32ctl.describe/1";

/// Paths the snippet keeps agents from editing: profiles and their scripts, the binding file,
/// the connection config and t32ctl's state.
const PROTECTED: [&str; 4] =
    ["**/support/t32ctl/**", "**/.t32ctl.toml", "~/.config/t32ctl/**", "~/.local/state/t32ctl/**"];

pub fn render(format: DescribeFormat) -> String {
    match format {
        DescribeFormat::Json => json(),
        DescribeFormat::Skill => skill(),
        DescribeFormat::Permissions => permissions(),
    }
}

/// A leaf of the clap tree with its spec.
#[derive(Debug)]
pub struct LeafDoc {
    pub path: Vec<String>,
    pub spec: &'static LeafSpec,
    pub cmd: Command,
}

/// Every leaf of the clap tree in definition order, each with the spec of the same path.
pub fn leaves() -> Vec<LeafDoc> {
    fn walk(cmd: &Command, prefix: &[String], out: &mut Vec<LeafDoc>) {
        for sub in cmd.get_subcommands() {
            let mut path = prefix.to_vec();
            path.push(sub.get_name().to_string());
            if sub.has_subcommands() {
                walk(sub, &path, out);
            } else if let Some(spec) = SPECS.iter().find(|s| s.path == path) {
                out.push(LeafDoc { path, spec, cmd: sub.clone() });
            }
        }
    }
    let mut out = Vec::new();
    walk(&Cli::command(), &[], &mut out);
    out
}

/// `t32ctl` and the path, positionals in index order, options in definition order, `last`
/// positionals, then the spec's tail. `sep` joins possible values. A tail replaces the positionals:
/// `proc` captures its name and raw tokens, which only the tail describes.
pub fn usage(path: &[String], cmd: &Command, tail: Option<&str>, sep: &str) -> String {
    let mut parts = vec!["t32ctl".to_string()];
    parts.extend(path.iter().cloned());
    let positionals: Vec<&Arg> = if tail.is_some() { Vec::new() } else { cmd.get_positionals().collect() };
    parts.extend(positionals.iter().filter(|a| !a.is_last_set()).map(|a| positional(a)));
    parts.extend(cmd.get_arguments().filter(|a| !a.is_positional() && !a.is_global_set()).map(|a| option(a, sep)));
    parts.extend(positionals.iter().filter(|a| a.is_last_set()).map(|a| format!("[-- {}...]", value_name(a))));
    parts.extend(tail.map(String::from));
    parts.join(" ")
}

fn value_name(a: &Arg) -> String {
    a.get_value_names().and_then(|v| v.first()).map_or_else(|| a.get_id().as_str().to_uppercase(), |v| v.to_string())
}

fn multiple(a: &Arg) -> bool {
    matches!(a.get_action(), ArgAction::Append) || a.get_num_args().is_some_and(|n| n.max_values() > 1)
}

fn positional(a: &Arg) -> String {
    let name = if multiple(a) { format!("{}...", value_name(a)) } else { value_name(a) };
    if a.is_required_set() { name } else { format!("[{name}]") }
}

fn option(a: &Arg, sep: &str) -> String {
    let long = format!("--{}", a.get_long().unwrap_or_else(|| a.get_id().as_str()));
    let text = if !a.get_action().takes_values() {
        long
    } else {
        let values: Vec<String> = a.get_possible_values().iter().map(|v| v.get_name().to_string()).collect();
        let value = if values.is_empty() { value_name(a) } else { values.join(sep) };
        format!("{long} {value}")
    };
    if a.is_required_set() { text } else { format!("[{text}]") }
}

fn about(cmd: &Command) -> String {
    cmd.get_about().map(ToString::to_string).unwrap_or_default()
}

/// The flags every leaf accepts, from the first grade.
fn globals() -> Vec<(String, Option<String>, String)> {
    let root = Cli::command();
    let Some(grade) = root.get_subcommands().next() else { return Vec::new() };
    grade
        .get_arguments()
        .filter(|a| a.is_global_set())
        .map(|a| {
            let flag = format!("--{}", a.get_long().unwrap_or_default());
            let value = a.get_action().takes_values().then(|| value_name(a));
            (flag, value, a.get_help().map(ToString::to_string).unwrap_or_default())
        })
        .collect()
}

#[derive(Serialize)]
struct Describe {
    schema: &'static str,
    version: &'static str,
    grades: Vec<GradeDoc>,
    commands: Vec<CommandDoc>,
    global_flags: Vec<FlagDoc>,
    notation: &'static [Form],
    exits: Vec<ExitDoc>,
    errors: Vec<ErrorDoc>,
}

#[derive(Serialize)]
struct GradeDoc {
    name: &'static str,
    rule: &'static str,
    never_allow: bool,
    order: Option<u8>,
}

#[derive(Serialize)]
struct CommandDoc {
    path: String,
    usage: String,
    does: String,
    waits: &'static str,
    connects: bool,
    examples: &'static [&'static str],
    args: Vec<ArgDoc>,
}

#[derive(Serialize)]
struct ArgDoc {
    name: String,
    /// `--long` for an option; `None` for a positional.
    flag: Option<String>,
    value: Option<String>,
    values: Vec<String>,
    required: bool,
    multiple: bool,
    does: String,
}

#[derive(Serialize)]
struct FlagDoc {
    flag: String,
    value: Option<String>,
    does: String,
}

#[derive(Serialize)]
struct ExitDoc {
    code: u8,
    class: &'static str,
    next: &'static str,
}

#[derive(Serialize)]
struct ErrorDoc {
    tag: &'static str,
    exit: u8,
    meaning: &'static str,
}

fn arg_doc(a: &Arg) -> ArgDoc {
    let takes = a.get_action().takes_values();
    ArgDoc {
        name: if a.is_positional() { value_name(a) } else { a.get_id().as_str().to_string() },
        flag: a.get_long().map(|l| format!("--{l}")),
        value: (takes && !a.is_positional()).then(|| value_name(a)),
        values: if takes {
            a.get_possible_values().iter().map(|v| v.get_name().to_string()).collect()
        } else {
            Vec::new()
        },
        required: a.is_required_set(),
        multiple: multiple(a),
        does: a.get_help().map(ToString::to_string).unwrap_or_default(),
    }
}

/// `t32ctl.describe/1`, pretty-printed.
pub fn json() -> String {
    let doc = Describe {
        schema: SCHEMA,
        version: env!("CARGO_PKG_VERSION"),
        grades: GradeId::ALL
            .iter()
            .map(|g| GradeDoc { name: g.name(), rule: g.rule().name(), never_allow: g.never_allow(), order: g.order() })
            .collect(),
        commands: leaves()
            .iter()
            .map(|l| CommandDoc {
                path: l.path.join(" "),
                usage: usage(&l.path, &l.cmd, l.spec.usage_tail, "|"),
                does: about(&l.cmd),
                waits: l.spec.waits,
                connects: l.spec.connects,
                examples: l.spec.examples,
                args: l.cmd.get_arguments().filter(|a| !a.is_global_set()).map(arg_doc).collect(),
            })
            .collect(),
        global_flags: globals().into_iter().map(|(flag, value, does)| FlagDoc { flag, value, does }).collect(),
        notation: FORMS,
        exits: Exit::ALL.iter().map(|e| ExitDoc { code: e.code(), class: e.class(), next: e.next() }).collect(),
        errors: ErrorKind::ALL
            .iter()
            .map(|k| ErrorDoc { tag: k.tag(), exit: k.exit().code(), meaning: k.meaning() })
            .collect(),
    };
    let mut text = serde_json::to_string_pretty(&doc).expect("the schema serializes");
    text.push('\n');
    text
}

fn block(name: &str, body: &str) -> String {
    format!(
        "<!-- {name}:start (generated by `t32ctl read describe --format skill`; do not edit) -->\n{body}<!-- {name}:end -->\n"
    )
}

fn cell(text: &str) -> String {
    text.replace('|', "\\|")
}

/// The `commands`, `exits` and `errors` blocks, each between its markers.
pub fn skill() -> String {
    let mut commands = String::from("| Command | Does | Waits for |\n|---|---|---|\n");
    for l in leaves() {
        let usage = usage(&l.path, &l.cmd, l.spec.usage_tail, "\\|");
        commands.push_str(&format!("| `{usage}` | {} | {} |\n", cell(&about(&l.cmd)), cell(l.spec.waits)));
    }
    let flags: Vec<String> = globals()
        .into_iter()
        .map(|(flag, value, _)| match value {
            Some(v) => format!("`{flag} {v}`"),
            None => format!("`{flag}`"),
        })
        .collect();
    commands.push_str(&format!("\nFlags on every command: {}.\n", flags.join(", ")));
    let mut exits = String::from("| Code | Class | Do next |\n|---|---|---|\n");
    for e in Exit::ALL {
        exits.push_str(&format!("| {} | {} | {} |\n", e.code(), e.class(), cell(e.next())));
    }
    let mut errors = String::from("| Tag | Exit | Meaning |\n|---|---|---|\n");
    for k in ErrorKind::ALL {
        errors.push_str(&format!("| `{}` | {} | {} |\n", k.tag(), k.exit().code(), cell(k.meaning())));
    }
    [block("commands", &commands), block("exits", &exits), block("errors", &errors)].join("\n")
}

#[derive(Serialize)]
struct Snippet {
    permissions: Rules,
    never_allow: Vec<String>,
}

#[derive(Serialize)]
struct Rules {
    allow: Vec<String>,
    ask: Vec<String>,
    deny: Vec<String>,
}

fn rule(g: GradeId) -> String {
    format!("Bash(t32ctl {}:*)", g.name())
}

/// The Claude Code permission rules for `.claude/settings.json`, from the grade table.
pub fn permissions() -> String {
    let by = |r: Rule| GradeId::ALL.iter().filter(|g| g.rule() == r).map(|g| rule(*g)).collect::<Vec<_>>();
    let mut deny = by(Rule::Deny);
    deny.extend(PROTECTED.iter().flat_map(|p| [format!("Edit({p})"), format!("Write({p})")]));
    let snippet = Snippet {
        permissions: Rules { allow: by(Rule::Allow), ask: by(Rule::Ask), deny },
        never_allow: GradeId::ALL.iter().filter(|g| g.never_allow()).map(|g| rule(*g)).collect(),
    };
    let mut text = serde_json::to_string_pretty(&snippet).expect("the snippet serializes");
    text.push('\n');
    text
}

#[cfg(test)]
#[path = "describe_tests.rs"]
mod tests;
