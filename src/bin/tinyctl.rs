//! tinyctl — command-line control for OBSBOT Tiny 2 / Tiny 3 series webcams,
//! one or several at a time.

use std::process::ExitCode;
use tiny_control::camera::{self, Cam};
use tiny_control::config;
use tiny_control::model::{self, Feature};
use tiny_control::{controls, discover, Device, Error, Result, TrackMode};

const USAGE: &str = "\
tinyctl — control OBSBOT Tiny 2 / Tiny 3 / Tiny 3 Lite cameras on Linux

USAGE:
    tinyctl list [--json]                  Show connected cameras
    tinyctl -c <CAMERA> [--json] <command> Control a camera
    tinyctl select <CAMERA>                Remember a camera as 'selected'

Every device command needs -c/--camera. There is no implicit default camera.

CAMERA is one of:
    <alias>               A name you gave it with `tinyctl alias`
    <serial>              Its serial number (see `tinyctl list`)
    <N>                   Its number in `tinyctl list` (ordered by USB port)
    <usb port>            e.g. 1-4.4, or its id port-1-4.4
    /dev/videoN           Or a /dev/v4l/by-id / by-path link
    selected              The camera last chosen with `tinyctl select`
    all                   Every connected camera, one after another

CAMERAS
    list                  Connected cameras: number, id, alias, model, USB port
    select <CAMERA>       Set the selected camera (used by the bar widget/hotkeys)
    select next|prev      Cycle the selection through connected cameras
    selected              Print the selected camera's id
    alias <CAMERA> <name> Name a camera (letters, digits, - and _)
    alias <CAMERA> --clear

POWER
    sleep                 Put the camera to sleep (LED off, gimbal parks)
    wake                  Wake the camera (LED on, gimbal lifts)
    auto                  Wake during video or microphone use; release when idle
    toggle                Sleep if awake, wake if asleep

INFO
    status [--raw]        Show current state (control read; does NOT wake a sleeping camera)
    probe fov|face-ae|track-speed
                          Find which status byte reports a setting (cycles its values)
    probe boot            Dump the camera's power-on records (read-only diagnostic)
    info                  Serial number, UUID, model, device node
    power                 USB power state from sysfs (opens nothing)
    node                  Print the capture device path (opens nothing)
    support               Which features are verified on this model (opens nothing)

AI TRACKING
    track on|off|toggle   Enable/disable tracking
    track <mode>          normal|upper|close|headless|lower|group|hand|whiteboard|desk
    track speed std|sport Tracking responsiveness (experimental)

GIMBAL
    recenter              Return gimbal to center (a.k.a. park/home)
    pan <deg>             Absolute pan  (range depends on the model; see `status`)
    tilt <deg>            Absolute tilt (range depends on the model; see `status`)
    zoom <0..100>         Absolute zoom
    preset list           The camera's 3 preset slots and its power-on position
    preset recall <1|2|3> Move to an on-camera preset (shared with OBSBOT Center)
    preset power-on <1|2|3>  Make that preset (position + Additional settings) the
                          camera's power-on state, like 'As Initial State'
    preset save|delete <1|2|3>, preset power-on off
                          DISABLED: use OBSBOT Center

IMAGE
    wb auto               Auto white balance (safe fallback)
    wb temp <2000..10000> Manual white-balance temperature (Kelvin)
    wb pin                Pin manual WB to this camera's configured target
    hdr on|off            HDR / WDR
    fov wide|medium|narrow
    exposure auto         Auto exposure
    exposure <1..2500>    Manual exposure time (0.1ms units)
    face-ae on|off        Face-priority auto exposure

MISC
    reset                 Safe defaults: wb auto, tracking off, recenter, wake
    -h, --help            This help

Settings are remembered per camera in ~/.config/tiny-control/config and
re-applied by tiny-guard. Features not yet confirmed on a model are still sent,
but you get an error if the camera does not confirm them.
With -c all and --json, one JSON object is printed per camera, one per line.
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(Error::Usage(msg)) => {
            eprintln!("tinyctl: {msg}\n\nRun `tinyctl --help` for usage.");
            ExitCode::from(2)
        }
        Err(e) => {
            eprintln!("tinyctl: {e}");
            ExitCode::FAILURE
        }
    }
}

struct Opts {
    json: bool,
    camera: Option<String>,
    device: Option<String>,
}

fn take_value(args: &mut Vec<String>, i: usize, flag: &str) -> Result<String> {
    args.remove(i);
    if i >= args.len() {
        return Err(Error::Usage(format!("{flag} needs a value")));
    }
    Ok(args.remove(i))
}

fn run(mut args: Vec<String>) -> Result<()> {
    let mut o = Opts {
        json: false,
        camera: None,
        device: None,
    };
    // Global flags are accepted anywhere, so a keybinding can append them.
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--json" => {
                o.json = true;
                args.remove(i);
            }
            "-c" | "--camera" => o.camera = Some(take_value(&mut args, i, "--camera")?),
            "-d" | "--device" => o.device = Some(take_value(&mut args, i, "--device")?),
            "-h" | "--help" => {
                print!("{USAGE}");
                return Ok(());
            }
            s if s.starts_with("--camera=") => {
                o.camera = Some(s["--camera=".len()..].to_string());
                args.remove(i);
            }
            _ => i += 1,
        }
    }

    let cmd = args
        .first()
        .cloned()
        .ok_or_else(|| Error::Usage("no command given".into()))?;
    let rest = args[1..].to_vec();

    // Commands that manage the camera list rather than a camera.
    match cmd.as_str() {
        "help" => {
            print!("{USAGE}");
            return Ok(());
        }
        "list" | "ls" => return cmd_list(o.json),
        "selected" => {
            let id =
                config::selected().ok_or_else(|| Error::Config("no camera is selected".into()))?;
            println!("{id}");
            return Ok(());
        }
        "select" => {
            let sel = rest
                .first()
                .or(o.camera.as_ref())
                .ok_or_else(|| Error::Usage("select <CAMERA>|next|prev".into()))?;
            let cams = camera::all();
            let cam = match sel.as_str() {
                "next" | "prev" => step_selection(&cams, sel == "next")?,
                _ => single(camera::resolve(sel, &cams)?, "select")?,
            };
            config::set_selected(&cam.id)?;
            println!("selected {} ({})", cam.label(), cam.info.model.name);
            return Ok(());
        }
        "alias" => return cmd_alias(&o, &rest),
        _ => {}
    }

    let cams = targets(&o)?;
    let many = cams.len() > 1;
    let mut failures = 0;
    for cam in &cams {
        if many && !o.json {
            println!(
                "== {} ({}, {}) ==",
                cam.label(),
                cam.info.model.name,
                cam.id
            );
        }
        if let Err(e) = run_one(cam, &cmd, &rest, o.json) {
            if !many {
                return Err(e);
            }
            eprintln!("tinyctl: [{}] {e}", cam.label());
            failures += 1;
        }
    }
    if failures > 0 {
        return Err(Error::Config(format!(
            "{failures} of {} cameras failed",
            cams.len()
        )));
    }
    Ok(())
}

/// The camera after (or before) the selected one in `list` order, wrapping.
fn step_selection(cams: &[Cam], forward: bool) -> Result<Cam> {
    if cams.is_empty() {
        return Err(Error::DeviceNotFound(
            "no OBSBOT cameras are connected".into(),
        ));
    }
    let n = cams.len();
    let cur = config::selected().and_then(|id| cams.iter().position(|c| c.id == id));
    let i = match (cur, forward) {
        (None, true) => 0,
        (None, false) => n - 1,
        (Some(i), true) => (i + 1) % n,
        (Some(i), false) => (i + n - 1) % n,
    };
    Ok(cams[i].clone())
}

fn single(mut v: Vec<Cam>, what: &str) -> Result<Cam> {
    if v.len() != 1 {
        return Err(Error::Usage(format!("{what} needs exactly one camera")));
    }
    Ok(v.remove(0))
}

/// The cameras a device command acts on. `--device` (used by the udev hook)
/// names a node directly; otherwise `-c` is mandatory.
fn targets(o: &Opts) -> Result<Vec<Cam>> {
    if let Some(path) = &o.device {
        let info = discover::camera_for_path(path)?;
        let serial = camera::read_serial(&info);
        let id = serial.clone().unwrap_or_else(|| camera::port_id(&info));
        let file = config::ConfigFile::load();
        return Ok(vec![Cam {
            alias: file.alias_of(&id).map(str::to_string),
            index: 0,
            provisional: serial.is_none(),
            id,
            serial,
            info,
        }]);
    }
    let sel = o.camera.as_deref().ok_or_else(|| {
        Error::Usage(
            "no camera given: use -c <alias|serial|number|selected|all> (see `tinyctl list`)"
                .into(),
        )
    })?;
    camera::resolve(sel, &camera::all())
}

fn run_one(cam: &Cam, cmd: &str, rest: &[String], json: bool) -> Result<()> {
    // Commands that must NOT open the device (to avoid waking the camera).
    match cmd {
        "power" => return cmd_power(cam, json),
        "node" => {
            println!("{}", cam.info.node);
            return Ok(());
        }
        "support" => return cmd_support(cam, json),
        _ => {}
    }

    let _lock = config::PolicyLock::acquire()?;
    let id = cam.id.as_str();

    let opened = std::cell::OnceCell::new();
    let open = || -> Result<&Device> {
        if opened.get().is_none() {
            let _ = opened.set(cam.open()?);
        }
        Ok(opened.get().unwrap())
    };

    // Gimbal-moving arguments (pan/tilt/zoom) and on/off toggles are validated
    // BEFORE opening the device, so a malformed move (e.g. `tinyctl pan abc`)
    // fails without touching the camera at all.
    match cmd {
        "sleep" => {
            open()?.sleep()?;
            config::save_setting(id, "power", "sleep")
        }
        "wake" => {
            wake_managed(open()?)?;
            config::save_setting(id, "power", "awake")
        }
        "auto" => config::save_setting(id, "power", "auto"),
        "toggle" => {
            let dev = open()?;
            // Reading the vendor sleep bit does NOT wake the camera, so we can
            // decide without disturbing a sleeping camera. Print the resulting
            // state so a keybinding can surface it in a notification.
            if dev.status()?.asleep {
                wake_managed(dev)?;
                config::save_setting(id, "power", "awake")?;
                println!("awake");
            } else {
                dev.sleep()?;
                config::save_setting(id, "power", "sleep")?;
                println!("asleep");
            }
            Ok(())
        }
        "status" => cmd_status(cam, open()?, json, rest.iter().any(|a| a == "--raw")),
        "probe" => cmd_probe(open()?, rest),
        "info" => cmd_info(cam, open()?, json),
        "track" => cmd_track(id, open()?, rest),
        "recenter" | "park" | "home" => open()?.recenter(),
        "pan" => {
            let d = need_finite(rest, "pan degrees")?;
            open()?.set_pan_deg(d)
        }
        "tilt" => {
            let d = need_finite(rest, "tilt degrees")?;
            open()?.set_tilt_deg(d)
        }
        "zoom" => {
            let z = need_i32(rest, "zoom 0..100")?;
            open()?.set_zoom(z)
        }
        "preset" => cmd_preset(id, open()?, rest, json),
        "wb" => cmd_wb(id, open()?, rest),
        "hdr" => {
            let on = need_on_off(rest, "hdr")?;
            open()?.set_hdr(on)?;
            config::save_setting(id, "hdr", if on { "true" } else { "false" })
        }
        "fov" => cmd_fov(id, open()?, rest),
        "exposure" => cmd_exposure(open()?, rest),
        "face-ae" => {
            let on = need_on_off(rest, "face-ae")?;
            open()?.set_face_ae(on)?;
            config::save_setting(id, "face_ae", if on { "true" } else { "false" })
        }
        "reset" => cmd_reset(id, open()?),
        other => Err(Error::Usage(format!("unknown command '{other}'"))),
    }?;

    // A successful ioctl only means transport success; confirm the camera
    // actually applied the setting, and say which model refused. The AI and
    // power transitions can take a moment (longer right after FOV/exposure
    // changes), so poll for up to ~2s before calling it unconfirmed.
    if matches!(
        cmd,
        "sleep" | "wake" | "toggle" | "track" | "hdr" | "wb" | "reset" | "fov" | "face-ae"
    ) {
        let dev = open()?;
        let desired = config::Config::load_for(id);
        let check = || -> Result<Option<Feature>> {
            let actual = dev.status()?;
            Ok(match cmd {
                "sleep" | "wake" | "toggle" => {
                    (actual.asleep != (desired.power == "sleep")).then_some(Feature::Sleep)
                }
                "track" if rest.first().map(String::as_str) == Some("speed") => {
                    let want = desired.track_speed.as_deref() == Some("sport");
                    // Models that don't report speed can't be confirmed; the
                    // model table already refuses it where it is known absent.
                    actual
                        .tracking_sport
                        .is_some_and(|s| s != want)
                        .then_some(Feature::TrackSpeed)
                }
                "track" => (actual.tracking != desired.tracking)
                    .then_some(Feature::Tracking(desired.tracking)),
                "hdr" => (desired.hdr != Some(actual.hdr)).then_some(Feature::Hdr),
                "wb" => (actual.auto_wb != desired.auto_wb
                    || (!desired.auto_wb && actual.wb_temp != desired.wb_temp))
                    .then_some(Feature::WhiteBalance),
                "fov" => {
                    (desired.fov.as_deref() != Some(fov_name(actual.fov))).then_some(Feature::Fov)
                }
                "face-ae" => (desired.face_ae != Some(actual.face_ae)).then_some(Feature::FaceAe),
                "reset" => (actual.asleep || actual.tracking != TrackMode::Off || !actual.auto_wb)
                    .then_some(Feature::Tracking(TrackMode::Off)),
                _ => None,
            })
        };
        let mut failed = None;
        for _ in 0..10 {
            std::thread::sleep(std::time::Duration::from_millis(200));
            failed = check()?;
            if failed.is_none() {
                break;
            }
        }
        if let Some(f) = failed {
            return Err(dev.model().unconfirmed(f));
        }
    }
    Ok(())
}

/// Minimal JSON string escaper for the fields we emit (control chars, quotes,
/// backslashes). Keeps `--json` output valid even if a value contains oddities.
fn json_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

fn json_opt(s: Option<&str>) -> String {
    s.map(|s| format!("\"{}\"", json_str(s)))
        .unwrap_or_else(|| "null".into())
}

/// Common identity fields for every per-camera JSON object.
fn json_camera(cam: &Cam) -> String {
    format!(
        "\"camera\":\"{}\",\"alias\":{},\"model\":\"{}\",\"model_id\":\"{}\",\"verified\":{}",
        json_str(&cam.id),
        json_opt(cam.alias.as_deref()),
        json_str(cam.info.model.name),
        cam.info.model.id,
        cam.info.model.is_verified()
    )
}

fn cmd_list(json: bool) -> Result<()> {
    let cams = camera::all();
    let selected = config::selected();
    if json {
        let items: Vec<String> = cams
            .iter()
            .map(|c| {
                format!(
                    "{{\"index\":{},{},\"serial\":{},\"product\":\"{}\",\"usb_pid\":\"{}\",\
                     \"usb_port\":\"{}\",\"node\":\"{}\",\"usb_power\":{},\"selected\":{}}}",
                    c.index,
                    json_camera(c),
                    json_opt(c.serial.as_deref()),
                    json_str(&c.info.product),
                    json_str(&c.info.pid),
                    json_str(&c.info.usb_path),
                    json_str(&c.info.node),
                    json_opt(discover::usb_power_state(&c.info.node).as_deref()),
                    selected.as_deref() == Some(c.id.as_str())
                )
            })
            .collect();
        println!("[{}]", items.join(","));
        return Ok(());
    }
    if cams.is_empty() {
        println!("(no OBSBOT cameras connected)");
        return Ok(());
    }
    println!(
        "  #  {:<16} {:<10} {:<20} {:<8} NODE",
        "ID", "ALIAS", "MODEL", "USB"
    );
    for c in &cams {
        let mark = if selected.as_deref() == Some(c.id.as_str()) {
            '*'
        } else {
            ' '
        };
        println!(
            "{mark} {}  {:<16} {:<10} {:<20} {:<8} {}{}",
            c.index,
            c.id,
            c.alias.as_deref().unwrap_or("-"),
            c.info.model.name.trim_start_matches("OBSBOT "),
            c.info.usb_path,
            c.info.node,
            if c.info.model.is_verified() {
                ""
            } else {
                "  (untested model)"
            }
        );
    }
    if cams.len() > 4 {
        eprintln!(
            "note: {} cameras connected; tiny-control is tested with up to 4",
            cams.len()
        );
    }
    Ok(())
}

fn cmd_alias(o: &Opts, rest: &[String]) -> Result<()> {
    let (sel, name) = match (o.camera.as_deref(), rest) {
        (Some(c), [n]) => (c, n.as_str()),
        (_, [c, n]) => (c.as_str(), n.as_str()),
        _ => return Err(Error::Usage("alias <CAMERA> <name>|--clear".into())),
    };
    let cam = single(camera::resolve(sel, &camera::all())?, "alias")?;
    if cam.provisional {
        eprintln!(
            "tinyctl: warning: could not read {}'s serial; the alias is tied to USB port {}",
            cam.info.model.name, cam.info.usb_path
        );
    }
    let _lock = config::PolicyLock::acquire()?;
    if name == "--clear" {
        config::set_alias(&cam.id, None)?;
        println!("cleared alias for {}", cam.id);
    } else {
        config::set_alias(&cam.id, Some(name))?;
        println!("{} is now '{name}'", cam.id);
    }
    Ok(())
}

fn cmd_support(cam: &Cam, json: bool) -> Result<()> {
    let m = cam.info.model;
    let rows: Vec<(String, &str, &str)> = model::feature_table()
        .into_iter()
        .map(|(f, cmd)| (f.label(), cmd, m.support(f).label()))
        .collect();
    if json {
        let items: Vec<String> = rows
            .iter()
            .map(|(f, c, s)| {
                format!(
                    "{{\"feature\":\"{}\",\"command\":\"{}\",\"support\":\"{s}\"}}",
                    json_str(f),
                    json_str(c)
                )
            })
            .collect();
        println!(
            "{{{},\"features\":[{}]}}",
            json_camera(cam),
            items.join(",")
        );
        return Ok(());
    }
    println!("{} ({})", cam.label(), m.name);
    for (f, c, s) in &rows {
        println!("  {s:<11} {f:<28} tinyctl {c}");
    }
    Ok(())
}

fn cmd_power(cam: &Cam, json: bool) -> Result<()> {
    let state = discover::usb_power_state(&cam.info.node).unwrap_or_else(|| "unknown".into());
    if json {
        println!(
            "{{{},\"usb_power\":\"{}\",\"node\":\"{}\"}}",
            json_camera(cam),
            json_str(&state),
            json_str(&cam.info.node)
        );
    } else {
        println!("USB power state : {state}");
        println!("device node     : {}", cam.info.node);
        println!(
            "(this is USB autosuspend, not the vendor sleep bit — the LED is the ground truth)"
        );
    }
    Ok(())
}

fn fov_name(level: u8) -> &'static str {
    match level {
        0 => "wide",
        1 => "medium",
        2 => "narrow",
        _ => "unknown",
    }
}

fn hex(b: &[u8]) -> String {
    b.iter()
        .map(|x| format!("{x:02x}"))
        .collect::<Vec<_>>()
        .join(" ")
}

fn cmd_status(cam: &Cam, dev: &Device, json: bool, raw: bool) -> Result<()> {
    let s = dev.status()?;
    if raw && !json {
        println!("camera      : {} ({})", cam.label(), cam.info.model.name);
        for (i, row) in s.raw.chunks(16).enumerate() {
            println!("  {:#04x}: {}", i * 16, hex(row));
        }
        return Ok(());
    }
    let desired = config::Config::load_for(&cam.id);
    let (pan_min, pan_max) = dev.gimbal_range_deg(controls::CID_PAN_ABSOLUTE);
    let (tilt_min, tilt_max) = dev.gimbal_range_deg(controls::CID_TILT_ABSOLUTE);
    if json {
        println!(
            "{{{},\"asleep\":{},\"tracking\":\"{}\",\"tracking_sport\":{},\"hdr\":{},\
             \"auto_wb\":{},\"wb_temp\":{},\"pan_deg\":{:.1},\"tilt_deg\":{:.1},\
             \"zoom\":{},\"auto_exposure\":{},\"auto_sleep_seconds\":{},\"power_policy\":\"{}\",\
             \"desired_tracking\":\"{}\",\"pan_range\":[{:.0},{:.0}],\"tilt_range\":[{:.0},{:.0}],\
             \"track_speed\":{},\"fov\":\"{}\",\"face_ae\":{}{}}}",
            json_camera(cam),
            s.asleep,
            json_str(s.tracking.label()),
            s.tracking_sport == Some(true),
            s.hdr,
            s.auto_wb,
            s.wb_temp,
            s.pan_deg,
            s.tilt_deg,
            s.zoom,
            s.auto_exposure,
            s.auto_sleep_seconds,
            desired.power,
            desired.tracking.label(),
            pan_min,
            pan_max,
            tilt_min,
            tilt_max,
            json_opt(s.tracking_sport.map(|b| if b { "sport" } else { "std" })),
            fov_name(s.fov),
            s.face_ae,
            if raw {
                format!(",\"raw\":\"{}\"", hex(&s.raw))
            } else {
                String::new()
            }
        );
    } else {
        println!("camera      : {} ({})", cam.label(), cam.info.model.name);
        println!(
            "power       : {}",
            if s.asleep { "asleep" } else { "awake" }
        );
        println!(
            "sleep timer : {} seconds (0 = disabled)",
            s.auto_sleep_seconds
        );
        println!("tracking    : {}", s.tracking.label());
        println!(
            "track speed : {}",
            match s.tracking_sport {
                Some(true) => "sport",
                Some(false) => "std",
                None => "n/a (not supported on this model)",
            }
        );
        println!("hdr         : {}", onoff(s.hdr));
        println!(
            "white bal.  : {}",
            if s.auto_wb {
                "auto".to_string()
            } else {
                format!("manual {}K", s.wb_temp)
            }
        );
        println!(
            "exposure    : {}",
            if s.auto_exposure { "auto" } else { "manual" }
        );
        println!("fov         : {}", fov_name(s.fov));
        println!("face AE     : {}", onoff(s.face_ae));
        println!(
            "gimbal      : pan {:.1}° ({pan_min:.0}..{pan_max:.0}), tilt {:.1}° ({tilt_min:.0}..{tilt_max:.0}), zoom {}",
            s.pan_deg, s.tilt_deg, s.zoom
        );
    }
    Ok(())
}

/// Find where a setting lives in the 60-byte status block: set each value in
/// turn, snapshot the block, and print the bytes that track the value. Uses
/// only the normal SET for that setting plus status reads (GET_CUR).
fn cmd_probe(dev: &Device, rest: &[String]) -> Result<()> {
    type Setter = Box<dyn Fn(&Device) -> Result<()>>;
    let what = rest.first().map(String::as_str).unwrap_or("");
    let steps: Vec<(&str, Setter)> = match what {
        "fov" => [("wide", 0u8), ("medium", 1), ("narrow", 2), ("wide", 0)]
            .into_iter()
            .map(|(n, l)| (n, Box::new(move |d: &Device| d.set_fov(l)) as Setter))
            .collect(),
        "face-ae" => [("off", false), ("on", true), ("off", false), ("on", true)]
            .into_iter()
            .map(|(n, v)| (n, Box::new(move |d: &Device| d.set_face_ae(v)) as Setter))
            .collect(),
        "track-speed" => [
            ("std", false),
            ("sport", true),
            ("std", false),
            ("sport", true),
        ]
        .into_iter()
        .map(|(n, v)| {
            (
                n,
                Box::new(move |d: &Device| d.set_track_speed(v)) as Setter,
            )
        })
        .collect(),
        "boot" => return probe_boot(dev),
        _ => return Err(Error::Usage("probe fov|face-ae|track-speed|boot".into())),
    };
    if what == "face-ae" {
        dev.set_auto_exposure(true)?;
    }
    let mut snaps: Vec<(&str, [u8; 60])> = Vec::new();
    for (name, set) in &steps {
        set(dev)?;
        std::thread::sleep(std::time::Duration::from_millis(600));
        snaps.push((name, dev.raw_status()?));
    }
    let hits = tracking_offsets(&snaps);
    println!("probe {what} on the {}:", dev.model().name);
    if hits.is_empty() {
        println!("  no status byte follows this setting (it may not be reported, or not applied)");
    }
    for off in hits {
        let vals: Vec<String> = snaps
            .iter()
            .map(|(n, s)| format!("{n}={:#04x}", s[off]))
            .collect();
        println!("  offset {off:#04x}: {}", vals.join("  "));
    }
    let last = steps.last().map(|(n, _)| *n).unwrap_or("");
    println!("(left at {what} = {last})");
    Ok(())
}

/// Dump the power-on records and raw preset entries (read-only).
fn probe_boot(dev: &Device) -> Result<()> {
    println!("probe boot on the {} (read-only):", dev.model().name);
    for (name, r) in dev.boot_records() {
        match r {
            Ok(p) if p.is_empty() => println!("  {name}: (empty reply)"),
            Ok(p) => println!("  {name}: {} bytes\n    {}", p.len(), hex(&p)),
            Err(e) => println!("  {name}: {e}"),
        }
    }
    match dev.raw_preset_entries() {
        Ok(entries) => {
            for e in entries {
                println!("  preset entry: {}", hex(&e));
            }
        }
        Err(e) => println!("  preset entries: {e}"),
    }
    let s = dev.status()?;
    println!(
        "  current: tracking {}, {}",
        s.tracking.label(),
        if s.asleep { "asleep" } else { "awake" }
    );
    Ok(())
}

/// Offsets whose byte differs between values and is identical every time the
/// same value is set — i.e. bytes that report the setting.
fn tracking_offsets(snaps: &[(&str, [u8; 60])]) -> Vec<usize> {
    (0..60)
        .filter(|&off| {
            let varies = snaps.iter().any(|(_, s)| s[off] != snaps[0].1[off]);
            let consistent = snaps.iter().all(|(n, s)| {
                snaps
                    .iter()
                    .filter(|(m, _)| m == n)
                    .all(|(_, t)| t[off] == s[off])
            });
            varies && consistent
        })
        .collect()
}

fn cmd_info(cam: &Cam, dev: &Device, json: bool) -> Result<()> {
    let serial = dev.serial().unwrap_or_else(|_| "(unavailable)".into());
    let uuid = dev.uuid_hex().unwrap_or_else(|_| "(unavailable)".into());
    if json {
        println!(
            "{{{},\"serial\":\"{}\",\"uuid\":\"{}\",\"node\":\"{}\",\"usb_port\":\"{}\",\"usb_pid\":\"{}\"}}",
            json_camera(cam),
            json_str(&serial),
            json_str(&uuid),
            json_str(dev.path()),
            json_str(&cam.info.usb_path),
            json_str(&cam.info.pid)
        );
    } else {
        println!(
            "model  : {}{}",
            cam.info.model.name,
            if cam.info.model.is_verified() {
                ""
            } else {
                " (not yet verified on hardware by this project)"
            }
        );
        println!("serial : {serial}");
        println!("uuid   : {uuid}");
        println!("alias  : {}", cam.alias.as_deref().unwrap_or("-"));
        println!("usb    : {} (3564:{})", cam.info.usb_path, cam.info.pid);
        println!("node   : {}", dev.path());
    }
    Ok(())
}

fn cmd_track(id: &str, dev: &Device, rest: &[String]) -> Result<()> {
    let arg = rest
        .first()
        .ok_or_else(|| Error::Usage("track needs on|off|toggle|<mode>|speed".into()))?;
    if arg == "toggle" {
        let on = dev.status()?.tracking != TrackMode::Off;
        let next = if on {
            TrackMode::Off
        } else {
            TrackMode::Normal
        };
        dev.set_tracking(next)?;
        config::save_setting(id, "tracking", next.label())?;
        println!("{}", if on { "off" } else { "on" });
        return Ok(());
    }
    if arg == "speed" {
        let s = rest.get(1).map(String::as_str);
        let sport = match s {
            Some("sport") | Some("fast") => true,
            Some("std") | Some("standard") | Some("normal") => false,
            _ => return Err(Error::Usage("track speed std|sport".into())),
        };
        dev.set_track_speed(sport)?;
        return config::save_setting(id, "track_speed", if sport { "sport" } else { "std" });
    }
    let mode = TrackMode::from_str(arg)
        .ok_or_else(|| Error::Usage(format!("unknown tracking mode '{arg}'")))?;
    dev.set_tracking(mode)?;
    config::save_setting(id, "tracking", mode.label())
}

fn pose_str(p: &tiny_control::device::Pose) -> String {
    format!(
        "pan {:.1}°, tilt {:.1}°, zoom {:.2}x",
        p.pan_deg, p.tilt_deg, p.zoom_ratio
    )
}

fn actions_summary(a: &tiny_control::device::PresetActions) -> String {
    let onoff_opt = |v: Option<bool>| v.map(|b| if b { "on" } else { "off" });
    let target = |v: Option<bool>| match v {
        Some(true) => " (face)",
        Some(false) => " (global)",
        None => "",
    };
    let mut parts = vec![match a.tracking {
        Some(TrackMode::Off) => "no tracking".to_string(),
        Some(m) => format!("tracking {}", m.label()),
        None => format!("tracking mode {:#04x}", a.tracking_raw),
    }];
    if let Some(s) = a.sport {
        parts.push(if s { "motion speed" } else { "standard speed" }.into());
    }
    if let Some(af) = onoff_opt(a.auto_focus) {
        parts.push(format!("AF {af}{}", target(a.face_focus)));
    }
    if let Some(ae) = onoff_opt(a.auto_exposure) {
        let comp = a
            .exposure_comp_thirds
            .map(|t| format!(" {:+.1}EV", t as f64 / 3.0))
            .unwrap_or_default();
        parts.push(format!("AE {ae}{}{comp}", target(a.face_ae)));
    }
    match (a.auto_wb, a.wb_temp) {
        (Some(true), _) => parts.push("WB auto".into()),
        (Some(false), Some(t)) => parts.push(format!("WB {t}K")),
        _ => {}
    }
    match (a.blur, a.blur_level) {
        (Some(true), Some(l)) => parts.push(format!("blur {l}")),
        (Some(false), _) => parts.push("blur off".into()),
        _ => {}
    }
    let img: Vec<String> = [
        ("contrast", a.contrast),
        ("sat", a.saturation),
        ("sharp", a.sharpness),
        ("hue", a.hue),
    ]
    .iter()
    .filter_map(|(n, v)| v.map(|v| format!("{n} {v}")))
    .collect();
    if !img.is_empty() {
        parts.push(img.join(" "));
    }
    parts.join(", ")
}

/// Save the guard-enforced settings (tracking, speed, WB) from a preset.
fn save_actions_config(id: &str, a: &tiny_control::device::PresetActions) -> Result<()> {
    if let Some(m) = a.tracking {
        config::save_setting(id, "tracking", m.label())?;
    }
    if let Some(s) = a.sport {
        config::save_setting(id, "track_speed", if s { "sport" } else { "std" })?;
    }
    match (a.auto_wb, a.wb_temp) {
        (Some(true), _) => config::save_setting(id, "auto_wb", "true")?,
        (Some(false), Some(t)) => {
            config::save_setting(id, "wb_temp", &t.to_string())?;
            config::save_setting(id, "auto_wb", "false")?;
        }
        _ => {}
    }
    Ok(())
}

/// Apply a preset's settings with commands verified on hardware, and save
/// the ones tiny-guard enforces so it does not revert them. Returns the
/// settings that could not be applied.
fn apply_actions(
    id: &str,
    dev: &Device,
    a: &tiny_control::device::PresetActions,
) -> Result<Vec<String>> {
    let mut notes = Vec::new();
    let mut note = |what: &str, r: Result<()>| {
        if let Err(e) = r {
            notes.push(format!("{what} ({e})"));
        }
    };
    for (cid, v, name) in [
        (controls::CID_CONTRAST, a.contrast, "contrast"),
        (controls::CID_SATURATION, a.saturation, "saturation"),
        (controls::CID_SHARPNESS, a.sharpness, "sharpness"),
        (controls::CID_HUE, a.hue, "hue"),
    ] {
        if let Some(v) = v {
            note(name, dev.fd().set_ctrl(cid, v as i32));
        }
    }
    if let Some(af) = a.auto_focus {
        note(
            "auto focus",
            dev.fd().set_ctrl(controls::CID_FOCUS_AUTO, af as i32),
        );
    }
    if let Some(ae) = a.auto_exposure {
        note("auto exposure", dev.set_auto_exposure(ae));
        if ae {
            if let Some(face) = a.face_ae {
                note("exposure target", dev.set_face_ae(face));
            }
        }
    }
    match (a.auto_wb, a.wb_temp) {
        (Some(true), _) => {
            note("white balance", dev.set_wb_auto());
            config::save_setting(id, "auto_wb", "true")?;
        }
        (Some(false), Some(t)) => {
            note("white balance", dev.set_wb_temp(t));
            config::save_setting(id, "wb_temp", &t.to_string())?;
            config::save_setting(id, "auto_wb", "false")?;
        }
        _ => {}
    }
    if let Some(sport) = a.sport {
        let r = dev.set_track_speed(sport);
        if r.is_ok() {
            config::save_setting(id, "track_speed", if sport { "sport" } else { "std" })?;
        }
        note("tracking speed", r);
    }
    match a.tracking {
        Some(m) => {
            let r = dev.set_tracking(m);
            if r.is_ok() {
                config::save_setting(id, "tracking", m.label())?;
            }
            note("tracking mode", r);
        }
        None => notes.push(format!(
            "tracking mode {:#04x} (not decoded yet)",
            a.tracking_raw
        )),
    }
    // Stored by OBSBOT Center but no verified command exists yet.
    if a.auto_focus == Some(true) && a.face_focus.is_some() {
        notes.push("focus target (no known command)".into());
    }
    if a.exposure_comp_thirds.is_some_and(|t| t != 0) {
        notes.push("exposure compensation (no known command)".into());
    }
    if a.blur == Some(true) {
        notes.push("background blur (no known command)".into());
    }
    Ok(notes)
}

fn need_slot(rest: &[String], usage: &str) -> Result<u8> {
    match rest.get(1).map(String::as_str) {
        Some(s @ ("1" | "2" | "3")) => Ok(s.parse().unwrap()),
        Some(other) => Err(Error::Usage(format!(
            "presets are the camera's slots 1, 2 and 3 (got '{other}'); {usage}"
        ))),
        None => Err(Error::Usage(usage.into())),
    }
}

/// Presets are the camera's own three slots — the same ones OBSBOT Center
/// manages — so they follow the camera between computers.
fn cmd_preset(id: &str, dev: &Device, rest: &[String], json: bool) -> Result<()> {
    match rest.first().map(String::as_str) {
        Some("list") | None => {
            let slots = dev.presets()?;
            let boot = dev.boot_pose();
            let boot_slot = boot.as_ref().ok().and_then(|b| {
                slots
                    .iter()
                    .find(|s| s.pose.is_some_and(|p| p.close_to(b)))
                    .map(|s| s.slot)
            });
            if json {
                let items: Vec<String> = slots
                    .iter()
                    .map(|s| match s.pose {
                        Some(p) => format!(
                            "{{\"slot\":{},\"occupied\":true,\"name\":{},\"pan_deg\":{:.1},\"tilt_deg\":{:.1},\"zoom_ratio\":{:.2},\"power_on\":{}}}",
                            s.slot,
                            json_opt(s.name.as_deref()),
                            p.pan_deg,
                            p.tilt_deg,
                            p.zoom_ratio,
                            boot_slot == Some(s.slot)
                        ),
                        None => format!("{{\"slot\":{},\"occupied\":false}}", s.slot),
                    })
                    .collect();
                let boot_json = match &boot {
                    Ok(b) => format!(
                        "{{\"pan_deg\":{:.1},\"tilt_deg\":{:.1},\"zoom_ratio\":{:.2},\"slot\":{}}}",
                        b.pan_deg,
                        b.tilt_deg,
                        b.zoom_ratio,
                        boot_slot.map_or("null".into(), |s| s.to_string())
                    ),
                    Err(_) => "null".into(),
                };
                println!(
                    "{{\"presets\":[{}],\"power_on\":{boot_json}}}",
                    items.join(",")
                );
                return Ok(());
            }
            for s in &slots {
                match s.pose {
                    Some(p) => println!(
                        "  {}  {}{}{}",
                        s.slot,
                        pose_str(&p),
                        s.name
                            .as_deref()
                            .map(|n| format!("  \"{n}\""))
                            .unwrap_or_default(),
                        if boot_slot == Some(s.slot) {
                            "  [power-on]"
                        } else {
                            ""
                        }
                    ),
                    None => println!("  {}  (empty)", s.slot),
                }
                if s.pose.is_some() {
                    if let Ok(a) = dev.preset_actions(s.slot) {
                        println!("       {}", actions_summary(&a));
                    }
                }
            }
            if let Ok(a) = dev.boot_actions() {
                let from = dev
                    .boot_slot()
                    .ok()
                    .flatten()
                    .map(|n| format!(" (copied from preset {n})"))
                    .unwrap_or_default();
                println!("power-on settings{from}: {}", actions_summary(&a));
            }
            match boot {
                Ok(b) => println!(
                    "power-on: {}",
                    match boot_slot {
                        Some(n) => format!("preset {n} ({})", pose_str(&b)),
                        None => format!("{} (not one of the presets)", pose_str(&b)),
                    }
                ),
                Err(e) => println!("power-on: unknown ({e})"),
            }
            Ok(())
        }
        Some("recall") | Some("load") | Some("goto") => {
            let slot = need_slot(rest, "preset recall <1|2|3>")?;
            let p = dev.preset_recall(slot)?;
            println!("recalled preset {slot} ({})", pose_str(&p));
            // The camera only moves on a recall; OBSBOT Center applies the
            // preset's "Additional settings" itself, so do the same here.
            match dev.preset_actions(slot) {
                Ok(a) => {
                    println!("  applying: {}", actions_summary(&a));
                    for note in apply_actions(id, dev, &a)? {
                        println!("  not applied: {note}");
                    }
                }
                Err(e) => println!("  preset settings not applied ({e})"),
            }
            Ok(())
        }
        Some("save") => {
            let slot = need_slot(rest, "preset save <1|2|3>")?;
            let p = dev.preset_save(slot)?;
            println!("saved preset {slot} on the camera ({})", pose_str(&p));
            Ok(())
        }
        Some("delete") | Some("clear") => {
            let slot = need_slot(rest, "preset delete <1|2|3>")?;
            dev.preset_delete(slot)?;
            println!("preset {slot} is now empty");
            Ok(())
        }
        Some("power-on") | Some("boot") => match rest.get(1).map(String::as_str) {
            None => cmd_preset(id, dev, &["list".into()], json),
            Some("off") | Some("reset") | Some("default") => {
                dev.reset_boot_pose()?;
                println!("power-on position reset to the factory default");
                Ok(())
            }
            Some(_) => {
                let slot = need_slot(rest, "preset power-on <1|2|3>|off")?;
                let (pose, actions) = dev.set_power_on_preset(slot)?;
                println!(
                    "the camera will power on at preset {slot} ({})\n  with: {}",
                    pose_str(&pose),
                    actions_summary(&actions)
                );
                // tiny-guard enforces saved settings; make it agree with the
                // camera's power-on state rather than override it at boot.
                save_actions_config(id, &actions)?;
                Ok(())
            }
        },
        _ => Err(Error::Usage(
            "preset list | recall|save|delete <1|2|3> | power-on <1|2|3>|off".into(),
        )),
    }
}

fn cmd_wb(id: &str, dev: &Device, rest: &[String]) -> Result<()> {
    match rest.first().map(String::as_str) {
        Some("auto") => {
            dev.set_wb_auto()?;
            config::save_setting(id, "auto_wb", "true")
        }
        Some("temp") => {
            let t = need_i32(&rest[1.min(rest.len())..], "white balance temperature")?;
            dev.set_wb_temp(t)?;
            config::save_setting(id, "wb_temp", &t.to_string())?;
            config::save_setting(id, "auto_wb", "false")
        }
        // Pin to this camera's configured target temperature (single source
        // of truth for the udev cold-plug hook).
        Some("pin") => {
            dev.set_wb_temp(config::Config::load_for(id).wb_temp)?;
            config::save_setting(id, "auto_wb", "false")
        }
        _ => Err(Error::Usage(format!(
            "wb auto | wb temp <{}..{}> | wb pin",
            controls::WB_TEMP_MIN,
            controls::WB_TEMP_MAX
        ))),
    }
}

fn cmd_fov(id: &str, dev: &Device, rest: &[String]) -> Result<()> {
    let (level, name) = match rest.first().map(String::as_str) {
        Some("wide") => (0, "wide"),
        Some("medium") | Some("med") => (1, "medium"),
        Some("narrow") => (2, "narrow"),
        _ => return Err(Error::Usage("fov wide|medium|narrow".into())),
    };
    dev.set_fov(level)?;
    config::save_setting(id, "fov", name)
}

fn cmd_exposure(dev: &Device, rest: &[String]) -> Result<()> {
    match rest.first().map(String::as_str) {
        Some("auto") => dev.set_auto_exposure(true),
        Some(v) => {
            let n: i32 = v
                .parse()
                .map_err(|_| Error::Usage("exposure auto|<1..2500>".into()))?;
            dev.set_exposure(n)
        }
        None => Err(Error::Usage("exposure auto|<1..2500>".into())),
    }
}

fn cmd_reset(id: &str, dev: &Device) -> Result<()> {
    wake_managed(dev)?;
    dev.set_tracking(TrackMode::Off)?;
    dev.recenter()?;
    dev.set_wb_auto()?;
    config::save_setting(id, "power", "awake")?;
    config::save_setting(id, "tracking", "off")?;
    config::save_setting(id, "auto_wb", "true")?;
    println!("reset: awake, tracking off, recentered, auto WB");
    Ok(())
}

/// A persistent Wake choice must first disable the independent firmware timer.
/// On models where that timer is untested, failing to disable it is a warning.
fn wake_managed(dev: &Device) -> Result<()> {
    if let Some(w) = dev.wake_managed()? {
        eprintln!("tinyctl: warning: {w}; the camera may still fall asleep on its own timer");
    }
    Ok(())
}

// --- small arg helpers ---

fn need_f64(rest: &[String], what: &str) -> Result<f64> {
    rest.first()
        .ok_or_else(|| Error::Usage(format!("expected {what}")))?
        .parse()
        .map_err(|_| Error::Usage(format!("{what} must be a number")))
}

/// Like [`need_f64`] but rejects NaN/inf, which would otherwise cast to 0 and
/// silently move the gimbal to center.
fn need_finite(rest: &[String], what: &str) -> Result<f64> {
    let v = need_f64(rest, what)?;
    if v.is_finite() {
        Ok(v)
    } else {
        Err(Error::Usage(format!("{what} must be a finite number")))
    }
}

fn need_i32(rest: &[String], what: &str) -> Result<i32> {
    rest.first()
        .ok_or_else(|| Error::Usage(format!("expected {what}")))?
        .parse()
        .map_err(|_| Error::Usage(format!("{what} must be an integer")))
}

fn need_on_off(rest: &[String], what: &str) -> Result<bool> {
    match rest.first().map(String::as_str) {
        Some("on") | Some("true") | Some("1") => Ok(true),
        Some("off") | Some("false") | Some("0") => Ok(false),
        _ => Err(Error::Usage(format!("{what} on|off"))),
    }
}

fn onoff(b: bool) -> &'static str {
    if b {
        "on"
    } else {
        "off"
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn probe_finds_only_bytes_that_follow_the_value() {
        let mut a = [0u8; 60];
        let mut b = a;
        b[0x2a] = 2; // follows the value
        let mut a2 = a;
        a2[0x30] = 9; // noise: changes between two reads of the same value
        b[0x30] = 9;
        a[0x31] = 1; // noise
        let snaps = [("x", a), ("y", b), ("x", a2), ("y", b)];
        assert_eq!(super::tracking_offsets(&snaps), [0x2a]);
    }
}
