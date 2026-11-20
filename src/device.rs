//! High-level device API: vendor commands (sleep/wake/tracking/gimbal) over the
//! framed V3 protocol and raw TLVs, plus the standard-UVC controls.
//!
//! Every method here that talks to the camera requires an open [`VideoFd`], and
//! **an open fd blocks the camera from sleeping** (it holds USB autosuspend
//! off). Hold a [`Device`] only while you intend the camera in use.

use crate::controls;
use crate::error::{Error, Result};
use crate::frame::{self, FLAG_GET, FLAG_SET};
use crate::ioctl::VideoFd;
use crate::model::{Feature, Model, Support};
use std::sync::atomic::{AtomicU16, Ordering};
use std::time::Duration;

// UVC extension unit, shared by Tiny 2 and Tiny 3 (bUnitID 2, GUID 9A1E7291-...).
const XU_UNIT: u8 = 0x02;
const SEL_CMD: u8 = 0x02; // framed V3 command channel + reply mailbox
const SEL_STATUS: u8 = 0x06; // 60-byte status block + raw-TLV write target

// Subsystem receiver ids.
const RCV_CAMERA: u8 = 0x02;
const RCV_GIMBAL: u8 = 0x03;
const RCV_AI: u8 = 0x04;
const RCV_UPGRADE: u8 = 0x0D;

// Wire command ids (little-endian on the wire).
const CMD_DEV_STATUS: u16 = 0xA0C2; // sleep/wake
const CMD_RECENTER: u16 = 0x00C3;
const CMD_GIMBAL_MOVE: u16 = 0x6444; // move-to-angle, 3x f32 motor degrees
const CMD_TRACK_SPEED: u16 = 0x0CC4; // payload [0]=standard [2]=sport
const CMD_GET_SN: u16 = 0x18C8;
const CMD_GET_UUID: u16 = 0x1808;
// On-camera presets and boot pose (AI receiver). Decoded by lxman/obsbot-mcp
// on the Tiny 2; see PROTOCOL.md "On-camera presets".
const CMD_PRESET_ADD: u16 = 0x3944;
const CMD_PRESET_DELETE: u16 = 0x3984;
const CMD_PRESET_TRIG: u16 = 0x39C4;
const CMD_PRESET_UPDATE: u16 = 0x3E04;
const CMD_BOOT_POS_SET: u16 = 0x3844;
const CMD_BOOT_POS_GET: u16 = 0x3884;
const CMD_BOOT_POS_RST: u16 = 0x38C4;
const CMD_PRESET_ACTIONS_GET: u16 = 0x3DC4;
const CMD_BOOT_ACTIONS_GET: u16 = 0x3E84;
const CMD_BOOT_ACTIONS_SET: u16 = 0x3E44;
// Flat (non-framed) XU selectors that expose the preset store.
const SEL_PRESET_LIST: u8 = 0x0C;
const SEL_PRESET_ENTRY: u8 = 0x0D;

/// Writes to the camera's preset store and power-on record are DISABLED.
/// A Tiny 2 began dropping contrast, saturation and sharpness to zero in
/// Desk mode (on Windows too) while these writes were being tested, so they
/// stay off until the cause is understood. Reading and recall still work.
const PRESET_WRITES_DISABLED: bool = true;

fn preset_writes_allowed() -> Result<()> {
    if PRESET_WRITES_DISABLED {
        return Err(Error::Config(
            "writing on-camera presets and the power-on position is disabled while a camera \
             side effect is investigated; use OBSBOT Center to save presets for now"
                .into(),
        ));
    }
    Ok(())
}

/// Number of on-camera preset slots (shown to users as 1..=3).
pub const PRESET_SLOTS: u8 = 3;

// Raw-TLV tags on selector 6.
const TLV_HDR: u8 = 0x01;
const TLV_FACE_AE: u8 = 0x03;
const TLV_FOV: u8 = 0x04;
const TLV_AI_TRACK: u8 = 0x16;
const TLV_AUTO_SLEEP: u8 = 0x0b;

// Status-block byte offsets (decoded from Tiny4Linux, confirmed reacting live).
const ST_SLEEP: usize = 0x02;
const ST_HDR: usize = 0x06;
const ST_AUTO_SLEEP: usize = 0x0a;
const ST_AI_CATEGORY: usize = 0x18;
const ST_AI_SUBMODE: usize = 0x1c;
const ST_FACE_AE: usize = 0x07; // 0 global / 1 face (verified Tiny 2 + Tiny 3)
const ST_FOV: usize = 0x11; // 0 wide / 1 medium / 2 narrow (verified Tiny 2 + Tiny 3)
                            // Tracking speed's offset differs per model: see Model::track_speed_offset.

/// AI tracking mode. The wire encoding is `[0x16, 0x02, category, submode]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrackMode {
    Off,
    /// Human tracking with a framing submode.
    Normal,
    UpperBody,
    CloseUp,
    Headless,
    LowerBody,
    Group,
    Hand,
    Whiteboard,
    Desk,
}

impl TrackMode {
    fn category_submode(self) -> (u8, u8) {
        match self {
            TrackMode::Off => (0, 0),
            TrackMode::Normal => (2, 0),
            TrackMode::UpperBody => (2, 1),
            TrackMode::CloseUp => (2, 2),
            TrackMode::Headless => (2, 3),
            TrackMode::LowerBody => (2, 4),
            TrackMode::Group => (1, 0),
            TrackMode::Hand => (3, 0),
            TrackMode::Whiteboard => (4, 0),
            TrackMode::Desk => (5, 0),
        }
    }

    // Deliberately not `std::str::FromStr`: an unknown mode is a plain
    // `None` here, not an error type the caller has to name.
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Option<TrackMode> {
        Some(match s {
            "off" => TrackMode::Off,
            "normal" | "on" => TrackMode::Normal,
            "upper" | "upper-body" | "upperbody" => TrackMode::UpperBody,
            "close" | "closeup" | "close-up" => TrackMode::CloseUp,
            "headless" => TrackMode::Headless,
            "lower" | "lower-body" | "lowerbody" => TrackMode::LowerBody,
            "group" => TrackMode::Group,
            "hand" => TrackMode::Hand,
            "whiteboard" => TrackMode::Whiteboard,
            "desk" => TrackMode::Desk,
            _ => return None,
        })
    }

    fn from_status(category: u8, submode: u8) -> TrackMode {
        match (category, submode) {
            (0, _) => TrackMode::Off,
            (2, 0) => TrackMode::Normal,
            (2, 1) => TrackMode::UpperBody,
            (2, 2) => TrackMode::CloseUp,
            (2, 3) => TrackMode::Headless,
            (2, 4) => TrackMode::LowerBody,
            (1, _) => TrackMode::Group,
            // The Tiny 2 reports hand tracking as category 6 in its status
            // block even though the SET uses 3 (observed by Tiny4Linux).
            (3, _) | (6, _) => TrackMode::Hand,
            (4, _) => TrackMode::Whiteboard,
            (5, _) => TrackMode::Desk,
            _ => TrackMode::Normal,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            TrackMode::Off => "off",
            TrackMode::Normal => "normal",
            TrackMode::UpperBody => "upper-body",
            TrackMode::CloseUp => "close-up",
            TrackMode::Headless => "headless",
            TrackMode::LowerBody => "lower-body",
            TrackMode::Group => "group",
            TrackMode::Hand => "hand",
            TrackMode::Whiteboard => "whiteboard",
            TrackMode::Desk => "desk",
        }
    }
}

/// Decoded snapshot of the camera's vendor status block plus key UVC controls.
#[derive(Debug, Clone)]
pub struct Status {
    pub asleep: bool,
    /// Firmware idle timer in seconds; zero disables vendor auto-sleep.
    pub auto_sleep_seconds: u16,
    pub hdr: bool,
    pub tracking: TrackMode,
    /// Some(true) = sport. None when this model does not report it.
    pub tracking_sport: Option<bool>,
    /// Field-of-view level: 0 wide, 1 medium, 2 narrow.
    pub fov: u8,
    /// Face-priority auto-exposure.
    pub face_ae: bool,
    pub auto_wb: bool,
    pub wb_temp: i32,
    pub pan_deg: f64,
    pub tilt_deg: f64,
    pub zoom: i32,
    pub auto_exposure: bool,
    /// The raw 60-byte vendor status block, for `status --raw` and `probe`.
    pub raw: [u8; 60],
}

/// A gimbal pose as the camera stores it in a preset or its boot pose.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Pose {
    pub pan_deg: f64,
    pub tilt_deg: f64,
    /// Zoom ratio: 1.0 = no zoom (UVC zoom 0), 2.0 = UVC zoom 100.
    pub zoom_ratio: f64,
}

impl Pose {
    /// Same place, within the camera's ~1° storage quantization.
    pub fn close_to(&self, o: &Pose) -> bool {
        (self.pan_deg - o.pan_deg).abs() <= 1.5 && (self.tilt_deg - o.tilt_deg).abs() <= 1.5
    }
}

/// One on-camera preset slot.
#[derive(Debug, Clone, PartialEq)]
pub struct PresetSlot {
    /// 1..=3
    pub slot: u8,
    /// None when the slot is empty.
    pub pose: Option<Pose>,
    pub name: Option<String>,
}

/// A preset's "Additional settings" from OBSBOT Center, as the camera stores
/// them (40-byte actions record, `AI_GET_PRESETS_ACTIONS` 0x3DC4). The camera
/// applies the power-on copy itself at boot, but NOT a preset's on recall —
/// tinyctl applies those. `None` = "not set" in the record (0xff / 0x80).
/// Decoded on a Tiny 2 against known OBSBOT Center settings; see PROTOCOL.md.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PresetActions {
    /// Some(Off) = tracking enabled with no mode selected (0xff).
    pub tracking: Option<TrackMode>,
    /// The raw mode byte, for modes not mapped yet.
    pub tracking_raw: u8,
    /// Center's "Motion" speed = sport.
    pub sport: Option<bool>,
    pub auto_focus: Option<bool>,
    /// Focus target: true = face, false = global.
    pub face_focus: Option<bool>,
    pub auto_exposure: Option<bool>,
    /// Exposure target: true = face, false = global.
    pub face_ae: Option<bool>,
    /// Exposure compensation in thirds of a stop (index 9 = 0).
    pub exposure_comp_thirds: Option<i8>,
    pub auto_wb: Option<bool>,
    pub wb_temp: Option<i32>,
    pub blur: Option<bool>,
    pub blur_level: Option<u8>,
    pub contrast: Option<u8>,
    pub saturation: Option<u8>,
    pub sharpness: Option<u8>,
    pub hue: Option<u8>,
    pub raw: [u8; 40],
}

/// Tracking-mode byte of an actions record. Observed in OBSBOT Center
/// presets: ff none, 00 normal, 01 upper, 05 desk, 06 whiteboard, 09 group.
/// 02-04 (close-up, headless, lower) follow the framing order and are
/// inferred, not observed.
fn actions_track_mode(b: u8) -> Option<TrackMode> {
    Some(match b {
        0xff => TrackMode::Off,
        0 => TrackMode::Normal,
        1 => TrackMode::UpperBody,
        2 => TrackMode::CloseUp,
        3 => TrackMode::Headless,
        4 => TrackMode::LowerBody,
        5 => TrackMode::Desk,
        6 => TrackMode::Whiteboard,
        9 => TrackMode::Group,
        _ => return None,
    })
}

impl PresetActions {
    pub fn decode(r: &[u8]) -> Option<PresetActions> {
        let raw: [u8; 40] = r.get(..40)?.try_into().ok()?;
        let flag = |b: u8| match b {
            0 => Some(false),
            1 => Some(true),
            _ => None,
        };
        let pct = |b: u8| (b <= 100).then_some(b);
        let temp = u16::from_le_bytes([raw[14], raw[15]]);
        Some(PresetActions {
            tracking: actions_track_mode(raw[0]),
            tracking_raw: raw[0],
            sport: flag(raw[1]),
            auto_focus: flag(raw[2]),
            face_focus: flag(raw[3]),
            auto_exposure: flag(raw[6]),
            face_ae: flag(raw[7]),
            exposure_comp_thirds: (raw[8] != 0x80 && raw[8] <= 18).then(|| raw[8] as i8 - 9),
            auto_wb: match raw[13] {
                0 => Some(true),
                0xff => None,
                _ => Some(false),
            },
            wb_temp: (raw[13] != 0xff
                && (controls::WB_TEMP_MIN..=controls::WB_TEMP_MAX).contains(&(temp as i32)))
            .then_some(temp as i32),
            blur: flag(raw[17]),
            blur_level: (raw[19] != 0xff).then_some(raw[19]),
            contrast: pct(raw[20]),
            saturation: pct(raw[21]),
            sharpness: pct(raw[22]),
            hue: pct(raw[23]),
            raw,
        })
    }
}

/// Decode a flat selector-13 preset entry. Layout (60 bytes): status u8
/// (0 = valid entry), slot index u8 (0-based), pad u16, pitch i16 and yaw i16
/// in hundredths of a degree, zoom u16 in hundredths of a ratio, then a
/// NUL-terminated base64 name.
fn decode_preset_entry(b: &[u8; 60]) -> Option<(u8, Pose, Option<String>)> {
    // An all-zero header is what a failed read looks like; a real preset
    // always has zoom >= 1.0, so never mistake it for an occupied slot.
    if b[0] != 0 || b[..10].iter().all(|&x| x == 0) || b[1] > PRESET_SLOTS - 1 {
        return None;
    }
    let i16_at = |o: usize| i16::from_le_bytes([b[o], b[o + 1]]) as f64 / 100.0;
    let pose = Pose {
        tilt_deg: i16_at(4),
        pan_deg: i16_at(6),
        zoom_ratio: u16::from_le_bytes([b[8], b[9]]) as f64 / 100.0,
    };
    let end = b[10..].iter().position(|&x| x == 0).map_or(60, |p| 10 + p);
    let name = base64_decode(&b[10..end])
        .map(|v| String::from_utf8_lossy(&v).trim().to_string())
        .filter(|n| !n.is_empty());
    Some((b[1] + 1, pose, name))
}

fn base64_decode(input: &[u8]) -> Option<Vec<u8>> {
    let val = |c: u8| -> Option<u32> {
        Some(match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        } as u32)
    };
    let mut out = Vec::new();
    let (mut acc, mut bits) = (0u32, 0);
    for &c in input.iter().filter(|&&c| c != b'=') {
        acc = (acc << 6) | val(c)?;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Some(out)
}

fn pose_payload(prefix: u32, p: &Pose, trailer: f32) -> Vec<u8> {
    let mut v = prefix.to_le_bytes().to_vec();
    for f in [
        p.pan_deg as f32,
        p.tilt_deg as f32,
        0.0,
        p.zoom_ratio as f32,
        trailer,
    ] {
        v.extend_from_slice(&f.to_le_bytes());
    }
    v
}

fn check_slot(slot: u8) -> Result<u32> {
    if !(1..=PRESET_SLOTS).contains(&slot) {
        return Err(Error::OutOfRange {
            what: "preset slot",
            min: 1,
            max: PRESET_SLOTS as i64,
            got: slot as i64,
        });
    }
    Ok(slot as u32 - 1)
}

/// An open camera (control fd; reads do not wake a sleeping camera).
pub struct Device {
    fd: VideoFd,
    path: String,
    model: &'static Model,
}

static SEQ: AtomicU16 = AtomicU16::new(0);

fn next_seq() -> u16 {
    // Seed once from the pid so concurrent CLI invocations rarely collide. Use
    // a CAS so a racing thread can't roll the counter back to the seed after
    // another thread has already advanced it.
    if SEQ.load(Ordering::Relaxed) == 0 {
        let seed = (std::process::id() as u16) | 0x0100;
        let _ = SEQ.compare_exchange(0, seed, Ordering::Relaxed, Ordering::Relaxed);
    }
    let v = SEQ.fetch_add(1, Ordering::Relaxed).wrapping_add(1);
    if v == 0 {
        1
    } else {
        v
    }
}

impl Device {
    /// Open a discovered camera. Holds the fd (blocks sleep).
    pub fn open_camera(cam: &crate::discover::Camera) -> Result<Device> {
        Ok(Device {
            fd: VideoFd::open(&cam.node)?,
            path: cam.node.clone(),
            model: cam.model,
        })
    }

    /// Open a specific node path. Holds the fd (blocks sleep). The model is
    /// looked up from sysfs; a non-OBSBOT node is refused.
    pub fn open_path(path: &str) -> Result<Device> {
        let cam = crate::discover::camera_for_path(path)?;
        Ok(Device {
            fd: VideoFd::open(path)?,
            path: path.to_string(),
            model: cam.model,
        })
    }

    pub fn path(&self) -> &str {
        &self.path
    }

    pub fn model(&self) -> &'static Model {
        self.model
    }

    pub fn fd(&self) -> &VideoFd {
        &self.fd
    }

    // --- framed V3 transport ---

    /// Send a framed SET (fire-and-forget: no mailbox reply is expected).
    fn send_set(&self, receiver: u8, cmd: u16, payload: &[u8]) -> Result<()> {
        let seq = next_seq();
        let f = frame::build(FLAG_SET, seq, receiver, cmd, payload);
        self.fd.xu_set(XU_UNIT, SEL_CMD, &f)
    }

    /// Send a framed GET and poll the mailbox for a reply whose seq AND cmd
    /// match. The mailbox retains the previous reply, so matching both is
    /// required; latency exceeds a fixed sleep, so we poll.
    fn transact_get(&self, receiver: u8, cmd: u16, what: &'static str) -> Result<frame::Reply> {
        self.transact_get_with(receiver, cmd, &[], what)
    }

    /// GET with a request payload (flags `0x21`), e.g. a slot index — the
    /// form parameterized preset GETs require. Empty payload = header-only.
    fn transact_get_with(
        &self,
        receiver: u8,
        cmd: u16,
        request: &[u8],
        what: &'static str,
    ) -> Result<frame::Reply> {
        let seq = next_seq();
        let flags = if request.is_empty() {
            FLAG_GET
        } else {
            FLAG_GET | 0x20
        };
        let f = frame::build(flags, seq, receiver, cmd, request);
        self.fd.xu_set(XU_UNIT, SEL_CMD, &f)?;
        for _ in 0..12 {
            std::thread::sleep(Duration::from_millis(50));
            let raw = self.fd.xu_get(XU_UNIT, SEL_CMD)?;
            if let Some(r) = frame::parse(&raw) {
                // Match seq AND cmd (the mailbox retains the previous reply),
                // and require it to actually be a device reply — a real reply
                // swaps sender/receiver, so sender is no longer the host. This
                // rejects an echoed request frame sitting in the mailbox.
                if r.seq == seq && r.cmd == cmd && r.sender != frame::SENDER_HOST {
                    if let Some(code) = r.error {
                        return Err(Error::Config(format!(
                            "the {} rejected the {what} request (firmware error {code:#06x})",
                            self.model.name
                        )));
                    }
                    return Ok(r);
                }
            }
        }
        Err(Error::NoReply(what))
    }

    // --- power ---

    /// Vendor sleep: LED off, gimbal parks. Verified on Tiny 3 Lite.
    pub fn sleep(&self) -> Result<()> {
        self.model.require(Feature::Sleep)?;
        self.set_power(true)
    }

    /// Vendor wake: LED on, gimbal lifts back up. Verified on Tiny 3 Lite.
    pub fn wake(&self) -> Result<()> {
        self.model.require(Feature::Sleep)?;
        self.set_power(false)
    }

    /// Disable the firmware idle timer, independently of USB autosuspend.
    /// An open control fd and active microphone do NOT inhibit this timer.
    /// Raw selector-6 TLV 0x0b carries a little-endian u16 seconds value.
    /// Readback at status bytes 10..12 is verified on Tiny 3 Lite.
    pub fn disable_auto_sleep(&self) -> Result<()> {
        self.model.require(Feature::AutoSleepTimer)?;
        if decode_auto_sleep(&self.fd.xu_get(XU_UNIT, SEL_STATUS)?) == 0 {
            return Ok(());
        }
        self.fd
            .xu_set(XU_UNIT, SEL_STATUS, &[TLV_AUTO_SLEEP, 2, 0, 0])?;
        for _ in 0..10 {
            std::thread::sleep(Duration::from_millis(50));
            if decode_auto_sleep(&self.fd.xu_get(XU_UNIT, SEL_STATUS)?) == 0 {
                return Ok(());
            }
        }
        Err(self.model.unconfirmed(Feature::AutoSleepTimer))
    }

    /// Disable the firmware timer where possible, then wake. On a model where
    /// the timer is not verified, failing to disable it is reported as a
    /// warning (returned) rather than blocking the wake itself.
    pub fn wake_managed(&self) -> Result<Option<Error>> {
        let warning = match self.disable_auto_sleep() {
            Ok(()) => None,
            Err(e) if self.model.support(Feature::AutoSleepTimer) != Support::Verified => Some(e),
            Err(e) => return Err(e),
        };
        self.wake()?;
        Ok(warning)
    }

    fn set_power(&self, asleep: bool) -> Result<()> {
        self.send_set(RCV_CAMERA, CMD_DEV_STATUS, &[asleep as u8, 0, 0, 0])?;
        // Gimbal/firmware transition takes longer than a control transfer.
        // Keep the descriptor open while waiting so USB cannot suspend midway.
        for _ in 0..30 {
            std::thread::sleep(Duration::from_millis(100));
            if (self.fd.xu_get(XU_UNIT, SEL_STATUS)?[ST_SLEEP] != 0) == asleep {
                return Ok(());
            }
        }
        Err(self.model.unconfirmed(Feature::Sleep))
    }

    // --- identity ---

    /// 14-character device serial, via the Upgrade subsystem (safe GET).
    pub fn serial(&self) -> Result<String> {
        let r = self.transact_get(RCV_UPGRADE, CMD_GET_SN, "serial number")?;
        Ok(String::from_utf8_lossy(&r.payload)
            .trim_end_matches('\0')
            .to_string())
    }

    /// 24-byte device UUID as hex.
    pub fn uuid_hex(&self) -> Result<String> {
        let r = self.transact_get(RCV_UPGRADE, CMD_GET_UUID, "uuid")?;
        Ok(r.payload.iter().map(|b| format!("{b:02x}")).collect())
    }

    // --- on-camera presets (slots 1..=3) and boot pose ---

    /// Read all three on-camera preset slots (the ones OBSBOT Center shows).
    /// Flat selector 12 lists occupied slots; echoing it back resets the
    /// selector-13 cursor, which then yields one entry per read.
    pub fn presets(&self) -> Result<Vec<PresetSlot>> {
        self.model.require(Feature::PresetRecall)?;
        let list = self.fd.xu_get(XU_UNIT, SEL_PRESET_LIST)?;
        let count = list[0];
        // A dead/asleep read returns all zeros; a genuine empty store does not.
        if count > PRESET_SLOTS || list.iter().all(|&b| b == 0) {
            return Err(Error::NoReply("on-camera preset list"));
        }
        // Echo exactly what the camera returned — the only write to selector
        // 12 known to be safe.
        self.fd.xu_set(XU_UNIT, SEL_PRESET_LIST, &list)?;
        let mut slots: Vec<PresetSlot> = (1..=PRESET_SLOTS)
            .map(|slot| PresetSlot {
                slot,
                pose: None,
                name: None,
            })
            .collect();
        for _ in 0..count {
            let entry = self.fd.xu_get(XU_UNIT, SEL_PRESET_ENTRY)?;
            match decode_preset_entry(&entry) {
                Some((slot, pose, name)) => {
                    let s = &mut slots[slot as usize - 1];
                    s.pose = Some(pose);
                    s.name = name;
                }
                None => break,
            }
        }
        Ok(slots)
    }

    fn preset(&self, slot: u8) -> Result<PresetSlot> {
        check_slot(slot)?;
        Ok(self.presets()?.swap_remove(slot as usize - 1))
    }

    /// The gimbal's current pose: pan/tilt from the UVC controls, zoom mapped
    /// from UVC zoom (min..max) onto the camera's 1.0..2.0 ratio.
    pub fn current_pose(&self) -> Result<Pose> {
        let pan = self.fd.get_ctrl(controls::CID_PAN_ABSOLUTE)?;
        let tilt = self.fd.get_ctrl(controls::CID_TILT_ABSOLUTE)?;
        let zoom = self.fd.get_ctrl(controls::CID_ZOOM_ABSOLUTE)?;
        let (zmin, zmax) = match self.fd.query_ctrl(controls::CID_ZOOM_ABSOLUTE) {
            Ok(r) if r.max > r.min => (r.min, r.max),
            _ => (0, 100),
        };
        Ok(Pose {
            pan_deg: pan as f64 / controls::ASEC_PER_DEG,
            tilt_deg: tilt as f64 / controls::ASEC_PER_DEG,
            zoom_ratio: 1.0 + (zoom - zmin) as f64 / (zmax - zmin) as f64,
        })
    }

    /// Move to on-camera preset `slot`. The vendor recall moves the gimbal
    /// but leaves uvcvideo's cached pan/tilt readback stale, so the slot's
    /// pan/tilt is also written through UVC (same target, keeps `status`
    /// honest).
    pub fn preset_recall(&self, slot: u8) -> Result<Pose> {
        let idx = check_slot(slot)?;
        // A sleeping camera acknowledges moves and ignores them.
        if self.fd.xu_get(XU_UNIT, SEL_STATUS)?[ST_SLEEP] != 0 {
            return Err(Error::Config(
                "the camera is asleep; wake it before recalling a preset".into(),
            ));
        }
        let p = self.preset(slot)?;
        let pose = p.pose.ok_or_else(|| {
            Error::Config(format!(
                "preset {slot} is empty on this camera (save it with `preset save {slot}`)"
            ))
        })?;
        let mut payload = idx.to_le_bytes().to_vec();
        for _ in 0..4 {
            payload.extend_from_slice(&1.0f32.to_le_bytes());
        }
        self.send_set(RCV_AI, CMD_PRESET_TRIG, &payload)?;
        let clamp = |cid, deg: f64| {
            let (lo, hi) = self.gimbal_range_deg(cid);
            ((deg.clamp(lo, hi)) * controls::ASEC_PER_DEG).round() as i32
        };
        self.fd.set_ctrl(
            controls::CID_PAN_ABSOLUTE,
            clamp(controls::CID_PAN_ABSOLUTE, pose.pan_deg),
        )?;
        self.fd.set_ctrl(
            controls::CID_TILT_ABSOLUTE,
            clamp(controls::CID_TILT_ABSOLUTE, pose.tilt_deg),
        )?;
        Ok(pose)
    }

    /// Store the current pose in on-camera slot `slot` (create or overwrite),
    /// then read the slot back to confirm the camera kept it.
    pub fn preset_save(&self, slot: u8) -> Result<Pose> {
        preset_writes_allowed()?;
        self.model.require(Feature::PresetSave)?;
        let idx = check_slot(slot)?;
        let occupied = self.preset(slot)?.pose.is_some();
        let pose = self.current_pose()?;
        let cmd = if occupied {
            CMD_PRESET_UPDATE
        } else {
            CMD_PRESET_ADD
        };
        // Trailing -1000.0 is the sentinel ADD/UPDATE require.
        self.send_set(RCV_AI, cmd, &pose_payload(idx, &pose, -1000.0))?;
        for _ in 0..10 {
            std::thread::sleep(Duration::from_millis(200));
            if self
                .preset(slot)?
                .pose
                .is_some_and(|stored| stored.close_to(&pose))
            {
                return Ok(pose);
            }
        }
        Err(self.model.unconfirmed(Feature::PresetSave))
    }

    /// Empty on-camera slot `slot`.
    pub fn preset_delete(&self, slot: u8) -> Result<()> {
        preset_writes_allowed()?;
        self.model.require(Feature::PresetSave)?;
        let idx = check_slot(slot)?;
        if self.preset(slot)?.pose.is_none() {
            return Ok(());
        }
        self.send_set(RCV_AI, CMD_PRESET_DELETE, &idx.to_le_bytes())?;
        for _ in 0..10 {
            std::thread::sleep(Duration::from_millis(200));
            if self.preset(slot)?.pose.is_none() {
                return Ok(());
            }
        }
        Err(self.model.unconfirmed(Feature::PresetSave))
    }

    /// A preset slot's stored "Additional settings" (read-only GET with the
    /// slot index).
    pub fn preset_actions(&self, slot: u8) -> Result<PresetActions> {
        self.model.require(Feature::PresetRecall)?;
        let idx = check_slot(slot)?;
        let r = self.transact_get_with(
            RCV_AI,
            CMD_PRESET_ACTIONS_GET,
            &idx.to_le_bytes(),
            "preset settings",
        )?;
        PresetActions::decode(&r.payload).ok_or(Error::NoReply("preset settings"))
    }

    /// The power-on settings (a copy taken when OBSBOT Center sets a preset
    /// "As Initial State"; the camera applies it at boot).
    pub fn boot_actions(&self) -> Result<PresetActions> {
        self.model.require(Feature::PowerOnPose)?;
        let r = self.transact_get(RCV_AI, CMD_BOOT_ACTIONS_GET, "power-on settings")?;
        PresetActions::decode(&r.payload).ok_or(Error::NoReply("power-on settings"))
    }

    /// Which slot the power-on position was copied from (first field of the
    /// boot-pose record, 0-based), if it holds a slot index.
    pub fn boot_slot(&self) -> Result<Option<u8>> {
        self.model.require(Feature::PowerOnPose)?;
        let r = self.transact_get(RCV_AI, CMD_BOOT_POS_GET, "power-on position")?;
        let idx = r
            .payload
            .get(..4)
            .map(|b| u32::from_le_bytes(b.try_into().unwrap()));
        Ok(idx
            .filter(|&i| i < PRESET_SLOTS as u32)
            .map(|i| i as u8 + 1))
    }

    /// The pose the camera moves to when it powers on.
    pub fn boot_pose(&self) -> Result<Pose> {
        self.model.require(Feature::PowerOnPose)?;
        let r = self.transact_get(RCV_AI, CMD_BOOT_POS_GET, "power-on position")?;
        // Six f32 LE: [0, yaw, pitch, roll, zoom, 0].
        if r.payload.len() < 20 {
            return Err(Error::NoReply("power-on position"));
        }
        let f =
            |i: usize| f32::from_le_bytes(r.payload[i * 4..i * 4 + 4].try_into().unwrap()) as f64;
        Ok(Pose {
            pan_deg: f(1),
            tilt_deg: f(2),
            zoom_ratio: f(4),
        })
    }

    /// Make on-camera preset `slot` the power-on state, as OBSBOT Center's
    /// "As Initial State" does: the power-on record becomes a copy of the
    /// slot's pose (with the slot index first) and of its 40-byte settings
    /// record, byte for byte. Both are read back and must match exactly;
    /// otherwise this fails and OBSBOT Center can set it instead.
    ///
    /// Only record shapes read from the camera are written: the 24-byte boot
    /// pose (`AI_SET_GIM_BOOT_POS`, confirmed by replug upstream) and the
    /// unchanged 40-byte actions record (`AI_SET_BOOT_PRESETS_ACTIONS`, the
    /// command Center sends).
    pub fn set_power_on_preset(&self, slot: u8) -> Result<(Pose, PresetActions)> {
        self.model.require(Feature::PowerOnPose)?;
        let idx = check_slot(slot)?;
        let pose = self.preset(slot)?.pose.ok_or_else(|| {
            Error::Config(format!(
                "preset {slot} is empty; save it in OBSBOT Center first"
            ))
        })?;
        let actions = self.preset_actions(slot)?;
        self.send_set(RCV_AI, CMD_BOOT_POS_SET, &pose_payload(idx, &pose, 0.0))?;
        std::thread::sleep(Duration::from_millis(200));
        self.send_set(RCV_AI, CMD_BOOT_ACTIONS_SET, &actions.raw)?;
        for _ in 0..10 {
            std::thread::sleep(Duration::from_millis(200));
            if self.boot_slot()? == Some(slot)
                && self.boot_pose()?.close_to(&pose)
                && self.boot_actions()?.raw == actions.raw
            {
                return Ok((pose, actions));
            }
        }
        Err(Error::Config(format!(
            "the {} did not confirm the new power-on state; check `preset list` and set it \
             in OBSBOT Center if it is wrong",
            self.model.name
        )))
    }

    /// Read-only dump of the records that may hold OBSBOT Center's "As
    /// Initial State" (power-on) settings, for decoding where the tracking
    /// mode is stored. Each is a header-only GET of a named firmware command;
    /// nothing is written.
    pub fn boot_records(&self) -> Vec<(&'static str, Result<Vec<u8>>)> {
        [
            ("AI_GET_GIM_BOOT_POS 0x3884", RCV_AI, CMD_BOOT_POS_GET),
            ("AI_GET_BOOT_PRESETS_ACTIONS 0x3E84", RCV_AI, 0x3E84),
            ("AI_GET_INITIAL_POSITION_OPA 0x1C44", RCV_AI, 0x1C44),
            ("CAM_GET_BOOT_STATE 0x0382", RCV_CAMERA, 0x0382),
        ]
        .into_iter()
        .map(|(name, rcv, cmd)| (name, self.transact_get(rcv, cmd, name).map(|r| r.payload)))
        .chain((0..PRESET_SLOTS as u32).flat_map(|idx| {
            const ACTIONS: [&str; 3] = [
                "AI_GET_PRESETS_ACTIONS 0x3DC4 slot 1",
                "AI_GET_PRESETS_ACTIONS 0x3DC4 slot 2",
                "AI_GET_PRESETS_ACTIONS 0x3DC4 slot 3",
            ];
            const OPA: [&str; 3] = [
                "AI_GET_PRESET_OPA 0x1D04 slot 1",
                "AI_GET_PRESET_OPA 0x1D04 slot 2",
                "AI_GET_PRESET_OPA 0x1D04 slot 3",
            ];
            let i = idx as usize;
            [(ACTIONS[i], 0x3DC4u16), (OPA[i], 0x1D04)].map(|(name, cmd)| {
                (
                    name,
                    self.transact_get_with(RCV_AI, cmd, &idx.to_le_bytes(), name)
                        .map(|r| r.payload),
                )
            })
        }))
        .chain(std::iter::once((
            "AI_GET_PRESETS_ACTIONS 0x3DC4 (no slot)",
            self.transact_get(RCV_AI, 0x3DC4, "AI_GET_PRESETS_ACTIONS")
                .map(|r| r.payload),
        )))
        .collect()
    }

    /// Raw selector-13 preset entries (all 60 bytes), to look for fields past
    /// the name. Read-only apart from the cursor-reset echo `presets` uses.
    pub fn raw_preset_entries(&self) -> Result<Vec<[u8; 60]>> {
        let list = self.fd.xu_get(XU_UNIT, SEL_PRESET_LIST)?;
        if list[0] > PRESET_SLOTS || list.iter().all(|&b| b == 0) {
            return Err(Error::NoReply("on-camera preset list"));
        }
        self.fd.xu_set(XU_UNIT, SEL_PRESET_LIST, &list)?;
        (0..list[0])
            .map(|_| self.fd.xu_get(XU_UNIT, SEL_PRESET_ENTRY))
            .collect()
    }

    /// Restore the factory power-on position.
    pub fn reset_boot_pose(&self) -> Result<()> {
        preset_writes_allowed()?;
        self.model.require(Feature::PowerOnPose)?;
        self.send_set(RCV_AI, CMD_BOOT_POS_RST, &[])
    }

    // --- AI tracking ---

    /// Set the AI tracking mode (or Off). Raw TLV on selector 6.
    pub fn set_tracking(&self, mode: TrackMode) -> Result<()> {
        self.model.require(Feature::Tracking(mode))?;
        let (category, submode) = mode.category_submode();
        self.fd.xu_set(
            XU_UNIT,
            SEL_STATUS,
            &[TLV_AI_TRACK, 0x02, category, submode],
        )
    }

    /// Set tracking speed. EXPERIMENTAL on Tiny 3 (frame from Tiny 2 capture).
    /// `sport` = fast; otherwise standard.
    pub fn set_track_speed(&self, sport: bool) -> Result<()> {
        self.model.require(Feature::TrackSpeed)?;
        let v: u8 = if sport { 0x02 } else { 0x00 };
        self.send_set(RCV_AI, CMD_TRACK_SPEED, &[v])
    }

    // --- gimbal ---

    /// Return the gimbal to mechanical center (pan 0°, tilt 0°).
    ///
    /// Implemented via the UVC pan/tilt controls rather than the vendor
    /// recenter frame (0x00C3) on purpose: uvcvideo caches pan/tilt readback
    /// from the last value it wrote, so a vendor recenter would leave `status`
    /// reporting the stale pre-recenter angle while the camera is physically
    /// centered. Writing pan=0/tilt=0 both moves the gimbal AND keeps the
    /// readback honest. The vendor frame is preserved in `recenter_vendor`.
    pub fn recenter(&self) -> Result<()> {
        self.model.require(Feature::Gimbal)?;
        self.fd.set_ctrl(controls::CID_PAN_ABSOLUTE, 0)?;
        self.fd.set_ctrl(controls::CID_TILT_ABSOLUTE, 0)
    }

    /// Vendor recenter/home frame. Moves the gimbal but desyncs the UVC
    /// pan/tilt readback cache — prefer [`recenter`]. Useful to also reset AI
    /// tracking drift.
    #[allow(dead_code)]
    pub fn recenter_vendor(&self) -> Result<()> {
        self.send_set(RCV_GIMBAL, CMD_RECENTER, &[0, 0, 0, 0, 0, 0])
    }

    /// Move the gimbal to an absolute motor angle (degrees). EXPERIMENTAL —
    /// prefer the UVC pan/tilt controls in `set_pan_deg`/`set_tilt_deg`.
    #[allow(dead_code)]
    pub fn gimbal_move(&self, roll: f32, pitch: f32, yaw: f32) -> Result<()> {
        let mut p = Vec::with_capacity(12);
        p.extend_from_slice(&roll.to_le_bytes());
        p.extend_from_slice(&pitch.to_le_bytes());
        p.extend_from_slice(&yaw.to_le_bytes());
        self.send_set(RCV_AI, CMD_GIMBAL_MOVE, &p)
    }

    /// Gimbal range in degrees for pan or tilt, as reported by this camera.
    /// Tiny 2 and Tiny 3 gimbals differ, so this is queried, not assumed.
    pub fn gimbal_range_deg(&self, cid: u32) -> (f64, f64) {
        let fallback = if cid == controls::CID_TILT_ABSOLUTE {
            controls::TILT_MAX
        } else {
            controls::PAN_MAX
        };
        let (min, max) = match self.fd.query_ctrl(cid) {
            Ok(r) if r.max > r.min => (r.min, r.max),
            _ => (-fallback, fallback),
        };
        (
            min as f64 / controls::ASEC_PER_DEG,
            max as f64 / controls::ASEC_PER_DEG,
        )
    }

    fn set_angle(&self, cid: u32, what: &'static str, deg: f64) -> Result<()> {
        self.model.require(Feature::Gimbal)?;
        let (min, max) = self.gimbal_range_deg(cid);
        if !(min..=max).contains(&deg) {
            return Err(Error::OutOfRange {
                what,
                min: min.round() as i64,
                max: max.round() as i64,
                got: deg.round() as i64,
            });
        }
        let asec = (deg * controls::ASEC_PER_DEG).round() as i32;
        self.fd.set_ctrl(cid, asec)
    }

    /// Absolute pan via the standard UVC control, within the camera's range.
    pub fn set_pan_deg(&self, deg: f64) -> Result<()> {
        self.set_angle(controls::CID_PAN_ABSOLUTE, "pan degrees", deg)
    }

    /// Absolute tilt via the standard UVC control, within the camera's range.
    pub fn set_tilt_deg(&self, deg: f64) -> Result<()> {
        self.set_angle(controls::CID_TILT_ABSOLUTE, "tilt degrees", deg)
    }

    /// Absolute zoom 0..=100 via the standard UVC control.
    pub fn set_zoom(&self, zoom: i32) -> Result<()> {
        self.model.require(Feature::Zoom)?;
        if !(0..=100).contains(&zoom) {
            return Err(Error::OutOfRange {
                what: "zoom",
                min: 0,
                max: 100,
                got: zoom as i64,
            });
        }
        self.fd.set_ctrl(controls::CID_ZOOM_ABSOLUTE, zoom)
    }

    // --- image ---

    /// Pin manual white balance at `temp` Kelvin, in the firmware-safe order.
    pub fn set_wb_temp(&self, temp: i32) -> Result<()> {
        self.model.require(Feature::WhiteBalance)?;
        if !(controls::WB_TEMP_MIN..=controls::WB_TEMP_MAX).contains(&temp) {
            return Err(Error::OutOfRange {
                what: "white balance temperature (Kelvin)",
                min: controls::WB_TEMP_MIN as i64,
                max: controls::WB_TEMP_MAX as i64,
                got: temp as i64,
            });
        }
        controls::pin_white_balance(&self.fd, temp)
    }

    /// Restore auto white balance (the safe watchable fallback).
    pub fn set_wb_auto(&self) -> Result<()> {
        controls::white_balance_auto(&self.fd)
    }

    /// HDR/WDR on or off. Raw TLV on selector 6.
    pub fn set_hdr(&self, on: bool) -> Result<()> {
        self.model.require(Feature::Hdr)?;
        self.fd
            .xu_set(XU_UNIT, SEL_STATUS, &[TLV_HDR, 0x01, on as u8])
    }

    /// Field of view: 0 wide(86°), 1 medium(78°), 2 narrow(65°).
    pub fn set_fov(&self, level: u8) -> Result<()> {
        self.model.require(Feature::Fov)?;
        if level > 2 {
            return Err(Error::OutOfRange {
                what: "fov level",
                min: 0,
                max: 2,
                got: level as i64,
            });
        }
        self.fd.xu_set(XU_UNIT, SEL_STATUS, &[TLV_FOV, 0x01, level])
    }

    /// Face-priority auto-exposure: false = global metering, true = face.
    /// Requires auto-exposure on.
    pub fn set_face_ae(&self, face: bool) -> Result<()> {
        self.model.require(Feature::FaceAe)?;
        self.fd
            .xu_set(XU_UNIT, SEL_STATUS, &[TLV_FACE_AE, 0x01, face as u8])
    }

    /// Auto exposure on/off (standard UVC menu control).
    pub fn set_auto_exposure(&self, auto: bool) -> Result<()> {
        self.model.require(Feature::Exposure)?;
        let v = if auto {
            controls::EXPOSURE_AUTO_MODE
        } else {
            controls::EXPOSURE_MANUAL_MODE
        };
        self.fd.set_ctrl(controls::CID_EXPOSURE_AUTO, v)
    }

    /// Manual exposure time (device units, 1..=2500 ≈ 0.1ms..250ms). Sets
    /// manual mode first.
    pub fn set_exposure(&self, value: i32) -> Result<()> {
        self.model.require(Feature::Exposure)?;
        if !(1..=2500).contains(&value) {
            return Err(Error::OutOfRange {
                what: "exposure",
                min: 1,
                max: 2500,
                got: value as i64,
            });
        }
        self.fd
            .set_ctrl(controls::CID_EXPOSURE_AUTO, controls::EXPOSURE_MANUAL_MODE)?;
        self.fd.set_ctrl(controls::CID_EXPOSURE_ABSOLUTE, value)
    }

    /// Raw vendor status block only (no UVC reads) — used by `probe`.
    pub fn raw_status(&self) -> Result<[u8; 60]> {
        self.fd.xu_get(XU_UNIT, SEL_STATUS)
    }

    // --- status ---

    /// Read and decode the full status: vendor block + key UVC controls.
    pub fn status(&self) -> Result<Status> {
        let s = self.fd.xu_get(XU_UNIT, SEL_STATUS)?;
        let pan = self.fd.get_ctrl(controls::CID_PAN_ABSOLUTE)?;
        let tilt = self.fd.get_ctrl(controls::CID_TILT_ABSOLUTE)?;
        let zoom = self.fd.get_ctrl(controls::CID_ZOOM_ABSOLUTE)?;
        let auto_wb = self.fd.get_ctrl(controls::CID_AUTO_WHITE_BALANCE)? != 0;
        let wb_temp = self.fd.get_ctrl(controls::CID_WHITE_BALANCE_TEMPERATURE)?;
        let ae = self.fd.get_ctrl(controls::CID_EXPOSURE_AUTO)?;
        Ok(Status {
            asleep: s[ST_SLEEP] != 0,
            auto_sleep_seconds: decode_auto_sleep(&s),
            hdr: s[ST_HDR] != 0,
            tracking: TrackMode::from_status(s[ST_AI_CATEGORY], s[ST_AI_SUBMODE]),
            tracking_sport: self.model.track_speed_offset.map(|o| s[o] == 2),
            fov: s[ST_FOV],
            face_ae: s[ST_FACE_AE] != 0,
            auto_wb,
            wb_temp,
            pan_deg: pan as f64 / controls::ASEC_PER_DEG,
            tilt_deg: tilt as f64 / controls::ASEC_PER_DEG,
            zoom,
            auto_exposure: ae == controls::EXPOSURE_AUTO_MODE,
            raw: s,
        })
    }
}

fn decode_auto_sleep(status: &[u8; 60]) -> u16 {
    u16::from_le_bytes([status[ST_AUTO_SLEEP], status[ST_AUTO_SLEEP + 1]])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hexbytes(s: &str) -> Vec<u8> {
        s.split_whitespace()
            .map(|b| u8::from_str_radix(b, 16).unwrap())
            .collect()
    }

    /// Real Tiny 2 records, captured against settings entered in OBSBOT Center.
    #[test]
    fn preset_actions_decode_matches_obsbot_center() {
        let pad = " 00".repeat(16);
        // Preset 1: Normal, Standard, AF on/global, AE on/global, comp +1,
        // WB auto, blur off, contrast/sat/sharp/hue 47/48/49/40.
        let a = PresetActions::decode(&hexbytes(&format!(
            "00 00 01 00 10 00 01 00 0c 32 00 40 00 00 b8 0b 01 00 00 01 2f 30 31 28{pad}"
        )))
        .unwrap();
        assert_eq!(a.tracking, Some(TrackMode::Normal));
        assert_eq!(a.sport, Some(false));
        assert_eq!((a.auto_focus, a.face_focus), (Some(true), Some(false)));
        assert_eq!((a.auto_exposure, a.face_ae), (Some(true), Some(false)));
        assert_eq!(a.exposure_comp_thirds, Some(3));
        assert_eq!(a.auto_wb, Some(true));
        assert_eq!(a.blur, Some(false));
        assert_eq!(
            (a.contrast, a.saturation, a.sharpness, a.hue),
            (Some(47), Some(48), Some(49), Some(40))
        );
        // Preset 2: Desk, face focus/AE, comp 0, WB 4400, blur on level 2.
        let b = PresetActions::decode(&hexbytes(&format!(
            "05 00 01 01 10 00 01 01 09 32 00 40 00 05 30 11 01 01 00 02 25 26 27 1e{pad}"
        )))
        .unwrap();
        assert_eq!(b.tracking, Some(TrackMode::Desk));
        assert_eq!((b.face_focus, b.face_ae), (Some(true), Some(true)));
        assert_eq!(b.exposure_comp_thirds, Some(0));
        assert_eq!((b.auto_wb, b.wb_temp), (Some(false), Some(4400)));
        assert_eq!((b.blur, b.blur_level), (Some(true), Some(2)));
        assert_eq!(b.hue, Some(30));
        // Preset 3: Group, Motion speed, AF/AE/WB off (unset), blur level 8.
        let c = PresetActions::decode(&hexbytes(&format!(
            "09 01 ff ff ff ff ff ff 80 ff ff ff ff ff ff ff 01 01 00 08 1b 1c 1d 14{pad}"
        )))
        .unwrap();
        assert_eq!(c.tracking, Some(TrackMode::Group));
        assert_eq!(c.sport, Some(true));
        assert_eq!(
            (c.auto_focus, c.auto_exposure, c.auto_wb),
            (None, None, None)
        );
        assert_eq!((c.exposure_comp_thirds, c.wb_temp), (None, None));
        assert_eq!(c.blur_level, Some(8));
        // Remaining observed mode bytes.
        assert_eq!(actions_track_mode(0xff), Some(TrackMode::Off));
        assert_eq!(actions_track_mode(1), Some(TrackMode::UpperBody));
        assert_eq!(actions_track_mode(6), Some(TrackMode::Whiteboard));
        assert_eq!(actions_track_mode(7), None);
    }

    #[test]
    fn preset_writes_stay_disabled() {
        let e = preset_writes_allowed().unwrap_err().to_string();
        assert!(e.contains("disabled"), "{e}");
    }

    #[test]
    fn preset_entry_decodes() {
        let mut b = [0u8; 60];
        b[1] = 1; // slot 2
        b[4..6].copy_from_slice(&(-1500i16).to_le_bytes()); // tilt -15.00
        b[6..8].copy_from_slice(&(2025i16).to_le_bytes()); // pan 20.25
        b[8..10].copy_from_slice(&150u16.to_le_bytes()); // zoom 1.5
        b[10..18].copy_from_slice(b"RGVzaw=="); // "Desk"
        let (slot, pose, name) = decode_preset_entry(&b).unwrap();
        assert_eq!(slot, 2);
        assert_eq!(
            (pose.pan_deg, pose.tilt_deg, pose.zoom_ratio),
            (20.25, -15.0, 1.5)
        );
        assert_eq!(name.as_deref(), Some("Desk"));
        // End-of-list markers and failed (all-zero) reads are not presets.
        b[0] = 0x02;
        assert!(decode_preset_entry(&b).is_none());
        assert!(decode_preset_entry(&[0u8; 60]).is_none());
        let mut bad = [0u8; 60];
        bad[1] = 5;
        bad[8] = 100;
        assert!(decode_preset_entry(&bad).is_none());
    }

    #[test]
    fn pose_payloads_have_the_widths_the_firmware_requires() {
        let p = Pose {
            pan_deg: -35.0,
            tilt_deg: -20.0,
            zoom_ratio: 1.0,
        };
        // Boot pose: [0, yaw, pitch, roll, zoom, 0] = 24 bytes.
        let boot = pose_payload(0, &p, 0.0);
        assert_eq!(boot.len(), 24);
        assert_eq!(&boot[4..8], &(-35.0f32).to_le_bytes());
        assert_eq!(&boot[20..24], &0.0f32.to_le_bytes());
        // ADD/UPDATE: slot index then the same pose with a -1000 sentinel.
        let add = pose_payload(2, &p, -1000.0);
        assert_eq!(&add[0..4], &2u32.to_le_bytes());
        assert_eq!(&add[20..24], &(-1000.0f32).to_le_bytes());
        assert!(check_slot(0).is_err() && check_slot(4).is_err());
        assert_eq!(check_slot(3).unwrap(), 2);
    }

    #[test]
    fn tiny2_hand_tracking_readback_decodes() {
        assert_eq!(TrackMode::from_status(6, 0), TrackMode::Hand);
        assert_eq!(TrackMode::from_status(3, 0), TrackMode::Hand);
        assert_eq!(TrackMode::from_status(2, 1), TrackMode::UpperBody);
    }

    #[test]
    fn firmware_sleep_timer_is_little_endian_and_distinct_from_sleep_state() {
        // Captured prefix from the affected Tiny 3 Lite: awake but configured
        // to park itself after 120 seconds, despite an active microphone.
        let mut status = [0; 60];
        status[..14].copy_from_slice(&[0x2e, 1, 0, 2, 0, 0, 0, 1, 0, 1, 120, 0, 0, 1]);
        assert_eq!(decode_auto_sleep(&status), 120);
        status[ST_SLEEP] = 1;
        assert_eq!(decode_auto_sleep(&status), 120);
        status[ST_AUTO_SLEEP..ST_AUTO_SLEEP + 2].copy_from_slice(&600u16.to_le_bytes());
        assert_eq!(decode_auto_sleep(&status), 600);
        status[ST_AUTO_SLEEP..ST_AUTO_SLEEP + 2].fill(0);
        assert_eq!(decode_auto_sleep(&status), 0);
    }
}
