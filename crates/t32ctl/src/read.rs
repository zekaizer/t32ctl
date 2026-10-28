// SPDX-License-Identifier: Apache-2.0
//! Bodies of the `read` leaves that connect.

use std::{fs, path::PathBuf, time::Duration};

use t32stage::RegName;

use crate::{
    Setup,
    cli::{ReadMem, ReadReg, ReadStatus},
    config::Sourced,
    ctx::{Ctx, MemPlace, MemPlan, MemRequest, MemTo, Transfer},
    error::{CtlError, ErrorKind},
    grade::Read,
    notation::{OutName, Place, Size},
    out::OutDir,
    profile::policy::effective_memory,
    report::{
        self, BindingView, Body, ConfigView, FileStats, MemBody, MemData, Phys, ProfileView, RegionRef, StatusBody,
        StoppedTransfer, cells,
    },
};

/// The target, the profile and PowerView; `--full` adds the profile hash, the binding, the config
/// and the state directory.
pub fn status(args: &ReadStatus, ctx: &mut Ctx<'_, Read>, setup: &Setup<'_>) -> Result<Body, CtlError> {
    let (target, pv) = ctx.status()?;
    let env = setup.env;
    let p = &setup.profile;
    let profile = ProfileView { dir: format!("{}/", env.display(&p.dir)), hash: None, files: None };
    let mut body = StatusBody { target, profile, pv, binding: None, config: None, state_dir: None };
    if args.full {
        let shown = |s: &Option<Sourced<PathBuf>>| {
            s.as_ref().map(|s| report::Sourced { path: env.display(&s.value), source: s.source.text().to_string() })
        };
        let b = &setup.binding;
        body.profile.hash = Some(p.hash.to_string());
        body.profile.files = Some(p.files.len());
        body.binding = Some(BindingView {
            build: shown(&b.build),
            profile_from: b.profile.as_ref().map_or_else(String::new, |s| s.source.text().to_string()),
            variant: p.variant.clone(),
            board: b.board.clone(),
        });
        body.config = Some(ConfigView {
            file: setup.config.file.as_deref().map(|f| env.display(f)),
            host: setup.config.host.clone(),
            attached: Some(ctx.host().to_string()),
            port: setup.config.port,
        });
        body.state_dir = Some(format!("{}/", env.display(&setup.pv_dir)));
    }
    Ok(Body::Status(Box::new(body)))
}

/// The AArch64 general-purpose set `read reg` reads without a name.
const GENERAL: [&str; 34] = [
    "x0", "x1", "x2", "x3", "x4", "x5", "x6", "x7", "x8", "x9", "x10", "x11", "x12", "x13", "x14", "x15", "x16", "x17",
    "x18", "x19", "x20", "x21", "x22", "x23", "x24", "x25", "x26", "x27", "x28", "x29", "x30", "sp", "pc", "cpsr",
];

/// The named registers, or the general-purpose set.
pub fn reg(args: &ReadReg, ctx: &mut Ctx<'_, Read>, _setup: &Setup<'_>) -> Result<Body, CtlError> {
    let names = if args.names.is_empty() {
        GENERAL.iter().map(|n| RegName::new(n).expect("a general-purpose register name is valid")).collect()
    } else {
        args.names.clone()
    };
    ctx.read_regs(&names).map(Body::Regs)
}

/// The most `read mem` prints as cells; more needs `--to`.
const PRINT_MAX: Size = Size(4096);

/// What `read mem` settled before connecting.
#[derive(Debug)]
pub struct MemPrep {
    req: MemRequest,
    /// `--to`: the fresh run directory and the file name.
    to: Option<(OutDir, OutName)>,
    bound: Duration,
}

fn notation(message: String) -> CtlError {
    CtlError::new(ErrorKind::Notation, message)
}

/// The class, `--len` and `--width` checks that need no PowerView, and the run directory for
/// `--to`.
pub fn mem_prepare(args: &ReadMem, setup: &Setup<'_>) -> Result<MemPrep, CtlError> {
    let typed = match &args.loc {
        Place::Addr { class, .. } => class.as_ref(),
        Place::Reg { .. } => None,
    };
    effective_memory(&setup.profile).class(typed)?;
    let width = args.width.map(|w| w.bytes());
    if let Some(len) = args.len {
        if len.bytes() == 0 {
            return Err(notation("--len 0: a read needs at least one byte".into()));
        }
        match args.to {
            None if len > PRINT_MAX => {
                return Err(notation(format!("--len {len} is above {PRINT_MAX}; above {PRINT_MAX} needs --to NAME")));
            }
            Some(_) if len > setup.config.transfer_max => {
                let max = setup.config.transfer_max;
                return Err(notation(format!("--len {len} is above [calls] transfer_max {max}")));
            }
            _ => {}
        }
        if let Some(w) = width
            && !len.bytes().is_multiple_of(u64::from(w))
        {
            return Err(notation(format!("--len {len} is not a multiple of --width {w}")));
        }
    }
    let to = match &args.to {
        Some(name) => Some((OutDir::create(&setup.paths.out_base(), "m")?, name.clone())),
        None => None,
    };
    let req = MemRequest { place: args.loc.clone(), len: args.len.map(Size::bytes), width };
    Ok(MemPrep { req, to, bound: setup.config.transfer_timeout.as_duration() })
}

/// The body of a read the memory map allowed, or of one stopped before the map checked `place`:
/// that one shows `--len` and `--width` as given.
fn mem_body(place: &MemPlace, plan: Option<&MemPlan>, req: &MemRequest, data: MemData) -> Body {
    let class = place.class.as_str();
    let shown = if place.default_class { format!("{:#x}", place.addr) } else { format!("{class}:{:#x}", place.addr) };
    let regions = plan.map_or_else(Vec::new, |p| {
        p.regions.iter().map(|(name, kind)| RegionRef { name: name.clone(), kind: kind.name() }).collect()
    });
    Body::Mem(MemBody {
        place: format!("{class}:{:#x}", place.addr),
        shown,
        class: class.to_string(),
        addr: place.addr,
        phys: plan.and_then(|p| p.phys).map(Phys),
        len: plan.map_or(req.len, |p| Some(p.len)),
        width: plan.map_or(req.width, |p| Some(p.width)),
        regions,
        data,
    })
}

/// Memory as cells, or into a new file in the run directory. A transfer that does not finish
/// leaves no file.
pub fn mem(_args: &ReadMem, prep: MemPrep, ctx: &mut Ctx<'_, Read>, setup: &Setup<'_>) -> Result<Body, CtlError> {
    let Some((dir, name)) = prep.to else {
        let (plan, data) = ctx.read_mem(&prep.req)?;
        let cells = cells(&data, plan.width);
        return Ok(mem_body(&plan.place, Some(&plan), &prep.req, MemData::Cells(cells)));
    };
    let (path, mut file) = dir.create_file(&name)?;
    let result = ctx.read_mem_to(&prep.req, prep.bound, &mut file);
    drop(file);
    if !matches!(result, Ok(MemTo::Checked(_, Transfer::Done { .. }))) {
        let _ = fs::remove_file(&path);
    }
    Ok(match result? {
        MemTo::Checked(plan, Transfer::Done { bytes, after_ms }) => {
            let file = MemData::File(FileStats { path: setup.env.display(&path), bytes, after_ms });
            mem_body(&plan.place, Some(&plan), &prep.req, file)
        }
        MemTo::Checked(plan, Transfer::Stopped { bytes, bound_ms }) => {
            mem_body(&plan.place, Some(&plan), &prep.req, MemData::Stopped(StoppedTransfer { bound_ms, bytes }))
        }
        MemTo::Unchecked { place, bound_ms } => {
            mem_body(&place, None, &prep.req, MemData::Stopped(StoppedTransfer { bound_ms, bytes: 0 }))
        }
    })
}
