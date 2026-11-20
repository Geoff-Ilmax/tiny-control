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
        let seq = next_seq();
        let f = frame::build(FLAG_GET, seq, receiver, cmd, &[]);
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
