# No_std ESP32 rust esp-now mqtt gateway

This project is a no_std esp32 esp-now mqtt gateway. It uses ESP-NOW for communication with remote devices and publishes their data to an MQTT server. The application also listens for updates on the MQTT server for the devices that connect to this gateway and sends them back using esp-now.

End device implementation: [ESP32C3 rust esp-now movement sensor](https://github.com/kamo104/esp32c3-rust-movement-sensor)