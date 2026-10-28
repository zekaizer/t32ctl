// SPDX-License-Identifier: Apache-2.0
//! The output directory: one fresh `<state base>/out/<run id>/` per call that writes files, so a
//! call never overwrites another call's output.

use std::{
    fs::{self, File},
    io::{self, Write},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use crate::{
    error::{CtlError, ErrorKind},
    notation::OutName,
};

/// Run ids tried before giving up.
const TRIES: u64 = 16;

/// A run directory this call created. Dropping it removes it if it is still empty.
#[derive(Debug)]
pub struct OutDir {
    path: PathBuf,
}

fn state_dir(path: &Path, e: &io::Error) -> CtlError {
    CtlError::new(ErrorKind::StateDir, format!("{}: {e}", path.display()))
}

impl OutDir {
    /// `<base>/<prefix>-<4 hex>`, made with `create_dir`, so an id another call holds is skipped.
    pub fn create(base: &Path, prefix: &str) -> Result<Self, CtlError> {
        fs::create_dir_all(base).map_err(|e| state_dir(base, &e))?;
        let since = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
        let seed = (since.as_nanos() as u64) ^ u64::from(std::process::id());
        for i in 0..TRIES {
            let path = base.join(format!("{prefix}-{:04x}", seed.wrapping_add(i) & 0xffff));
            match fs::create_dir(&path) {
                Ok(()) => return Ok(Self { path }),
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
                Err(e) => return Err(state_dir(&path, &e)),
            }
        }
        Err(CtlError::new(ErrorKind::StateDir, format!("{}: no free run id after {TRIES} tries", base.display())))
    }

    /// `path` itself, which must not exist (`file-exists`) and whose parent must (`notation`).
    pub fn at(path: &Path, shown: &str) -> Result<Self, CtlError> {
        match fs::create_dir(path) {
            Ok(()) => Ok(Self { path: path.to_path_buf() }),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Err(CtlError::new(
                ErrorKind::FileExists,
                format!("--out {shown} exists; raw script --out never overwrites; choose a new directory"),
            )),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                Err(CtlError::new(ErrorKind::Notation, format!("--out {shown}: the parent directory does not exist")))
            }
            Err(e) => Err(state_dir(path, &e)),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// A new file in the directory; an existing one is never overwritten.
    pub fn create_file(&self, name: &OutName) -> Result<(PathBuf, File), CtlError> {
        let path = self.path.join(name.as_str());
        let file = File::options().write(true).create_new(true).open(&path).map_err(|e| state_dir(&path, &e))?;
        Ok((path, file))
    }

    /// `bytes` as the new file `rel` (`/` separated, no `..`), its directories made.
    pub fn write(&self, rel: &str, bytes: &[u8]) -> Result<(), CtlError> {
        let path = rel.split('/').fold(self.path.clone(), |p, c| p.join(c));
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| state_dir(parent, &e))?;
        }
        let mut file = File::options().write(true).create_new(true).open(&path).map_err(|e| state_dir(&path, &e))?;
        file.write_all(bytes).map_err(|e| state_dir(&path, &e))
    }
}

impl Drop for OutDir {
    fn drop(&mut self) {
        // Fails, and keeps the directory, when it holds a file.
        let _ = fs::remove_dir(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_run_gets_its_own_directory_which_goes_away_when_empty() {
        let base = std::env::temp_dir().join(format!("t32ctl-out-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let a = OutDir::create(&base, "m").unwrap();
        let b = OutDir::create(&base, "m").unwrap();
        assert_ne!(a.path(), b.path());
        let name = a.path().file_name().unwrap().to_string_lossy().into_owned();
        assert!(name.len() == 6 && name.starts_with("m-"), "{name}");
        let out: OutName = "x.bin".parse().unwrap();
        let (path, _) = a.create_file(&out).unwrap();
        assert_eq!(a.create_file(&out).unwrap_err().kind, ErrorKind::StateDir, "never overwrites");
        let (kept, gone) = (a.path().to_path_buf(), b.path().to_path_buf());
        drop((a, b));
        assert!(path.is_file() && kept.is_dir());
        assert!(!gone.exists());
        fs::remove_dir_all(&base).unwrap();
    }
}
