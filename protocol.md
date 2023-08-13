# Bluetooth protocol

I will attempt to document the commands that I can find here. [This
stackexchange
post](https://security.stackexchange.com/questions/205534/problem-with-reverse-engineering-gvm-light-possibly-encoding-issue)
contained the pieces I was missing - the checksum algo is CRC-16/XMODEM. I
lifted that algo from
[here](https://github.com/kelvinlawson/xmodem-1k/blob/master/crc16.c).

`4c540900305700` -- appears to be the header on most command packets

Packet Format

The packet appears to communicate a 1 byte command and a 1 byte argument. These take the following format.

Let's look at the command `4c54090030570005010589ab`, the command to set the
saturation to 5% in HSI mode.

```
4c540900305700 -- Header common to most commands (afaict)
05 -- command (set saturation)
01 -- constant
05 -- arg (5%)
89ab -- CRC-16/XMODEM checksum of previous data
```

# Commands

- `0x00` -- power
- `0x01`
- `0x02` -- intensity
- `0x03` -- color temperature
- `0x04` -- hue
- `0x05` -- saturation
- `0x06`
- `0x07` -- pick scene
- `0x08` -- scene transition interval

# Misc

```
4c5409000053000001009474 -- seems to be sent by the application at session start, different header
```

# Modes -- `0x06`

```
06 01 -- CCT (corrected color temperature)
06 02 -- HSI (hue / saturation / intensity)
06 03 -- Scene mode
```

# Power -- `0x00`

```
00 01 -- turn on -- blue in wireshark
00 00 -- turn off
```

# HSI

## Hue -- `0x04`

Argument is in range `[0x00, 0x53)`, values above 0x53 will turn off the light.

```
04 30 -- blue
04 10 -- green
04 3c -- pink
```

## Intensity -- `0x02`

```
02 03 -- 3%
02 04 -- 4%
02 07 -- 7%
02 11 -- 17%
02 25 -- 38%
02 64 -- 100%
```

## Saturation -- `0x05`

```
05 05 -- 5%
05 31 -- 49%
05 64 -- 100%
```

# Scene Mode

Scenes can be sent an "intensity" packet to set the brightness during their
playback (`0x02`).

## Pick scene -- `0x07`

```
07 01 -- lightning
07 02 -- cop car
07 03 -- candle
07 04 -- tv
07 05 -- bad bulb
07 06 -- party
07 07 -- disco
07 08 -- paparazzi
```

## Scene transition interval -- `0x08`

Appears to be a value in the range `[0x01, 0x32]` which is a point in the
interval `[0.1s, 5s]`. This is the time between each light color transition in
the scene.

```
08 01 -- 0.1s
08 32 -- 5s
```

# Color Temperature `0x03`

Value in the range `[0x20, 0x38]` which is a point in the interval [3200K,
5600K].
