# No_std ESP32 rust esp-now mqtt gateway

This project is an embedded Rust application designed to run on an ESP32 microcontroller without the standard library (`no_std`). It uses ESP-NOW for communication with remote devices and publishes their data to an MQTT server. The application also listens for updates on the MQTT server for the devices that connect to this gateway.
