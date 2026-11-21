// SPDX-License-Identifier: GPL-3.0-only
#include <Arduino.h>
#include <Servo.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

namespace Config {
const uint8_t SERVO_PIN = 9;
const int SERVO_CENTER = 90 + 7;
const int SERVO_LEFT = 43;
const int SERVO_RIGHT = 40;
const bool STEERING_INVERTED = false;
const bool MOTOR_INVERTED = false;
const uint8_t MOTOR_PWM = 3;
const uint8_t MOTOR_IN1 = 2;
const uint8_t MOTOR_IN2 = 4;
const uint8_t MOTOR_STBY = 5;
const uint32_t CONTROL_MS = 10;
const uint32_t WATCHDOG_MS = 300;
const uint32_t FRAME_TIMEOUT_MS = 100;
const uint32_t REVERSE_PAUSE_MS = 150;
const int ACCEL_STEP = 5; // PWM per control tick: 0 -> 255 in 510 ms
const int DECEL_STEP = 10;
const int SERVO_STEP = 2; // degrees per control tick
const uint8_t LINE_SIZE = 48;
const uint8_t RX_BUDGET = 32; // serial flooding must not starve control
} // namespace Config

Servo steering;
int targetThrottle = 0;
int appliedThrottle = 0;
int targetSteering = 0;
int servoAngle = Config::SERVO_CENTER;
bool safe = true;
int lastDirection = 0;
uint32_t zeroSince = 0;
uint32_t lastDrive = 0;
uint32_t lastControl = 0;
char line[Config::LINE_SIZE];
uint8_t lineLength = 0;
bool discardLine = false;
uint32_t frameStarted = 0;

int signOf(int value) { return (value > 0) - (value < 0); }

int approach(int value, int target, int step) {
    if (value < target)
        return min(value + step, target);
    return max(value - step, target);
}

// Never block control waiting for the host to read responses. One reply fits
// the AVR Serial TX buffer. Congested replies may be dropped.
void reply(const char *message) {
    size_t size = strlen(message);
    if (Serial.availableForWrite() >= (int)size)
        Serial.write(message, size);
}

void writeMotor(int throttle) {
    // PWM=0 + both inputs HIGH + STBY HIGH is TB6612FNG short brake.
    // Disable PWM before changing direction inputs.
    analogWrite(Config::MOTOR_PWM, 0);
    int physical = Config::MOTOR_INVERTED ? -throttle : throttle;
    digitalWrite(Config::MOTOR_IN1, physical >= 0 ? HIGH : LOW);
    digitalWrite(Config::MOTOR_IN2, physical <= 0 ? HIGH : LOW);
    digitalWrite(Config::MOTOR_STBY, HIGH);
    if (physical != 0)
        analogWrite(Config::MOTOR_PWM, abs(physical));
}

void stopVehicle(uint32_t now) {
    targetThrottle = 0;
    targetSteering = 0;
    safe = true;
    if (appliedThrottle != 0)
        zeroSince = now;
    appliedThrottle = 0;
    writeMotor(0); // immediate brake; steering returns smoothly
}

void fault(const char *message, uint32_t now) {
    stopVehicle(now);
    reply(message);
}

void checkWatchdog(uint32_t now) {
    if (!safe && uint32_t(now - lastDrive) >= Config::WATCHDOG_MS) {
        stopVehicle(now);
        reply("EVENT TIMEOUT\n");
    }
}

void updateControl(uint32_t now) {
    if (uint32_t(now - lastControl) < Config::CONTROL_MS)
        return;
    lastControl = now; // no burst of catch-up steps after a stall

    int logical = Config::STEERING_INVERTED ? -targetSteering : targetSteering;
    int offset = logical *
                 (logical < 0 ? Config::SERVO_LEFT : Config::SERVO_RIGHT) / 100;
    int nextAngle =
        approach(servoAngle, Config::SERVO_CENTER + offset, Config::SERVO_STEP);
    if (nextAngle != servoAngle) {
        servoAngle = nextAngle;
        steering.write(servoAngle);
    }

    int desired = targetThrottle;
    bool reversing =
        desired != 0 && lastDirection != 0 && signOf(desired) != lastDirection;
    if (reversing) {
        if (appliedThrottle != 0 ||
            uint32_t(now - zeroSince) < Config::REVERSE_PAUSE_MS)
            desired = 0;
    }
    int step = abs(desired) > abs(appliedThrottle) ? Config::ACCEL_STEP
                                                   : Config::DECEL_STEP;
    int next = approach(appliedThrottle, desired, step);
    if (next != appliedThrottle) {
        if (next == 0)
            zeroSince = now;
        else
            lastDirection = signOf(next);
        appliedThrottle = next;
        writeMotor(appliedThrottle);
    }
}

bool readNumber(char *token, int limit, int &value) {
    if (!token || !*token)
        return false;
    // Bound before conversion, including on AVR where int is only 16 bits.
    const char *p = token;
    if (*p == '-' || *p == '+')
        ++p;
    if (!*p)
        return false;
    int result = 0;
    for (; *p; ++p) {
        if (*p < '0' || *p > '9')
            return false;
        int digit = *p - '0';
        if (result > (limit - digit) / 10)
            return false;
        result = result * 10 + digit;
        if (result > limit)
            return false;
    }
    value = token[0] == '-' ? -result : result;
    return true;
}

void processLine(uint32_t now) {
    char *context = nullptr;
    char *command = strtok_r(line, " \t", &context);
    if (!command)
        return; // empty lines do not renew watchdog
    char *first = strtok_r(nullptr, " \t", &context);
    char *second = strtok_r(nullptr, " \t", &context);
    char *extra = strtok_r(nullptr, " \t", &context);
    if (strcmp(command, "DRIVE") == 0) {
        int throttle, turn;
        if (extra || !readNumber(first, 255, throttle) ||
            !readNumber(second, 100, turn)) {
            fault("ERR DRIVE\n", now);
            return;
        }
        // Commit both values together, only after validating the entire frame.
        targetThrottle = throttle;
        targetSteering = turn;
        lastDrive = now;
        safe = false;
        reply("OK DRIVE\n");
    } else if (strcmp(command, "STOP") == 0 && !first) {
        stopVehicle(now);
        reply("OK STOP\n");
    } else if (strcmp(command, "STATUS") == 0 && !first) {
        char response[64];
        snprintf(response, sizeof(response),
                 "STATE t=%d p=%d s=%d a=%d safe=%d\n", targetThrottle,
                 appliedThrottle, targetSteering, servoAngle, safe ? 1 : 0);
        reply(response);
    } else if (strcmp(command, "HELLO") == 0 && !first) {
        reply("HELLO CUTTLEFISH 1\n");
    } else {
        fault("ERR COMMAND\n", now);
    }
}

void readSerial(uint32_t now) {
    if (lineLength &&
        uint32_t(now - frameStarted) >= Config::FRAME_TIMEOUT_MS) {
        lineLength = 0;
        discardLine = true; // never reinterpret a broken frame's tail
        fault("ERR FRAME_TIMEOUT\n", now);
    }
    for (uint8_t count = 0; count < Config::RX_BUDGET && Serial.available();
         ++count) {
        char c = (char)Serial.read();
        if (c == '\n') {
            if (!discardLine) {
                line[lineLength] = '\0';
                processLine(now);
            }
            lineLength = 0;
            discardLine = false;
        } else if (!discardLine) {
            // Accept LF and CRLF; CR is whitespace, never a command delimiter.
            if (c == '\r')
                c = ' ';
            if ((c < 32 && c != '\t') || c > 126 ||
                lineLength >= sizeof(line) - 1) {
                lineLength = 0;
                discardLine = true;
                fault("ERR FRAME\n", now);
            } else {
                if (lineLength == 0)
                    frameStarted = now;
                line[lineLength++] = c;
            }
        }
    }
}

void setup() {
    pinMode(Config::MOTOR_STBY, OUTPUT);
    digitalWrite(Config::MOTOR_STBY, LOW);
    pinMode(Config::MOTOR_PWM, OUTPUT);
    pinMode(Config::MOTOR_IN1, OUTPUT);
    pinMode(Config::MOTOR_IN2, OUTPUT);
    stopVehicle(millis());
    // Preload the calibrated center before attach to avoid a pulse at 90
    // degrees.
    steering.write(Config::SERVO_CENTER);
    steering.attach(Config::SERVO_PIN);
    Serial.begin(115200);
    lastControl = millis();
    reply("HELLO CUTTLEFISH 1\n");
}

void loop() {
    uint32_t now = millis();
    checkWatchdog(now);
    updateControl(now);
    readSerial(now);
}
