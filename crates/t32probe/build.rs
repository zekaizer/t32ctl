// SPDX-License-Identifier: Apache-2.0
//! `T32PROBE_VERSION`: the Cargo version, plus the release tag the release workflow passes in
//! `T32CTL_RELEASE_TAG`, so a report names the exact build that produced it.

fn main() {
    println!("cargo:rerun-if-env-changed=T32CTL_RELEASE_TAG");
    let version = std::env::var("CARGO_PKG_VERSION").expect("cargo sets CARGO_PKG_VERSION");
    let full = match std::env::var("T32CTL_RELEASE_TAG") {
        Ok(tag) if !tag.is_empty() => format!("{version} ({tag})"),
        _ => version,
    };
    println!("cargo:rustc-env=T32PROBE_VERSION={full}");
}
