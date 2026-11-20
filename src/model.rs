//! Supported OBSBOT models and what each one is known to support.
//!
//! The Tiny 2 and Tiny 3 generations speak the same UVC extension-unit vendor
//! protocol, so one driver serves both. What differs per model is the USB
//! product ID, the gimbal range (queried from the device at runtime, never
//! hardcoded) and which features have actually been confirmed on real hardware.
//!
//! Every feature on every model has one of three support levels:
//!
//! * [`Support::Verified`] — confirmed on real hardware by this project.
//! * [`Support::Expected`] — same protocol, should work, not yet confirmed.
//!   Commands are sent and their result is read back; if the camera does not
//!   confirm the change the user gets an error naming the model, never a
//!   silent no-op.
//! * [`Support::Unsupported`] — known not to exist on that model. Refused
//!   before anything is sent to the camera.

use crate::TrackMode;

/// Vendor ID shared by every OBSBOT camera (Remo Tech Co., Ltd.).
pub const VENDOR_ID: &str = "3564";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Support {
    Verified,
    Expected,
    Unsupported,
}

/// A user-facing capability, used to gate commands per model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Feature {
    Sleep,
    AutoSleepTimer,
    Tracking(TrackMode),
    TrackSpeed,
    Gimbal,
    Zoom,
    WhiteBalance,
    Hdr,
    Fov,
    Exposure,
    FaceAe,
    /// Reading and recalling the on-camera preset slots.
    PresetRecall,
    /// Writing (saving/deleting) the on-camera preset slots.
    PresetSave,
    /// The camera's stored power-on (boot) gimbal position.
    PowerOnPose,
}

impl Feature {
    pub fn label(self) -> String {
        match self {
            Feature::Sleep => "sleep/wake".into(),
            Feature::AutoSleepTimer => "firmware auto-sleep timer".into(),
            Feature::Tracking(TrackMode::Off) => "AI tracking".into(),
            Feature::Tracking(m) => format!("'{}' tracking", m.label()),
            Feature::TrackSpeed => "tracking speed".into(),
            Feature::Gimbal => "gimbal pan/tilt".into(),
            Feature::Zoom => "zoom".into(),
            Feature::WhiteBalance => "white balance".into(),
            Feature::Hdr => "HDR".into(),
            Feature::Fov => "field of view".into(),
            Feature::Exposure => "exposure".into(),
            Feature::FaceAe => "face auto-exposure".into(),
            Feature::PresetRecall => "on-camera preset recall".into(),
            Feature::PresetSave => "saving on-camera presets".into(),
            Feature::PowerOnPose => "power-on position".into(),
        }
    }

    /// Matching key for the model tables: all tracking modes share one entry
    /// unless a model lists a specific mode.
    fn matches(self, other: Feature) -> bool {
        match (self, other) {
            (Feature::Tracking(a), Feature::Tracking(b)) => a == b || b == TrackMode::Off,
            (a, b) => a == b,
        }
    }
}

impl Support {
    pub fn label(self) -> &'static str {
        match self {
            Support::Verified => "verified",
            Support::Expected => "untested",
            Support::Unsupported => "unsupported",
        }
    }
}

/// Every command-level feature, with the `tinyctl` command that exercises it,
/// for `tinyctl support`. Tracking modes are listed individually.
pub fn feature_table() -> Vec<(Feature, &'static str)> {
    let mut v = vec![
        (Feature::Sleep, "sleep / wake / toggle"),
        (Feature::AutoSleepTimer, "wake (firmware sleep timer)"),
    ];
    for (m, cmd) in [
        (TrackMode::Normal, "track on"),
        (TrackMode::UpperBody, "track upper"),
        (TrackMode::CloseUp, "track close"),
        (TrackMode::Headless, "track headless"),
        (TrackMode::LowerBody, "track lower"),
        (TrackMode::Group, "track group"),
        (TrackMode::Hand, "track hand"),
        (TrackMode::Whiteboard, "track whiteboard"),
        (TrackMode::Desk, "track desk"),
    ] {
        v.push((Feature::Tracking(m), cmd));
    }
    v.extend([
        (Feature::TrackSpeed, "track speed std|sport"),
        (Feature::Gimbal, "pan / tilt / recenter"),
        (Feature::Zoom, "zoom"),
        (Feature::WhiteBalance, "wb auto|temp|pin"),
        (Feature::Hdr, "hdr"),
        (Feature::Fov, "fov"),
        (Feature::Exposure, "exposure"),
        (Feature::FaceAe, "face-ae"),
        (Feature::PresetRecall, "preset list / recall"),
        (Feature::PresetSave, "preset save / delete"),
        (Feature::PowerOnPose, "preset power-on"),
    ]);
    v
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Family {
    Tiny2,
    Tiny3,
    /// An OBSBOT product ID this build does not know. Treated as protocol
    /// compatible, with every feature [`Support::Expected`].
    Unknown,
}

#[derive(Debug)]
pub struct Model {
    /// Short stable id used in JSON output (`tiny2`, `tiny3`, `tiny3-lite`).
    pub id: &'static str,
    pub name: &'static str,
    /// USB product ID (lowercase hex), or "" for the unknown fallback.
    pub pid: &'static str,
    pub family: Family,
    /// Features confirmed on real hardware. `Tracking(Off)` stands for every
    /// tracking mode not listed separately.
    verified: &'static [Feature],
    /// Features known to be absent on this model.
    unsupported: &'static [Feature],
    /// Status-block byte that reports tracking speed (0 std / 2 sport), where
    /// the model reports it at all. It moved between generations.
    pub track_speed_offset: Option<usize>,
}

impl Model {
    pub fn support(&self, f: Feature) -> Support {
        // A specific unsupported mode wins over a generic verified entry.
        if self.unsupported.iter().any(|&u| f.matches(u)) {
            Support::Unsupported
        } else if self.verified.iter().any(|&v| f.matches(v)) {
            Support::Verified
        } else {
            Support::Expected
        }
    }

    /// Ok unless the feature is known to be missing on this model.
    pub fn require(&self, f: Feature) -> crate::Result<()> {
        if self.support(f) == Support::Unsupported {
            return Err(crate::Error::Unsupported {
                feature: f.label(),
                model: self.name.into(),
            });
        }
        Ok(())
    }

    /// The error for a command the transport accepted but the camera did not
    /// apply. On an untested model this says so, since the likeliest cause is
    /// that the model lacks the feature.
    pub fn unconfirmed(&self, f: Feature) -> crate::Error {
        match self.support(f) {
            Support::Verified => crate::Error::Config(format!(
                "the {} did not confirm the {} change; saved intent will be retried by tiny-guard",
                self.name,
                f.label()
            )),
            _ => crate::Error::Unsupported {
                feature: format!(
                    "{} (sent, but the camera did not confirm it; untested on this model)",
                    f.label()
                ),
                model: self.name.into(),
            },
        }
    }

    /// True when this model has been confirmed by real-hardware testing for
    /// at least the core power + tracking path.
    pub fn is_verified(&self) -> bool {
        self.support(Feature::Sleep) == Support::Verified
    }
}

/// Features confirmed on the Tiny 3 Lite (fw 0510) — see PROTOCOL.md.
const TINY3_LITE_VERIFIED: &[Feature] = &[
    Feature::Sleep,
    Feature::AutoSleepTimer,
    Feature::Tracking(TrackMode::Off),
    Feature::Gimbal,
    Feature::Zoom,
    Feature::WhiteBalance,
    Feature::Hdr,
];

/// Framing sub-modes that the Tiny 3 generation accepts (and even echoes in
/// status) but does not act on: there they are Auto Zoom options, which only
/// OBSBOT Center can set. Confirmed visually on a Tiny 3; group, hand,
/// whiteboard and desk do work. See PROTOCOL.md.
const TINY3_FRAMING_MODES: [Feature; 4] = [
    Feature::Tracking(TrackMode::UpperBody),
    Feature::Tracking(TrackMode::CloseUp),
    Feature::Tracking(TrackMode::Headless),
    Feature::Tracking(TrackMode::LowerBody),
];

const TINY3_UNSUPPORTED: &[Feature] = &[
    TINY3_FRAMING_MODES[0],
    TINY3_FRAMING_MODES[1],
    TINY3_FRAMING_MODES[2],
    TINY3_FRAMING_MODES[3],
    // The speed frame is accepted but no status byte follows it and no
    // visible change was seen (also ignored per obsbot-tiny3-protocol).
    Feature::TrackSpeed,
];

/// The Tiny 3 Lite shares the Tiny 3 AI firmware; its framing modes were only
/// ever confirmed by status echo, which the Tiny 3 also gives while ignoring
/// them, so treat them the same way.
const TINY3_LITE_UNSUPPORTED: &[Feature] = &[
    TINY3_FRAMING_MODES[0],
    TINY3_FRAMING_MODES[1],
    TINY3_FRAMING_MODES[2],
    TINY3_FRAMING_MODES[3],
];

/// The Tiny 3 also recalls on-camera presets (settings applied by tinyctl,
/// tracking confirmed on hardware). Its power-on copy is not possible here:
/// it does not report preset positions.
const TINY3_VERIFIED: &[Feature] = &[
    Feature::Sleep,
    Feature::AutoSleepTimer,
    Feature::Tracking(TrackMode::Off),
    Feature::Gimbal,
    Feature::Zoom,
    Feature::WhiteBalance,
    Feature::Hdr,
    Feature::Fov,
    Feature::Exposure,
    Feature::FaceAe,
    Feature::PresetRecall,
];

/// The Tiny 2 also reports tracking speed (status byte 0x24 follows std/sport).
const TINY2_VERIFIED: &[Feature] = &[
    Feature::Sleep,
    Feature::AutoSleepTimer,
    Feature::Tracking(TrackMode::Off),
    Feature::Gimbal,
    Feature::Zoom,
    Feature::WhiteBalance,
    Feature::Hdr,
    Feature::Fov,
    Feature::Exposure,
    Feature::FaceAe,
    Feature::TrackSpeed,
    // On-camera preset recall (with its settings applied by tinyctl) and the
    // power-on state copy, confirmed by readback and a replug.
    Feature::PresetRecall,
    Feature::PowerOnPose,
];

pub static MODELS: &[Model] = &[
    Model {
        id: "tiny3-lite",
        name: "OBSBOT Tiny 3 Lite",
        pid: "ff04",
        family: Family::Tiny3,
        verified: TINY3_LITE_VERIFIED,
        unsupported: TINY3_LITE_UNSUPPORTED,
        // From the Tiny 2 lineage notes; never exercised on the Lite.
        track_speed_offset: Some(0x21),
    },
    Model {
        id: "tiny3",
        name: "OBSBOT Tiny 3",
        pid: "ff02",
        family: Family::Tiny3,
        verified: TINY3_VERIFIED,
        unsupported: TINY3_UNSUPPORTED,
        track_speed_offset: None,
    },
    Model {
        id: "tiny2",
        name: "OBSBOT Tiny 2",
        pid: "fef8",
        family: Family::Tiny2,
        verified: TINY2_VERIFIED,
        unsupported: &[],
        track_speed_offset: Some(0x24),
    },
];

pub static UNKNOWN: Model = Model {
    id: "unknown",
    name: "OBSBOT camera (unrecognized model)",
    pid: "",
    family: Family::Unknown,
    verified: &[],
    unsupported: &[],
    track_speed_offset: Some(0x21),
};

/// Look up a model by USB product ID (case-insensitive). Unknown OBSBOT PIDs
/// map to [`UNKNOWN`] so new models work out of the box, flagged as untested.
pub fn by_pid(pid: &str) -> &'static Model {
    let pid = pid.trim().to_ascii_lowercase();
    MODELS.iter().find(|m| m.pid == pid).unwrap_or(&UNKNOWN)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pids_resolve_to_models() {
        assert_eq!(by_pid("ff04").id, "tiny3-lite");
        assert_eq!(by_pid("FF02").id, "tiny3");
        assert_eq!(by_pid("fef8\n").id, "tiny2");
        assert_eq!(by_pid("abcd").id, "unknown");
    }

    #[test]
    fn support_levels() {
        let lite = by_pid("ff04");
        assert_eq!(lite.support(Feature::Sleep), Support::Verified);
        assert_eq!(
            lite.support(Feature::Tracking(TrackMode::Desk)),
            Support::Verified
        );
        assert_eq!(lite.support(Feature::Fov), Support::Expected);
        assert!(lite.is_verified());
        for pid in ["fef8", "ff02"] {
            let m = by_pid(pid);
            assert!(m.is_verified(), "{pid}");
            for f in [
                Feature::Sleep,
                Feature::Tracking(TrackMode::Hand),
                Feature::Gimbal,
                Feature::WhiteBalance,
                Feature::Hdr,
            ] {
                assert_eq!(m.support(f), Support::Verified, "{pid} {f:?}");
            }
            assert_eq!(m.support(Feature::AutoSleepTimer), Support::Verified);
            assert_eq!(m.support(Feature::Fov), Support::Verified);
        }
        assert!(!by_pid("abcd").is_verified());
    }

    #[test]
    fn support_table_for_tiny2_and_tiny3() {
        let not_verified = |pid: &str| -> Vec<(String, Support)> {
            let m = by_pid(pid);
            feature_table()
                .into_iter()
                .map(|(f, _)| (f.label(), m.support(f)))
                .filter(|(_, s)| *s != Support::Verified)
                .collect()
        };
        // On-camera presets and the power-on position are new and untested.
        let presets = [
            ("on-camera preset recall", Support::Expected),
            ("saving on-camera presets", Support::Expected),
            ("power-on position", Support::Expected),
        ]
        .map(|(f, s)| (f.to_string(), s));
        assert_eq!(not_verified("fef8"), [presets[1].clone()]);
        assert_eq!(
            not_verified("ff02"),
            [
                "'upper-body' tracking",
                "'close-up' tracking",
                "'headless' tracking",
                "'lower-body' tracking",
                "tracking speed"
            ]
            .map(|f| (f.to_string(), Support::Unsupported))
            .into_iter()
            .chain(presets[1..].iter().cloned())
            .collect::<Vec<_>>()
        );
        let t3 = by_pid("ff02");
        for m in [
            TrackMode::Normal,
            TrackMode::Group,
            TrackMode::Hand,
            TrackMode::Desk,
        ] {
            assert_eq!(t3.support(Feature::Tracking(m)), Support::Verified, "{m:?}");
        }
        assert_eq!(
            t3.require(Feature::Tracking(TrackMode::UpperBody))
                .unwrap_err()
                .to_string(),
            "'upper-body' tracking is not supported on the OBSBOT Tiny 3"
        );
        assert_eq!(
            by_pid("ff04").support(Feature::Tracking(TrackMode::CloseUp)),
            Support::Unsupported
        );
        assert_eq!(by_pid("fef8").track_speed_offset, Some(0x24));
        assert_eq!(by_pid("ff02").track_speed_offset, None);
    }

    #[test]
    fn unsupported_is_refused_with_model_name() {
        static M: Model = Model {
            id: "test",
            name: "Test Cam",
            pid: "0000",
            family: Family::Unknown,
            verified: &[Feature::Tracking(TrackMode::Off)],
            unsupported: &[Feature::Tracking(TrackMode::Whiteboard), Feature::Fov],
            track_speed_offset: None,
        };
        assert_eq!(
            M.support(Feature::Tracking(TrackMode::Whiteboard)),
            Support::Unsupported
        );
        assert_eq!(
            M.support(Feature::Tracking(TrackMode::Normal)),
            Support::Verified
        );
        let e = M.require(Feature::Fov).unwrap_err().to_string();
        assert_eq!(e, "field of view is not supported on the Test Cam");
        assert!(M.require(Feature::Hdr).is_ok());
    }
}
