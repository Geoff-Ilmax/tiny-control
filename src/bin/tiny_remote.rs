//! tiny-remote — drive tiny-control from the OBSBOT Tiny Smart Remote.
//!
//! Reads the remote's USB dongle (a keyboard sending Ctrl+Alt shortcuts, see
//! `tiny_control::remote`), grabs it so those shortcuts do not reach other
//! apps, and runs the matching `tinyctl -c selected …` command for each
//! button. Camera buttons 1–4 change the selected camera, which the bar
//! widget and keybindings follow too. Re-scans every few seconds, so the
//! dongle can be plugged in or out at any time.
use std::io::Read;
use std::os::fd::AsRawFd;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};
use tiny_control::remote::{self, Button, Decoder, Press, EVENT_SIZE, EV_KEY};
use tiny_control::{camera, config, Device};

const HELP: &str = "\
tiny-remote [--device /dev/input/eventN] [--no-grab] [--dry-run]

Maps the OBSBOT Tiny Smart Remote to tinyctl commands on the selected camera:
  Power          sleep / wake            Track         tracking on / off
  Close-up, Hand, Whiteboard, Desk       that mode (press again: tracking off)
  D-pad          tap: one step; hold: smooth move until released (uses the
                 camera's Fine control and swap settings from `tinyctl ui`)
  Center         recenter
  Zoom - / +     zoom out / in           P1 P2 P3      recall that preset
  1 2 3 4        select that camera (order of `tinyctl list`)

--no-grab   let the shortcuts also reach other apps (debugging)
--dry-run   print the actions instead of running them";

// EVIOCGRAB = _IOW('E', 0x90, int)
const EVIOCGRAB: libc::c_ulong = 0x4004_4590;
/// Minimum spacing of auto-repeated zoom steps while a button is held.
const REPEAT_EVERY: Duration = Duration::from_millis(150);
/// Gimbal speed while a direction is held (degrees per second).
const MOVE_DPS: f32 = 30.0;
const FINE_MOVE_DPS: f32 = 8.0;
/// Stop a held move if auto-repeat events stop arriving (lost release).
const MOVE_WATCHDOG: Duration = Duration::from_millis(500);

fn is_direction(b: Button) -> bool {
    matches!(b, Button::Up | Button::Down | Button::Left | Button::Right)
}

/// A smooth move in progress: the open camera and when we last heard the
/// button being held.
struct Motion {
    dev: Option<Device>,
    last_seen: Instant,
}

impl Motion {
    /// Start moving the selected camera in `b`'s direction.
    fn start(b: Button, dry_run: bool) -> Result<Motion, String> {
        if dry_run {
            println!("move {b:?} (smooth, until released)");
            return Ok(Motion {
                dev: None,
                last_seen: Instant::now(),
            });
        }
        let cams = camera::all();
        let cam = camera::resolve("selected", &cams)
            .map_err(|e| e.to_string())?
            .remove(0);
        let ui = config::ConfigFile::load();
        let on = |k: &str| {
            ui.cameras
                .get(&cam.id)
                .and_then(|m| m.get(&format!("ui_{k}")))
                .is_some_and(|v| v == "true")
        };
        let speed = if on("fine_control") {
            FINE_MOVE_DPS
        } else {
            MOVE_DPS
        };
        // Positive pan = camera's left; positive tilt = up (as in the widget).
        let (pan, tilt) = match b {
            Button::Left => (speed, 0.0),
            Button::Right => (-speed, 0.0),
            Button::Up => (0.0, speed),
            _ => (0.0, -speed),
        };
        let pan = if on("swap_pan") { -pan } else { pan };
        let tilt = if on("swap_tilt") { -tilt } else { tilt };
        let dev = cam.open().map_err(|e| e.to_string())?;
        dev.gimbal_speed(pan, tilt).map_err(|e| e.to_string())?;
        Ok(Motion {
            dev: Some(dev),
            last_seen: Instant::now(),
        })
    }

    /// Stop, then re-sync the cached position with where the gimbal is.
    fn stop(self) {
        let Some(dev) = self.dev else {
            println!("move stop");
            return;
        };
        if dev.gimbal_speed(0.0, 0.0).is_err() {
            std::thread::sleep(Duration::from_millis(50));
            if let Err(e) = dev.gimbal_speed(0.0, 0.0) {
                eprintln!("tiny-remote: could not stop the gimbal: {e}");
            }
        }
        std::thread::sleep(Duration::from_millis(150)); // let it settle
        let _ = dev.sync_gimbal();
    }
}

fn main() {
    let mut device = None;
    let mut grab = true;
    let mut dry_run = false;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--device" | "-d" => device = args.next(),
            "--no-grab" => grab = false,
            "--dry-run" => dry_run = true,
            "-h" | "--help" => {
                println!("{HELP}");
                return;
            }
            _ => {
                eprintln!("tiny-remote: unknown argument '{a}'; see --help");
                std::process::exit(2);
            }
        }
    }

    let busy = Arc::new(AtomicBool::new(false));
    let (tx, rx) = mpsc::channel::<Button>();
    {
        let busy = busy.clone();
        std::thread::spawn(move || {
            let mut runner = Runner::new(dry_run);
            for b in rx {
                busy.store(true, Ordering::SeqCst);
                runner.run(b);
                busy.store(false, Ordering::SeqCst);
            }
        });
    }

    let mut last_problem = String::new();
    loop {
        let Some(path) = device
            .clone()
            .map(std::path::PathBuf::from)
            .or_else(remote::find_remote)
        else {
            note(
                &mut last_problem,
                "waiting for the OBSBOT remote dongle".into(),
            );
            std::thread::sleep(Duration::from_secs(3));
            continue;
        };
        let mut file = match std::fs::File::open(&path) {
            Ok(f) => f,
            Err(e) => {
                let hint = if e.kind() == std::io::ErrorKind::PermissionDenied {
                    " (install the udev rule: sudo ./packaging/install-root.sh, then replug)"
                } else {
                    ""
                };
                note(
                    &mut last_problem,
                    format!("cannot open {}: {e}{hint}", path.display()),
                );
                std::thread::sleep(Duration::from_secs(5));
                continue;
            }
        };
        if grab && unsafe { libc::ioctl(file.as_raw_fd(), EVIOCGRAB as _, 1 as libc::c_int) } < 0 {
            eprintln!(
                "tiny-remote: could not grab {} ({}); shortcuts will also reach other apps",
                path.display(),
                std::io::Error::last_os_error()
            );
        }
        last_problem.clear();
        eprintln!("tiny-remote: listening on {}", path.display());

        let mut decoder = Decoder::default();
        let mut last_repeat = Instant::now();
        let mut motion: Option<Motion> = None;
        // A hold whose move could not start: don't retry on every repeat and
        // don't turn the release into a tap step.
        let mut hold_failed = false;
        let mut buf = vec![0u8; EVENT_SIZE * 64];
        loop {
            // Wake at least every 100 ms so a held move can be stopped by the
            // watchdog even if no event arrives.
            let mut pfd = libc::pollfd {
                fd: file.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            let ready = unsafe { libc::poll(&mut pfd, 1, 100) };
            if motion
                .as_ref()
                .is_some_and(|m| m.last_seen.elapsed() > MOVE_WATCHDOG)
            {
                motion.take().unwrap().stop();
            }
            if ready <= 0 {
                continue;
            }
            let n = match file.read(&mut buf) {
                Ok(0) | Err(_) => break, // unplugged
                Ok(n) => n,
            };
            for ev in buf[..n].chunks_exact(EVENT_SIZE) {
                let Some((kind, code, value)) = remote::parse_event(ev) else {
                    continue;
                };
                if kind != EV_KEY {
                    continue;
                }
                match decoder.key(code, value) {
                    // Directions act on release (tap) or first repeat (hold).
                    Some(Press::Down(b)) if is_direction(b) => hold_failed = false,
                    Some(Press::Repeat(b)) if is_direction(b) => match &mut motion {
                        Some(m) => m.last_seen = Instant::now(),
                        None if hold_failed => {}
                        None => match Motion::start(b, dry_run) {
                            Ok(m) => motion = Some(m),
                            Err(e) => {
                                hold_failed = true;
                                eprintln!("tiny-remote: {e}");
                                notify(&e);
                            }
                        },
                    },
                    Some(Press::Up(b)) if is_direction(b) => match motion.take() {
                        Some(m) => m.stop(),
                        None if hold_failed => hold_failed = false,
                        None => {
                            let _ = tx.send(b); // a tap: one step
                        }
                    },
                    Some(Press::Down(b)) => {
                        last_repeat = Instant::now();
                        let _ = tx.send(b);
                    }
                    // Held zoom: keep stepping, but never pile up commands
                    // behind a slow camera.
                    Some(Press::Repeat(b))
                        if last_repeat.elapsed() >= REPEAT_EVERY
                            && !busy.load(Ordering::SeqCst) =>
                    {
                        last_repeat = Instant::now();
                        let _ = tx.send(b);
                    }
                    _ => {}
                }
            }
        }
        if let Some(m) = motion.take() {
            m.stop();
        }
        eprintln!("tiny-remote: remote disconnected; waiting for it to return");
    }
}

/// Log a waiting/error state once, not every retry.
fn note(last: &mut String, msg: String) {
    if *last != msg {
        eprintln!("tiny-remote: {msg}");
        *last = msg;
    }
}

struct Runner {
    tinyctl: std::path::PathBuf,
    dry_run: bool,
    /// (fine, swap_pan, swap_tilt) for the selected camera, briefly cached
    /// so a held D-pad does not re-read it on every step.
    ui: Option<(Instant, (bool, bool, bool))>,
    last_error: Option<(Instant, String)>,
}

impl Runner {
    fn new(dry_run: bool) -> Self {
        // Prefer the tinyctl installed next to this binary.
        let sibling = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.join("tinyctl")))
            .filter(|p| p.exists());
        Runner {
            tinyctl: sibling.unwrap_or_else(|| "tinyctl".into()),
            dry_run,
            ui: None,
            last_error: None,
        }
    }

    fn tc(&self, args: &[&str]) -> Result<String, String> {
        if self.dry_run {
            println!("tinyctl {}", args.join(" "));
            return Ok(String::new());
        }
        let out = Command::new(&self.tinyctl)
            .args(args)
            .output()
            .map_err(|e| format!("cannot run tinyctl: {e}"))?;
        if out.status.success() {
            Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
        } else {
            let err = String::from_utf8_lossy(&out.stderr);
            Err(err
                .lines()
                .find(|l| !l.trim().is_empty())
                .unwrap_or("command failed")
                .trim_start_matches("tinyctl: ")
                .to_string())
        }
    }

    fn sel(&self, rest: &[&str]) -> Result<String, String> {
        let mut a = vec!["-c", "selected"];
        a.extend_from_slice(rest);
        self.tc(&a)
    }

    fn ui_prefs(&mut self) -> (bool, bool, bool) {
        if let Some((t, v)) = self.ui {
            if t.elapsed() < Duration::from_secs(3) {
                return v;
            }
        }
        let j = self.sel(&["ui"]).unwrap_or_default();
        let v = (
            json_bool(&j, "fine_control"),
            json_bool(&j, "swap_pan"),
            json_bool(&j, "swap_tilt"),
        );
        self.ui = Some((Instant::now(), v));
        v
    }

    fn run(&mut self, b: Button) {
        let result = match b {
            Button::Power => self.sel(&["toggle"]).map(|s| Some(format!("Camera {s}"))),
            Button::Track => self
                .sel(&["track", "toggle"])
                .map(|s| Some(format!("Tracking {s}"))),
            Button::CloseUp | Button::Hand | Button::Whiteboard | Button::Desk => {
                let mode = match b {
                    Button::CloseUp => "close-up",
                    Button::Hand => "hand",
                    Button::Whiteboard => "whiteboard",
                    _ => "desk",
                };
                let current = self
                    .sel(&["status", "--json"])
                    .ok()
                    .and_then(|j| json_str(&j, "tracking"));
                if current.as_deref() == Some(mode) {
                    self.sel(&["track", "off"])
                        .map(|_| Some("Tracking off".into()))
                } else {
                    self.sel(&["track", mode])
                        .map(|_| Some(format!("Tracking: {mode}")))
                }
            }
            Button::ZoomIn => self.sel(&["zoom", "by", "10"]).map(|_| None),
            Button::ZoomOut => self.sel(&["zoom", "by", "-10"]).map(|_| None),
            Button::Up | Button::Down | Button::Left | Button::Right => {
                let (fine, swap_pan, swap_tilt) = self.ui_prefs();
                let step = if fine { 1 } else { 5 };
                // Positive pan turns the camera to its left (as in the widget).
                let (axis, dir, swap) = match b {
                    Button::Left => ("pan", 1, swap_pan),
                    Button::Right => ("pan", -1, swap_pan),
                    Button::Up => ("tilt", 1, swap_tilt),
                    _ => ("tilt", -1, swap_tilt),
                };
                let delta = (if swap { -dir } else { dir } * step).to_string();
                self.sel(&[axis, "by", &delta]).map(|_| None)
            }
            Button::Center => self.sel(&["recenter"]).map(|_| None),
            Button::Preset(n) => {
                let n = n.to_string();
                self.sel(&["preset", "recall", &n])
                    .map(|_| Some(format!("Preset {n}")))
            }
            Button::Camera(n) => {
                self.ui = None; // preferences are per camera
                self.tc(&["select", &n.to_string()]).map(|_| None)
            }
        };
        match result {
            Ok(Some(msg)) => notify(&msg),
            Ok(None) => {}
            Err(e) => {
                // A held button can fail repeatedly; tell the user once.
                let repeat = self
                    .last_error
                    .as_ref()
                    .is_some_and(|(t, m)| *m == e && t.elapsed() < Duration::from_secs(3));
                if !repeat {
                    eprintln!("tiny-remote: {e}");
                    notify(&e);
                }
                self.last_error = Some((Instant::now(), e));
            }
        }
    }
}

fn notify(msg: &str) {
    let _ = Command::new("notify-send")
        .args([
            "-a",
            "tiny-control",
            "-t",
            "1500",
            "-h",
            "string:x-canonical-private-synchronous:tiny-remote",
            "Camera remote",
            msg,
        ])
        .spawn()
        // Reap it in the background so a slow notification daemon never
        // delays the next button.
        .map(|mut c| {
            std::thread::spawn(move || c.wait());
        });
}

/// `"key":"value"` from tinyctl's flat JSON.
fn json_str(j: &str, key: &str) -> Option<String> {
    let pat = format!("\"{key}\":\"");
    let start = j.find(&pat)? + pat.len();
    let end = j[start..].find('"')? + start;
    Some(j[start..end].to_string())
}

fn json_bool(j: &str, key: &str) -> bool {
    j.contains(&format!("\"{key}\":true"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_helpers() {
        let j = r#"{"camera":"X","tracking":"desk","ui":{"fine_control":true,"swap_pan":false}}"#;
        assert_eq!(json_str(j, "tracking").as_deref(), Some("desk"));
        assert_eq!(json_str(j, "missing"), None);
        assert!(json_bool(j, "fine_control"));
        assert!(!json_bool(j, "swap_pan"));
        assert!(!json_bool(j, "swap_tilt"));
    }
}
