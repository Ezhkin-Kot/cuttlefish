# Cuttlefish MCU Firmware

Arduino Nano (ATmega328P), MG90S steering, TB6612FNG motor driver.
USB Serial: **115200 baud, 8N1**. Pinout: servo D9,
PWMA D3, AIN1 D2, AIN2 D4, STBY D5.

The MCU accepts complete throttle/steering setpoints, ramps the outputs,
and brakes if fresh drive commands stop arriving. No encoders are installed:
throttle controls PWM duty cycle, not measured speed. Reported steering is
commanded servo position, not a sensor reading.

## Building and Uploading

Install Arduino CLI, the `arduino:avr` core and the `Servo` library, or open
`mcu-firmware.ino` in Arduino IDE with board **Arduino Nano / ATmega328P**.

```bash
arduino-cli core install arduino:avr
arduino-cli lib install Servo
arduino-cli compile --fqbn arduino:avr:nano mcu-firmware
arduino-cli board list
arduino-cli upload --fqbn arduino:avr:nano --port /dev/ttyUSB0 mcu-firmware
```

Run these commands from the repository root. Substitute the actual serial
port. For a Nano with the old bootloader, use
`arduino:avr:nano:cpu=atmega328old` for both compilation and upload.
Close the controller or serial monitor before uploading.

## Laptop Control

Use the [Rust video + web-control server](../software/drive-test/README.md)
on the Orange Pi and open its page on the laptop. It bridges browser input to
this MCU protocol and stops forwarding drive commands when the browser becomes
stale. This is the recommended path for remote driving.

## Serial Protocol v1

The following values and timing settings describe the current sketch.
Integrated operation on the vehicle still requires hardware validation.

ASCII lines, case sensitive, terminated by LF (`\n`); CRLF also works.
Spaces and tabs separate tokens. Maximum **47 bytes before LF**, including CR.
No checksums or request IDs are provided. This is a local USB protocol.

| Request                       | Meaning                                                                | Response                            |
| ----------------------------- | ---------------------------------------------------------------------- | ----------------------------------- |
| `HELLO`                       | Identify firmware/protocol; also emitted at boot                       | `HELLO CUTTLEFISH 1`                |
| `DRIVE <throttle> <steering>` | Atomically set both axes and renew the 300 ms lease                    | `OK DRIVE`                          |
| `STOP`                        | Immediate motor short brake, cancel throttle, center steering smoothly | `OK STOP`                           |
| `STATUS`                      | Read commanded state, without renewing the lease                       | `STATE t=80 p=25 s=-50 a=87 safe=0` |

Throttle: integer **−255…255**, negative reverse, positive forward.
Steering: integer **−100…100**, negative left, positive right, zero center.
The default servo mapping is −100 → 54°, 0 → 97°, +100 → 137°.

`STATE` fields: `t` target throttle; `p` applied signed PWM; `s` target
steering percent; `a` current commanded servo angle in degrees; `safe=1`
means stopped by startup, STOP, timeout or protocol error. A new valid `DRIVE`
sets `safe=0` and permits driving again; STOP is not a latched emergency lock.

For example, send `DRIVE 80 -50\n` at 20 Hz for moderate forward throttle and
half left steering. Send `DRIVE 0 0\n` to decelerate and center, or `STOP\n`
for immediate short braking. A single command in a serial monitor expires
after 300 ms; keep streaming complete setpoints for sustained movement.

Only valid `DRIVE` frames renew the lease. Queries, blank lines and partial
frames do not. The MCU emits `EVENT TIMEOUT` once when the lease expires.
The motor is immediately short-braked and steering returns to center.
There is no measured guarantee of physical stopping distance.

Malformed commands stop the vehicle immediately and return `ERR DRIVE`
(invalid axes/arity), `ERR COMMAND` (unknown command/extra arguments),
`ERR FRAME` (oversized frame or invalid character), or `ERR FRAME_TIMEOUT`
(unfinished line after 100 ms). Oversized frames, invalid characters and
frame timeouts discard input through the next LF. Syntax and range errors
reject the already-completed line; parsing resumes with the following line.
The two axes are never partially applied. Old one-letter commands are replaced by this protocol, which is
used by the Rust server in `software/drive-test/`.

Responses are best effort: if the Serial TX buffer lacks room for a complete
reply, that reply is dropped so control and the watchdog cannot block.
Read responses continuously. Streaming much faster than the recommended
20–50 Hz can queue stale commands; send current state rather than buffered
history. A network bridge must stop sending DRIVE when its upstream
controller disconnects or becomes stale, instead of repeatedly forwarding
the last command. The MCU watchdog alone cannot detect that situation.

## Motion and Calibration

Settings live in `namespace Config` at the top of the sketch:

- Servo calibration: center 90° + 7° trim; left limit 43°,
  right limit 40°. Check clearance over the whole range on the actual linkage.
- `STEERING_INVERTED` and `MOTOR_INVERTED` correct reversed installation.
- Servo commands change by up to 2° per 10 ms tick (nominally 200°/s).
  The physical servo response may differ.
- Acceleration is 5 PWM counts per 10 ms tick (0→255 in about 510 ms
  with regular updates and a continuously renewed drive command).
  Deceleration is 10 PWM counts per tick.
- Reversal first ramps down to zero, brakes for at least 150 ms, then ramps
  into the opposite direction. STOP and watchdog braking skip the ramp;
  they preserve the reverse interlock.
- Startup requests short brake and calibrated servo center. Short brake uses
  STBY HIGH, AIN1/AIN2 HIGH and PWM LOW, following the
  [Toshiba TB6612FNG truth table](https://www.pololu.com/file/0J86/TB6612FNG.pdf).
  This is electrical braking, without wheel speed feedback.

Start verification with driven wheels lifted: confirm forward polarity,
steering direction and clearance, simultaneous keys, key release, Space,
focus loss, client termination, USB loss and reversal. Then tune throttle,
ramp and reversal pause at low speed on the floor. The 150 ms interval is a
starting setting, not a measurement of when this motor has stopped.

## Validation

Use the Arduino CLI build command above to compile the sketch for the Nano.
The repository currently contains no standalone MCU test suite. The Rust
server's [validation checks](../software/drive-test/README.md#validation)
exercise its control and serial bridge using simulated devices; they do not
execute the Arduino sketch.

Verify the firmware on the actual Arduino, servo and motor before driving.
