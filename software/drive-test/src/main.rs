// SPDX-License-Identifier: GPL-3.0-only
//! Headless Orange Pi server: MJPEG preview + browser control -> USB Arduino.
mod camera;
mod control;
mod serial;
mod web;

use rand::RngCore;
use std::{
    net::{IpAddr, Ipv4Addr},
    sync::Arc,
};
use tokio::sync::watch;

struct Config {
    camera: camera::Config,
    port: u16,
    bind: IpAddr,
    serial: Option<String>,
    no_camera: bool,
}
impl Config {
    fn from_args() -> Result<Option<Self>, String> {
        let mut args = std::env::args().skip(1);
        let mut positional = Vec::new();
        let mut serial = None;
        let mut no_camera = false;
        let mut bind = IpAddr::V4(Ipv4Addr::UNSPECIFIED);
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--serial" => serial = Some(args.next().ok_or("--serial requires a device path")?),
                "--bind" => {
                    bind = args
                        .next()
                        .ok_or("--bind requires an IP address")?
                        .parse()
                        .map_err(|_| "invalid bind IP")?
                }
                "--no-camera" => no_camera = true,
                "--help" | "-h" => {
                    println!(
                        "Usage: drive-test [camera width height fps port] [--serial PATH] [--bind IP] [--no-camera]\n\
                        Defaults: /dev/video0 1280 720 30 8080, bind 0.0.0.0\n\
                        --serial /dev/serial/by-id/... enables Arduino protocol v1 control\n\
                        --no-camera explicitly allows control without video (bench tests)\n\
                        CUTTLEFISH_TOKEN sets a shared access token (min 16 chars); otherwise generated at startup"
                    );
                    return Ok(None);
                }
                flag if flag.starts_with('-') => return Err(format!("unknown option: {flag}")),
                _ => positional.push(arg),
            }
        }
        if positional.len() > 5 {
            return Err("too many positional arguments; use --help".into());
        }
        let get =
            |i: usize, default: &str| positional.get(i).cloned().unwrap_or_else(|| default.into());
        let width = get(1, "1280").parse::<u32>().map_err(|_| "invalid width")?;
        let height = get(2, "720").parse::<u32>().map_err(|_| "invalid height")?;
        let fps = get(3, "30").parse::<u32>().map_err(|_| "invalid fps")?;
        let port = get(4, "8080").parse::<u16>().map_err(|_| "invalid port")?;
        if width == 0 || height == 0 || fps == 0 || port == 0 {
            return Err("dimensions, fps and port must be positive".into());
        }
        Ok(Some(Self {
            camera: camera::Config {
                device: get(0, "/dev/video0"),
                width,
                height,
                fps,
            },
            port,
            bind,
            serial,
            no_camera,
        }))
    }
}

fn token() -> Result<String, String> {
    if let Ok(token) = std::env::var("CUTTLEFISH_TOKEN") {
        if token.len() < 16
            || !token
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        {
            return Err(
                "CUTTLEFISH_TOKEN must contain at least 16 ASCII letters/digits/-/_".into(),
            );
        }
        return Ok(token);
    }
    let mut bytes = [0u8; 16];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .expect("SIGTERM handler");
        tokio::select! { _ = tokio::signal::ctrl_c() => {}, _ = terminate.recv() => {} }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let Some(cfg) = Config::from_args().map_err(std::io::Error::other)? else {
        return Ok(());
    };
    let token = token().map_err(std::io::Error::other)?;
    // Bind first: a port conflict must not leave a background MCU worker running.
    let listener = tokio::net::TcpListener::bind((cfg.bind, cfg.port)).await?;
    let control = control::Control::new(cfg.serial.is_some(), !cfg.no_camera);
    let (frames, _) = watch::channel(None);
    let worker = cfg.serial.map(|path| serial::spawn(path, control.clone()));
    if !cfg.no_camera {
        camera::spawn(cfg.camera, frames.clone(), control.clone());
    }
    let hostname = if cfg.bind.is_unspecified() {
        "<orange-pi-ip>".into()
    } else {
        cfg.bind.to_string()
    };
    eprintln!("Open http://{hostname}:{}/#token={token}", cfg.port);
    if cfg.no_camera {
        eprintln!("Camera interlock disabled by --no-camera");
    }
    let app = web::App {
        control: control.clone(),
        frames,
        token: Arc::new(token),
    };
    let shutdown = control.clone();
    let result = axum::serve(listener, web::router(app))
        .with_graceful_shutdown(async move {
            shutdown_signal().await;
            let mut state = shutdown.lock().unwrap();
            state.shutdown = true;
            state.stop("Server shutting down");
        })
        .await;
    {
        let mut state = control.lock().unwrap();
        state.shutdown = true;
        state.stop("Server stopped");
    }
    if let Some(worker) = worker {
        let _ = tokio::task::spawn_blocking(move || worker.join()).await;
    }
    result?;
    Ok(())
}
