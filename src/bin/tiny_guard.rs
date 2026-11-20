//! tiny-guard — one daemon for every connected camera. Each tick it
//! reconciles each camera with that camera's saved settings (power, tracking,
//! HDR, white balance) and keeps a camera awake while its video node or its own
//! microphone is in use. Cameras are found by sysfs on every tick, so hot-plug
//! needs no restart.
use std::collections::HashMap;
use std::time::Duration;
use tiny_control::{
    camera,
    config::{Config, ConfigFile, PolicyLock},
    discover::{self, Camera},
    model::{Feature, Support},
    Device,
};

const HELP: &str = "\
tiny-guard [--camera SEL] [--temp KELVIN]
tiny-guard --device PATH --once [--temp KELVIN]

Enforces each camera's saved tracking, HDR, WB and power, and keeps a camera
awake while its video or microphone is in use. Manages every connected OBSBOT
camera unless --camera (alias, serial or USB port) limits it to one.
--once pins WB on one device and exits (cold-plug behavior).";

/// Serial reads that fail this many ticks in a row fall back to a port id.
const SERIAL_RETRIES: u32 = 3;

#[derive(Default)]
struct CamState {
    id: Option<String>,
    serial_failures: u32,
    held: Option<Device>,
    last_config: Option<Config>,
    last_error: String,
    /// The firmware sleep timer could not be disabled on this (untested)
    /// model; stop retrying it every tick.
    timer_unsupported: bool,
}

fn main() {
    let mut device = None;
    let mut only = None;
    let mut temp = None;
    let mut once = false;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--device" | "-d" => device = Some(args.next().unwrap_or_else(|| usage())),
            "--camera" | "-c" => only = Some(args.next().unwrap_or_else(|| usage())),
            "--temp" | "-t" => {
                temp = Some(
                    args.next()
                        .unwrap_or_else(|| usage())
                        .parse::<i32>()
                        .unwrap_or_else(|_| usage()),
                )
            }
            "--once" => once = true,
            "--help" | "-h" => {
                println!("{HELP}");
                return;
            }
            _ => usage(),
        }
    }

    if once {
        let path = device.unwrap_or_else(|| usage());
        let result = (|| {
            let dev = Device::open_path(&path)?;
            let info = discover::camera_for_path(&path)?;
            let id = camera::read_serial(&info).unwrap_or_else(|| camera::port_id(&info));
            let mut config = Config::load_for(&id);
            if let Some(t) = temp {
                config.wb_temp = t.clamp(2000, 10000);
            }
            dev.set_wb_temp(config.wb_temp)
        })();
        if let Err(e) = result {
            eprintln!("tiny-guard: {e}");
            std::process::exit(1);
        }
        return;
    }

    // Keyed by physical plug-in (port + devnum), so a replug starts fresh.
    let mut states: HashMap<String, CamState> = HashMap::new();
    loop {
        let cams: Vec<Camera> = match &device {
            Some(p) => discover::camera_for_path(p).into_iter().collect(),
            None => discover::enumerate(),
        };
        let keys: Vec<String> = cams
            .iter()
            .map(|c| format!("{}#{}", c.usb_path, c.devnum))
            .collect();
        states.retain(|k, _| keys.contains(k));

        for (cam, key) in cams.iter().zip(keys) {
            let st = states.entry(key).or_default();
            let Some(id) = identify(cam, st) else {
                continue;
            };
            if let Some(sel) = &only {
                let file = ConfigFile::load();
                let alias = file.alias_of(&id).unwrap_or("");
                if ![id.as_str(), alias, cam.usb_path.as_str()]
                    .iter()
                    .any(|s| s.eq_ignore_ascii_case(sel))
                {
                    continue;
                }
            }
            let label = format!("{} {}", cam.model.name, id);
            match reconcile(cam, &id, temp, st) {
                Ok(()) => st.last_error.clear(),
                Err(e) => {
                    st.held = None;
                    st.last_config = None;
                    let message = e.to_string();
                    if message != st.last_error {
                        eprintln!("tiny-guard: [{label}] {message}");
                        st.last_error = message;
                    }
                }
            }
        }
        std::thread::sleep(Duration::from_secs(1));
    }
}

/// Stable id for a camera, read once per plug-in. A camera whose serial cannot
/// be read is skipped for a few ticks (so a transient failure cannot apply
/// another camera's or the default settings), then keyed by USB port.
fn identify(cam: &Camera, st: &mut CamState) -> Option<String> {
    if st.id.is_none() {
        match camera::read_serial(cam) {
            Some(s) => st.id = Some(s),
            None => {
                st.serial_failures += 1;
                if st.serial_failures < SERIAL_RETRIES {
                    return None;
                }
                eprintln!(
                    "tiny-guard: [{}] serial unreadable; using {}",
                    cam.model.name,
                    camera::port_id(cam)
                );
                st.id = Some(camera::port_id(cam));
            }
        }
    }
    st.id.clone()
}

fn reconcile(
    cam: &Camera,
    id: &str,
    temp: Option<i32>,
    st: &mut CamState,
) -> tiny_control::Result<()> {
    let _lock = PolicyLock::acquire()?;
    let mut config = Config::load_for(id);
    if let Some(t) = temp {
        config.wb_temp = t.clamp(2000, 10000);
    }
    let real = cam.node.as_str();
    let active = !holders(real).is_empty() || discover::audio_active(real);
    let keep_awake = config.keep_awake(active);
    if !keep_awake {
        st.held = None;
    }
    let changed = st.last_config.as_ref() != Some(&config);
    // No device descriptor while idle. Explicit sleep still reconciles
    // periodically so a voice command cannot override a privacy choice.
    if !keep_awake && !active && !changed && config.power != "sleep" {
        return Ok(());
    }
    let temporary;
    let dev = if keep_awake {
        if st.held.as_ref().map(|d| d.path()) != Some(real) {
            st.held = Some(Device::open_camera(cam)?);
        }
        st.held.as_ref().unwrap()
    } else {
        temporary = Device::open_camera(cam)?;
        &temporary
    };
    let mut status = dev.status()?;
    // Prevent timed sleep before it happens. Reactively waking the camera
    // every time this timer fires causes visible power cycling. Recheck on
    // every active tick to cover firmware/app/replug resets.
    if keep_awake && status.auto_sleep_seconds != 0 && !st.timer_unsupported {
        match dev.disable_auto_sleep() {
            Ok(()) => eprintln!(
                "tiny-guard: [{id}] disabled firmware auto-sleep (was {}s)",
                status.auto_sleep_seconds
            ),
            Err(e) if dev.model().support(Feature::AutoSleepTimer) != Support::Verified => {
                eprintln!("tiny-guard: [{id}] warning: {e}; will not retry until replug");
                st.timer_unsupported = true;
            }
            Err(e) => return Err(e),
        }
    }
    if config.power == "sleep" && !status.asleep {
        dev.sleep()?;
        eprintln!("tiny-guard: [{id}] restored requested sleep");
    } else if keep_awake && status.asleep {
        dev.wake()?;
        std::thread::sleep(Duration::from_millis(300));
        status = dev.status()?;
        eprintln!("tiny-guard: [{id}] restored awake for requested power / active call");
    }
    // A saved mode this model cannot do (e.g. a framing mode on a Tiny 3)
    // must not block reconciling everything else: report once, skip it.
    let tracking_ok =
        dev.model().support(Feature::Tracking(config.tracking)) != Support::Unsupported;
    if !tracking_ok && changed {
        eprintln!(
            "tiny-guard: [{id}] warning: saved tracking '{}' is not supported on the {}; leaving tracking alone",
            config.tracking.label(),
            dev.model().name
        );
    }
    if tracking_ok && config.tracking != status.tracking {
        dev.set_tracking(config.tracking)?;
        eprintln!(
            "tiny-guard: [{id}] restored tracking {}",
            config.tracking.label()
        );
    }
    if let Some(hdr) = config.hdr {
        if hdr != status.hdr {
            dev.set_hdr(hdr)?;
        }
    }
    if config.auto_wb {
        if !status.auto_wb {
            dev.set_wb_auto()?;
        }
    } else if status.auto_wb || status.wb_temp != config.wb_temp || changed {
        dev.set_wb_temp(config.wb_temp)?;
    }
    // Verify vendor SETs as well as UVC controls. A successful ioctl only
    // means transport success, not that firmware applied it.
    let actual = dev.status()?;
    if (keep_awake && (actual.asleep || (!st.timer_unsupported && actual.auto_sleep_seconds != 0)))
        || (config.power == "sleep" && !actual.asleep)
        || (tracking_ok && actual.tracking != config.tracking)
        || config.hdr.is_some_and(|h| h != actual.hdr)
        || actual.auto_wb != config.auto_wb
        || (!config.auto_wb && actual.wb_temp != config.wb_temp)
    {
        return Err(tiny_control::Error::Config(
            "camera has not accepted requested settings; retrying".into(),
        ));
    }
    st.last_config = Some(config);
    Ok(())
}

fn usage() -> ! {
    eprintln!("tiny-guard: invalid arguments; see --help");
    std::process::exit(2)
}

/// Video holders excluding our own keep-awake fd and transient control clients.
fn holders(real: &str) -> Vec<u32> {
    let me = std::process::id();
    let mut pids = Vec::new();
    let entries = match std::fs::read_dir("/proc") {
        Ok(e) => e,
        Err(_) => return pids,
    };
    for e in entries.flatten() {
        let name = e.file_name();
        let pid: u32 = match name.to_string_lossy().parse() {
            Ok(p) => p,
            Err(_) => continue,
        };
        if pid == me {
            continue;
        }
        if std::fs::read_to_string(format!("/proc/{pid}/comm"))
            .is_ok_and(|s| matches!(s.trim(), "tinyctl" | "tiny-guard"))
        {
            continue;
        }
        let fd_dir = format!("/proc/{pid}/fd");
        if let Ok(fds) = std::fs::read_dir(&fd_dir) {
            for fd in fds.flatten() {
                if let Ok(target) = std::fs::read_link(fd.path()) {
                    if target.to_string_lossy() == real {
                        pids.push(pid);
                        break;
                    }
                }
            }
        }
    }
    pids
}
