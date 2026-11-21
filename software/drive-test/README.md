# Cuttlefish Drive Test

`drive-test` is the Rust application running on the Orange Pi. It streams MJPEG
video from a USB camera to a browser and forwards manual driving commands to
the Arduino over USB Serial.

```text
Laptop browser ◄── MJPEG / WebSocket ── Orange Pi ── USB Serial ── Arduino
               ── Control / WebSocket ►
```

The application runs without a desktop environment on the Orange Pi. The
browser provides the video display and controls. Autonomous navigation and
dataset recording are planned separately.

## Requirements

- Orange Pi running Linux with V4L2 support. The project targets Armbian (Debian 13).
- A current stable Rust toolchain supporting edition 2024.
- A USB camera with MJPG output.
- Arduino Nano running [MCU firmware protocol v1](../../mcu-firmware/README.md).
- A laptop with a current browser and network access to the Orange Pi.

Install the build tools and camera utilities on the Orange Pi:

```bash
sudo apt update
sudo apt install build-essential pkg-config libclang-dev clang v4l-utils
```

These packages do not install Rust. Check that the Rust toolchain is available:

```bash
rustc --version
cargo --version
```

`libclang-dev` and the Linux V4L2 headers are needed by the camera dependency.
The server and control tests can also be built on macOS; camera capture requires
Linux. Use `--no-camera` for control-only bench checks on macOS.

## Device Setup

Find the camera and Arduino device paths:

```bash
v4l2-ctl --list-devices
ls -l /dev/v4l/by-id/
ls -l /dev/serial/by-id/
```

Select the camera's video node rather than its metadata node. Check its modes:

```bash
v4l2-ctl -d /dev/video4 --list-formats-ext
```

Replace `/dev/video4` with the selected device. Persistent paths under
`/dev/v4l/by-id/` and `/dev/serial/by-id/` avoid changes to device numbers
between boots, when those paths are available.

The server user needs permission to access both devices. On Armbian/Debian,
this usually requires membership in the `video` and `dialout` groups:

```bash
sudo usermod -aG dialout,video "$USER"
```

Log out and reconnect after changing groups. Close any Serial Monitor or other
application using the Arduino port before starting the server.

## Building and Running

Run these commands on the Orange Pi, starting from the repository root:

```bash
cd software/drive-test
cargo build --release --locked
./target/release/drive-test /dev/video4 1280 720 30 8080 --serial /dev/ttyUSB0
```

Replace both device paths with the actual camera and Arduino paths. The Nano
must run the current firmware; the original one-letter protocol is incompatible.

The server prints a URL containing an access token:

```text
Open http://<orange-pi-ip>:8080/#token=<access-token>
```

Replace `<orange-pi-ip>` with the Orange Pi's local IP address and open the URL
on the laptop. Copy the complete token printed by the server. Alternatively,
open the page and enter the token in its connection form.

The page stores the token in the tab's `sessionStorage` and removes it from the
address bar. The token is required for video, control and status requests.
Multiple tabs can view video; only one can hold driving control at a time.

Opening the serial port may reset the Nano. The server allows up to five
seconds for protocol identification. Camera or serial errors are reported on
the page and prevent driving; the HTTP server remains available.

Keep the server process running in the SSH session. For a persistent terminal
session, install `tmux` with `sudo apt install tmux`, run `tmux new -s cuttlefish`,
then start the server. Use `Ctrl+B`, then `D` to detach, and
`tmux attach -t cuttlefish` to return. Stop the server with `Ctrl+C`;
SIGTERM also requests a stop before shutdown.

### Access Token

A new token is generated at each startup. To keep the same token between runs,
set `CUTTLEFISH_TOKEN` in the server's environment. It must contain at least
16 ASCII letters, digits, hyphens or underscores.

The default listener is `0.0.0.0`, exposing the server on all IPv4 interfaces.
HTTP does not encrypt the token, video or commands. An SSH tunnel can carry the
connection over SSH instead.

### SSH Tunnel

On the Orange Pi, from `software/drive-test/`, bind the server to loopback:

```bash
./target/release/drive-test /dev/video4 1280 720 30 8080 \
  --serial /dev/ttyUSB0 --bind 127.0.0.1
```

In a terminal on the laptop:

```bash
ssh -N -L 8080:127.0.0.1:8080 orangepi@192.168.1.50
```

Substitute the actual username and IP address. Open
`http://localhost:8080/#token=<access-token>` on the laptop, using the token
printed by the server. Both video and control use the tunnel.

## Browser Controls

Wait for video and Arduino readiness, then click **Take control**.

| Input                  | Action                                               |
| ---------------------- | ---------------------------------------------------- |
| Hold W / Up            | Forward throttle                                     |
| Hold S / Down          | Reverse throttle                                     |
| Hold A / Left          | Steer left                                           |
| Hold D / Right         | Steer right                                          |
| Release throttle keys  | Request zero throttle; firmware decelerates          |
| Release steering keys  | Request centered steering; firmware returns smoothly |
| Space / Esc / **STOP** | Request immediate motor braking and release control  |

Throttle and steering work simultaneously. Opposite directions cancel each
other. WASD uses physical key positions, independently of the selected keyboard
layout. Arrow keys adjust the throttle slider while that slider has focus;
move focus back to the page to use them for driving. Driving keys are ignored
while editing the access-token field.

The throttle slider sets the PWM limit from 1 to 255; the default is 80.
This is a duty-cycle command, not measured speed. Low PWM may not overcome
motor friction. Direction buttons also support holding with a pointer or touch.

Window focus loss or a hidden/closed tab releases control. Brief control delays
or lost/stale video pause motion while retaining the driver's session. When the
connection recovers, the browser clears held inputs and sends neutral axes;
press a direction key again to move. Held-key autorepeat cannot restart motion.
Video checks are disabled with `--no-camera`.

Explicit stop, a closed control WebSocket, device failure or five seconds
without a fresh control response releases the session. Take control again after
these events; a short pause does not require clicking the button.
A viewer with the access token can also use the stop button to stop the driver.

Verify direction, steering clearance, key release, stop, connection loss and
reversal with the driven wheels lifted before a low-speed driving test.
Motion settings are described in the
[firmware documentation](../../mcu-firmware/README.md#motion-and-calibration).

## Communication and Stop Behavior

Control uses a separate WebSocket from video. The server requests current
input nominally every 50 ms, with one request outstanding at a time. Each
request contains a new `ticket`; a drive response is accepted only within
200 ms of issuing that request. Expired replies are discarded, without applying
their axes or renewing the session. A missed deadline pauses motion and starts
a new request on the same WebSocket. Invalid JSON, a future ticket or
out-of-range axes still revoke control.

A dedicated serial thread checks for stale input independently of the HTTP
runtime. A 200 ms input timeout requests `STOP` and retains ownership for up
to five seconds without fresh responses. Once paused, the server requires a
fresh sample with both axes at zero before accepting movement again. Samples
received while waiting for camera recovery also keep motion stopped.
The thread consumes the latest
pending setpoint at most once; a newer setpoint can replace one that has not
yet been sent. It does not periodically resend cached throttle.

The MCU provides a separate communication watchdog: it requests motor braking
when no valid `DRIVE` has been received for 300 ms. This also covers a stalled
or terminated Rust server while the MCU is running. These are software timeout
settings, not guarantees of physical stopping time or distance.

With camera checks enabled, the server requires a captured frame less than
700 ms old. The browser separately checks when it last displayed a frame.
Video connection loss or stale displayed frames pause motion. Video reconnects
independently of control; fresh frames and neutral input allow the existing
session to resume. A slow video socket gets up to two seconds to complete a
write before it is closed and reconnected. These checks do not measure
camera-to-display latency.

Serial I/O errors, a protocol mismatch, an MCU restart or a one-second response
timeout disable serial control. Restore the device and restart the server.
Camera capture errors also require a server restart. A temporary frame stall
pauses motion instead. The MCU's `EVENT TIMEOUT` indicates that its watchdog
already stopped the motor; this pauses the session and requires neutral input
without treating a healthy serial port as disconnected.

The camera supplies JPEG frames, which the server forwards without decoding
or re-encoding. The server retains the latest frame for delivery; the browser
keeps at most one pending frame while decoding another. Network and device
buffers still contribute latency. The page also displays the latest MCU
`STATE` line, containing commanded PWM and servo position rather than sensor
measurements.

## Command-Line Options

```text
drive-test [camera width height fps port] [--serial PATH] [--bind IP] [--no-camera]
```

| Argument        | Default       | Description                                     |
| --------------- | ------------- | ----------------------------------------------- |
| `camera`        | `/dev/video0` | V4L2 video device                               |
| `width`         | `1280`        | Requested frame width                           |
| `height`        | `720`         | Requested frame height                          |
| `fps`           | `30`          | Requested frame rate                            |
| `port`          | `8080`        | HTTP port                                       |
| `--serial PATH` | Unset         | Enable Arduino protocol v1 control              |
| `--bind IP`     | `0.0.0.0`     | Listener IP address                             |
| `--no-camera`   | Disabled      | Disable capture and camera checks for bench use |

Without `--serial`, the server provides video and status with driving disabled.
The camera may negotiate a different format size or frame interval; the server
prints the actual settings. See `drive-test --help` for the command summary.

## HTTP and WebSocket API

API requests and WebSocket connections require `?token=<access-token>`.
Requests containing an `Origin` from a different host or port are rejected.
The page reads the initial token from the URL fragment and includes it in
same-server requests.

| Endpoint          | Purpose                                                                          |
| ----------------- | -------------------------------------------------------------------------------- |
| `GET /api/status` | Camera and serial readiness, control ownership, stop reason and latest MCU state |
| `POST /api/stop`  | Request a stop and revoke the driver's control                                   |
| `WS /ws`          | Binary JPEG frames for viewers                                                   |
| `WS /control`     | Acquire exclusive driving control                                                |

If driving control cannot be acquired, `/control` sends
`{"type":"error","message":"…"}` and closes. Once acquired, a server request
has the following form:

```json
{
  "type": "sample",
  "ticket": 1,
  "status": {
    "serial_ready": true,
    "camera_required": true,
    "camera_ready": true,
    "controlling": true,
    "paused": false,
    "reason": "Control connected",
    "mcu": "STATE t=0 p=0 s=0 a=97 safe=1",
    "frames": 100
  }
}
```

`status` contains the same fields returned by `/api/status`. Reply with the
matching ticket and integer axes:

```json
{ "type": "sample", "ticket": 1, "throttle": 80, "steering": -50 }
```

Throttle is −255…255; steering is −100…100. `status.paused` reports that motion
is stopped while the driver still owns the session. A fresh neutral sample is
required to clear that pause. `{"type":"pause"}` requests the same temporary
stop without giving up control; `{"type":"stop"}` releases control and
requests braking. After disconnection, open a new `/control`
connection to acquire control again.

## Validation

From the repository root:

```bash
cd software/drive-test
cargo test --locked
cargo build --release --locked
```

Rust tests cover control ownership, stale input, stop priority, camera/serial
failure handling, actual WebSocket connections and the serial worker using
pseudo-terminals. They do not validate the Arduino sketch or physical hardware.
The Linux camera path and integrated driving behavior still require validation
on the Orange Pi with the real camera and Arduino.

Optional browser checks require Node.js 22+ and Chrome/Chromium. From
`software/drive-test/`:

```bash
node --check src/app.js
CHROME_BIN=/usr/bin/chromium cargo test --locked browser_smoke -- --ignored --nocapture
```

Substitute the installed browser path. On macOS, the test defaults to Google
Chrome's application path if `CHROME_BIN` is unset. It starts a temporary HTTP
server with test frames and a simulated command consumer, checks browser
controls, a delayed browser event loop, an in-flight status failure, video
reconnection, and desktop/mobile layout. Screenshots are saved in the OS
temporary directory.
The serial worker is tested separately through pseudo-terminals.

### Connection Diagnostics

The server logs control acquisition, pause, recovery and release with a reason.
The browser shows pauses under **Controls**. Read these messages before
restarting to distinguish delayed input/video from USB or camera failure.
The separate HTTP status poll is used only when not driving; its failure does
not close an already-established control connection.
