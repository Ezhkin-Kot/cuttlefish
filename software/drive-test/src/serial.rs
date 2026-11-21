// SPDX-License-Identifier: GPL-3.0-only
//! A dedicated thread owns the serial port. Bounded I/O timeouts keep USB
//! problems away from the HTTP runtime; the MCU watchdog remains independent.
use crate::control::{Command, Shared};
use serialport::SerialPort;
use std::{
    io, thread,
    time::{Duration, Instant},
};

const IO_TIMEOUT: Duration = Duration::from_millis(20);

pub fn spawn(path: String, state: Shared) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let result = serialport::new(&path, 115_200)
            .timeout(IO_TIMEOUT)
            .open()
            .map_err(|e| e.to_string())
            .and_then(|mut port| {
                let result = run(port.as_mut(), &state);
                // Best effort even after error/shutdown. Never flush indefinitely.
                let _ = port.write_all(b"\nSTOP\n");
                result
            });
        if let Err(error) = result {
            eprintln!("Arduino: {error}");
            state.lock().unwrap().serial_failed(&error);
        }
    })
}

fn send(port: &mut dyn SerialPort, command: Command) -> Result<(), String> {
    let frame = match command {
        Command::Stop => "STOP\n".to_string(),
        Command::Drive { throttle, steering } => format!("DRIVE {throttle} {steering}\n"),
    };
    port.write_all(frame.as_bytes())
        .map_err(|e| format!("serial write: {e}"))
}

pub fn run(port: &mut dyn SerialPort, state: &Shared) -> Result<(), String> {
    let started = Instant::now();
    let mut ready = false;
    let mut last_query = started;
    let mut greeted_at = None;
    let mut hello_sent = false;
    let mut last_response = started;
    let mut input = Vec::new();
    port.write_all(b"\nSTOP\n").map_err(|e| e.to_string())?;
    loop {
        let now = Instant::now();
        if state.lock().unwrap().shutdown {
            return send(port, Command::Stop);
        }
        // bytes_to_read prevents a 20 ms read timeout on every idle iteration.
        let count = port
            .bytes_to_read()
            .map_err(|e| format!("serial read: {e}"))?;
        if count > 0 {
            let mut buffer = [0u8; 512];
            let count = count.min(buffer.len() as u32) as usize;
            match port.read(&mut buffer[..count]) {
                Ok(0) => return Err("serial closed".into()),
                Ok(n) => input.extend_from_slice(&buffer[..n]),
                Err(e)
                    if matches!(
                        e.kind(),
                        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
                    ) => {}
                Err(e) => return Err(format!("serial read: {e}")),
            }
            while let Some(end) = input.iter().position(|b| *b == b'\n') {
                let frame: Vec<_> = input.drain(..=end).collect();
                if frame.len() > 128 {
                    return Err("MCU response too long".into());
                }
                let line = std::str::from_utf8(&frame)
                    .map_err(|e| e.to_string())?
                    .trim();
                if line == "HELLO CUTTLEFISH 1" {
                    if ready {
                        return Err("MCU restarted; control stopped".into());
                    }
                    greeted_at = Some(now);
                    last_response = now;
                } else if line.starts_with("HELLO") {
                    return Err(format!("unsupported firmware: {line}"));
                } else if ready && line == "EVENT TIMEOUT" {
                    // The firmware already braked. Keep the healthy USB link;
                    // require neutral input before resuming the same session.
                    last_response = now;
                    state
                        .lock()
                        .unwrap()
                        .pause_current("MCU watchdog stopped the motor; waiting for neutral input");
                    send(port, Command::Stop)?;
                } else if ready && (line.starts_with("ERR") || line.starts_with("EVENT")) {
                    return Err(format!("MCU stopped: {line}"));
                } else if ready && (line.starts_with("STATE ") || line.starts_with("OK ")) {
                    last_response = now;
                    if line.starts_with("STATE ") {
                        state.lock().unwrap().telemetry(line);
                    }
                }
            }
            if input.len() > 128 {
                return Err("unfinished MCU response too long".into());
            }
        }
        // Let boot/handshake greetings settle before allowing acquisition.
        if !ready
            && greeted_at.is_some_and(|at| now.duration_since(at) >= Duration::from_millis(100))
        {
            send(port, Command::Stop)?;
            state.lock().unwrap().serial_ready();
            ready = true;
        }
        if ready {
            if now.duration_since(last_response) >= Duration::from_secs(1) {
                return Err("MCU stopped responding".into());
            }
            let command = state.lock().unwrap().next_command(now);
            if let Some(command) = command {
                send(port, command)?;
            }
            if now.duration_since(last_query) >= Duration::from_millis(250) {
                port.write_all(b"STATUS\n").map_err(|e| e.to_string())?;
                last_query = now;
            }
        } else {
            if now.duration_since(started) >= Duration::from_secs(5) {
                return Err("protocol v1 handshake timed out".into());
            }
            if greeted_at.is_none()
                && !hello_sent
                && now.duration_since(started) >= Duration::from_secs(2)
            {
                port.write_all(b"HELLO\n").map_err(|e| e.to_string())?;
                hello_sent = true;
            }
        }
        thread::sleep(Duration::from_millis(5));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control::Control;
    use serialport::TTYPort;
    use std::io::{Read, Write};
    use std::sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    };

    #[test]
    fn real_pty_handshake_drive_expiry_and_restart() {
        let (mut emulator, mut port) = TTYPort::pair().unwrap();
        emulator.set_timeout(Duration::from_millis(10)).unwrap();
        port.set_timeout(IO_TIMEOUT).unwrap();
        let running = Arc::new(AtomicBool::new(true));
        let reboot = Arc::new(AtomicBool::new(false));
        let watchdog = Arc::new(AtomicBool::new(false));
        let commands = Arc::new(Mutex::new(Vec::<String>::new()));
        let emulator_thread = {
            let running = running.clone();
            let reboot = reboot.clone();
            let watchdog = watchdog.clone();
            let commands = commands.clone();
            thread::spawn(move || {
                // Emulate the startup greeting, just as the actual firmware does.
                emulator.write_all(b"HELLO CUTTLEFISH 1\n").unwrap();
                let mut input = Vec::new();
                while running.load(Ordering::Relaxed) {
                    if watchdog.swap(false, Ordering::Relaxed) {
                        emulator.write_all(b"EVENT TIMEOUT\n").unwrap();
                    }
                    if reboot.swap(false, Ordering::Relaxed) {
                        emulator.write_all(b"HELLO CUTTLEFISH 1\n").unwrap();
                    }
                    let mut buffer = [0u8; 256];
                    match emulator.read(&mut buffer) {
                        Ok(n) => input.extend_from_slice(&buffer[..n]),
                        Err(e) if e.kind() == io::ErrorKind::TimedOut => {}
                        _ => break,
                    }
                    while let Some(end) = input.iter().position(|b| *b == b'\n') {
                        let frame: Vec<_> = input.drain(..=end).collect();
                        let line = String::from_utf8(frame).unwrap().trim().to_owned();
                        if line.is_empty() {
                            continue;
                        }
                        commands.lock().unwrap().push(line.clone());
                        let reply = if line == "HELLO" {
                            "HELLO CUTTLEFISH 1\n"
                        } else if line == "STATUS" {
                            "STATE t=0 p=0 s=0 a=97 safe=1\n"
                        } else if line == "STOP" {
                            "OK STOP\n"
                        } else {
                            "OK DRIVE\n"
                        };
                        if emulator.write_all(reply.as_bytes()).is_err() {
                            return;
                        }
                    }
                }
            })
        };
        let state = Control::new(true, false);
        let worker = {
            let state = state.clone();
            thread::spawn(move || {
                let result = run(&mut port, &state);
                let _ = port.write_all(b"STOP\n");
                if let Err(error) = result {
                    state.lock().unwrap().serial_failed(&error);
                }
            })
        };
        let wait_until = |condition: &dyn Fn() -> bool| {
            let end = Instant::now() + Duration::from_secs(4);
            while !condition() {
                assert!(Instant::now() < end, "PTY condition timed out");
                thread::sleep(Duration::from_millis(5));
            }
        };
        wait_until(&|| state.lock().unwrap().status(Instant::now()).serial_ready);
        let owner = state.lock().unwrap().acquire(Instant::now()).unwrap();
        assert!(state.lock().unwrap().drive(owner, 80, -50, Instant::now()));
        wait_until(&|| commands.lock().unwrap().iter().any(|c| c == "DRIVE 80 -50"));
        let drive_position = commands
            .lock()
            .unwrap()
            .iter()
            .position(|c| c == "DRIVE 80 -50")
            .unwrap();
        wait_until(&|| {
            commands
                .lock()
                .unwrap()
                .iter()
                .skip(drive_position + 1)
                .any(|c| c == "STOP")
        });
        assert_eq!(
            commands
                .lock()
                .unwrap()
                .iter()
                .filter(|c| c.starts_with("DRIVE"))
                .count(),
            1
        );
        assert!(state.lock().unwrap().owns(owner));
        assert!(state.lock().unwrap().status(Instant::now()).paused);
        state.lock().unwrap().drive(owner, 0, 0, Instant::now());
        assert!(!state.lock().unwrap().status(Instant::now()).paused);
        watchdog.store(true, Ordering::Relaxed);
        wait_until(&|| state.lock().unwrap().status(Instant::now()).paused);
        assert!(state.lock().unwrap().status(Instant::now()).serial_ready);
        assert!(state.lock().unwrap().owns(owner));
        state.lock().unwrap().release(owner, "Test completed");
        let second = state.lock().unwrap().acquire(Instant::now()).unwrap();
        assert!(state.lock().unwrap().drive(second, -50, 0, Instant::now()));
        reboot.store(true, Ordering::Relaxed);
        wait_until(&|| !state.lock().unwrap().status(Instant::now()).serial_ready);
        assert!(!state.lock().unwrap().owns(second));
        assert!(state.lock().unwrap().acquire(Instant::now()).is_err());
        worker.join().unwrap();
        running.store(false, Ordering::Relaxed);
        emulator_thread.join().unwrap();
    }
}
