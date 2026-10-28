// SPDX-License-Identifier: Apache-2.0
//! Every global PRACTICE macro t32ctl touches, in one table. Generated scripts, the client and
//! the probes all take names from here.
//!
//! Global macros are PowerView-wide shared state; every name has the `t32ctl_` prefix, and the
//! run token makes values left by an earlier or crashed run harmless.

use t32rcl::{LocalError, MacroName};

macro_rules! macro_table {
    ($( $variant:ident = $text:literal, $doc:literal; )+) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        pub enum Macro { $( #[doc = $doc] $variant ),+ }

        impl Macro {
            pub const ALL: &'static [Macro] = &[ $( Macro::$variant ),+ ];

            pub const fn text(self) -> &'static str {
                match self { $( Macro::$variant => $text ),+ }
            }
        }
    };
}

macro_table! {
    Snap  = "t32ctl_snap",  "client -> scripts: snapshot directory.";
    Out   = "t32ctl_out",   "client -> scripts: per-run output directory.";
    Token = "t32ctl_token", "client -> scripts: unique per run; copied into `Done` as the last statement.";
    Entry = "t32ctl_entry", "client -> runner: entry script, relative to the snapshot directory.";
    Args  = "t32ctl_args",  "client -> runner: unquoted argument text appended to the DO line.";
    Done  = "t32ctl_done",  "scripts -> client: equals `Token` once the script finished, success or failure.";
    Fail  = "t32ctl_fail",  "runner -> client: `1` when the runner caught a PRACTICE error; empty otherwise.";
    Rc    = "t32ctl_rc",    "entry script -> client: its result code; empty when it set none.";
    Msg   = "t32ctl_msg",   "scripts -> client: one line for the report; the caught error's message after a catch.";
    Ret   = "t32ctl_ret",   "scripts -> client: what the entry script returned with ENDDO.";
}

impl Macro {
    pub fn name(self) -> MacroName {
        MacroName::new(self.text()).expect("table entries are valid macro names")
    }

    /// `&t32ctl_x`, for PRACTICE text.
    pub fn amp(self) -> String {
        format!("&{}", self.text())
    }
}

/// The prefix of a procedure parameter's macro.
pub const PARAM_PREFIX: &str = "t32ctl_p_";

/// `t32ctl_p_<name>`, the macro a script reads a parameter from. `name` is not empty.
pub fn param_macro(name: &str) -> Result<MacroName, LocalError> {
    let full = format!("{PARAM_PREFIX}{name}");
    if name.is_empty() {
        return Err(LocalError::BadMacroName(full));
    }
    MacroName::new(&full)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_entry_is_a_valid_unique_prefixed_name() {
        let mut seen = std::collections::HashSet::new();
        for m in Macro::ALL {
            m.name();
            assert!(m.text().starts_with("t32ctl_"));
            assert!(seen.insert(m.text()));
        }
    }

    #[test]
    fn a_parameter_macro_is_prefixed_and_a_valid_name() {
        assert_eq!(param_macro("part").map(|m| m.amp()), Ok("&t32ctl_p_part".to_string()));
        assert!(param_macro(&"a".repeat(55)).is_ok());
        for bad in ["a-b", "", &"a".repeat(56)] {
            assert!(param_macro(bad).is_err(), "{bad}");
        }
    }
}
