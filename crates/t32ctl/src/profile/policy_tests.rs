// SPDX-License-Identifier: Apache-2.0
use std::path::Path;

use super::*;
use crate::{error::ErrorKind, profile::ProfileDoc};

const MAP: &str = r#"schema = 1

[target]
name = "b"
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
name = "boot"
start = 0x00000000
size = 0x00010000
kind = "rom"

[[memory]]
name = "uart0"
start = 0x09000000
size = 0x00001000
kind = "mmio"
width = 4

[[memory]]
name = "uart0-fifo"
start = 0x09000000
size = 0x00000004
kind = "deny"

[[memory]]
name = "gpio"
start = 0x09001000
size = 0x00001000
kind = "mmio"
width = 2

[[memory]]
name = "secure"
start = 0x09002000
size = 0x00001000
kind = "mmio"
width = 4
read = false
"#;

fn policy() -> MemoryPolicy {
    let (_, target, memory) = ProfileDoc::parse(MAP, Path::new("p.toml"), &[]).unwrap().select(None);
    MemoryPolicy { regions: memory, classes: target.classes, default_class: target.default_class, word: target.word }
}

fn whole(phys: u64, len: u64) -> Vec<Piece> {
    vec![Piece { offset: 0, phys, len }]
}

fn refusal(r: Result<Checked, CtlError>) -> (ErrorKind, String) {
    let e = r.unwrap_err();
    (e.kind, e.message)
}

/// Offset, physical start, length and region name of each segment.
fn segments<'p>(p: &'p MemoryPolicy, c: &Checked) -> Vec<(u64, u64, u64, &'p str)> {
    c.segments.iter().map(|s| (s.offset, s.phys, s.len, p.regions[s.region].name.as_str())).collect()
}

#[test]
fn an_access_inside_one_region_is_one_segment_at_the_word_width() {
    let p = policy();
    let c = p.check(&whole(0x4010_7e40, 64), false, None).unwrap();
    assert_eq!(c.width, 8);
    assert_eq!(segments(&p, &c), [(0, 0x4010_7e40, 64, "ddr")]);
    assert_eq!(c.regions, [0]);
    let c = p.check(&whole(0x0000_0100, 16), false, Some(1)).unwrap();
    assert_eq!((c.width, segments(&p, &c)), (1, vec![(0, 0x100, 16, "boot")]), "rom is readable");
}

#[test]
fn every_byte_counts_against_its_smallest_region_so_a_deny_hole_refuses_a_spanning_read() {
    let p = policy();
    assert_eq!(
        refusal(p.check(&whole(0x0900_0000, 16), false, None)),
        (ErrorKind::MemoryPolicy, "0x09000000 in deny region uart0-fifo".into())
    );
    let c = p.check(&whole(0x0900_0004, 16), false, None).unwrap();
    assert_eq!((c.width, segments(&p, &c)), (4, vec![(0, 0x0900_0004, 16, "uart0")]), "beside the hole");
    assert_eq!(
        refusal(p.check(&whole(0x08ff_fff8, 16), false, Some(4))),
        (ErrorKind::MemoryPolicy, "0x08fffff8 in no region".into())
    );
    assert_eq!(
        refusal(p.check(&whole(0x0a00_0000, 4), false, None)),
        (ErrorKind::MemoryPolicy, "0x0a000000 in no region".into())
    );
    assert_eq!(
        refusal(p.check(&whole(0x0900_2000, 4), false, None)),
        (ErrorKind::MemoryPolicy, "0x09002000 in mmio region secure, which declares read = false".into())
    );
}

#[test]
fn the_first_refused_byte_is_named_in_access_order_across_pieces() {
    let p = policy();
    let pieces = [Piece { offset: 0, phys: 0x4000_0ff0, len: 16 }, Piece { offset: 16, phys: 0x0a00_0000, len: 16 }];
    assert_eq!(refusal(p.check(&pieces, false, None)).1, "0x0a000000 in no region");
    let pieces = [Piece { offset: 0, phys: 0x4000_0ff0, len: 16 }, Piece { offset: 16, phys: 0x4020_0000, len: 16 }];
    let c = p.check(&pieces, false, None).unwrap();
    assert_eq!(segments(&p, &c), [(0, 0x4000_0ff0, 16, "ddr"), (16, 0x4020_0000, 16, "ddr")]);
    assert_eq!(c.regions, [0], "a region touched twice is listed once");
}

#[test]
fn mmio_takes_its_declared_width_and_refuses_another_a_mix_or_a_misaligned_access() {
    let p = policy();
    assert_eq!(
        refusal(p.check(&whole(0x0900_0004, 16), false, Some(8))),
        (ErrorKind::MemoryPolicy, "--width 8 does not match mmio region uart0, which declares width = 4".into())
    );
    assert_eq!(
        refusal(p.check(&whole(0x0900_0ff8, 16), false, None)),
        (
            ErrorKind::MemoryPolicy,
            "the access touches mmio regions of different widths: uart0 (4), gpio (2); read them separately".into()
        )
    );
    assert_eq!(
        refusal(p.check(&whole(0x0900_0006, 8), false, None)),
        (ErrorKind::MemoryPolicy, "0x09000006 in mmio region uart0 is not aligned to its width 4".into())
    );
    assert_eq!(
        refusal(p.check(&whole(0x0900_0004, 6), false, None)),
        (ErrorKind::Notation, "--len 6 is not a multiple of the access width 4".into())
    );
    let c = p.check(&whole(0x0900_1000, 6), false, None).unwrap();
    assert_eq!((c.width, segments(&p, &c)), (2, vec![(0, 0x0900_1000, 6, "gpio")]));
}

#[test]
fn a_running_target_reads_only_regions_that_declare_running() {
    let p = policy();
    let c = p.check(&whole(0x4000_0000, 16), true, None).unwrap();
    assert_eq!(segments(&p, &c), [(0, 0x4000_0000, 16, "ddr")]);
    assert_eq!(
        refusal(p.check(&whole(0x0900_0004, 4), true, None)),
        (ErrorKind::NotHalted, "the target is running and region uart0 does not declare running = true".into())
    );
    assert_eq!(
        refusal(p.check(&whole(0x0a00_0000, 4), true, None)).0,
        ErrorKind::MemoryPolicy,
        "a map refusal comes before the run state"
    );
}

#[test]
fn one_cell_is_the_smallest_regions_mmio_width_else_the_word() {
    let p = policy();
    assert_eq!([p.cell_width(0x0900_0004), p.cell_width(0x0900_1000), p.cell_width(0x4000_0000)], [4, 2, 8]);
    assert_eq!(p.cell_width(0x0900_0000), 8, "a deny hole has no width of its own");
    assert_eq!(p.cell_width(0x0a00_0000), 8, "outside the map the check refuses anyway");
}

#[test]
fn a_place_takes_the_default_class_and_only_a_declared_class() {
    let p = policy();
    let class = |c: &str| AccessClass::parse(c).unwrap();
    assert_eq!(p.class(None), Ok(class("D")));
    assert_eq!(p.class(Some(&class("a"))), Ok(class("A")));
    let e = p.class(Some(&class("C15"))).unwrap_err();
    assert_eq!(
        (e.kind, e.message.as_str()),
        (
            ErrorKind::MemoryPolicy,
            "access class C15 is not one of the profile's classes A, D, P; such an access is raw"
        )
    );
}

#[test]
fn the_effective_grade_is_the_declared_one_and_none_for_an_undeclared_procedure() {
    let text = format!(
        "{MAP}\n[proc.reset]\nabout = \"r\"\nscript = \"reset.cmm\"\ngrade = \"write\"\ntimeout = \"20s\"\n\
         effects = [\"target-reset\"]\n"
    );
    let doc = ProfileDoc::parse(&text, Path::new("p.toml"), &["reset.cmm"]).unwrap();
    let (_, target, memory) = doc.select(None);
    let p = crate::profile::Profile {
        dir: "b/support/t32ctl".into(),
        target,
        memory,
        variant: None,
        procs: doc.procs,
        files: Vec::new(),
        hash: crate::profile::ProfileHash([0; 32]),
    };
    assert_eq!(effective_grade(&p, "reset"), Some(GradeId::Write));
    assert_eq!(effective_grade(&p, "flash"), None);
}
