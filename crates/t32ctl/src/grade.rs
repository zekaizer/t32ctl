// SPDX-License-Identifier: Apache-2.0
//! Permission grades: the first word after `t32ctl`, and the policy that follows from it.
//!
//! The grades form a total order, `read < run < write < sys < burn < raw`; `admin` is outside it.
//! A leaf under grade `G` receives a `Ctx<G>`, whose operations live in impl blocks bounded by
//! grade.

/// A grade as data: its word, permission rule and place in the order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum GradeId {
    Read,
    Run,
    Write,
    Sys,
    Burn,
    Raw,
    Admin,
}

/// The Claude Code permission rule the generated snippet gives a grade.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rule {
    Allow,
    Ask,
    Deny,
}

impl Rule {
    pub fn name(self) -> &'static str {
        match self {
            Rule::Allow => "allow",
            Rule::Ask => "ask",
            Rule::Deny => "deny",
        }
    }
}

impl GradeId {
    pub const ALL: [GradeId; 7] =
        [GradeId::Read, GradeId::Run, GradeId::Write, GradeId::Sys, GradeId::Burn, GradeId::Raw, GradeId::Admin];

    pub fn name(self) -> &'static str {
        match self {
            GradeId::Read => "read",
            GradeId::Run => "run",
            GradeId::Write => "write",
            GradeId::Sys => "sys",
            GradeId::Burn => "burn",
            GradeId::Raw => "raw",
            GradeId::Admin => "admin",
        }
    }

    pub fn rule(self) -> Rule {
        match self {
            GradeId::Read | GradeId::Run => Rule::Allow,
            GradeId::Write | GradeId::Sys | GradeId::Burn | GradeId::Raw => Rule::Ask,
            GradeId::Admin => Rule::Deny,
        }
    }

    /// The rule stays `ask` when the operator relaxes others: a flash and raw PRACTICE always ask.
    pub fn never_allow(self) -> bool {
        matches!(self, GradeId::Burn | GradeId::Raw)
    }

    /// Position in the order; `None` for `admin`, which never acts on the debugger.
    pub fn order(self) -> Option<u8> {
        match self {
            GradeId::Read => Some(0),
            GradeId::Run => Some(1),
            GradeId::Write => Some(2),
            GradeId::Sys => Some(3),
            GradeId::Burn => Some(4),
            GradeId::Raw => Some(5),
            GradeId::Admin => None,
        }
    }
}

mod sealed {
    pub trait Sealed {}
}

/// A grade as a type, for `Ctx<G>`. Sealed: the six markers below are all there is.
pub trait Grade: sealed::Sealed + 'static {
    const ID: GradeId;
}

macro_rules! markers {
    ($($name:ident),*) => {$(
        #[derive(Debug)]
        pub struct $name;
        impl sealed::Sealed for $name {}
        impl Grade for $name {
            const ID: GradeId = GradeId::$name;
        }
    )*};
}

markers!(Read, Run, Write, Sys, Burn, Raw);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_order_is_total_below_admin_and_only_burn_and_raw_are_never_allowed() {
        let ordered: Vec<&str> = {
            let mut g: Vec<GradeId> = GradeId::ALL.iter().copied().filter(|g| g.order().is_some()).collect();
            g.sort_by_key(|g| g.order());
            g.iter().map(|g| g.name()).collect()
        };
        assert_eq!(ordered, ["read", "run", "write", "sys", "burn", "raw"]);
        let rules: Vec<(&str, &str, bool)> =
            GradeId::ALL.iter().map(|g| (g.name(), g.rule().name(), g.never_allow())).collect();
        assert_eq!(
            rules,
            [
                ("read", "allow", false),
                ("run", "allow", false),
                ("write", "ask", false),
                ("sys", "ask", false),
                ("burn", "ask", true),
                ("raw", "ask", true),
                ("admin", "deny", false),
            ]
        );
        assert_eq!([Read::ID, Run::ID, Raw::ID], [GradeId::Read, GradeId::Run, GradeId::Raw]);
    }
}
