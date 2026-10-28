// SPDX-License-Identifier: Apache-2.0
//! The policy hooks: the only readers of declared grades and of the memory map. Each returns
//! what the profile declares; a trust layer can replace them without changing the command line.

use t32rcl::AccessClass;

use std::marker::PhantomData;

use super::{ProcDecl, Profile, Region, RegionKind};
use crate::{
    error::{CtlError, ErrorKind},
    grade::{Grade, GradeId},
    notation::Size,
};

/// The grade procedure `proc` runs at, which the grade word must equal; `None` when the profile
/// declares no such procedure. The declared grade is private to the profile module, so every other
/// reader goes through this hook:
///
/// ```compile_fail,E0616
/// fn declared(decl: &t32ctl::profile::ProcDecl) -> t32ctl::grade::GradeId {
///     decl.grade
/// }
/// ```
pub fn effective_grade(p: &Profile, proc: &str) -> Option<GradeId> {
    p.proc(proc).map(|d| d.grade)
}

/// Proof that procedure `decl` runs at grade `G`: its effective grade is `G`. Built only by
/// [`admit`], so no leaf runs a procedure past the grade check:
///
/// ```compile_fail,E0451
/// use t32ctl::{grade::Read, profile::policy::Admitted};
/// fn forge(decl: t32ctl::profile::ProcDecl) -> Admitted<Read> {
///     Admitted { decl, _grade: std::marker::PhantomData }
/// }
/// ```
#[derive(Debug)]
pub struct Admitted<G: Grade> {
    decl: ProcDecl,
    _grade: PhantomData<G>,
}

impl<G: Grade> Admitted<G> {
    pub fn decl(&self) -> &ProcDecl {
        &self.decl
    }
}

/// Admit procedure `name` under the grade word `G`. An undeclared name is `unknown-proc`; another
/// grade is `grade-mismatch`, whose message names the declared grade. `raw` is never declared, so
/// `admit::<Raw>` never succeeds.
pub fn admit<G: Grade>(p: &Profile, name: &str) -> Result<Admitted<G>, CtlError> {
    let (Some(decl), Some(grade)) = (p.proc(name), effective_grade(p, name)) else {
        return Err(crate::procs::unknown(p, name));
    };
    if grade != G::ID {
        let message = format!("proc {name} runs at grade {}, not {}", grade.name(), G::ID.name());
        return Err(CtlError::new(ErrorKind::GradeMismatch, message));
    }
    Ok(Admitted { decl: decl.clone(), _grade: PhantomData })
}

/// The memory policy a call works under.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MemoryPolicy {
    /// Physical regions that nest or are disjoint.
    pub regions: Vec<Region>,
    pub classes: Vec<AccessClass>,
    pub default_class: AccessClass,
    /// The cell width outside `mmio`.
    pub word: u8,
}

/// The profile's memory map, classes, default class and word, as declared.
pub fn effective_memory(p: &Profile) -> MemoryPolicy {
    MemoryPolicy {
        regions: p.memory.clone(),
        classes: p.target.classes.clone(),
        default_class: p.target.default_class.clone(),
        word: p.target.word,
    }
}

/// A run of the access that is contiguous in physical memory: `len` bytes from `offset` bytes
/// into the access, at physical `phys`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Piece {
    pub offset: u64,
    pub phys: u64,
    pub len: u64,
}

/// A piece cut at region boundaries: all its bytes share one smallest containing region.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Segment {
    pub offset: u64,
    pub phys: u64,
    pub len: u64,
    /// Index into `MemoryPolicy::regions`.
    pub region: usize,
}

/// An allowed access: its width, the regions it touches in order, and its segments.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Checked {
    pub width: u8,
    pub regions: Vec<usize>,
    pub segments: Vec<Segment>,
}

fn refuse(message: String) -> CtlError {
    CtlError::new(ErrorKind::MemoryPolicy, message)
}

impl MemoryPolicy {
    /// The class of a place: as typed when the profile lists it, else the default class.
    pub fn class(&self, typed: Option<&AccessClass>) -> Result<AccessClass, CtlError> {
        let Some(c) = typed else { return Ok(self.default_class.clone()) };
        if self.classes.contains(c) {
            return Ok(c.clone());
        }
        let listed: Vec<&str> = self.classes.iter().map(AccessClass::as_str).collect();
        Err(refuse(format!(
            "access class {} is not one of the profile's classes {}; such an access is raw",
            c.as_str(),
            listed.join(", ")
        )))
    }

    /// The width of one cell at `phys`: its smallest region's width when that is `mmio`, else
    /// `word`.
    pub fn cell_width(&self, phys: u64) -> u8 {
        self.smallest(phys).and_then(|i| self.regions[i].width).unwrap_or(self.word)
    }

    /// The smallest region holding `at`.
    fn smallest(&self, at: u64) -> Option<usize> {
        let holding = self.regions.iter().enumerate().filter(|(_, r)| r.start <= at && at < r.end());
        holding.min_by_key(|(_, r)| r.size).map(|(i, _)| i)
    }

    /// The first region boundary after `at`, at most `end`.
    fn next_boundary(&self, at: u64, end: u64) -> u64 {
        let bounds = self.regions.iter().flat_map(|r| [r.start, r.end()]);
        bounds.filter(|&b| b > at).fold(end, u64::min)
    }

    /// A read of `pieces`: every byte must be readable in its smallest region, the length must be
    /// whole cells, every touched `mmio` region must have the access width and be accessed in
    /// aligned cells, and while the target runs every region must declare `running = true`. Map
    /// refusals come before the run state, so a read that could never pass is not reported as a
    /// wait. Without `width`, the touched `mmio` regions' width applies, else `word`.
    pub fn check(&self, pieces: &[Piece], running: bool, width: Option<u8>) -> Result<Checked, CtlError> {
        let mut segments = Vec::new();
        for p in pieces {
            let (mut at, end) = (p.phys, p.phys + p.len);
            while at < end {
                let next = self.next_boundary(at, end);
                let Some(region) = self.smallest(at) else {
                    return Err(refuse(format!("{at:#010x} in no region")));
                };
                let r = &self.regions[region];
                if !r.read.unwrap_or(r.kind != RegionKind::Deny) {
                    let why = match r.read {
                        Some(false) => format!("{} region {}, which declares read = false", r.kind.name(), r.name),
                        _ => format!("{} region {}", r.kind.name(), r.name),
                    };
                    return Err(refuse(format!("{at:#010x} in {why}")));
                }
                segments.push(Segment { offset: p.offset + (at - p.phys), phys: at, len: next - at, region });
                at = next;
            }
        }
        let mut regions: Vec<usize> = Vec::new();
        for s in &segments {
            if !regions.contains(&s.region) {
                regions.push(s.region);
            }
        }
        let mmio: Vec<&Region> =
            regions.iter().map(|&i| &self.regions[i]).filter(|r| r.kind == RegionKind::Mmio).collect();
        let width = match (width, mmio.first()) {
            (Some(w), _) => {
                if let Some(r) = mmio.iter().find(|r| r.width != Some(w)) {
                    return Err(refuse(format!(
                        "--width {w} does not match mmio region {}, which declares width = {}",
                        r.name,
                        r.width.unwrap_or_default()
                    )));
                }
                w
            }
            (None, Some(first)) => {
                if mmio.iter().any(|r| r.width != first.width) {
                    let all: Vec<String> =
                        mmio.iter().map(|r| format!("{} ({})", r.name, r.width.unwrap_or_default())).collect();
                    return Err(refuse(format!(
                        "the access touches mmio regions of different widths: {}; read them separately",
                        all.join(", ")
                    )));
                }
                first.width.unwrap_or(self.word)
            }
            (None, None) => self.word,
        };
        let total: u64 = pieces.iter().map(|p| p.len).sum();
        if !total.is_multiple_of(u64::from(width)) {
            let message = format!("--len {} is not a multiple of the access width {width}", Size(total));
            return Err(CtlError::new(ErrorKind::Notation, message));
        }
        for s in &segments {
            let r = &self.regions[s.region];
            let w = u64::from(width);
            if r.kind == RegionKind::Mmio && !(s.phys.is_multiple_of(w) && s.len.is_multiple_of(w)) {
                let at = if s.phys.is_multiple_of(w) { s.phys + s.len - s.len % w } else { s.phys };
                return Err(refuse(format!(
                    "{at:#010x} in mmio region {} is not aligned to its width {width}",
                    r.name
                )));
            }
        }
        if running && let Some(s) = segments.iter().find(|s| !self.regions[s.region].running) {
            let name = &self.regions[s.region].name;
            return Err(CtlError::new(
                ErrorKind::NotHalted,
                format!("the target is running and region {name} does not declare running = true"),
            ));
        }
        Ok(Checked { width, regions, segments })
    }
}

#[cfg(test)]
#[path = "policy_tests.rs"]
mod tests;
