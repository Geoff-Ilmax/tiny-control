//! Find OBSBOT cameras without opening them (opening a node blocks USB
//! autosuspend). Discovery walks sysfs: every `video4linux` capture node
//! (`index` 0) whose owning USB device has vendor 0x3564 is a camera. This
//! works for any number of cameras and does not depend on `/dev/v4l/by-id`
//! names, which collide for two identical models because these cameras
//! publish no USB serial string. The numeric `/dev/videoN` is not stable
//! across re-enumeration and is never stored; the USB port path is.

use crate::error::{Error, Result};
use crate::model::{self, Model};
use std::path::{Path, PathBuf};

pub use crate::model::VENDOR_ID;

/// One attached camera, as seen in sysfs. Nothing here opens the device.
#[derive(Debug, Clone)]
pub struct Camera {
    /// Capture node, e.g. `/dev/video2`. Changes across replug — don't persist.
    pub node: String,
    /// sysfs USB device name, e.g. `1-4.4`: stable for a given physical port.
    pub usb_path: String,
    pub busnum: u32,
    pub devnum: u32,
    /// USB product ID, lowercase hex.
    pub pid: String,
    /// USB product string, e.g. `OBSBOT Tiny 2`.
    pub product: String,
    /// USB iSerial, if the firmware publishes one (current models do not).
    pub usb_serial: Option<String>,
    pub model: &'static Model,
}

fn read_trim(p: &Path) -> Option<String> {
    std::fs::read_to_string(p)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// The usb_device directory that owns a video4linux entry.
fn usb_device_dir(sys: &Path, node_name: &str) -> Option<PathBuf> {
    let dev =
        std::fs::canonicalize(sys.join("class/video4linux").join(node_name).join("device")).ok()?;
    dev.ancestors()
        .find(|p| p.join("idVendor").exists() && p.join("busnum").exists())
        .map(Path::to_path_buf)
}

fn camera_from_sysfs(sys: &Path, dev_root: &str, node_name: &str) -> Option<Camera> {
    let class = sys.join("class/video4linux").join(node_name);
    // index 0 is the capture node; index 1 is the metadata node.
    if read_trim(&class.join("index")).as_deref() != Some("0") {
        return None;
    }
    let usb = usb_device_dir(sys, node_name)?;
    if !read_trim(&usb.join("idVendor"))?.eq_ignore_ascii_case(VENDOR_ID) {
        return None;
    }
    let pid = read_trim(&usb.join("idProduct"))?.to_ascii_lowercase();
    let model = model::by_pid(&pid);
    Some(Camera {
        node: format!("{dev_root}/{node_name}"),
        usb_path: usb.file_name()?.to_string_lossy().to_string(),
        busnum: read_trim(&usb.join("busnum"))?.parse().ok()?,
        devnum: read_trim(&usb.join("devnum"))?.parse().ok()?,
        product: read_trim(&usb.join("product")).unwrap_or_else(|| model.name.to_string()),
        usb_serial: read_trim(&usb.join("serial")),
        pid,
        model,
    })
}

/// Every attached OBSBOT camera, ordered by USB port path (stable while
/// cameras stay in the same ports). Opens nothing.
pub fn enumerate() -> Vec<Camera> {
    enumerate_in(Path::new("/sys"), "/dev")
}

/// [`enumerate`] against an alternate sysfs root (for tests).
pub fn enumerate_in(sys: &Path, dev_root: &str) -> Vec<Camera> {
    let mut cams: Vec<Camera> = std::fs::read_dir(sys.join("class/video4linux"))
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| camera_from_sysfs(sys, dev_root, &e.file_name().to_string_lossy()))
        .collect();
    cams.sort_by_key(|c| port_key(&c.usb_path));
    cams
}

/// Numeric sort key for a port path like `3-4.10` so 4.10 sorts after 4.9.
fn port_key(p: &str) -> Vec<u32> {
    p.split(|c: char| !c.is_ascii_digit())
        .filter_map(|n| n.parse().ok())
        .collect()
}

/// The camera behind any node path (`/dev/video2`, a by-id or by-path link).
pub fn camera_for_path(path: &str) -> Result<Camera> {
    let real =
        real_node(path).map_err(|_| Error::DeviceNotFound(format!("{path} does not exist")))?;
    enumerate()
        .into_iter()
        .find(|c| c.node == real)
        .ok_or_else(|| {
            Error::DeviceNotFound(format!("{path} ({real}) is not an OBSBOT capture node"))
        })
}

/// Resolve a by-id (or any) path to its real `/dev/videoN`, following symlinks.
pub fn real_node(path: &str) -> Result<String> {
    let p = std::fs::canonicalize(path)?;
    Ok(p.to_string_lossy().to_string())
}

/// Best-effort USB runtime power state ("active" / "suspended") for the device
/// backing `video_path`, read from sysfs WITHOUT opening the video node.
///
/// Walks /sys/class/video4linux/<node>/device up to the USB interface and then
/// to the usb_device that owns `power/runtime_status`. Returns None if it
/// cannot be determined. NOTE: "suspended" only means USB autosuspend engaged
/// (all interfaces idle) — it does NOT distinguish a vendor-sleep from a plain
/// idle camera. The physical LED is the only ground truth for vendor sleep.
pub fn usb_power_state(video_path: &str) -> Option<String> {
    let real = real_node(video_path).ok()?;
    let usb = usb_device_dir(Path::new("/sys"), real.rsplit('/').next()?)?;
    read_trim(&usb.join("power/runtime_status"))
}

/// Match the ALSA card to the video's physical USB device, never a fixed card
/// number. Reading procfs status does not open a capture stream or record audio.
pub fn audio_active(video_path: &str) -> bool {
    let Ok(real) = real_node(video_path) else {
        return false;
    };
    let Some(usb) = real
        .rsplit('/')
        .next()
        .and_then(|n| usb_device_dir(Path::new("/sys"), n))
    else {
        return false;
    };
    let Ok(cards) = std::fs::read_dir("/sys/class/sound") else {
        return false;
    };
    for card in cards.flatten() {
        let name = card.file_name().to_string_lossy().to_string();
        if !name
            .strip_prefix("card")
            .is_some_and(|s| !s.is_empty() && s.bytes().all(|c| c.is_ascii_digit()))
        {
            continue;
        }
        let Ok(device) = std::fs::canonicalize(card.path().join("device")) else {
            continue;
        };
        if !device.starts_with(&usb) {
            continue;
        }
        let Ok(pcms) = std::fs::read_dir(format!("/proc/asound/{name}")) else {
            continue;
        };
        for pcm in pcms.flatten() {
            let n = pcm.file_name().to_string_lossy().to_string();
            if !n.starts_with("pcm") || !n.ends_with('c') {
                continue;
            }
            let Ok(subs) = std::fs::read_dir(pcm.path()) else {
                continue;
            };
            for sub in subs.flatten() {
                if std::fs::read_to_string(sub.path().join("status"))
                    .is_ok_and(|s| capture_running(&s))
                {
                    return true;
                }
            }
        }
    }
    false
}

fn capture_running(status: &str) -> bool {
    status.lines().any(|l| {
        l.split_once(':')
            .is_some_and(|(k, v)| k.trim() == "state" && v.trim() == "RUNNING")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a fake sysfs with the user's real topology: a Tiny 2 on 3-4.1
    /// (video0/1) and a Tiny 3 on 1-4.4 (video2/3), plus a non-OBSBOT webcam.
    fn fake_sysfs() -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "tiny-control-sysfs-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let class = root.join("class/video4linux");
        std::fs::create_dir_all(&class).unwrap();
        let add = |port: &str,
                   bus: u32,
                   dev: u32,
                   vid: &str,
                   pid: &str,
                   product: &str,
                   nodes: [&str; 2]| {
            let usb = root.join("devices/pci/usb").join(port);
            for (f, v) in [
                ("idVendor", vid.to_string()),
                ("idProduct", pid.to_string()),
                ("product", product.to_string()),
                ("busnum", bus.to_string()),
                ("devnum", dev.to_string()),
            ] {
                std::fs::create_dir_all(&usb).unwrap();
                std::fs::write(usb.join(f), format!("{v}\n")).unwrap();
            }
            for (i, n) in nodes.iter().enumerate() {
                let intf = usb.join(format!("{port}:1.0/video4linux/{n}"));
                std::fs::create_dir_all(&intf).unwrap();
                let entry = class.join(n);
                std::fs::create_dir_all(&entry).unwrap();
                std::fs::write(entry.join("index"), format!("{i}\n")).unwrap();
                std::os::unix::fs::symlink(usb.join(format!("{port}:1.0")), entry.join("device"))
                    .unwrap();
            }
        };
        add(
            "3-4.1",
            3,
            6,
            "3564",
            "fef8",
            "OBSBOT Tiny 2",
            ["video0", "video1"],
        );
        add(
            "1-4.4",
            1,
            6,
            "3564",
            "ff02",
            "OBSBOT Tiny 3",
            ["video2", "video3"],
        );
        add(
            "1-2",
            1,
            3,
            "046d",
            "085e",
            "Logitech",
            ["video4", "video5"],
        );
        root
    }

    #[test]
    fn enumerates_every_obsbot_capture_node_by_port() {
        let root = fake_sysfs();
        let cams = enumerate_in(&root, "/dev");
        let summary: Vec<_> = cams
            .iter()
            .map(|c| (c.node.as_str(), c.usb_path.as_str(), c.model.id))
            .collect();
        assert_eq!(
            summary,
            [
                ("/dev/video2", "1-4.4", "tiny3"),
                ("/dev/video0", "3-4.1", "tiny2")
            ]
        );
        assert_eq!(cams[1].product, "OBSBOT Tiny 2");
        assert_eq!((cams[1].busnum, cams[1].devnum), (3, 6));
        assert!(cams[0].usb_serial.is_none());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn port_paths_sort_numerically() {
        let mut v = vec!["1-4.10", "1-4.9", "1-10", "1-2"];
        v.sort_by_key(|p| port_key(p));
        assert_eq!(v, ["1-2", "1-4.9", "1-4.10", "1-10"]);
    }
    #[test]
    fn audio_activity_requires_running_capture() {
        assert!(capture_running("state: RUNNING\nowner_pid: 123"));
        for s in [
            "closed",
            "state: SUSPENDED",
            "state: PREPARED",
            "state: XRUN",
            "",
        ] {
            assert!(!capture_running(s));
        }
    }
}
