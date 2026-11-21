// SPDX-License-Identifier: GPL-3.0-only
use crate::control::Shared;
use std::sync::Arc;
#[cfg(target_os = "linux")]
use std::time::Instant;
use tokio::sync::watch;

pub type Frame = Arc<Vec<u8>>;
pub type Frames = watch::Sender<Option<Frame>>;

pub struct Config {
    pub device: String,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
}

pub fn spawn(config: Config, frames: Frames, state: Shared) {
    std::thread::spawn(move || {
        if let Err(error) = capture(config, frames, &state) {
            eprintln!("Camera: {error}");
            state.lock().unwrap().camera_failed(error);
        }
    });
}

#[cfg(target_os = "linux")]
fn capture(cfg: Config, frames: Frames, state: &Shared) -> Result<(), String> {
    use v4l::{
        Device, FourCC, Fraction,
        buffer::Type,
        io::{mmap::Stream, traits::CaptureStream},
        video::Capture,
    };
    let run = || -> Result<(), Box<dyn std::error::Error>> {
        let dev = Device::with_path(&cfg.device)?;
        let mut fmt = dev.format()?;
        fmt.width = cfg.width;
        fmt.height = cfg.height;
        fmt.fourcc = FourCC::new(b"MJPG");
        let fmt = dev.set_format(&fmt)?;
        if fmt.fourcc != FourCC::new(b"MJPG") {
            return Err(format!("camera does not support MJPG (got {})", fmt.fourcc).into());
        }
        let mut params = dev.params()?;
        params.interval = Fraction::new(1, cfg.fps);
        let params = dev.set_params(&params)?;
        eprintln!(
            "Camera {}: {}x{} {} @ {}/{} s",
            cfg.device,
            fmt.width,
            fmt.height,
            fmt.fourcc,
            params.interval.numerator,
            params.interval.denominator
        );
        let mut stream = Stream::with_buffers(&dev, Type::VideoCapture, 4)?;
        loop {
            if state.lock().unwrap().shutdown {
                return Ok(());
            }
            let (buffer, meta) = stream.next()?;
            let data = &buffer[..meta.bytesused as usize];
            if data.len() < 4 || !data.starts_with(&[0xff, 0xd8]) {
                continue;
            }
            state.lock().unwrap().frame(Instant::now());
            // One latest frame, never a FIFO of old pictures.
            frames.send_replace(Some(Arc::new(data.to_vec())));
        }
    };
    run().map_err(|e| e.to_string())
}

#[cfg(not(target_os = "linux"))]
fn capture(cfg: Config, _frames: Frames, _state: &Shared) -> Result<(), String> {
    Err(format!(
        "V4L2 requires Linux ({} {}x{} @ {}); use --no-camera to test control",
        cfg.device, cfg.width, cfg.height, cfg.fps
    ))
}
