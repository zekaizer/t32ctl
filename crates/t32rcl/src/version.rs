// SPDX-License-Identifier: Apache-2.0
//! PowerView build numbers and the feature table.
//!
//! The build number is a hint and runtime detection is the truth. `support()` says `Present`
//! (documented as present at this build) or `Unknown` (try it). It never says "absent": the
//! manuals' `[build N - DVD x]` tags are not monotonic across DVDs, so only a failed call can say
//! that a feature is missing.

use std::fmt;

/// Spellings tried in order (pyrcl `_functions.py` lists both families).
pub const BUILD_FUNCTIONS: [&str; 2] = ["SOFTWARE.BUILD()", "VERSION.BUILD()"];
pub const BASE_FUNCTIONS: [&str; 2] = ["SOFTWARE.BUILD.BASE()", "VERSION.BUILD.BASE()"];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PowerViewBuild {
    pub build: u32,
    pub base: Option<u32>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Feature {
    /// `RCL.TCP.NrUsedCons()`, or `api.RCL.TCP.NrUsedCons()` from build 171257.
    NrUsedCons,
    /// `Data.LOAD.Elf <file>.gz /UNZIP`.
    ElfUnzip,
    /// `Data.CLEARVM <range>`, used to restore never-initialized `VM:`.
    ClearVm,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Since {
    /// Present in every supported PowerView (DVD 02/2024 and later).
    Minimum,
    /// Expected from this software build on.
    Build(u32),
    /// No threshold known: decide by trying it.
    Unknown,
}

#[derive(Clone, Copy, Debug)]
pub struct FeatureInfo {
    pub feature: Feature,
    /// Stable id for reports.
    pub id: &'static str,
    pub since: Since,
    pub source: &'static str,
}

/// Single list. Probe output and `t32ctl status` read it.
pub const FEATURES: &[FeatureInfo] = &[
    FeatureInfo {
        feature: Feature::NrUsedCons,
        id: "nrusedcons",
        since: Since::Minimum,
        source: "DVD 09/2022, older than the minimum DVD 02/2024",
    },
    FeatureInfo {
        feature: Feature::ElfUnzip,
        id: "elfunzip",
        since: Since::Build(172_803),
        source: "general_ref_d release history 21-Oct-2024; 172803 is the first DVD 02/2025 build tag",
    },
    FeatureInfo {
        feature: Feature::ClearVm,
        id: "clearvm",
        since: Since::Unknown,
        source: "general_ref_d Data.CLEARVM, no history entry",
    },
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Support {
    Present,
    Unknown,
}

impl Support {
    pub fn tag(self) -> &'static str {
        match self {
            Support::Present => "y",
            Support::Unknown => "?",
        }
    }
}

/// Where the build sits relative to the minimum supported PowerView (DVD 02/2024). DVD 02/2024
/// has no build tag in the manuals, so the bounds are the neighbouring DVDs' tags.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MinSupport {
    /// Below the largest 09/2023 tag: older than DVD 02/2024.
    TooOld,
    /// Between the 09/2023 and 09/2024 tags.
    Uncertain,
    /// At or above the smallest 09/2024 tag.
    Ok,
}

impl MinSupport {
    pub fn tag(self) -> &'static str {
        match self {
            MinSupport::TooOld => "too-old",
            MinSupport::Uncertain => "uncertain",
            MinSupport::Ok => "ok",
        }
    }
}

const MIN_TOO_OLD_BELOW: u32 = 160_740;
const MIN_OK_FROM: u32 = 166_507;

/// `[build N - DVD x]` tags from the manuals, ascending. Used for the `~DVD` hint only.
const ANCHORS: &[(u32, &str)] = &[
    (122_232, "09/2020"),
    (137_983, "02/2022"),
    (150_073, "09/2022"),
    (160_740, "09/2023"),
    (166_507, "09/2024"),
    (172_803, "02/2025"),
    (182_699, "09/2025"),
    (185_686, "02/2026"),
];

impl Feature {
    pub fn info(self) -> &'static FeatureInfo {
        FEATURES.iter().find(|i| i.feature == self).expect("every Feature has a FEATURES row")
    }
}

impl PowerViewBuild {
    pub fn support(&self, feature: Feature) -> Support {
        let present = match feature.info().since {
            Since::Minimum => self.min_support() != MinSupport::TooOld,
            Since::Build(b) => self.build >= b,
            Since::Unknown => false,
        };
        if present { Support::Present } else { Support::Unknown }
    }

    pub fn min_support(&self) -> MinSupport {
        if self.build < MIN_TOO_OLD_BELOW {
            MinSupport::TooOld
        } else if self.build >= MIN_OK_FROM {
            MinSupport::Ok
        } else {
            MinSupport::Uncertain
        }
    }

    /// Newest tag at or below the build, e.g. `"~DVD 02/2025+"`.
    pub fn release_hint(&self) -> Option<String> {
        ANCHORS.iter().rev().find(|(b, _)| self.build >= *b).map(|(_, dvd)| format!("~DVD {dvd}+"))
    }
}

impl fmt::Display for PowerViewBuild {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.build)?;
        if let Some(base) = self.base {
            write!(f, "/base {base}")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [Feature; 3] = [Feature::NrUsedCons, Feature::ElfUnzip, Feature::ClearVm];

    #[test]
    fn every_feature_has_exactly_one_row() {
        for f in ALL {
            assert_eq!(FEATURES.iter().filter(|i| i.feature == f).count(), 1, "{f:?}");
        }
        assert_eq!(FEATURES.len(), ALL.len());
    }

    #[test]
    fn build_number_never_says_absent() {
        let old = PowerViewBuild { build: 150_000, base: None };
        let new = PowerViewBuild { build: 180_000, base: Some(179_000) };
        assert_eq!(old.min_support(), MinSupport::TooOld);
        assert_eq!(PowerViewBuild { build: 163_000, base: None }.min_support(), MinSupport::Uncertain);
        assert_eq!(new.min_support(), MinSupport::Ok);
        assert_eq!(new.support(Feature::NrUsedCons), Support::Present);
        assert_eq!(new.support(Feature::ElfUnzip), Support::Present);
        assert_eq!(new.support(Feature::ClearVm), Support::Unknown);
        assert_eq!(old.support(Feature::NrUsedCons), Support::Unknown);
        assert_eq!(new.release_hint().as_deref(), Some("~DVD 02/2025+"));
        assert_eq!(PowerViewBuild { build: 1, base: None }.release_hint(), None);
    }
}
