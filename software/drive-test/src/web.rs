// SPDX-License-Identifier: GPL-3.0-only
use crate::{
    camera::Frames,
    control::{INPUT_TIMEOUT, Shared, Status},
};
use axum::{
    Json, Router,
    extract::{
        Query, State,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    http::{HeaderMap, StatusCode, header},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
};
use serde::Deserialize;
use serde_json::json;
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::time::{MissedTickBehavior, interval, timeout};

#[derive(Clone)]
pub struct App {
    pub control: Shared,
    pub frames: Frames,
    pub token: Arc<String>,
}

#[derive(Deserialize)]
pub struct Auth {
    token: Option<String>,
    reason: Option<String>,
}

// Browser reasons are untrusted text: bound them and strip control characters
// before including them in the state and terminal log.
fn browser_stop_reason(reason: Option<&str>) -> String {
    let reason: String = reason
        .unwrap_or("")
        .chars()
        .filter(|c| !c.is_control())
        .take(160)
        .collect();
    if reason.is_empty() {
        "Stop requested from browser".into()
    } else {
        format!("Browser stop: {reason}")
    }
}

fn authorize(app: &App, auth: &Auth, headers: &HeaderMap) -> Result<(), StatusCode> {
    if auth.token.as_deref() != Some(app.token.as_str()) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    // Reject cross-origin browser requests, including authenticated WS attempts.
    if let Some(origin) = headers.get(header::ORIGIN) {
        let origin = origin.to_str().map_err(|_| StatusCode::FORBIDDEN)?;
        let authority = origin
            .strip_prefix("http://")
            .or_else(|| origin.strip_prefix("https://"));
        if authority != headers.get(header::HOST).and_then(|h| h.to_str().ok()) {
            return Err(StatusCode::FORBIDDEN);
        }
    }
    Ok(())
}

pub fn router(app: App) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/app.js", get(script))
        .route("/ws", get(video_upgrade))
        .route("/control", get(control_upgrade))
        .route("/api/status", get(status))
        .route("/api/stop", post(stop))
        .with_state(app)
}

async fn index() -> impl IntoResponse {
    (
        [
            (header::CACHE_CONTROL, "no-store"),
            (header::REFERRER_POLICY, "no-referrer"),
            (
                header::CONTENT_SECURITY_POLICY,
                "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; connect-src 'self'; img-src 'self' blob:; frame-ancestors 'none'",
            ),
        ],
        Html(include_str!("index.html")),
    )
}
async fn script() -> impl IntoResponse {
    (
        [
            (header::CONTENT_TYPE, "text/javascript; charset=utf-8"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        include_str!("app.js"),
    )
}
async fn status(
    State(app): State<App>,
    Query(auth): Query<Auth>,
    headers: HeaderMap,
) -> Result<Json<Status>, StatusCode> {
    authorize(&app, &auth, &headers)?;
    Ok(Json(app.control.lock().unwrap().status(Instant::now())))
}
async fn stop(
    State(app): State<App>,
    Query(auth): Query<Auth>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, StatusCode> {
    authorize(&app, &auth, &headers)?;
    app.control
        .lock()
        .unwrap()
        .stop(&browser_stop_reason(auth.reason.as_deref()));
    Ok(Json(json!({"stopped": true})))
}
async fn video_upgrade(
    ws: WebSocketUpgrade,
    State(app): State<App>,
    Query(auth): Query<Auth>,
    headers: HeaderMap,
) -> Response {
    if let Err(code) = authorize(&app, &auth, &headers) {
        return code.into_response();
    }
    ws.on_upgrade(move |socket| video(socket, app))
        .into_response()
}
async fn video(mut socket: WebSocket, app: App) {
    let mut frames = app.frames.subscribe();
    loop {
        // Borrow and mark the latest frame, releasing the watch lock before await.
        let frame = frames.borrow_and_update().clone();
        if let Some(frame) = frame {
            if !matches!(
                timeout(
                    Duration::from_secs(2),
                    socket.send(Message::Binary(frame.as_ref().clone()))
                )
                .await,
                Ok(Ok(()))
            ) {
                return;
            }
        }
        loop {
            tokio::select! {
                changed = frames.changed() => {
                    if changed.is_err() { return; }
                    break;
                },
                message = socket.recv() => {
                    match message {
                        Some(Ok(Message::Ping(_) | Message::Pong(_))) => {},
                        _ => return,
                    }
                }
            }
        }
    }
}
async fn control_upgrade(
    ws: WebSocketUpgrade,
    State(app): State<App>,
    Query(auth): Query<Auth>,
    headers: HeaderMap,
) -> Response {
    if let Err(code) = authorize(&app, &auth, &headers) {
        return code.into_response();
    }
    ws.max_message_size(512)
        .max_frame_size(512)
        .on_upgrade(move |socket| control(socket, app))
        .into_response()
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum Input {
    Sample {
        ticket: u64,
        throttle: i16,
        steering: i16,
    },
    Stop {
        #[serde(default)]
        reason: Option<String>,
    },
    Pause,
}

// Dropping a connection, aborted handler or failed send always revokes ownership.
struct Owner {
    state: Shared,
    id: u64,
}
impl Drop for Owner {
    fn drop(&mut self) {
        self.state
            .lock()
            .unwrap()
            .release(self.id, "Control disconnected. Vehicle stopped");
    }
}

async fn send_json(socket: &mut WebSocket, value: serde_json::Value) -> bool {
    matches!(
        timeout(INPUT_TIMEOUT, socket.send(Message::Text(value.to_string()))).await,
        Ok(Ok(()))
    )
}

async fn control(mut socket: WebSocket, app: App) {
    let acquired = app.control.lock().unwrap().acquire(Instant::now());
    let id = match acquired {
        Ok(id) => id,
        Err(error) => {
            send_json(&mut socket, json!({"type":"error", "message":error})).await;
            return;
        }
    };
    let owner = Owner {
        state: app.control.clone(),
        id,
    };
    let mut ticks = interval(Duration::from_millis(50));
    ticks.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut ticket = 0u64;
    loop {
        ticks.tick().await;
        let status = app.control.lock().unwrap().status(Instant::now());
        if !app.control.lock().unwrap().owns(owner.id) {
            send_json(
                &mut socket,
                json!({"type":"error", "message":status.reason}),
            )
            .await;
            return;
        }
        ticket += 1;
        // One outstanding server-issued ticket. Buffered/replayed old browser
        // input cannot refresh the lease after a network/event-loop stall.
        let requested_at = Instant::now();
        if !send_json(
            &mut socket,
            json!({"type":"sample", "ticket":ticket, "status":status}),
        )
        .await
        {
            return;
        }
        let deadline = tokio::time::Instant::from_std(requested_at + INPUT_TIMEOUT);
        loop {
            let message = match tokio::time::timeout_at(deadline, socket.recv()).await {
                Ok(Some(Ok(message))) => message,
                Err(_) => {
                    app.control
                        .lock()
                        .unwrap()
                        .pause(owner.id, "Control response delayed. Vehicle stopped");
                    break; // issue a new ticket, keeping the existing driver
                }
                _ => return,
            };
            let input = match message {
                Message::Text(text) => serde_json::from_str::<Input>(&text),
                Message::Ping(_) | Message::Pong(_) => continue,
                _ => return,
            };
            match input {
                Ok(Input::Sample {
                    ticket: received,
                    throttle,
                    steering,
                }) if received == ticket && requested_at.elapsed() < INPUT_TIMEOUT => {
                    if !app.control.lock().unwrap().drive(
                        owner.id,
                        throttle,
                        steering,
                        Instant::now(),
                    ) {
                        return;
                    }
                    break;
                }
                Ok(Input::Sample {
                    ticket: received, ..
                }) if received <= ticket => {
                    // Late replies are discarded, never applied or used to
                    // renew the session. Wait for the current fresh response.
                    continue;
                }
                Ok(Input::Pause) => {
                    app.control.lock().unwrap().pause(
                        owner.id,
                        "Browser paused driving; waiting for neutral input",
                    );
                }
                Ok(Input::Stop { reason }) => {
                    app.control
                        .lock()
                        .unwrap()
                        .release(owner.id, &browser_stop_reason(reason.as_deref()));
                    return;
                }
                _ => {
                    app.control
                        .lock()
                        .unwrap()
                        .release(owner.id, "Invalid control message");
                    return;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control::{Command, Control};
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::{
        connect_async,
        tungstenite::{self, client::IntoClientRequest},
    };

    async fn serve() -> (String, Shared, Frames, tokio::task::JoinHandle<()>) {
        serve_with_camera(false).await
    }
    async fn serve_with_camera(
        camera_required: bool,
    ) -> (String, Shared, Frames, tokio::task::JoinHandle<()>) {
        let control = Control::new(true, camera_required);
        control.lock().unwrap().serial_ready();
        let (frames, _) = tokio::sync::watch::channel(None);
        let app = App {
            control: control.clone(),
            frames: frames.clone(),
            token: Arc::new("test-control-token".into()),
        };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            axum::serve(listener, router(app)).await.unwrap();
        });
        (format!("ws://{address}"), control, frames, task)
    }
    type Client = tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >;
    async fn ticket(client: &mut Client) -> u64 {
        let frame = timeout(Duration::from_secs(1), client.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let message: serde_json::Value = serde_json::from_str(frame.to_text().unwrap()).unwrap();
        assert_eq!(message["type"], "sample");
        message["ticket"].as_u64().unwrap()
    }

    #[test]
    fn browser_stop_reasons_are_bounded_and_legacy_stop_still_works() {
        assert_eq!(
            browser_stop_reason(Some("Window lost focus\n")),
            "Browser stop: Window lost focus"
        );
        assert_eq!(
            browser_stop_reason(Some(&"x".repeat(200))).len(),
            "Browser stop: ".len() + 160
        );
        assert!(matches!(
            serde_json::from_str::<Input>(r#"{"type":"stop"}"#).unwrap(),
            Input::Stop { reason: None }
        ));
        assert!(matches!(
            serde_json::from_str::<Input>(r#"{"type":"stop","reason":"Window lost focus"}"#)
                .unwrap(),
            Input::Stop { reason: Some(_) }
        ));
    }

    #[tokio::test]
    async fn auth_and_origin_are_checked_before_acquiring() {
        let (base, state, _, task) = serve().await;
        assert!(
            connect_async(format!("{base}/control?token=wrong"))
                .await
                .is_err()
        );
        let mut request = format!("{base}/control?token=test-control-token")
            .into_client_request()
            .unwrap();
        request
            .headers_mut()
            .insert(header::ORIGIN, "http://another-host".parse().unwrap());
        assert!(connect_async(request).await.is_err());
        assert!(!state.lock().unwrap().status(Instant::now()).controlling);
        task.abort();
    }

    #[tokio::test]
    async fn websocket_drives_exclusively_then_disconnect_brakes() {
        let (base, state, _, task) = serve().await;
        let url = format!("{base}/control?token=test-control-token");
        let (mut driver, _) = connect_async(&url).await.unwrap();
        let first = ticket(&mut driver).await;
        driver
            .send(tungstenite::Message::Text(
                json!({"type":"sample", "ticket":first, "throttle":80, "steering":-50}).to_string(),
            ))
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(10)).await;
        let now = Instant::now();
        assert_eq!(state.lock().unwrap().next_command(now), Some(Command::Stop));
        assert_eq!(
            state.lock().unwrap().next_command(now),
            Some(Command::Drive {
                throttle: 80,
                steering: -50
            })
        );
        let (mut observer, _) = connect_async(&url).await.unwrap();
        let rejected = observer.next().await.unwrap().unwrap();
        assert!(rejected.to_text().unwrap().contains("error"));
        assert!(state.lock().unwrap().status(Instant::now()).controlling);
        driver.close(None).await.unwrap();
        tokio::time::sleep(Duration::from_millis(70)).await;
        assert!(!state.lock().unwrap().status(Instant::now()).controlling);
        assert_eq!(
            state.lock().unwrap().next_command(Instant::now()),
            Some(Command::Stop)
        );
        task.abort();
    }

    #[tokio::test]
    async fn delayed_replies_pause_without_losing_ownership_or_replaying_throttle() {
        let (base, state, _, task) = serve().await;
        let url = format!("{base}/control?token=test-control-token");
        let (mut driver, _) = connect_async(&url).await.unwrap();
        let first = ticket(&mut driver).await;
        driver
            .send(tungstenite::Message::Text(
                json!({"type":"sample", "ticket":first, "throttle":100, "steering":0}).to_string(),
            ))
            .await
            .unwrap();
        let second = ticket(&mut driver).await;
        {
            let mut c = state.lock().unwrap();
            c.next_command(Instant::now()); // startup brake
            assert_eq!(
                c.next_command(Instant::now()),
                Some(Command::Drive {
                    throttle: 100,
                    steering: 0
                })
            );
        }
        // A reply arrives after its ticket expired, with a now-stale throttle.
        tokio::time::sleep(INPUT_TIMEOUT + Duration::from_millis(30)).await;
        driver
            .send(tungstenite::Message::Text(
                json!({"type":"sample", "ticket":second, "throttle":255, "steering":0}).to_string(),
            ))
            .await
            .unwrap();
        let fresh = ticket(&mut driver).await;
        assert!(fresh > second);
        assert!(state.lock().unwrap().status(Instant::now()).controlling);
        assert!(state.lock().unwrap().status(Instant::now()).paused);
        assert_eq!(
            state.lock().unwrap().next_command(Instant::now()),
            Some(Command::Stop)
        );
        // Even a fresh held-throttle sample cannot bypass the neutral barrier.
        driver
            .send(tungstenite::Message::Text(
                json!({"type":"sample", "ticket":fresh, "throttle":255, "steering":0}).to_string(),
            ))
            .await
            .unwrap();
        let neutral = ticket(&mut driver).await;
        assert!(state.lock().unwrap().status(Instant::now()).paused);
        assert_eq!(state.lock().unwrap().next_command(Instant::now()), None);
        driver
            .send(tungstenite::Message::Text(
                json!({"type":"sample", "ticket":neutral, "throttle":0, "steering":0}).to_string(),
            ))
            .await
            .unwrap();
        let resumed = ticket(&mut driver).await;
        assert!(!state.lock().unwrap().status(Instant::now()).paused);
        assert_eq!(
            state.lock().unwrap().next_command(Instant::now()),
            Some(Command::Drive {
                throttle: 0,
                steering: 0
            })
        );
        driver
            .send(tungstenite::Message::Text(
                json!({"type":"sample", "ticket":resumed, "throttle":70, "steering":0}).to_string(),
            ))
            .await
            .unwrap();
        ticket(&mut driver).await;
        assert_eq!(
            state.lock().unwrap().next_command(Instant::now()),
            Some(Command::Drive {
                throttle: 70,
                steering: 0
            })
        );
        driver.close(None).await.unwrap();
        task.abort();
    }

    #[tokio::test]
    async fn malformed_axes_stop_and_video_delivers_latest_jpeg() {
        let (base, state, frames, task) = serve().await;
        let (mut driver, _) = connect_async(format!("{base}/control?token=test-control-token"))
            .await
            .unwrap();
        let first = ticket(&mut driver).await;
        driver
            .send(tungstenite::Message::Text(
                json!({"type":"sample", "ticket":first, "throttle":999, "steering":0}).to_string(),
            ))
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(!state.lock().unwrap().status(Instant::now()).controlling);
        let latest = vec![0xff, 0xd8, 4, 5, 0xff, 0xd9];
        frames.send_replace(Some(Arc::new(vec![1, 2, 3])));
        frames.send_replace(Some(Arc::new(latest.clone())));
        let (mut video, _) = connect_async(format!("{base}/ws?token=test-control-token"))
            .await
            .unwrap();
        let received = timeout(Duration::from_secs(1), video.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(received.into_data(), latest);
        video.close(None).await.unwrap();
        task.abort();
    }
    // Optional real-browser smoke test. It uses the real HTTP/WS stack and a
    // simulated command consumer; the serial worker has a separate PTY test.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "requires Node >=22 and Chrome/Chromium (CHROME_BIN)"]
    async fn browser_smoke() {
        let (base, state, frames, server) = serve_with_camera(true).await;
        let commands = Arc::new(std::sync::Mutex::new(Vec::new()));
        let consumer = {
            let state = state.clone();
            let commands = commands.clone();
            tokio::spawn(async move {
                loop {
                    if let Some(command) = state.lock().unwrap().next_command(Instant::now()) {
                        commands.lock().unwrap().push(command);
                    }
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            })
        };
        let image = Arc::new(include_bytes!("../../../docs/images/05-front.png").to_vec());
        let camera_state = state.clone();
        let camera = tokio::spawn(async move {
            loop {
                camera_state.lock().unwrap().frame(Instant::now());
                frames.send_replace(Some(image.clone()));
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        });
        let output = tokio::task::spawn_blocking(move || {
            std::process::Command::new("node")
                .arg(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/browser.mjs"))
                .arg(base.replace("ws://", "http://"))
                .output()
                .expect("launch Node browser test")
        })
        .await
        .unwrap();
        consumer.abort();
        camera.abort();
        server.abort();
        assert!(
            output.status.success(),
            "Browser test failed:\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let log = commands.lock().unwrap();
        assert!(log.contains(&Command::Drive {
            throttle: 80,
            steering: -100
        }));
        assert!(log.contains(&Command::Drive {
            throttle: 80,
            steering: 100
        }));
        assert!(log.contains(&Command::Drive {
            throttle: 0,
            steering: 0
        }));
        assert!(log.iter().filter(|c| **c == Command::Stop).count() >= 3);
        println!("{}", String::from_utf8_lossy(&output.stdout));
    }
}
