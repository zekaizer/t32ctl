// SPDX-License-Identifier: Apache-2.0
//! Script builders for scripted conversations with a `Stage` (feature `testkit`): each appends
//! what `Stage` sends and what PowerView answers to a `t32rcl::testkit::Script`.
//!
//! The builders assume `OS.PTD()` answers [`PTD`], no explicit root and no subdirectory, the
//! default `VM:` window base, `Stage::set_token(TOKEN)`, and reads of at most
//! `Tuning::MAX_CHUNK` bytes per request.

use t32rcl::{
    Tuning,
    testkit::{Request, Script},
};

use crate::{bundle::SnapshotKey, macros::Macro, tar::normalise_name, vm::VmWindow};

/// What `OS.PTD()` answers.
pub const PTD: &str = "C:\\T";
/// The stage root below it.
pub const ROOT: &str = "C:\\T\\t32ctl";
/// The token `Stage::set_token` fixes for a scripted conversation.
pub const TOKEN: &str = "tok";

/// `ROOT\<rel>` with `\` separators.
pub fn at(rel: &str) -> String {
    format!("{ROOT}\\{}", rel.replace('/', "\\"))
}

/// The snapshot directory of a bundle.
pub fn snap_dir(key: &SnapshotKey) -> String {
    at(&key.hex())
}

/// The runner of a snapshot.
pub fn runner(key: &SnapshotKey) -> String {
    format!("{}\\.t32ctl\\runner.cmm", snap_dir(key))
}

/// The run's output directory, `&t32ctl_out`.
pub fn out_dir() -> String {
    at(&format!("out-{TOKEN}"))
}

pub fn set(s: Script, m: Macro, v: &str) -> Script {
    set_name(s, m.text(), v)
}

/// An EXP 2 set of `&<name>`, for macros outside the table (parameters).
pub fn set_name(s: Script, name: &str, v: &str) -> Script {
    s.expect_req(Request::MacroSet { name: format!("&{name}"), value: v.into() }).reply_macro("")
}

pub fn get(s: Script, m: Macro, v: &str) -> Script {
    s.expect_req(Request::MacroGet { name: m.amp() }).reply_macro(v)
}

/// PRACTICE idle with `depth` scripts on the stack.
pub fn idle(s: Script, depth: i64) -> Script {
    s.expect_req(Request::PracticeState).reply_state(0).expect_fn("PRACTICE.StackDepth()").reply_decimal(depth)
}

/// `Stage::open`: the root from `OS.PTD()`, every t32ctl macro declared, the root exists.
pub fn opened(s: Script) -> Script {
    s.expect_fn("OS.PTD()")
        .reply_text(PTD)
        .expect_cmd(
            "GLOBAL &t32ctl_snap &t32ctl_out &t32ctl_token &t32ctl_entry &t32ctl_args &t32ctl_done &t32ctl_fail \
             &t32ctl_rc &t32ctl_msg &t32ctl_ret",
        )
        .reply_ok()
        .expect_fn(&format!("OS.DIR(\"{ROOT}\")"))
        .reply_bool(true)
}

/// `ensure_snapshot` of a bundle whose snapshot is complete: one function call.
pub fn cached(s: Script, key: &SnapshotKey) -> Script {
    s.expect_fn(&format!("OS.FILE.readable(\"{}\\.t32ctl\\complete\")", snap_dir(key))).reply_bool(true)
}

/// `Stage::run` up to the runner's `DO`: the idle check, the output directory when `out`, the
/// parameter macros (`t32ctl_p_<name>`, value), the snapshot, entry (`\` separators) and
/// argument macros.
pub fn run_started(
    s: Script,
    key: &SnapshotKey,
    entry: &str,
    args: &str,
    out: bool,
    params: &[(&str, &str)],
) -> Script {
    let mut s = idle(s, 0);
    if out {
        let dir = out_dir();
        s = s
            .expect_fn(&format!("OS.DIR(\"{dir}\")"))
            .reply_bool(false)
            .expect_cmd(&format!("MKDIR \"{dir}\""))
            .reply_ok();
        s = set(s, Macro::Out, &dir);
    }
    for (name, value) in params {
        s = set_name(s, &format!("t32ctl_p_{name}"), value);
    }
    s = set(s, Macro::Snap, &snap_dir(key));
    s = set(s, Macro::Entry, entry);
    s = set(s, Macro::Args, args);
    started_do(s, &runner(key))
}

/// The token, the result macros emptied, then `ERROR.RESet` and `DO "<script>"`.
pub fn started_do(mut s: Script, script: &str) -> Script {
    s = set(s, Macro::Token, TOKEN);
    for m in [Macro::Done, Macro::Fail, Macro::Rc, Macro::Msg, Macro::Ret] {
        s = set(s, m, "");
    }
    s.expect_cmd("ERROR.RESet").reply_ok().expect_cmd(&format!("DO \"{script}\"")).reply_ok()
}

/// The poll that finds the runner done: idle, the token in `&t32ctl_done`, an empty stack.
pub fn done(s: Script) -> Script {
    let s = s.expect_req(Request::PracticeState).reply_state(0);
    get(s, Macro::Done, TOKEN).expect_fn("PRACTICE.StackDepth()").reply_decimal(0)
}

/// The `Finished` read-back: what the scripts left in `fail`, `rc`, `msg` and `ret`.
pub fn read_back(mut s: Script, fail: &str, rc: &str, msg: &str, ret: &str) -> Script {
    s = get(s, Macro::Fail, fail);
    s = get(s, Macro::Rc, rc);
    s = get(s, Macro::Msg, msg);
    get(s, Macro::Ret, ret)
}

/// `DO "<script>"`, one running sample, then a finish with `rc` and `ret` and nothing caught.
pub fn executed(s: Script, script: &str, rc: &str, ret: &str) -> Script {
    let s = started_do(s, script).expect_req(Request::PracticeState).reply_state(1);
    read_back(done(s), "", rc, "", ret)
}

/// `fetch_outputs` of `&t32ctl_out` holding `files` (names below it, `/` separators): the
/// listing, and for any file the `TAR` of the directory and its download.
pub fn fetched(mut s: Script, files: &[(&str, &[u8])]) -> Script {
    let dir = out_dir();
    s = s.expect_fn(&format!("OS.FIRSTFILE(\"{dir}\\*\")")).reply_text(&format!("{dir}\\."));
    s = s.expect_fn("OS.NEXTFILE()").reply_text(&format!("{dir}\\.."));
    let Some((first, _)) = files.first() else {
        return s.expect_fn("OS.NEXTFILE()").reply_text("");
    };
    s = s.expect_fn("OS.NEXTFILE()").reply_text(&format!("{dir}\\{}", first.split('/').next().unwrap_or(first)));
    let archive = format!("{dir}.tar");
    s = s.expect_cmd(&format!("TAR \"{archive}\" \"{dir}\\*\"")).reply_ok();
    let prefix = normalise_name(&dir).expect("the output directory is a path");
    let named: Vec<(String, &[u8])> = files.iter().map(|(n, d)| (format!("{prefix}/{n}"), *d)).collect();
    downloaded(s, &archive, &tar(&named.iter().map(|(n, d)| (n.as_str(), *d)).collect::<Vec<_>>()))
}

/// `Stage::download` of `src` holding `bytes`: PowerView zips it, the client reads the `.gz`
/// through the window base and removes it.
pub fn downloaded(mut s: Script, src: &str, bytes: &[u8]) -> Script {
    let gz = crate::transfer::gzip(bytes);
    let gz_path = format!("{src}.gz");
    let base = VmWindow::DEFAULT_BASE;
    s = s.expect_fn(&format!("OS.FILE.SIZE(\"{src}\")")).reply_decimal(bytes.len() as i64);
    s = s.expect_cmd(&format!("ZIP \"{src}\" \"{gz_path}\"")).reply_ok();
    s = s.expect_fn(&format!("OS.FILE.SIZE(\"{gz_path}\")")).reply_decimal(gz.len() as i64);
    s = s.expect_cmd(&format!("Data.LOAD.Binary \"{gz_path}\" VM:0x{base:08X}")).reply_ok();
    for (i, chunk) in gz.chunks(Tuning::MAX_CHUNK).enumerate() {
        let addr = u64::from(base) + (i * Tuning::MAX_CHUNK) as u64;
        s = s.expect_req(Request::MemRead { access: "VM".into(), addr, len: chunk.len() as u16 }).reply_mem(chunk);
    }
    s.expect_cmd(&format!("RM \"{gz_path}\"")).reply_ok()
}

/// A cleanup step that finds the file and removes it.
pub fn file_removed(s: Script, path: &str) -> Script {
    let check = format!("OS.FILE.readable(\"{path}\")");
    s.expect_fn(&check)
        .reply_bool(true)
        .expect_cmd(&format!("RM \"{path}\""))
        .reply_ok()
        .expect_fn(&check)
        .reply_bool(false)
}

/// A cleanup step that finds the file already gone.
pub fn file_absent(s: Script, path: &str) -> Script {
    s.expect_fn(&format!("OS.FILE.readable(\"{path}\")")).reply_bool(false)
}

/// A cleanup step that finds the directory and removes it.
pub fn dir_removed(s: Script, path: &str) -> Script {
    let check = format!("OS.DIR(\"{path}\")");
    s.expect_fn(&check)
        .reply_bool(true)
        .expect_cmd(&format!("RMDIR \"{path}\""))
        .reply_ok()
        .expect_fn(&check)
        .reply_bool(false)
}

/// A cleanup step that finds the run directory `path`, lists it empty and removes it.
pub fn run_dir_removed(s: Script, path: &str) -> Script {
    let check = format!("OS.DIR(\"{path}\")");
    let s = s.expect_fn(&check).reply_bool(true);
    let s = s.expect_fn(&format!("OS.FIRSTFILE(\"{path}\\*\")")).reply_text(&format!("{path}\\."));
    let s = s.expect_fn("OS.NEXTFILE()").reply_text(&format!("{path}\\..")).expect_fn("OS.NEXTFILE()").reply_text("");
    s.expect_cmd(&format!("RMDIR \"{path}\"")).reply_ok().expect_fn(&check).reply_bool(false)
}

/// Cleanup after `fetched(files)`, newest first: each file, the directories that held them, the
/// archive's `.gz` (already gone) and the archive, then the output directory.
pub fn outputs_removed(mut s: Script, files: &[(&str, &[u8])]) -> Script {
    let dir = out_dir();
    let native = |rel: &str| format!("{dir}\\{}", rel.replace('/', "\\"));
    if !files.is_empty() {
        for (name, _) in files.iter().rev() {
            s = file_removed(s, &native(name));
        }
        let parents: std::collections::BTreeSet<&str> =
            files.iter().flat_map(|(n, _)| n.match_indices('/').map(|(i, _)| &n[..i])).collect();
        for p in parents.iter().rev() {
            s = dir_removed(s, &native(p));
        }
        s = file_absent(s, &format!("{dir}.tar.gz"));
        s = file_removed(s, &format!("{dir}.tar"));
    }
    run_dir_removed(s, &dir)
}

/// `Stage::close` at its end: the PRACTICE unwind when `unwind` (it finds an empty stack), then
/// every t32ctl macro cleared.
pub fn closed(mut s: Script, unwind: bool) -> Script {
    if unwind {
        s = idle(s, 0);
    }
    for m in Macro::ALL {
        s = set(s, *m, "");
    }
    s
}

/// A ustar archive of `entries`, as build 168999's `TAR` writes one: regular files under the
/// names given, then two zero blocks.
pub fn tar(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut out = Vec::new();
    for (name, data) in entries {
        assert!(name.len() <= 100, "{name} needs a long-name entry");
        let mut h = [0u8; 512];
        h[..name.len()].copy_from_slice(name.as_bytes());
        h[100..108].copy_from_slice(b"0000644\0");
        h[108..116].copy_from_slice(b"0000000\0");
        h[116..124].copy_from_slice(b"0000000\0");
        h[124..136].copy_from_slice(format!("{:011o}\0", data.len()).as_bytes());
        h[136..148].copy_from_slice(b"00000000000\0");
        h[148..156].copy_from_slice(b"        ");
        h[156] = b'0';
        h[257..265].copy_from_slice(b"ustar\x0000");
        let sum: u32 = h.iter().map(|&b| u32::from(b)).sum();
        h[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
        out.extend_from_slice(&h);
        out.extend_from_slice(data);
        out.resize(out.len().next_multiple_of(512), 0);
    }
    out.resize(out.len() + 1024, 0);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_archive_reads_back_with_checksums_powerview_would_write() {
        let a = tar(&[("T/t32ctl/out-tok/a.txt", b"one"), ("T/t32ctl/out-tok/s/b.bin", &[7; 600])]);
        let walked = crate::tar::walk(&a).unwrap();
        let got: Vec<(&str, usize)> = walked.iter().map(|e| (e.name.as_str(), e.data.len())).collect();
        assert_eq!(got, [("T/t32ctl/out-tok/a.txt", 3), ("T/t32ctl/out-tok/s/b.bin", 600)]);
        let d = crate::tar::diagnose(&a);
        assert!(d.checksum_ok && d.ustar_magic && d.zero_end);
    }
}
