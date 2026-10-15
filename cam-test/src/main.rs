//! Тест камеры: читает MJPEG-кадры с UVC-камеры (V4L2) и рассылает их
//! как есть (без перекодирования) всем клиентам по WebSocket.
//! Открыть в браузере: http://<ip-одноплатника>:8080/

use std::sync::Arc;

use axum::{
    Router,
    extract::{
        State,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    response::{Html, IntoResponse},
    routing::get,
};
use tokio::sync::broadcast::{self, error::RecvError};
use v4l::{
    Device, Fraction, FourCC,
    buffer::Type,
    io::{mmap::Stream, traits::CaptureStream},
    video::Capture,
};

type Frame = Arc<Vec<u8>>;

struct Config {
    device: String,
    width: u32,
    height: u32,
    fps: u32,
    port: u16,
}

impl Config {
    /// Использование: cam-test [устройство] [ширина] [высота] [fps] [порт]
    fn from_args() -> Self {
        let a: Vec<String> = std::env::args().skip(1).collect();
        let get = |i: usize, d: &str| a.get(i).cloned().unwrap_or_else(|| d.to_string());
        Config {
            device: get(0, "/dev/video0"),
            width: get(1, "1280").parse().expect("width"),
            height: get(2, "720").parse().expect("height"),
            fps: get(3, "30").parse().expect("fps"),
            port: get(4, "8080").parse().expect("port"),
        }
    }
}

/// Блокирующий цикл захвата, работает в отдельном потоке.
fn capture_loop(cfg: Config, tx: broadcast::Sender<Frame>) -> Result<(), Box<dyn std::error::Error>> {
    let dev = Device::with_path(&cfg.device)?;

    let mut fmt = dev.format()?;
    fmt.width = cfg.width;
    fmt.height = cfg.height;
    fmt.fourcc = FourCC::new(b"MJPG");
    let fmt = dev.set_format(&fmt)?;
    if fmt.fourcc != FourCC::new(b"MJPG") {
        return Err(format!("камера не отдаёт MJPG, выбран формат {}", fmt.fourcc).into());
    }

    let mut params = dev.params()?;
    params.interval = Fraction::new(1, cfg.fps);
    let params = dev.set_params(&params)?;
    eprintln!(
        "камера {}: {}x{} {} @ {}/{} с",
        cfg.device, fmt.width, fmt.height, fmt.fourcc, params.interval.numerator, params.interval.denominator
    );

    let mut stream = Stream::with_buffers(&dev, Type::VideoCapture, 4)?;
    loop {
        let (buf, meta) = stream.next()?;
        if tx.receiver_count() == 0 {
            continue; // никто не смотрит - не копируем
        }
        let data = &buf[..meta.bytesused as usize];
        if data.is_empty() {
            continue;
        }
        let _ = tx.send(Arc::new(data.to_vec()));
    }
}

async fn index() -> Html<&'static str> {
    Html(include_str!("index.html"))
}

async fn ws_handler(ws: WebSocketUpgrade, State(tx): State<broadcast::Sender<Frame>>) -> impl IntoResponse {
    ws.on_upgrade(move |socket| client(socket, tx.subscribe()))
}

async fn client(mut socket: WebSocket, mut rx: broadcast::Receiver<Frame>) {
    loop {
        match rx.recv().await {
            Ok(frame) => {
                if socket.send(Message::Binary(frame.to_vec())).await.is_err() {
                    break; // клиент отключился
                }
            }
            // Клиент не успевает (медленный Wi-Fi) - пропускаем старые кадры
            Err(RecvError::Lagged(_)) => continue,
            Err(RecvError::Closed) => break,
        }
    }
}

#[tokio::main]
async fn main() {
    let cfg = Config::from_args();
    let port = cfg.port;
    // Небольшой буфер: лучше потерять кадр, чем копить задержку
    let (tx, _) = broadcast::channel::<Frame>(2);

    let capture_tx = tx.clone();
    std::thread::spawn(move || {
        if let Err(e) = capture_loop(cfg, capture_tx) {
            eprintln!("ошибка захвата: {e}");
            std::process::exit(1);
        }
    });

    let app = Router::new()
        .route("/", get(index))
        .route("/ws", get(ws_handler))
        .with_state(tx);

    let listener = tokio::net::TcpListener::bind(("0.0.0.0", port)).await.expect("bind");
    eprintln!("открой http://<ip-одноплатника>:{port}/");
    axum::serve(listener, app).await.expect("server");
}
