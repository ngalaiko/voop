# voop

a screenless device to auto-record bike rides

## Architecture

```
   Garmin cadence      Grove GPS
   sensor (BLE)        (UART)
         │                │
         └───────┬────────┘
                 ▼
         ┌───────────────┐
         │   MCU (Rust)  │   ring-buffers data points,
         │   nRF52840    │   streams them over BLE
         └───────┬───────┘
                 │ BLE
                 ▼
         ┌───────────────┐
         │  iOS (Swift)  │   persists points,
         │     app       │   derives rides
         └───────┬───────┘
                 ▼
           Apple Health
```

## Repo layout

```
mcu/           Rust/Embassy firmware for the nRF52840
protocol/      shared wire format (DataPoint, Time) — single source of truth
protocol-ffi/  UniFFI bindings → ios/Voop/Generated/voop_protocol.swift
ios/           Swift/SwiftUI iOS app
```

## Hardware

- [Seeed Studio XIAO ESP32C3](https://wiki.seeedstudio.com/XIAO_ESP32C3_Getting_Started/)
- [Seeed Studio XIAO Expansion Board](https://wiki.seeedstudio.com/Seeeduino-XIAO-Expansion-Board/)
- [Grove - GPS (Air530)](https://wiki.seeedstudio.com/Grove-GPS-Air530/)
- [Garmin Cadence Sensor 2](https://www.garmin.com/en-US/p/641212/)
