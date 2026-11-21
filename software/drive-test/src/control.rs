// SPDX-License-Identifier: GPL-3.0-only
//! Shared state. The serial worker consumes each fresh command once, never
//! replaying an old throttle while the browser/network is stalled.
use serde::Serialize;
use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

pub const INPUT_TIMEOUT: Duration = Duration::from_millis(200);
pub const CAMERA_TIMEOUT: Duration = Duration::from_millis(700);
pub type Shared = Arc<Mutex<Control>>;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Command {
    Stop,
    Drive { throttle: i16, steering: i16 },
}

#[derive(Clone, Serialize)]
pub struct Status {
    pub serial_ready: bool,
    pub camera_required: bool,
    pub camera_ready: bool,
    pub controlling: bool,
    pub reason: String,
    pub mcu: String,
    pub frames: u64,
}

pub struct Control {
    pub shutdown: bool,
    serial_ready: bool,
    camera_required: bool,
    camera_at: Option<Instant>,
    camera_error: Option<String>,
    frames: u64,
    owner: Option<u64>,
    next_owner: u64,
    last_input: Instant,
    pending: Option<Command>,
    stop_pending: bool,
    reason: String,
    mcu: String,
}

impl Control {
    pub fn new(serial_configured: bool, camera_required: bool) -> Shared {
        Arc::new(Mutex::new(Self {
            shutdown: false,
            serial_ready: false,
            camera_required,
            camera_at: None,
            camera_error: None,
            frames: 0,
            owner: None,
            next_owner: 0,
            last_input: Instant::now(),
            pending: None,
            stop_pending: true,
            reason: if serial_configured {
                "Waiting for Arduino"
            } else {
                "Serial port not configured: video only"
            }
            .into(),
            mcu: String::new(),
        }))
    }

    fn camera_ready(&self, now: Instant) -> bool {
        !self.camera_required
            || (self.camera_error.is_none()
                && self
                    .camera_at
                    .is_some_and(|at| now.saturating_duration_since(at) < CAMERA_TIMEOUT))
    }

    #[cfg(any(target_os = "linux", test))]
    pub fn frame(&mut self, now: Instant) {
        self.frames += 1;
        self.camera_at = Some(now);
    }

    pub fn camera_failed(&mut self, error: String) {
        self.camera_error = Some(error.clone());
        self.stop(&format!("Camera error: {error}"));
    }

    pub fn serial_ready(&mut self) {
        self.serial_ready = true;
        self.stop("Ready. Take control to drive");
    }

    pub fn serial_failed(&mut self, error: &str) {
        self.serial_ready = false;
        self.stop(&format!("Arduino: {error}. Restart the server"));
    }

    pub fn telemetry(&mut self, line: &str) {
        self.mcu = line.into();
    }

    pub fn acquire(&mut self, now: Instant) -> Result<u64, &'static str> {
        if self.shutdown || !self.serial_ready {
            return Err("Arduino not ready");
        }
        if !self.camera_ready(now) {
            return Err("No fresh camera frames");
        }
        if self.owner.is_some() {
            return Err("Control is already held by another tab");
        }
        self.next_owner += 1;
        self.owner = Some(self.next_owner);
        self.last_input = now;
        self.pending = None;
        self.stop_pending = true;
        self.reason = "Control connected".into();
        Ok(self.next_owner)
    }

    pub fn owns(&self, owner: u64) -> bool {
        self.owner == Some(owner)
    }

    pub fn drive(&mut self, owner: u64, throttle: i16, steering: i16, now: Instant) -> bool {
        self.check(now);
        if !self.owns(owner) {
            return false;
        }
        if !(-255..=255).contains(&throttle) || !(-100..=100).contains(&steering) {
            self.stop("Invalid control command");
            return false;
        }
        self.pending = Some(Command::Drive { throttle, steering });
        self.last_input = now;
        true
    }

    pub fn release(&mut self, owner: u64, reason: &str) {
        if self.owns(owner) {
            self.stop(reason);
        }
    }

    pub fn stop(&mut self, reason: &str) {
        self.owner = None;
        self.pending = None;
        self.stop_pending = true;
        self.reason = reason.into();
    }

    pub fn check(&mut self, now: Instant) {
        if self.owner.is_some() {
            if self.shutdown || !self.serial_ready {
                self.stop("Server shutting down or Arduino unavailable");
            } else if !self.camera_ready(now) {
                self.stop("Camera stopped delivering frames");
            } else if now.saturating_duration_since(self.last_input) >= INPUT_TIMEOUT {
                self.stop("No fresh browser input");
            }
        }
    }

    pub fn next_command(&mut self, now: Instant) -> Option<Command> {
        self.check(now);
        if self.stop_pending {
            self.stop_pending = false;
            Some(Command::Stop)
        } else {
            self.pending.take()
        }
    }

    pub fn status(&mut self, now: Instant) -> Status {
        self.check(now);
        Status {
            serial_ready: self.serial_ready,
            camera_required: self.camera_required,
            camera_ready: self.camera_ready(now),
            controlling: self.owner.is_some(),
            reason: self
                .camera_error
                .as_ref()
                .map(|e| format!("Camera error: {e}"))
                .unwrap_or_else(|| self.reason.clone()),
            mcu: self.mcu.clone(),
            frames: self.frames,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn ready() -> Shared {
        let state = Control::new(true, false);
        state.lock().unwrap().serial_ready();
        state
    }

    #[test]
    fn consumes_once_and_expires_without_replaying() {
        let state = ready();
        let mut c = state.lock().unwrap();
        let now = Instant::now();
        let owner = c.acquire(now).unwrap();
        assert_eq!(c.next_command(now), Some(Command::Stop));
        assert!(c.drive(owner, 100, -50, now));
        assert_eq!(
            c.next_command(now),
            Some(Command::Drive {
                throttle: 100,
                steering: -50
            })
        );
        assert_eq!(c.next_command(now + Duration::from_millis(100)), None);
        assert_eq!(c.next_command(now + INPUT_TIMEOUT), Some(Command::Stop));
        assert!(!c.drive(owner, 100, 0, now + INPUT_TIMEOUT));
        assert!(!c.owns(owner));
    }

    #[test]
    fn exclusive_owner_stop_priority_and_stale_disconnect() {
        let state = ready();
        let mut c = state.lock().unwrap();
        let now = Instant::now();
        let first = c.acquire(now).unwrap();
        assert!(c.acquire(now).is_err());
        assert!(c.drive(first, 90, 10, now));
        // Startup STOP cannot be overwritten by a fast DRIVE.
        assert_eq!(c.next_command(now), Some(Command::Stop));
        c.release(first, "lost connection");
        let second = c.acquire(now).unwrap();
        c.release(first, "late cleanup must not stop the new owner");
        assert!(c.owns(second));
        assert!(!c.drive(first, 255, 100, now));
        assert!(c.drive(second, 0, 0, now));
        assert!(!c.drive(second, 256, 0, now));
        assert_eq!(c.next_command(now), Some(Command::Stop));
        assert_eq!(c.next_command(now), None);
    }

    #[test]
    fn camera_and_serial_failures_revoke_control() {
        let state = Control::new(true, true);
        let mut c = state.lock().unwrap();
        let now = Instant::now();
        assert!(c.acquire(now).is_err());
        c.serial_ready();
        assert!(c.acquire(now).is_err());
        c.frame(now);
        let owner = c.acquire(now).unwrap();
        c.camera_failed("unplugged".into());
        assert!(!c.owns(owner));
        assert_eq!(c.next_command(now), Some(Command::Stop));
        assert!(c.acquire(now).is_err());
        c.camera_required = false;
        let owner = c.acquire(now).unwrap();
        c.serial_failed("USB lost");
        assert!(!c.owns(owner));
        assert!(c.acquire(now).is_err());
    }

    #[test]
    fn delayed_worker_discards_expired_pending_throttle() {
        let state = ready();
        let mut c = state.lock().unwrap();
        let now = Instant::now();
        let owner = c.acquire(now).unwrap();
        c.next_command(now);
        c.drive(owner, 200, 0, now);
        assert_eq!(c.next_command(now + INPUT_TIMEOUT), Some(Command::Stop));
        assert_eq!(c.next_command(now + INPUT_TIMEOUT), None);
    }
    #[test]
    fn fresh_input_cannot_keep_driving_after_camera_stalls() {
        let state = Control::new(true, true);
        let mut c = state.lock().unwrap();
        let now = Instant::now();
        c.serial_ready();
        c.frame(now);
        let owner = c.acquire(now).unwrap();
        for millis in (0..700).step_by(100) {
            assert!(c.drive(owner, 100, 0, now + Duration::from_millis(millis)));
        }
        assert!(!c.drive(owner, 100, 0, now + CAMERA_TIMEOUT));
        assert_eq!(c.next_command(now + CAMERA_TIMEOUT), Some(Command::Stop));
        assert!(!c.owns(owner));
    }
}
