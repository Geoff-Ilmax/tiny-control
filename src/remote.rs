//! The OBSBOT Tiny Smart Remote (USB 2.4 GHz dongle, `1106:b106`,
//! "CY.Ltd OBSBOT Remote").
//!
//! The dongle is a plain USB keyboard: every button sends a Ctrl+Alt shortcut
//! (OBSBOT Center listens for these). Its vendor HID interface stays silent.
//! Captured on real hardware:
//!
//! | Button | Key | | Button | Key |
//! |---|---|---|---|---|
//! | P1 / P2 / P3 | Q / E / R | | ▲ ▼ ◀ ▶ | Up / Down / Left / Right |
//! | Zoom − / + | H / F | | Center | 0 |
//! | Track | L | | 1 / 2 / 3 / 4 | − / = / , / . |
//! | Close-up / Hand | [ / ] | | Power | T |
//! | Whiteboard / Desk | \ / ; | | | |
//!
//! `tiny-remote` reads the keyboard event node, grabs it so the shortcuts do
//! not also reach other apps, and maps each button to a tinyctl action.

use std::path::{Path, PathBuf};

pub const REMOTE_VID: &str = "1106";
pub const REMOTE_PID: &str = "b106";

/// Linux input key codes (linux/input-event-codes.h) the remote uses.
pub mod key {
    pub const LEFTCTRL: u16 = 29;
    pub const RIGHTCTRL: u16 = 97;
    pub const LEFTALT: u16 = 56;
    pub const RIGHTALT: u16 = 100;
    pub const Q: u16 = 16;
    pub const E: u16 = 18;
    pub const R: u16 = 19;
    pub const T: u16 = 20;
    pub const H: u16 = 35;
    pub const F: u16 = 33;
    pub const L: u16 = 38;
    pub const LEFTBRACE: u16 = 26;
    pub const RIGHTBRACE: u16 = 27;
    pub const BACKSLASH: u16 = 43;
    pub const SEMICOLON: u16 = 39;
    pub const MINUS: u16 = 12;
    pub const EQUAL: u16 = 13;
    pub const COMMA: u16 = 51;
    pub const DOT: u16 = 52;
    pub const KEY_0: u16 = 11;
    pub const UP: u16 = 103;
    pub const DOWN: u16 = 108;
    pub const LEFT: u16 = 105;
    pub const RIGHT: u16 = 106;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Button {
    Power,
    Track,
    CloseUp,
    Hand,
    Whiteboard,
    Desk,
    ZoomIn,
    ZoomOut,
    Up,
    Down,
    Left,
    Right,
    Center,
    /// On-camera preset 1..=3.
    Preset(u8),
    /// Camera 1..=4 (order of `tinyctl list`).
    Camera(u8),
}

impl Button {
    /// The button a Ctrl+Alt+`code` shortcut stands for.
    pub fn from_key(code: u16) -> Option<Button> {
        use key::*;
        Some(match code {
            T => Button::Power,
            L => Button::Track,
            LEFTBRACE => Button::CloseUp,
            RIGHTBRACE => Button::Hand,
            BACKSLASH => Button::Whiteboard,
            SEMICOLON => Button::Desk,
            F => Button::ZoomIn,
            H => Button::ZoomOut,
            UP => Button::Up,
            DOWN => Button::Down,
            LEFT => Button::Left,
            RIGHT => Button::Right,
            KEY_0 => Button::Center,
            Q => Button::Preset(1),
            E => Button::Preset(2),
            R => Button::Preset(3),
            MINUS => Button::Camera(1),
            EQUAL => Button::Camera(2),
            COMMA => Button::Camera(3),
            DOT => Button::Camera(4),
            _ => return None,
        })
    }

    /// Buttons that keep acting while held (gimbal and zoom).
    pub fn repeats(self) -> bool {
        matches!(
            self,
            Button::Up
                | Button::Down
                | Button::Left
                | Button::Right
                | Button::ZoomIn
                | Button::ZoomOut
        )
    }
}

/// Tracks Ctrl/Alt state and turns raw key events into button presses.
#[derive(Debug, Default)]
pub struct Decoder {
    ctrl: bool,
    alt: bool,
}

/// What a key event means for the caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Press {
    /// First press of a button.
    Down(Button),
    /// Kernel auto-repeat while the button is held.
    Repeat(Button),
}

impl Decoder {
    /// Feed one EV_KEY event (`value`: 1 press, 0 release, 2 auto-repeat).
    pub fn key(&mut self, code: u16, value: i32) -> Option<Press> {
        let down = value != 0;
        match code {
            key::LEFTCTRL | key::RIGHTCTRL => self.ctrl = down,
            key::LEFTALT | key::RIGHTALT => self.alt = down,
            _ if self.ctrl && self.alt => {
                let b = Button::from_key(code)?;
                return match value {
                    1 => Some(Press::Down(b)),
                    2 if b.repeats() => Some(Press::Repeat(b)),
                    _ => None,
                };
            }
            _ => {}
        }
        None
    }
}

/// One `struct input_event` from an evdev node (24 bytes on 64-bit Linux:
/// timeval, then type u16, code u16, value i32).
pub const EVENT_SIZE: usize = std::mem::size_of::<libc::timeval>() + 8;
pub const EV_KEY: u16 = 1;

pub fn parse_event(b: &[u8]) -> Option<(u16, u16, i32)> {
    if b.len() < EVENT_SIZE {
        return None;
    }
    let o = std::mem::size_of::<libc::timeval>();
    Some((
        u16::from_ne_bytes([b[o], b[o + 1]]),
        u16::from_ne_bytes([b[o + 2], b[o + 3]]),
        i32::from_ne_bytes([b[o + 4], b[o + 5], b[o + 6], b[o + 7]]),
    ))
}

/// The remote's keyboard event node, found through sysfs (vendor/product),
/// so it does not rely on `/dev/input/by-id` names. The dongle also exposes
/// a mouse node; the keyboard is the one advertising key auto-repeat
/// (`EV_REP`, bit 20 of `capabilities/ev`).
pub fn find_remote() -> Option<PathBuf> {
    find_remote_in(Path::new("/sys"), "/dev/input")
}

const EV_REP_BIT: u64 = 1 << 20;

pub fn find_remote_in(sys: &Path, dev_root: &str) -> Option<PathBuf> {
    let read = |p: &Path| {
        std::fs::read_to_string(p)
            .ok()
            .map(|s| s.trim().to_ascii_lowercase())
    };
    let mut nodes: Vec<String> = std::fs::read_dir(sys.join("class/input"))
        .ok()?
        .flatten()
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| n.starts_with("event"))
        .collect();
    nodes.sort_by_key(|n| n[5..].parse::<u32>().unwrap_or(u32::MAX));
    nodes.into_iter().find_map(|n| {
        // eventN/device is the inputN directory.
        let input = std::fs::canonicalize(sys.join("class/input").join(&n).join("device")).ok()?;
        let usb = input
            .ancestors()
            .find(|p| p.join("idVendor").exists() && p.join("idProduct").exists())?;
        if read(&usb.join("idVendor"))? != REMOTE_VID || read(&usb.join("idProduct"))? != REMOTE_PID
        {
            return None;
        }
        let ev = u64::from_str_radix(&read(&input.join("capabilities/ev"))?, 16).ok()?;
        (ev & EV_REP_BIT != 0).then(|| PathBuf::from(format!("{dev_root}/{n}")))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buttons_need_ctrl_and_alt() {
        let mut d = Decoder::default();
        assert_eq!(d.key(key::T, 1), None); // plain T: not the remote
        d.key(key::LEFTCTRL, 1);
        assert_eq!(d.key(key::T, 1), None); // Ctrl+T only
        d.key(key::LEFTALT, 1);
        assert_eq!(d.key(key::T, 1), Some(Press::Down(Button::Power)));
        assert_eq!(d.key(key::T, 2), None); // power does not repeat
        assert_eq!(d.key(key::LEFT, 1), Some(Press::Down(Button::Left)));
        assert_eq!(d.key(key::LEFT, 2), Some(Press::Repeat(Button::Left)));
        assert_eq!(d.key(key::LEFT, 0), None);
        d.key(key::LEFTALT, 0);
        assert_eq!(d.key(key::LEFT, 1), None);
    }

    #[test]
    fn captured_shortcuts_map_to_buttons() {
        use Button::*;
        let map = [
            (key::Q, Preset(1)),
            (key::E, Preset(2)),
            (key::R, Preset(3)),
            (key::H, ZoomOut),
            (key::F, ZoomIn),
            (key::L, Track),
            (key::LEFTBRACE, CloseUp),
            (key::RIGHTBRACE, Hand),
            (key::BACKSLASH, Whiteboard),
            (key::SEMICOLON, Desk),
            (key::MINUS, Camera(1)),
            (key::EQUAL, Camera(2)),
            (key::COMMA, Camera(3)),
            (key::DOT, Camera(4)),
            (key::T, Power),
            (key::UP, Up),
            (key::DOWN, Down),
            (key::LEFT, Left),
            (key::RIGHT, Right),
            (key::KEY_0, Center),
        ];
        for (code, b) in map {
            assert_eq!(Button::from_key(code), Some(b), "key {code}");
        }
        assert_eq!(Button::from_key(30), None); // A
    }

    /// The user's topology: the dongle's keyboard on event2, its mouse on
    /// event4, and an unrelated keyboard on event10.
    #[test]
    fn finds_the_remote_keyboard_node() {
        let root = std::env::temp_dir().join(format!("tiny-remote-sysfs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let class = root.join("class/input");
        std::fs::create_dir_all(&class).unwrap();
        let add = |port: &str, vid: &str, pid: &str, node: &str, input: &str, ev: &str| {
            let usb = root.join("devices/usb").join(port);
            std::fs::create_dir_all(&usb).unwrap();
            std::fs::write(usb.join("idVendor"), vid).unwrap();
            std::fs::write(usb.join("idProduct"), pid).unwrap();
            let inp = usb.join(format!("{port}:1.0")).join(input);
            std::fs::create_dir_all(inp.join("capabilities")).unwrap();
            std::fs::write(inp.join("capabilities/ev"), format!("{ev}\n")).unwrap();
            std::fs::create_dir_all(class.join(node)).unwrap();
            std::os::unix::fs::symlink(&inp, class.join(node).join("device")).unwrap();
        };
        add("5-1", "1106", "b106", "event4", "input8", "17"); // mouse
        add("5-1", "1106", "b106", "event2", "input6", "120013"); // keyboard
        add("3-2", "3151", "4002", "event10", "input20", "120013"); // other keyboard
        assert_eq!(
            find_remote_in(&root, "/dev/input"),
            Some(PathBuf::from("/dev/input/event2"))
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn parses_input_events() {
        let mut b = vec![0u8; EVENT_SIZE];
        let o = std::mem::size_of::<libc::timeval>();
        b[o..o + 2].copy_from_slice(&EV_KEY.to_ne_bytes());
        b[o + 2..o + 4].copy_from_slice(&key::L.to_ne_bytes());
        b[o + 4..o + 8].copy_from_slice(&1i32.to_ne_bytes());
        assert_eq!(parse_event(&b), Some((EV_KEY, key::L, 1)));
        assert_eq!(parse_event(&b[..4]), None);
    }
}
