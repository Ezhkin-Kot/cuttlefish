#include <Servo.h>

const int BT_RX = 10;
const int BT_TX = 11;
const int SERVO_PIN = 9;
const int MOTOR_PWMA = 3;
const int MOTOR_AIN1 = 2;
const int MOTOR_AIN2 = 4;
const int MOTOR_STBY = 5;

const int SERVO_CENTER = 90;
const int SERVO_LIMIT = 40; 

unsigned long lastServoUpdate = 0;
const int STEERING_INTERVAL = 15; 
const float STEERING_STEP = 0.8; // Шаг можно чуть увеличить для отзывчивости

float currentAngle = 90.0;
float targetAngle = 90.0;

// Флаги для удержания поворота
bool steeringLeft = false;
bool steeringRight = false;

Servo steering;

void setup() {
  Serial.begin(115200);
  steering.attach(SERVO_PIN);
  steering.write(SERVO_CENTER);
  pinMode(MOTOR_PWMA, OUTPUT);
  pinMode(MOTOR_AIN1, OUTPUT);
  pinMode(MOTOR_AIN2, OUTPUT);
  pinMode(MOTOR_STBY, OUTPUT);
  digitalWrite(MOTOR_STBY, HIGH);
}

void loop() {
  if (Serial.available()) {
    char cmd = Serial.read();
    
    switch (cmd) {
      case 'w': moveMotor(true, 50);  break;
      case 's': moveMotor(false, 50); break;
      case 'x': stopMotor(); break;

      // УПРАВЛЕНИЕ ПОВОРОТОМ
      case 'a': steeringLeft = true;  steeringRight = false; break;
      case 'd': steeringRight = true; steeringLeft = false;  break;
      case 'e': steeringLeft = false;  break; // Отпустили влево — замерли
      case 'q': steeringRight = false; break; // Отпустили вправо — замерли

      case 'c': // МГНОВЕННЫЙ ВОЗВРАТ В ЦЕНТР
        targetAngle = SERVO_CENTER;
        currentAngle = SERVO_CENTER;
        steering.write(SERVO_CENTER);
        steeringLeft = false;
        steeringRight = false;
        break;
    }
  }

  unsigned long currentMillis = millis();
  if (currentMillis - lastServoUpdate >= STEERING_INTERVAL) {
    lastServoUpdate = currentMillis;

    // Если зажата кнопка, двигаем цель в соответствующую сторону
    if (steeringLeft) {
      targetAngle -= STEERING_STEP;
    } else if (steeringRight) {
      targetAngle += STEERING_STEP;
    }

    // Ограничиваем цель лимитами
    targetAngle = constrain(targetAngle, SERVO_CENTER - SERVO_LIMIT, SERVO_CENTER + SERVO_LIMIT);

    // Плавно подтягиваем физический угол серво к цели
    if (abs(currentAngle - targetAngle) > 0.1) {
      if (currentAngle < targetAngle) currentAngle += STEERING_STEP;
      else if (currentAngle > targetAngle) currentAngle -= STEERING_STEP;
      
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
