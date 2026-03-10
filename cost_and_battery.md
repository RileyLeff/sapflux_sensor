# Cost & Battery Life Comparison: 8-Sensor Sap Flux Station

## Legacy vs Standard Field Instruments v1.1

|                                | **Legacy** | **SFI v1.1** |
|--------------------------------|------------|--------------|
| **Hardware** | | |
| Logic board / logger           | $1,500     | $20          |
| Per sensor                     | $1,000     | $5           |
| 8x sensors                     | $8,000     | $40          |
| **Total system cost**          | **$9,500** | **$60**      |
| **Sensor Coverage** | | |
| Thermistor pairs per sensor    | 2 (2 depths) | 3 (3 depths) |
| Total thermistor channels      | 32         | 48           |
| Total measurement depths       | 16         | 24           |
| **Power (avg @ 30-min interval)** | | |
| System voltage                 | 12V        | 24V          |
| Board idle                     | 84.8 mW   | 0.36 mW      |
| Board idle (from battery)      | 7,067 µA   | 15 µA        |
| 8x sensor idle                 | 384 mW     | 0 mW         |
| Heater pulses (avg)            | 76.8 mW   | 128.0 mW     |
| Measurement electronics (avg)  | (incl above) | 12.9 mW   |
| **Total average power**        | **545.6 mW** | **141.2 mW** |
| **Battery Life (96 Wh)**       | **7.3 days** | **28.3 days** |

## Assumptions

- Legacy baseline: empirical 200 mW measurement of 2-sensor + 1-logger station on 12V
- Legacy sensor specs (IX-SF30): idle 4 mA @ 12V, measurement cycle 0.4 mAh net additional @ 12V
- Legacy logger idle derived by subtraction: 200 - 2(48) - 2(4.8/0.5) = 84.8 mW
- Heater needle resistance: 40 ohms (both systems)
- Legacy heater pulse: 3s @ 12V (3.6W) = 3.00 mWh/sensor/cycle
- SFI heater pulse: 2s @ 24V (14.4W) = 8.00 mWh/sensor/cycle
- SFI RS-485 in polling mode: 50ms window every 10s (4 µA avg), no interaction assumed
- SFI board cost assumes 10k manufacturing volume
- SFI passive sensors: thermistors + resistors + cable + housing (~$5 each)
