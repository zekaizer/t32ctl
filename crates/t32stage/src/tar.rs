// SPDX-License-Identifier: Apache-2.0
//! Read-only walker for archives made by PowerView's `TAR` command.
//!
//! Small and tolerant: 512-byte headers, name at 0..100, size (octal or base-256) at 124..136,
//! type at 156, ustar prefix at 345..500, GNU long names (`L`), end at a zero block or EOF. The
//! flavor PowerView writes is not documented; `diagnose` reports it for `t32probe tar-download`.

use crate::error::TarError;

const BLOCK: usize = 512;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TarEntry {
    /// `\` turned into `/`, drive letter and leading `/` or `./` removed, never contains `..`.
    pub name: String,
    pub data: Vec<u8>,
}

/// What the archive looks like, best effort, for the probe report.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TarDiag {
    pub entries: usize,
    pub first_name_raw: String,
    pub ustar_magic: bool,
    pub backslash: bool,
    pub drive_letter: bool,
    pub checksum_ok: bool,
    pub zero_end: bool,
    pub first_header_hex: String,
}

fn field(h: &[u8], from: usize, to: usize) -> String {
    let f = &h[from..to];
    let end = f.iter().position(|&b| b == 0).unwrap_or(f.len());
    String::from_utf8_lossy(&f[..end]).into_owned()
}

fn size(h: &[u8], offset: usize) -> Result<usize, TarError> {
    let f = &h[124..136];
    if f[0] & 0x80 != 0 {
        return Ok(f[1..].iter().fold(0usize, |acc, &b| (acc << 8) | usize::from(b)));
    }
    let text = field(h, 124, 136);
    let t = text.trim_matches(|c: char| c == ' ' || c == '\0');
    if t.is_empty() {
        return Ok(0);
    }
    usize::from_str_radix(t, 8).map_err(|_| TarError::Header { offset, reason: "size is not octal" })
}

fn checksum_ok(h: &[u8]) -> bool {
    let want = field(h, 148, 156);
    let Ok(want) = u32::from_str_radix(want.trim_matches(|c: char| c == ' ' || c == '\0'), 8) else { return false };
    let sum: u32 = h.iter().enumerate().map(|(i, &b)| if (148..156).contains(&i) { 32 } else { u32::from(b) }).sum();
    sum == want
}

fn full_name(h: &[u8]) -> String {
    let name = field(h, 0, 100);
    let prefix = if &h[257..262] == b"ustar" { field(h, 345, 500) } else { String::new() };
    if prefix.is_empty() { name } else { format!("{prefix}/{name}") }
}

pub(crate) fn normalise_name(raw: &str) -> Result<String, TarError> {
    let mut t = raw.replace('\\', "/");
    if t.len() >= 2 && t.as_bytes()[1] == b':' && t.as_bytes()[0].is_ascii_alphabetic() {
        t.drain(..2);
    }
    let parts: Vec<&str> = t.split('/').filter(|p| !p.is_empty() && *p != ".").collect();
    if parts.is_empty() || parts.contains(&"..") {
        return Err(TarError::UnsafeName(raw.to_string()));
    }
    Ok(parts.join("/"))
}

/// `name` relative to the archived directory `dir` (its PowerView path). Build 168999 stores the
/// full path without the drive (`tar-download`: `names=prefixed`); a name outside `dir` is kept.
pub fn below(name: &str, dir: &str) -> String {
    let Ok(prefix) = normalise_name(dir) else { return name.to_string() };
    name.strip_prefix(&prefix).and_then(|rest| rest.strip_prefix('/')).unwrap_or(name).to_string()
}

/// Regular files only; directory entries and extended headers are skipped. A name that would
/// escape the extraction root is an error, not silently dropped.
pub fn walk(archive: &[u8]) -> Result<Vec<TarEntry>, TarError> {
    let mut out = Vec::new();
    let mut at = 0;
    let mut long_name: Option<String> = None;
    while at + BLOCK <= archive.len() {
        let h = &archive[at..at + BLOCK];
        if h.iter().all(|&b| b == 0) {
            break;
        }
        let len = size(h, at)?;
        let data_at = at + BLOCK;
        let name = long_name.take().unwrap_or_else(|| full_name(h));
        let data = archive.get(data_at..data_at + len).ok_or_else(|| TarError::Truncated { name: name.clone() })?;
        match h[156] {
            b'0' | 0 => out.push(TarEntry { name: normalise_name(&name)?, data: data.to_vec() }),
            b'L' => long_name = Some(field(data, 0, data.len())),
            _ => {}
        }
        at = data_at + len.next_multiple_of(BLOCK);
    }
    Ok(out)
}

pub fn diagnose(archive: &[u8]) -> TarDiag {
    let Some(h) = archive.get(..BLOCK) else { return TarDiag::default() };
    let raw = full_name(h);
    let tail_zero = archive.len() >= 2 * BLOCK && archive[archive.len() - BLOCK..].iter().all(|&b| b == 0);
    TarDiag {
        entries: walk(archive).map_or(0, |v| v.len()),
        backslash: raw.contains('\\'),
        drive_letter: raw.as_bytes().get(1) == Some(&b':'),
        first_name_raw: raw,
        ustar_magic: &h[257..262] == b"ustar",
        checksum_ok: checksum_ok(h),
        zero_end: tail_zero,
        first_header_hex: h[..160].iter().map(|b| format!("{b:02x}")).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_below_the_archived_directory_drop_its_path() {
        let dir = "C:\\Users\\op\\T\\t32ctl\\s\\o";
        assert_eq!(below("Users/op/T/t32ctl/s/o/a.txt", dir), "a.txt");
        assert_eq!(below("Users/op/T/t32ctl/s/o/s d/e.bin", dir), "s d/e.bin");
        assert_eq!(below("a.txt", dir), "a.txt");
        assert_eq!(below("Users/op/T/t32ctl/s/other/a.txt", dir), "Users/op/T/t32ctl/s/other/a.txt");
        assert_eq!(below("Users/op/T/t32ctl/s/o2/a.txt", dir), "Users/op/T/t32ctl/s/o2/a.txt");
    }

    fn archive(entries: &[(&str, &[u8])], gnu: bool) -> Vec<u8> {
        let mut b = tar::Builder::new(Vec::new());
        for (name, data) in entries {
            let mut h = if gnu { tar::Header::new_gnu() } else { tar::Header::new_ustar() };
            h.set_size(data.len() as u64);
            h.set_mode(0o644);
            h.set_entry_type(tar::EntryType::Regular);
            b.append_data(&mut h, name, *data).unwrap();
        }
        b.into_inner().unwrap()
    }

    #[test]
    fn reads_ustar_and_gnu_archives_with_long_names() {
        let long = format!("{}/x.bin", "d".repeat(120));
        let files: [(&str, &[u8]); 3] = [("a.txt", b"one"), ("sub/b.bin", &[7; 1000]), (&long, b"z")];
        for gnu in [false, true] {
            let got = walk(&archive(&files, gnu)).unwrap();
            let names: Vec<_> = got.iter().map(|e| e.name.as_str()).collect();
            assert_eq!(names, ["a.txt", "sub/b.bin", long.as_str()], "gnu={gnu}");
            assert_eq!(got[1].data, vec![7; 1000]);
        }
        let d = diagnose(&archive(&files, false));
        assert!(d.ustar_magic && d.checksum_ok && d.zero_end && !d.backslash);
        assert_eq!(d.entries, 3);
    }

    #[test]
    fn windows_style_names_are_normalised_and_escapes_refused() {
        assert_eq!(normalise_name("C:\\T\\out\\a.txt").unwrap(), "T/out/a.txt");
        assert_eq!(normalise_name("./sub/./b").unwrap(), "sub/b");
        assert_eq!(normalise_name("/abs/x").unwrap(), "abs/x");
        assert!(normalise_name("a/../../etc").is_err());
        assert!(normalise_name("").is_err());
        let mut a = archive(&[("a.txt", b"x")], false);
        a[..5].copy_from_slice(b"..\\..");
        assert!(matches!(walk(&a), Err(TarError::UnsafeName(_))));
    }

    #[test]
    fn a_truncated_entry_is_an_error() {
        let a = archive(&[("a.bin", &[1; 600])], false);
        assert!(matches!(walk(&a[..800]), Err(TarError::Truncated { .. })));
    }
}
