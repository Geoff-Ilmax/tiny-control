//! Camera identity and selection for multi-camera setups.
//!
//! Identity: OBSBOT cameras publish no USB serial string, so two identical
//! models are indistinguishable in `/dev/v4l/by-id`. The stable id is instead
//! the vendor serial number read over the control channel (a control read: it
//! does NOT wake a sleeping camera). It is cached per USB port + device number
//! in `$XDG_RUNTIME_DIR/tiny-control/ids`, so each camera is opened for this at
//! most once per plug-in. If the serial cannot be read the id falls back to
//! `port-<usb path>`, which is stable as long as the camera stays in that port.
//!
//! Selection: every device command needs an explicit camera (`-c`). There is
//! deliberately no implicit default, even with one camera attached.

use crate::config::{self, ConfigFile};
use crate::discover::{self, Camera};
use crate::error::{Error, Result};
use crate::Device;

/// A discovered camera with its resolved identity.
#[derive(Debug, Clone)]
pub struct Cam {
    pub info: Camera,
    /// Stable id: vendor serial, or `port-<usb path>`.
    pub id: String,
    pub serial: Option<String>,
    pub alias: Option<String>,
    /// 1-based position in `tinyctl list` (ordered by USB port).
    pub index: usize,
    /// True when the serial could not be read this time and `id` is a
    /// port-based stand-in that may change once the serial is readable.
    pub provisional: bool,
}

impl Cam {
    /// Human label: alias if set, else the id.
    pub fn label(&self) -> &str {
        self.alias.as_deref().unwrap_or(&self.id)
    }

    pub fn open(&self) -> Result<Device> {
        Device::open_camera(&self.info)
    }
}

pub fn port_id(c: &Camera) -> String {
    format!("port-{}", c.usb_path)
}

fn cache_key(c: &Camera) -> String {
    format!("{} {} {}", c.usb_path, c.busnum, c.devnum)
}

fn cache_path() -> std::path::PathBuf {
    config::runtime_dir().join("ids")
}

fn cached_serial(c: &Camera) -> Option<String> {
    let key = cache_key(c);
    let text = std::fs::read_to_string(cache_path()).ok()?;
    text.lines()
        .find_map(|l| l.strip_prefix(&key)?.strip_prefix(' ').map(str::to_string))
        .filter(|s| config::valid_id(s))
}

fn store_serial(c: &Camera, serial: &str) {
    let key = cache_key(c);
    let port = format!("{} ", c.usb_path);
    let old = std::fs::read_to_string(cache_path()).unwrap_or_default();
    // Drop stale entries for the same port (previous plug-in).
    let mut body: String = old
        .lines()
        .filter(|l| !l.starts_with(&port))
        .map(|l| format!("{l}\n"))
        .collect();
    body.push_str(&format!("{key} {serial}\n"));
    let _ = std::fs::create_dir_all(config::runtime_dir());
    let tmp = cache_path().with_extension(format!("tmp{}", std::process::id()));
    if std::fs::write(&tmp, body).is_ok() {
        let _ = std::fs::rename(tmp, cache_path());
    }
}

/// Read a camera's vendor serial, from cache or the device.
pub fn read_serial(c: &Camera) -> Option<String> {
    if let Some(s) = c.usb_serial.clone().filter(|s| config::valid_id(s)) {
        return Some(s);
    }
    if let Some(s) = cached_serial(c) {
        return Some(s);
    }
    let serial = Device::open_camera(c).ok()?.serial().ok()?;
    let serial = serial.trim().to_string();
    if !config::valid_id(&serial) {
        return None;
    }
    store_serial(c, &serial);
    Some(serial)
}

/// Every attached camera with its identity and alias.
pub fn all() -> Vec<Cam> {
    let file = ConfigFile::load();
    discover::enumerate()
        .into_iter()
        .enumerate()
        .map(|(i, info)| {
            let serial = read_serial(&info);
            let id = serial.clone().unwrap_or_else(|| port_id(&info));
            // A port id is only provisional if the user has not chosen to key
            // config on it (i.e. there is no section for it yet).
            let provisional = serial.is_none() && !file.cameras.contains_key(&id);
            Cam {
                alias: file.alias_of(&id).map(str::to_string),
                index: i + 1,
                id,
                serial,
                info,
                provisional,
            }
        })
        .collect()
}

fn describe(cams: &[Cam]) -> String {
    if cams.is_empty() {
        return "no OBSBOT cameras are connected".into();
    }
    let list: Vec<String> = cams
        .iter()
        .map(|c| {
            format!(
                "  {}  {}{}  ({}, USB {})",
                c.index,
                c.id,
                c.alias
                    .as_deref()
                    .map(|a| format!(" \"{a}\""))
                    .unwrap_or_default(),
                c.info.model.name,
                c.info.usb_path
            )
        })
        .collect();
    format!("connected cameras:\n{}", list.join("\n"))
}

/// Resolve a `-c` selector against the attached cameras.
///
/// Accepted forms, tried in order: `all`; `selected` (see `tinyctl select`);
/// a list index (`1`..); a device path (`/dev/video2`, by-id, by-path); an
/// id (serial or `port-…`, case-insensitive); a USB port path (`1-4.4`); an
/// alias.
pub fn resolve(selector: &str, cams: &[Cam]) -> Result<Vec<Cam>> {
    let not_found = |what: String| Error::DeviceNotFound(format!("{what}\n{}", describe(cams)));
    let sel = selector.trim();
    if sel.eq_ignore_ascii_case("all") {
        if cams.is_empty() {
            return Err(not_found("no camera matches 'all'".into()));
        }
        return Ok(cams.to_vec());
    }
    let one = |c: Option<&Cam>| c.cloned().map(|c| vec![c]);
    let found = if sel.eq_ignore_ascii_case("selected") {
        let id = config::selected().ok_or_else(|| {
            Error::Usage("no camera is selected; run `tinyctl select <camera>` first".into())
        })?;
        one(cams.iter().find(|c| c.id == id))
            .ok_or_else(|| not_found(format!("the selected camera {id} is not connected")))?
    } else if let Ok(n) = sel.parse::<usize>() {
        one(cams.iter().find(|c| c.index == n))
            .ok_or_else(|| not_found(format!("there is no camera #{n}")))?
    } else if sel.starts_with('/') {
        let node =
            discover::real_node(sel).map_err(|_| not_found(format!("{sel} does not exist")))?;
        one(cams.iter().find(|c| c.info.node == node))
            .ok_or_else(|| not_found(format!("{sel} is not an OBSBOT camera")))?
    } else {
        let file = ConfigFile::load();
        let alias_id = file.id_for_alias(sel);
        one(cams.iter().find(|c| c.id.eq_ignore_ascii_case(sel)))
            .or_else(|| one(cams.iter().find(|c| c.info.usb_path == sel)))
            .or_else(|| {
                one(cams.iter().find(|c| {
                    c.alias
                        .as_deref()
                        .is_some_and(|a| a.eq_ignore_ascii_case(sel))
                }))
            })
            .or_else(|| alias_id.and_then(|id| one(cams.iter().find(|c| c.id == id))))
            .ok_or_else(|| match alias_id {
                Some(id) => not_found(format!("camera '{sel}' ({id}) is not connected")),
                None => not_found(format!("no camera matches '{sel}'")),
            })?
    };
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cam(index: usize, id: &str, port: &str, node: &str, alias: Option<&str>) -> Cam {
        Cam {
            info: Camera {
                node: node.into(),
                usb_path: port.into(),
                busnum: 1,
                devnum: index as u32,
                pid: "fef8".into(),
                product: "OBSBOT Tiny 2".into(),
                usb_serial: None,
                model: crate::model::by_pid("fef8"),
            },
            id: id.into(),
            serial: Some(id.into()),
            alias: alias.map(Into::into),
            index,
            provisional: false,
        }
    }

    fn ids(v: Result<Vec<Cam>>) -> Vec<String> {
        v.unwrap().into_iter().map(|c| c.id).collect()
    }

    #[test]
    fn selectors_resolve() {
        let cams = vec![
            cam(1, "RMOWAAA", "1-4.4", "/dev/video2", None),
            cam(2, "RMOWBBB", "3-4.1", "/dev/video0", Some("desk")),
        ];
        assert_eq!(ids(resolve("all", &cams)), ["RMOWAAA", "RMOWBBB"]);
        assert_eq!(ids(resolve("2", &cams)), ["RMOWBBB"]);
        assert_eq!(ids(resolve("rmowaaa", &cams)), ["RMOWAAA"]);
        assert_eq!(ids(resolve("3-4.1", &cams)), ["RMOWBBB"]);
        assert_eq!(ids(resolve("DESK", &cams)), ["RMOWBBB"]);
        assert!(resolve("3", &cams)
            .unwrap_err()
            .to_string()
            .contains("no camera #3"));
        let e = resolve("nope", &cams).unwrap_err().to_string();
        assert!(e.contains("no camera matches 'nope'"), "{e}");
        assert!(e.contains("RMOWBBB \"desk\""), "{e}");
        assert!(resolve("all", &[]).is_err());
    }

    #[test]
    fn cache_key_includes_devnum_so_replug_rereads() {
        let a = cam(1, "X", "1-4.4", "/dev/video2", None);
        let mut b = a.clone();
        b.info.devnum = 9;
        assert_ne!(cache_key(&a.info), cache_key(&b.info));
        assert_eq!(port_id(&a.info), "port-1-4.4");
    }
}
