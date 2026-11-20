//! Config file shared by `tinyctl` and `tiny-guard`.
//!
//! Format is a minimal INI-ish file (no external deps). Keys before any section
//! are defaults for every camera; a `[camera <ID>]` section overrides them for
//! one camera, where `<ID>` is the camera's stable id (its vendor serial number,
//! or `port-<usb path>` when the serial cannot be read):
//!
//! ```text
//! wb_temp = 4000          # default for all cameras
//!
//! [camera RMOWUHI3111PLN]
//! alias = main
//! tracking = upper-body
//! wb_temp = 4500
//! ```
//!
//! Lives at `$XDG_CONFIG_HOME/tiny-control/config` (default
//! `~/.config/tiny-control/config`), falling back to `/etc/tiny-control/config`.
//! Gimbal presets are not stored here: they live in the camera's own three
//! preset slots (see `Device::presets`).

use crate::error::{Error, Result};
use std::collections::BTreeMap;
use std::path::PathBuf;

pub const APP_DIR: &str = "tiny-control";
pub const SYSTEM_CONFIG: &str = "/etc/tiny-control/config";

/// Effective settings for one camera (defaults ⊕ global keys ⊕ its section).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    /// Target white-balance temperature the guard re-pins to (Kelvin).
    pub wb_temp: i32,
    pub power: String,
    pub tracking: crate::TrackMode,
    pub hdr: Option<bool>,
    pub auto_wb: bool,
    /// Last values set with tinyctl for settings whose camera readback is not
    /// decoded yet. Shown by `status`; not enforced by the guard.
    pub fov: Option<String>,
    pub face_ae: Option<bool>,
    pub track_speed: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        // 4000K is the tuned baseline for a green-walled office; auto WB drowns
        // in green rooms on these cameras. Overridable in the config file.
        Config {
            wb_temp: 4000,
            power: "auto".into(),
            tracking: crate::TrackMode::Off,
            hdr: None,
            auto_wb: false,
            fov: None,
            face_ae: None,
            track_speed: None,
        }
    }
}

fn xdg_dir(var: &str, fallback: &str) -> PathBuf {
    if let Ok(x) = std::env::var(var) {
        if !x.is_empty() {
            return PathBuf::from(x).join(APP_DIR);
        }
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".into());
    PathBuf::from(home).join(fallback).join(APP_DIR)
}

pub fn config_dir() -> PathBuf {
    xdg_dir("XDG_CONFIG_HOME", ".config")
}

/// Persistent per-user state that is not configuration (the selected camera).
pub fn state_dir() -> PathBuf {
    xdg_dir("XDG_STATE_HOME", ".local/state")
}

/// Volatile cache (cleared on reboot when XDG_RUNTIME_DIR is a tmpfs).
pub fn runtime_dir() -> PathBuf {
    match std::env::var("XDG_RUNTIME_DIR") {
        Ok(x) if !x.is_empty() => PathBuf::from(x).join(APP_DIR),
        _ => state_dir(),
    }
}

pub fn config_path() -> PathBuf {
    config_dir().join("config")
}

type Map = BTreeMap<String, String>;

/// The whole config file: global keys plus one section per camera id.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConfigFile {
    pub global: Map,
    pub cameras: BTreeMap<String, Map>,
}

fn strip_comment(line: &str) -> &str {
    match line.find(" #") {
        Some(i) => &line[..i],
        None => line,
    }
}

impl ConfigFile {
    pub fn parse(text: &str) -> Self {
        let mut f = ConfigFile::default();
        let mut section: Option<String> = None;
        for raw in text.lines() {
            let line = strip_comment(raw).trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some(h) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
                section = h
                    .trim()
                    .strip_prefix("camera")
                    .map(|id| id.trim().to_string())
                    .filter(|id| valid_id(id));
                if let Some(id) = &section {
                    f.cameras.entry(id.clone()).or_default();
                }
                continue;
            }
            if let Some((k, v)) = line.split_once('=') {
                let (k, v) = (k.trim().to_string(), v.trim().to_string());
                match &section {
                    Some(id) => {
                        f.cameras.get_mut(id).unwrap().insert(k, v);
                    }
                    None => {
                        f.global.insert(k, v);
                    }
                }
            }
        }
        f
    }

    pub fn render(&self) -> String {
        let mut out = String::from("# tiny-control configuration (rewritten by tinyctl).\n");
        for (k, v) in &self.global {
            out.push_str(&format!("{k} = {v}\n"));
        }
        for (id, m) in &self.cameras {
            out.push_str(&format!("\n[camera {id}]\n"));
            for (k, v) in m {
                out.push_str(&format!("{k} = {v}\n"));
            }
        }
        out
    }

    fn read(path: &std::path::Path) -> std::io::Result<Option<ConfigFile>> {
        match std::fs::read_to_string(path) {
            Ok(t) => Ok(Some(Self::parse(&t))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// The per-user file if it exists, else the system file, else empty.
    pub fn load() -> ConfigFile {
        [config_path(), PathBuf::from(SYSTEM_CONFIG)]
            .iter()
            .find_map(|p| Self::read(p).ok().flatten())
            .unwrap_or_default()
    }

    /// Effective settings for camera `id`.
    pub fn camera(&self, id: &str) -> Config {
        let mut m = self.global.clone();
        if let Some(s) = self.cameras.get(id) {
            m.extend(s.iter().map(|(k, v)| (k.clone(), v.clone())));
        }
        Config::from_map(&m)
    }

    pub fn alias_of(&self, id: &str) -> Option<&str> {
        self.cameras.get(id)?.get("alias").map(String::as_str)
    }

    /// Camera id whose alias is `alias` (case-insensitive).
    pub fn id_for_alias(&self, alias: &str) -> Option<&str> {
        self.cameras
            .iter()
            .find(|(_, m)| {
                m.get("alias")
                    .is_some_and(|a| a.eq_ignore_ascii_case(alias))
            })
            .map(|(id, _)| id.as_str())
    }
}

/// Camera ids become section headers and directory names.
pub fn valid_id(id: &str) -> bool {
    // Must start alphanumeric so an id can never be `.` / `..` as a path part.
    id.chars().next().is_some_and(|c| c.is_ascii_alphanumeric())
        && id.len() <= 64
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

/// Aliases must be usable on a command line and in a keybinding, and must not
/// look like the other selector forms (an index, a path, `all`, `selected`).
pub fn valid_alias(a: &str) -> bool {
    valid_name(a)
        && !a.bytes().all(|b| b.is_ascii_digit())
        && !matches!(a.to_ascii_lowercase().as_str(), "all" | "selected")
        && !a.starts_with("port-")
}

impl Config {
    fn from_map(m: &Map) -> Self {
        let mut c = Self::default();
        if let Some(v) = m.get("wb_temp").and_then(|s| s.parse::<i32>().ok()) {
            c.wb_temp = v.clamp(crate::controls::WB_TEMP_MIN, crate::controls::WB_TEMP_MAX);
        }
        if let Some(v) = m
            .get("power")
            .filter(|v| matches!(v.as_str(), "auto" | "awake" | "sleep"))
        {
            c.power = v.clone();
        }
        if let Some(v) = m
            .get("tracking")
            .and_then(|v| crate::TrackMode::from_str(v))
        {
            c.tracking = v;
        }
        c.hdr = m.get("hdr").and_then(|v| v.parse().ok());
        if let Some(v) = m.get("auto_wb").and_then(|v| v.parse().ok()) {
            c.auto_wb = v;
        }
        c.fov = m
            .get("fov")
            .filter(|v| matches!(v.as_str(), "wide" | "medium" | "narrow"))
            .cloned();
        c.face_ae = m.get("face_ae").and_then(|v| v.parse().ok());
        c.track_speed = m
            .get("track_speed")
            .filter(|v| matches!(v.as_str(), "std" | "sport"))
            .cloned();
        c
    }

    pub fn keep_awake(&self, active: bool) -> bool {
        self.power == "awake" || (self.power == "auto" && active)
    }

    /// Effective settings for camera `id`, from the current config file.
    pub fn load_for(id: &str) -> Config {
        ConfigFile::load().camera(id)
    }
}

/// Caller holds PolicyLock. Read-modify-write the per-user file atomically,
/// seeding it from the system file the first time.
fn update(f: impl FnOnce(&mut ConfigFile) -> Result<()>) -> Result<()> {
    let path = config_path();
    let mut file = match ConfigFile::read(&path)? {
        Some(c) => c,
        None => ConfigFile::read(std::path::Path::new(SYSTEM_CONFIG))?.unwrap_or_default(),
    };
    f(&mut file)?;
    std::fs::create_dir_all(config_dir())?;
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, file.render())?;
    std::fs::rename(tmp, path)?;
    Ok(())
}

/// Caller holds PolicyLock. Save one setting for one camera.
pub fn save_setting(id: &str, key: &str, value: &str) -> Result<()> {
    if !valid_id(id) {
        return Err(Error::Config(format!("invalid camera id '{id}'")));
    }
    update(|f| {
        f.cameras
            .entry(id.to_string())
            .or_default()
            .insert(key.into(), value.into());
        Ok(())
    })
}

/// Caller holds PolicyLock. Save a setting that applies to every camera
/// (a key outside any `[camera …]` section).
pub fn save_global(key: &str, value: &str) -> Result<()> {
    update(|f| {
        f.global.insert(key.into(), value.into());
        Ok(())
    })
}

/// Whether tiny-remote may show desktop notifications (`notifications = true`
/// in the config). Off unless turned on.
pub fn notifications_enabled() -> bool {
    ConfigFile::load()
        .global
        .get("notifications")
        .is_some_and(|v| v == "true")
}

/// Caller holds PolicyLock. Name a camera (or clear its name with `None`).
pub fn set_alias(id: &str, alias: Option<&str>) -> Result<()> {
    if let Some(a) = alias {
        if !valid_alias(a) {
            return Err(Error::Usage(format!(
                "invalid alias '{a}' (letters, digits, - and _; not all digits, not 'all'/'selected')"
            )));
        }
    }
    update(|f| {
        if let Some(a) = alias {
            if let Some(other) = f.id_for_alias(a).filter(|o| *o != id) {
                return Err(Error::Usage(format!(
                    "alias '{a}' is already used by camera {other}"
                )));
            }
        }
        let section = f.cameras.entry(id.to_string()).or_default();
        match alias {
            Some(a) => section.insert("alias".into(), a.into()),
            None => section.remove("alias"),
        };
        Ok(())
    })
}

// --- selected camera (what the bar widget and keybindings act on) ---

fn selected_path() -> PathBuf {
    state_dir().join("selected")
}

pub fn selected() -> Option<String> {
    std::fs::read_to_string(selected_path())
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| valid_id(s))
}

pub fn set_selected(id: &str) -> Result<()> {
    std::fs::create_dir_all(state_dir())?;
    let tmp = selected_path().with_extension("tmp");
    std::fs::write(&tmp, format!("{id}\n"))?;
    std::fs::rename(tmp, selected_path())?;
    Ok(())
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Serialize cooperating CLI/guard operations, including config read-modify-write.
/// Never hold this lock between guard ticks.
pub struct PolicyLock(std::fs::File);
impl PolicyLock {
    pub fn acquire() -> Result<Self> {
        use std::os::fd::AsRawFd;
        std::fs::create_dir_all(config_dir())?;
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(config_dir().join("control.lock"))?;
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } < 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        Ok(Self(file))
    }
}
impl Drop for PolicyLock {
    fn drop(&mut self) {
        use std::os::fd::AsRawFd;
        unsafe {
            libc::flock(self.0.as_raw_fd(), libc::LOCK_UN);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(text: &str) -> Config {
        ConfigFile::parse(text).camera("CAM1")
    }

    #[test]
    fn explicit_sleep_wins_over_active_calls() {
        for active in [true, false] {
            assert!(!cfg("power = sleep").keep_awake(active));
            assert!(cfg("power = awake").keep_awake(active));
            assert_eq!(cfg("power = auto").keep_awake(active), active);
        }
    }

    #[test]
    fn legacy_config_and_invalid_values_are_safe() {
        let c = cfg("wb_temp = 4000");
        assert!(!c.auto_wb);
        assert_eq!(c.tracking, crate::TrackMode::Off);
        assert_eq!(c.power, "auto");
        let c = cfg("power = nonsense\ntracking = nonsense\nwb_temp = 100000");
        assert_eq!(c.power, "auto");
        assert_eq!(c.tracking, crate::TrackMode::Off);
        assert_eq!(c.wb_temp, 10000);
    }

    #[test]
    fn camera_sections_override_globals_independently() {
        let f = ConfigFile::parse(
            "wb_temp = 4000\npower = auto\n\n[camera CAM1]\nalias = desk\nwb_temp = 4500 # warmer\ntracking = upper-body\n\n[camera port-1-4.4]\npower = sleep\n",
        );
        let a = f.camera("CAM1");
        assert_eq!((a.wb_temp, a.power.as_str()), (4500, "auto"));
        assert_eq!(a.tracking, crate::TrackMode::UpperBody);
        let b = f.camera("port-1-4.4");
        assert_eq!((b.wb_temp, b.power.as_str()), (4000, "sleep"));
        assert_eq!(b.tracking, crate::TrackMode::Off);
        // A camera with no section gets the globals.
        assert_eq!(f.camera("OTHER").wb_temp, 4000);
        assert_eq!(f.alias_of("CAM1"), Some("desk"));
        assert_eq!(f.id_for_alias("DESK"), Some("CAM1"));
        assert_eq!(f.id_for_alias("nope"), None);
    }

    #[test]
    fn last_set_display_settings_parse() {
        let c = cfg("[camera CAM1]\nfov = narrow\nface_ae = true\ntrack_speed = sport\n");
        assert_eq!(c.fov.as_deref(), Some("narrow"));
        assert_eq!(c.face_ae, Some(true));
        assert_eq!(c.track_speed.as_deref(), Some("sport"));
        let c = cfg("fov = huge\ntrack_speed = warp");
        assert_eq!((c.fov, c.track_speed), (None, None));
    }

    #[test]
    fn notifications_are_a_global_key() {
        let f = ConfigFile::parse("notifications = true\n[camera A]\nnotifications = false\n");
        assert_eq!(
            f.global.get("notifications").map(String::as_str),
            Some("true")
        );
    }

    #[test]
    fn render_round_trips() {
        let f =
            ConfigFile::parse("wb_temp = 4000\n[camera A]\nalias = x\n[camera B]\nhdr = true\n");
        assert_eq!(ConfigFile::parse(&f.render()), f);
    }

    #[test]
    fn ids_and_aliases_are_validated() {
        assert!(valid_id("RMOWUHI3111PLN") && valid_id("port-1-4.4"));
        for bad in ["a b", "../x", "..", ".hidden", "", "a/b"] {
            assert!(!valid_id(bad), "{bad}");
        }
        assert!(valid_alias("desk") && valid_alias("cam_2"));
        for bad in ["1", "all", "Selected", "port-x", "has space", "/dev/video0"] {
            assert!(!valid_alias(bad), "{bad}");
        }
    }
}
