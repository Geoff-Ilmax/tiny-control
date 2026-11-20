//! `tiny-control` — a Linux control library for OBSBOT Tiny 2 and Tiny 3
//! series webcams, with support for several cameras at once.
//!
//! Both generations speak the same UVC extension-unit vendor protocol
//! (bUnitID 2, framed "V3" frames on selector 2, raw TLVs on selector 6), which
//! this crate implements from scratch. See `PROTOCOL.md` for the wire details
//! and firmware quirks, and [`model`] for what is verified on which model.
//!
//! Entry points: [`camera::all`] / [`camera::resolve`] to find cameras, then
//! [`device::Device`] to control one. Note that **an open fd blocks the camera
//! from sleeping** — see the module docs.

pub mod camera;
pub mod config;
pub mod controls;
pub mod crc;
pub mod device;
pub mod discover;
pub mod error;
pub mod frame;
pub mod ioctl;
pub mod model;
pub mod remote;

pub use device::{Device, Status, TrackMode};
pub use error::{Error, Result};
