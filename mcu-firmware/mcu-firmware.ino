#include <Servo.h>

// Servo
const int SERVO_PIN = 9;
const int SERVO_CENTER = 90;
const int SERVO_TRIM = 7;
const int SERVO_LIMIT_R = 40;
const int SERVO_LIMIT_L = 43;

// Motor driver
const int MOTOR_PWMA = 3;
const int MOTOR_AIN1 = 2;
const int MOTOR_AIN2 = 4;
const int MOTOR_STBY = 5;

// Steering movement
const unsigned long STEERING_INTERVAL = 15;
const float STEERING_STEP = 0.8;

unsigned long lastServoUpdate = 0;

float currentAngle = 90.0;
float targetAngle = 90.0;

bool steeringLeft = false;
bool steeringRight = false;

Servo steering;

int calibratedCenter() {
  return SERVO_CENTER + SERVO_TRIM;
}

void setup() {
  Serial.begin(115200);

  pinMode(MOTOR_PWMA, OUTPUT);
  pinMode(MOTOR_AIN1, OUTPUT);
  pinMode(MOTOR_AIN2, OUTPUT);
  pinMode(MOTOR_STBY, OUTPUT);

  digitalWrite(MOTOR_STBY, HIGH);

  steering.attach(SERVO_PIN);

  currentAngle = calibratedCenter();
  targetAngle = calibratedCenter();

  steering.write(calibratedCenter());
}

void loop() {
  while (Serial.available()) {
    char cmd = Serial.read();

    switch (cmd) {
      case 'w':
        moveMotor(true, 250);
        break;

      case 's':
        moveMotor(false, 250);
        break;

      case 'x':
        stopMotor();
        break;

      case 'a':
        steeringLeft = true;
        steeringRight = false;
        break;

      case 'd':
        steeringRight = true;
        steeringLeft = false;
        break;

      case 'e':
        steeringLeft = false;
        break;

      case 'q':
        steeringRight = false;
        break;

      case 'c':
        targetAngle = calibratedCenter();
        currentAngle = calibratedCenter();

        steering.write(calibratedCenter());

        steeringLeft = false;
        steeringRight = false;
        break;
    }
  }

  unsigned long currentMillis = millis();

  if (currentMillis - lastServoUpdate >= STEERING_INTERVAL) {
    lastServoUpdate = currentMillis;

    int center = calibratedCenter();

    if (steeringLeft) {
      targetAngle -= STEERING_STEP;
    }
    else if (steeringRight) {
      targetAngle += STEERING_STEP;
    }

    targetAngle = constrain(
      targetAngle,
      center - SERVO_LIMIT_L,
      center + SERVO_LIMIT_R
    );

    float difference = targetAngle - currentAngle;

    if (fabsf(difference) > 0.1) {
      if (fabsf(difference) <= STEERING_STEP) {
        currentAngle = targetAngle;
      }
      else if (difference > 0) {
        currentAngle += STEERING_STEP;
      }
      else {
        currentAngle -= STEERING_STEP;
      }

      steering.write((int)currentAngle);
    }
  }
}

void moveMotor(bool forward, int speed) {
  digitalWrite(MOTOR_AIN1, forward ? HIGH : LOW);
  digitalWrite(MOTOR_AIN2, forward ? LOW : HIGH);

  analogWrite(MOTOR_PWMA, speed);
}

void stopMotor() {
  analogWrite(MOTOR_PWMA, 0);
}
