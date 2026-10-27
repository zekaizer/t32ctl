// SPDX-License-Identifier: Apache-2.0
//! The bundle: files that travel to PowerView as one gzip blob and unpack themselves.
//!
//! This module owns the bundle format: validation, order, content key, blob layout and the
//! `unpack.cmm` text derived from that layout. Script offsets and blob offsets come from one
//! `Layout`, never from two computations.
//!
//! ```text
//! offset 0            unpack.cmm             generated, `unpack_len` bytes
//! offset unpack_len   file 0 .. file n-1     sorted by path, runner included, empty files take 0 bytes
//! offset marker       .t32ctl/complete text  saved last by unpack.cmm
//! ```
//!
//! `unpack.cmm` saves every non-empty file with `Data.SAVE.Binary "&t32ctl_snap\<path>"
//! VM:<b+offset>++<len-1>` and checks its size. Numbers are fixed-width hex, so the script length
//! does not depend on the offsets it contains: `pack` renders it with placeholder offsets, lays
//! out the blob, renders again and asserts the same length.

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt, fs,
    path::Path,
};

use sha2::{Digest, Sha256};

use crate::{error::BundleError, macros::Macro, practice::RelPath, runner};

/// Generated files live here; user files may not.
pub const RESERVED_DIR: &str = ".t32ctl";
pub const RUNNER_PATH: &str = ".t32ctl/runner.cmm";
pub const UNPACK_PATH: &str = ".t32ctl/unpack.cmm";
pub const MARKER_PATH: &str = ".t32ctl/complete";
/// Part of the key: bump when the layout or the unpack semantics change.
const FORMAT_VERSION: u32 = 1;

/// 128-bit content key of a bundle, runner included. Names the snapshot directory.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SnapshotKey([u8; 16]);

impl SnapshotKey {
    pub fn hex(&self) -> String {
        self.0.iter().map(|b| format!("{b:02x}")).collect()
    }
}

impl fmt::Display for SnapshotKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.hex())
    }
}

impl fmt::Debug for SnapshotKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SnapshotKey({})", self.hex())
    }
}

#[derive(Clone, Copy, Debug)]
pub struct BundleLimits {
    pub max_files: usize,
    pub max_bytes: u64,
}

impl Default for BundleLimits {
    fn default() -> Self {
        Self { max_files: 512, max_bytes: 32 * 1024 * 1024 }
    }
}

/// A validated set of files. Invariants checked by `insert`, so every `Bundle` packs: unique
/// paths, no two equal under ASCII case folding, no path both a file and a directory prefix,
/// nothing under `.t32ctl/`, within the limits.
#[derive(Clone, Debug, Default)]
pub struct Bundle {
    files: BTreeMap<RelPath, Vec<u8>>,
    limits: BundleLimits,
}

impl Bundle {
    pub fn new(limits: BundleLimits) -> Self {
        Self { files: BTreeMap::new(), limits }
    }

    pub fn insert(&mut self, path: RelPath, bytes: Vec<u8>) -> Result<(), BundleError> {
        if path.as_str().split('/').next().is_some_and(|c| c.eq_ignore_ascii_case(RESERVED_DIR)) {
            return Err(BundleError::ReservedPrefix(path.to_string()));
        }
        let fold = path.fold();
        for other in self.files.keys().filter(|k| **k != path) {
            if other.fold() == fold {
                return Err(BundleError::CaseCollision { a: other.to_string(), b: path.to_string() });
            }
            let is_prefix =
                |dir: &RelPath, file: &RelPath| file.parents().any(|p| p.eq_ignore_ascii_case(dir.as_str()));
            if is_prefix(other, &path) || is_prefix(&path, other) {
                return Err(BundleError::FileDirConflict { a: other.to_string(), b: path.to_string() });
            }
        }
        let files = self.files.len() + usize::from(!self.files.contains_key(&path));
        let bytes_total = self.total_bytes() - self.files.get(&path).map_or(0, |v| v.len() as u64) + bytes.len() as u64;
        if files > self.limits.max_files || bytes_total > self.limits.max_bytes {
            return Err(BundleError::TooLarge { files, bytes: bytes_total });
        }
        self.files.insert(path, bytes);
        Ok(())
    }

    /// Every regular file under `root`, recursively. Symlinks and special files are refused,
    /// not followed: a silently missing script is the failure this tool exists to prevent.
    pub fn from_dir(root: &Path, limits: BundleLimits) -> Result<Self, BundleError> {
        let mut bundle = Bundle::new(limits);
        let mut stack = vec![root.to_path_buf()];
        while let Some(dir) = stack.pop() {
            let io = |source| BundleError::Io { path: dir.clone(), source };
            let mut entries: Vec<_> = fs::read_dir(&dir).map_err(io)?.collect::<Result<_, _>>().map_err(io)?;
            entries.sort_by_key(|e| e.file_name());
            for e in entries {
                let path = e.path();
                let kind = e.file_type().map_err(|source| BundleError::Io { path: path.clone(), source })?;
                if kind.is_dir() {
                    stack.push(path);
                } else if kind.is_file() {
                    let rel = path.strip_prefix(root).expect("walk stays below root");
                    let text = rel.to_str().ok_or_else(|| BundleError::FileName(path.clone()))?.replace('\\', "/");
                    let bytes = fs::read(&path).map_err(|source| BundleError::Io { path: path.clone(), source })?;
                    bundle.insert(RelPath::new(&text)?, bytes)?;
                } else {
                    return Err(BundleError::NotRegular(path));
                }
            }
        }
        Ok(bundle)
    }

    pub fn len(&self) -> usize {
        self.files.len()
    }

    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    pub fn total_bytes(&self) -> u64 {
        self.files.values().map(|v| v.len() as u64).sum()
    }

    pub fn files(&self) -> impl Iterator<Item = (&RelPath, &[u8])> {
        self.files.iter().map(|(k, v)| (k, v.as_slice()))
    }

    /// SHA-256 over the format version and `(path, len, SHA-256(content))` of every entry in path
    /// order, runner included, truncated to 128 bits. Independent of `VM:` addresses.
    pub fn key(&self) -> SnapshotKey {
        let mut h = Sha256::new();
        h.update(b"t32ctl-bundle\0");
        h.update(FORMAT_VERSION.to_le_bytes());
        for (path, bytes) in self.entries() {
            h.update((path.as_str().len() as u32).to_le_bytes());
            h.update(path.as_str().as_bytes());
            h.update((bytes.len() as u64).to_le_bytes());
            h.update(Sha256::digest(&bytes));
        }
        let mut k = [0u8; 16];
        k.copy_from_slice(&h.finalize()[..16]);
        SnapshotKey(k)
    }

    /// Every file that ends up in the snapshot directory except `unpack.cmm` and the marker.
    fn entries(&self) -> Vec<(RelPath, Vec<u8>)> {
        let runner = RelPath::new(RUNNER_PATH).expect("constant path");
        let mut v: Vec<(RelPath, Vec<u8>)> = self.files.iter().map(|(k, b)| (k.clone(), b.clone())).collect();
        v.push((runner, runner::runner_cmm().into_bytes()));
        v.sort_by(|a, b| a.0.cmp(&b.0));
        v
    }

    /// Blob and `unpack.cmm` for decompressed placement at `VM:b_start`.
    pub fn pack(&self, b_start: u32) -> Packed {
        let key = self.key();
        let entries = self.entries();
        let marker = key.hex().into_bytes();
        let draft = render_unpack(&key, &place(&entries, 0, marker.len()), b_start);
        let layout = place(&entries, draft.len() as u32, marker.len());
        let text = render_unpack(&key, &layout, b_start);
        assert_eq!(text.len(), draft.len(), "unpack.cmm length must not depend on offsets");

        let mut blob = text.clone().into_bytes();
        for (_, bytes) in &entries {
            blob.extend_from_slice(bytes);
        }
        blob.extend_from_slice(&marker);
        assert_eq!(blob.len() as u32, layout.marker.offset + layout.marker.len);
        Packed {
            key,
            raw_len: blob.len() as u32,
            unpack_len: layout.unpack_len,
            gz: crate::transfer::gzip(&blob),
            unpack_text: text,
            dirs: layout.dirs.iter().cloned().collect(),
            files: entries.into_iter().map(|(p, _)| p).collect(),
        }
    }
}

/// Result of `Bundle::pack`, consumed by `Stage`.
#[derive(Debug)]
pub struct Packed {
    pub key: SnapshotKey,
    pub gz: Vec<u8>,
    /// Decompressed size: decides the size of `VM:` region B.
    pub raw_len: u32,
    pub unpack_len: u32,
    pub unpack_text: String,
    /// Directories `unpack.cmm` creates, shallowest first.
    pub dirs: Vec<String>,
    /// Files it saves, runner included.
    pub files: Vec<RelPath>,
}

#[derive(Debug)]
struct Placed {
    path: String,
    offset: u32,
    len: u32,
}

/// The single computation of offsets.
#[derive(Debug)]
struct Layout {
    unpack_len: u32,
    entries: Vec<Placed>,
    marker: Placed,
    dirs: BTreeSet<String>,
}

fn place(entries: &[(RelPath, Vec<u8>)], unpack_len: u32, marker_len: usize) -> Layout {
    let mut offset = unpack_len;
    let mut placed = Vec::with_capacity(entries.len());
    let mut dirs = BTreeSet::new();
    for (path, bytes) in entries {
        dirs.extend(path.parents().map(str::to_string));
        placed.push(Placed { path: path.to_string(), offset, len: bytes.len() as u32 });
        offset += bytes.len() as u32;
    }
    dirs.insert(RESERVED_DIR.to_string());
    let marker = Placed { path: MARKER_PATH.to_string(), offset, len: marker_len as u32 };
    Layout { unpack_len, entries: placed, marker, dirs }
}

fn snap(path: &str) -> String {
    format!("\"{}\\{}\"", Macro::Snap.amp(), path.replace('/', "\\"))
}

/// PRACTICE text of the self-extracting script.
///
/// ASSUMED(bundle): `IF` with a one-line body on the next line, `GOTO` to a label in the same
/// file, `OPEN /Create /Binary` + `CLOSE` create an empty file, and macros set over EXP 2 are
/// visible to a script. ASSUMED(script-error): `ON ERROR JUMPTO` catches a failing
/// `Data.SAVE.Binary`. The syntax itself follows practice_ref (`IF`, `GOTO`, `ON`, `ENDDO`).
fn render_unpack(key: &SnapshotKey, layout: &Layout, b_start: u32) -> String {
    let (rc, msg, done, token) = (Macro::Rc.amp(), Macro::Msg.amp(), Macro::Done.amp(), Macro::Token.amp());
    let mut s = format!("; t32ctl unpack v{FORMAT_VERSION} key={key}\nON ERROR JUMPTO t32ctl_unpack_failed\n");
    for d in &layout.dirs {
        s += &format!("IF OS.DIR({p})==FALSE()\n  MKDIR {p}\n", p = snap(d));
    }
    for e in layout.entries.iter().chain([&layout.marker]) {
        let p = snap(&e.path);
        if e.len == 0 {
            s += &format!("OPEN #1 {p} /Create /Binary\nCLOSE #1\n");
            continue;
        }
        s += &format!("Data.SAVE.Binary {p} VM:0x{:08X}++0x{:08X}\n", b_start + e.offset, e.len - 1);
        s += &format!("IF OS.FILE.SIZE({p})!=0x{:08X}\n  GOTO t32ctl_unpack_badsize\n", e.len);
    }
    s += &format!("ON ERROR inherit\n{rc}=\"0\"\n{done}=\"{token}\"\nENDDO\n");
    s += &format!("t32ctl_unpack_badsize:\n{msg}=\"unpack: size check failed\"\nGOTO t32ctl_unpack_end\n");
    s += &format!("t32ctl_unpack_failed:\n{msg}=\"unpack: PRACTICE error\"\n");
    s += &format!("t32ctl_unpack_end:\nON ERROR inherit\n{rc}=\"1\"\n{done}=\"{token}\"\nENDDO\n");
    s
}

#[cfg(test)]
mod tests {
    use std::io::Read;

    use super::*;

    /// Applies the `MKDIR`, `Data.SAVE.Binary` and `OPEN` lines of a generated script to a blob.
    /// Understands only the forms `render_unpack` emits: it checks offset arithmetic, not
    /// PowerView behavior.
    fn replay(script: &str, blob: &[u8], b_start: u32) -> BTreeMap<String, Vec<u8>> {
        let path =
            |line: &str| line.split('"').nth(1).expect("quoted path").replace("&t32ctl_snap\\", "").replace('\\', "/");
        let mut out = BTreeMap::new();
        for line in script.lines() {
            if line.starts_with("Data.SAVE.Binary") {
                let range = line.rsplit(' ').next().expect("range").strip_prefix("VM:0x").expect("VM range");
                let (a, n) = range.split_once("++0x").expect("++ form");
                let a = u32::from_str_radix(a, 16).unwrap() - b_start;
                let n = u32::from_str_radix(n, 16).unwrap() + 1;
                out.insert(path(line), blob[a as usize..(a + n) as usize].to_vec());
            } else if line.starts_with("OPEN #1") {
                out.insert(path(line), Vec::new());
            }
        }
        out
    }

    fn gunzip(gz: &[u8]) -> Vec<u8> {
        let mut v = Vec::new();
        flate2::read::GzDecoder::new(gz).read_to_end(&mut v).unwrap();
        v
    }

    fn sample(order: &[(&str, usize)]) -> Bundle {
        let mut b = Bundle::new(BundleLimits::default());
        for (p, n) in order {
            b.insert(RelPath::new(p).unwrap(), (0..*n).map(|i| (i * 31 % 251) as u8).collect()).unwrap();
        }
        b
    }

    const FILES: [(&str, usize); 6] = [
        ("main.cmm", 37),
        ("sub/b.cmm", 64),
        ("sub/deep/data.bin", 4097),
        ("empty.txt", 0),
        ("dir with space/x.cmm", 1),
        ("z", 4096),
    ];

    #[test]
    fn unpack_replay_reproduces_every_file_and_the_marker() {
        let b = sample(&FILES);
        let packed = b.pack(0x0200_0000);
        let blob = gunzip(&packed.gz);
        assert_eq!(blob.len() as u32, packed.raw_len);
        let files = replay(&packed.unpack_text, &blob, 0x0200_0000);
        for (p, bytes) in b.files() {
            assert_eq!(files[p.as_str()], bytes, "{p}");
        }
        assert_eq!(files[RUNNER_PATH], runner::runner_cmm().as_bytes());
        assert_eq!(files[MARKER_PATH], packed.key.hex().as_bytes());
        let last_save = packed.unpack_text.lines().rfind(|l| l.starts_with("Data.SAVE.Binary")).unwrap();
        assert!(last_save.contains("complete"), "the marker is saved last");
        assert_eq!(packed.dirs, [".t32ctl", "dir with space", "sub", "sub/deep"]);
        assert!(packed.unpack_text.lines().all(|l| l.len() < 2000));
    }

    #[test]
    fn key_ignores_vm_addresses_and_insertion_order() {
        let mut reversed = FILES;
        reversed.reverse();
        let (a, b) = (sample(&FILES), sample(&reversed));
        assert_eq!(a.key(), b.key());
        let (p1, p2) = (a.pack(0x0100_0000), a.pack(0x0300_0000));
        assert_eq!(p1.key, p2.key);
        assert_ne!(p1.unpack_text, p2.unpack_text);
        assert_eq!(p1.unpack_len, p2.unpack_len);
        assert_eq!(a.pack(0x0100_0000).gz, p1.gz, "gzip output is deterministic");
        assert_ne!(sample(&[("main.cmm", 37)]).key(), sample(&[("main.cmm", 38)]).key());
    }

    #[test]
    fn collisions_reserved_names_and_limits_are_refused() {
        let mut b = Bundle::new(BundleLimits { max_files: 2, max_bytes: 10 });
        b.insert(RelPath::new("Main.cmm").unwrap(), vec![1]).unwrap();
        let err = |b: &mut Bundle, p: &str, n: usize| b.insert(RelPath::new(p).unwrap(), vec![0; n]).unwrap_err();
        assert!(matches!(err(&mut b, "main.cmm", 1), BundleError::CaseCollision { .. }));
        assert!(matches!(err(&mut b, ".t32ctl/x", 1), BundleError::ReservedPrefix(_)));
        assert!(matches!(err(&mut b, "Main.cmm/x", 1), BundleError::FileDirConflict { .. }));
        assert!(matches!(err(&mut b, "big", 10), BundleError::TooLarge { .. }));
        b.insert(RelPath::new("Main.cmm").unwrap(), vec![2]).unwrap();
    }

    #[test]
    fn from_dir_walks_regular_files_with_forward_slashes() {
        let dir = crate::testutil::temp_dir("bundle-from-dir");
        fs::create_dir_all(dir.join("sub")).unwrap();
        fs::write(dir.join("main.cmm"), "ENDDO\n").unwrap();
        fs::write(dir.join("sub/b.cmm"), "ENDDO\n").unwrap();
        let b = Bundle::from_dir(&dir, BundleLimits::default()).unwrap();
        assert_eq!(b.files().map(|(p, _)| p.to_string()).collect::<Vec<_>>(), ["main.cmm", "sub/b.cmm"]);
        std::os::unix::fs::symlink(dir.join("main.cmm"), dir.join("link.cmm")).unwrap();
        assert!(matches!(Bundle::from_dir(&dir, BundleLimits::default()), Err(BundleError::NotRegular(_))));
        let _ = fs::remove_dir_all(dir);
    }
}
