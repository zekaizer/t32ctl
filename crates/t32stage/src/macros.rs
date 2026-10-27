// SPDX-License-Identifier: Apache-2.0
//! Every global PRACTICE macro t32ctl touches, in one table. Generated scripts, the client and
//! the probes all take names from here.
//!
//! Global macros are PowerView-wide shared state; every name has the `t32ctl_` prefix, and the
//! run token makes values left by an earlier or crashed run harmless.

use t32rcl::MacroName;

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
    Root  = "t32ctl_root",  "Stage root in `Macro` root mode: `OS.PTD()+\"\\t32ctl\"`, computed by PowerView.";
    Snap  = "t32ctl_snap",  "client -> scripts: snapshot directory.";
    Out   = "t32ctl_out",   "client -> scripts: per-run output directory.";
    Token = "t32ctl_token", "client -> scripts: unique per run; copied into `Done` as the last statement.";
    Entry = "t32ctl_entry", "client -> runner: entry script, relative to the snapshot directory.";
    Args  = "t32ctl_args",  "client -> runner: quoted argument text appended to the DO line.";
    Done  = "t32ctl_done",  "scripts -> client: equals `Token` once the script finished, success or failure.";
    Rc    = "t32ctl_rc",    "scripts -> client: 0 success, 1 PRACTICE error caught.";
    Msg   = "t32ctl_msg",   "scripts -> client: error text when `Rc` is not 0.";
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
}
