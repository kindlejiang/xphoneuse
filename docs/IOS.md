# iOS Support

> **Note.** Since the CoreDevice USB tunnel landed, iOS 17+ mirrors and takes
> input over USB (the developer image is installed by the app), and that is the
> default on iOS 27. This document focuses on the older AirPlay + BLE-HID
> bridge path, which is still used on iOS 16 and as a screen fallback.

XPhoneUse mirrors iPhones over **AirPlay** and reads device information and
screenshots over **usbmuxd**. AirPlay itself is receive-only, so input
control comes from a separate piece of hardware: the **AC636N BLE-HID
bridge** (firmware `fw-AC63_BT_SDK`). When the bridge is plugged in (USB
CDC-ACM), iOS sessions are fully controllable — touch, keyboard, buttons,
scroll and macros — through the bridge's encrypted UART protocol. Without
the bridge, sessions are view-only. Screenshots work either way (with one
caveat on iOS 17+, see below).

## Architecture

```
iPhone ──AirPlay (mirroring)──▶ uxplay.dll (headless fork, FFI) ──NAL callbacks──▶ Rust
                                                                                    │
iPhone ──usbmuxd (USB)────────▶ idevice (Rust) ─── device info / screenshots        │
iPhone ──Bluetooth HID────────▶ AC636N bridge ──USB CDC-ACM────▶ encrypted UART      │
                                                                                    │
                                                    Annex-B AUs + timestamps        │
                                                    (same conversion as scrcpy path)│
                                                                                    ▼
                                              Tauri IPC FrameEvents → WebCodecs → <canvas>
```

### The BLE-HID bridge (input path)

- The bridge enumerates as a **USB CDC-ACM** serial port and controls the
  iPhone over **Bluetooth HID** (keyboard + absolute touch digitizer +
  consumer keys).
- Wire protocol: ASCII command lines (`TAP x,y`, `MDOWN/M/MUP`,
  `KD/KU/K`, `TEXT …`, `KEY HOME/BACK/POWER/VOL±`, `SWIPE`, `SCROLL`,
  `STATUS`…) wrapped in encrypted frames:
  `$ BASE64(AES128-CBC(PKCS7("JLHD"‖DIR‖NONCE‖text)))`.
  Session opens with an `AUTH` frame (NONCE=0); the reply both proves the
  device (`OK DEV MAC=… MODEL=AC636N-KBMS VER=1.4.0`) and carries the first
  one-time NONCE. Every reply rotates the NONCE; replays are rejected.
  Full spec: `fw-AC63_BT_SDK/docs/uart-hid-command-protocol.md` and
  `uart-hid-security-design.md`.
- Host side: `crates/xphoneuse-ios/src/hid.rs` (framing + `HidSession` with
  AUTH handshake, nonce rotation, resync). Touch coordinates map linearly
  from normalized (0..1) to the bridge's 1920×1920 logical plane.
- The bridge is detected at connect time (`AUTH` probe of all serial ports;
  `UART_HID_PORT` pins a port, e.g. `COM10`). Key override:
  `UART_HID_KEY=<32 hex chars>` (dev key is the firmware's placeholder).
- The iPhone must be **paired with the bridge** once (Settings → Bluetooth,
  Just Works, no PIN); it reconnects automatically afterwards.
- Probe tool: `cargo run -p xphoneuse-ios --example hid_probe [-- COM10 [STATUS]]`.

- **uxplay.dll** is a fork of [UxPlay](https://github.com/FDH2/UxPlay)
  (vendored under `vendor/uxplay`, see `FORK.md`) with **GStreamer removed
  entirely**. It exposes a tiny C ABI (`uxplay_api.h`): `uxplay_start` /
  `uxplay_stop` / `uxplay_is_running` plus callbacks that deliver decrypted
  Annex-B NAL access units with stream timestamps (`on_video_data`), flush,
  codec, connect/disconnect and log events. The AirPlay protocol stack
  (`lib/raop*`, mirror buffer, FairPlay crypto, built-in mDNS responder)
  is unchanged pure C.
- The MSVC app loads the MinGW-built DLL dynamically (`libloading`); only
  C-callable symbols cross the toolchain boundary.
- `crates/xphoneuse-ios/src/uxplay_ffi.rs` marshals the callbacks into an
  event queue; `src-tauri/src/ios.rs` converts the Annex-B access units to
  the same `FrameEvent`s the Android/scrcpy path produces, so the frontend
  decoder and renderer are shared.
- **Legacy fallback**: if `uxplay.dll` is missing, the app falls back to
  spawning an old upstream `uxplay.exe` with `-vrtp` (RTP over localhost
  UDP + pure-Rust depacketizer). The fork's own `uxplay.exe` no longer has
  `-vrtp`; it exists as a debug CLI only.

### Runtime dependencies (Windows)

The DLL is built with MinGW-64 (MSYS2 UCRT64). Its full runtime closure:

```
uxplay.dll
libgcc_s_seh-1.dll  libglib-2.0-0.dll  libwinpthread-1.dll  libstdc++-6.dll
libcrypto-3-x64.dll  libplist-2.0.dll
libintl-8.dll  libiconv-2.dll  libpcre2-8-0.dll  libffi-8.dll  zlib1.dll
```

All are bundled into `src-tauri/resources/` by `scripts/build-uxplay.sh`
(release bundles ship them automatically). **No GStreamer, no Bonjour** —
AirPlay discovery uses uxplay's built-in mDNS responder.

## Prerequisites (device side)

1. iPhone connected via **USB** (or previously paired on the same network).
2. **iTunes** or the **Apple Devices** app installed on the PC (this provides
   usbmuxd / Apple Mobile Device Service).
3. Unlock the iPhone and tap **Trust** when prompted.
4. **Developer Mode** - but only to install the developer image, and the app
   will flip it for you. The screen, touch, screenshot and pasteboard services
   come from the **developer disk image** the device mounts, and the device
   refuses to install one while Developer Mode is off (it answers
   `ImageMountFailed`). So when the services are missing, the app downloads and
   mounts a current image (about 16 MB the first time, cached in the app data
   directory), and if the switch is off it warns for 10 seconds, then turns it
   on itself - the same AMFI request *Settings → Privacy & Security →
   Developer Mode* makes, which restarts the phone once. That is the one-time
   thing after each iOS update: once the image is mounted you can turn
   Developer Mode off again and the tunnel keeps working (with it off, iOS 27
   still publishes all five services). Do it by hand if you prefer (unplugging
   the cable during the countdown stops the app from doing it), and
   `XPHONEUSE_DDI_BASE` points the download at another mirror.
   iOS 16 is the same story, not an exception: `screenshotr` cannot be reached
   until **both** Developer Mode is on **and** a classic `DeveloperDiskImage.dmg`
   is mounted at `/Developer` (tested on 16.7.12 - pymobiledevice3's classic API
   failed identically before the mount and returned a 5.5 MB PNG after it, as
   did our own `screenshotr_probe`). Which image a device wants is its iOS
   version, and the app prepares either one: pre-17 the classic image at
   `/Developer`, 17+ the personalized bundle at `/System/Developer` (both from
   the same mirror, cached per version). `pymobiledevice3 mounter auto-mount`
   does the same by hand.

## Building uxplay.dll (MSYS2 + MinGW-64)

UxPlay only builds with MinGW-64 in an MSYS2 environment (it does not build
with the MSVC toolchain the app itself uses, which is why the DLL is loaded
dynamically instead of linked). A helper script does everything: run
`scripts/build-uxplay.sh` **inside an MSYS2 UCRT64 shell**. Manual steps:

1. Install [MSYS2](https://www.msys2.org/) (default location `C:\msys64`).
   Open an **MSYS2 UCRT64** terminal and update: `pacman -Syu`.
2. Install toolchain and dependencies:

   ```
   pacman -S mingw-w64-ucrt-x86_64-cmake mingw-w64-ucrt-x86_64-gcc ninja \
             mingw-w64-ucrt-x86_64-libplist mingw-w64-ucrt-x86_64-openssl \
             mingw-w64-ucrt-x86_64-gettext-runtime mingw-w64-ucrt-x86_64-libiconv \
             mingw-w64-ucrt-x86_64-pcre2 mingw-w64-ucrt-x86_64-libffi mingw-w64-ucrt-x86_64-zlib
   ```

   (glib comes in as a libplist dependency. No GStreamer packages needed.)
3. Build + copy into the app:

   ```
   cd vendor/uxplay
   mkdir -p build && cd build
   cmake .. -G Ninja -DNO_MARCH_NATIVE=ON
   ninja
   ```

   then copy `uxplay.dll` and the dependency DLLs listed above from
   `/ucrt64/bin` into `src-tauri/resources/` (the script does this).
4. Smoke test (starts the server, listens for 5 s, stops):

   ```
   cargo run -p xphoneuse-ios --example uxplay_smoke
   ```

### Windows Firewall

Windows Firewall must allow inbound connections for the app (TCP 7000/5000/7100
for the AirPlay service/stream ports; the video callbacks stay in-process).
Windows usually prompts on first run; if not: *Settings → Update & Security →
Windows Security → Firewall & network protection → Allow an app through
firewall*. If the "XPhoneUse" receiver does not appear in Screen Mirroring,
check the firewall first.

## Usage

1. Plug in the iPhone and tap **Trust**. Pair the iPhone with the AC636N
   bridge once (Settings → Bluetooth → `AC696X_1` or similar; Just Works).
2. The device list shows the iPhone with an  icon, iOS version, and
   pairing state.
3. Click the device. A hint appears: *Control Center → Screen Mirroring →
   "XPhoneUse"*. Tap the iPhone's **Screen Mirroring** button and select
   **XPhoneUse**.
4. The window resizes to the phone's aspect ratio as soon as the first frame
   arrives. If the bridge was detected, the session is fully controllable
   (mouse = touch, keyboard = keys, wheel = scroll); if not, a toast says
   "Input bridge not found - iPhone view-only".

## Non-English text input (AgentInput)

The HID bridge's `TEXT` command is printable-ASCII only (firmware/HID
protocol limit - standard keyboards cannot type CJK). For Chinese and other
non-ASCII input, install [AgentInput](https://github.com/everettjf/Remoboard)
on the iPhone (enable the keyboard in Settings -> General -> Keyboard, with
"Allow Full Access"). **No app-side setup is needed**: when the iPhone starts
mirroring, the app learns its LAN address from the AirPlay connection and
auto-configures AgentInput (port 8787, PIN-less by default). Text containing
non-ASCII is then routed over `ws://<phone>:8787/ws`
(`{"v":1,"t":"input","text":...}`); pure ASCII still goes through the
lower-latency HID bridge. Notes:

- The AgentInput server lives inside the keyboard extension, so it only
  answers while the AgentInput keyboard is the **active** keyboard on screen.
  The app re-dials automatically on each send, so switching keyboards just
  works (dialing takes a moment when the keyboard was closed).
- The Settings -> "iOS Text Input (AgentInput)" panel is only needed if you
  enabled a PIN in the AgentInput app, or to pin a different host/port.
- When AgentInput is configured, the text input bar is available even for
  view-only iOS sessions (no HID bridge needed).
- The `↵` toggle next to the Send button makes each send press Enter on the
  phone afterwards (e.g. to submit a chat message). All input paths support
  it: AgentInput (`input` with `\n`), the HID bridge (ENTER key), Android
  (keycode press), and MCP (`xphoneuse_send_text { text, enter }`).

## Limitations

- **Audio is never forwarded** for iOS sessions (no mute/unmute).
- No home/back/recents on iOS (Android consumer-key semantics); power and
  volume keys go through the bridge where iOS honors them.
- No rotation control (the mirror follows the phone's orientation).
- AirPlay mirroring latency is typically higher than the wired scrcpy path.
- The screen, touch, screenshot and pasteboard services on iOS 17+ (and the
  screenshot service on iOS 16, and the DVT screenshot channel on 17-26) come
  from the developer disk image the device has mounted, so an iOS update (which
  invalidates it) takes them away until a current one is mounted - which the
  app does on its own whenever something comes up short for want of it, from
  the connect path and from a screenshot alike. Every iPhone is asked that
  question before its mirror opens: iOS 16 by probing `screenshotr`, iOS 27+
  by the tunnel start itself - or, when the screen mode is AirPlay and the
  tunnel would not start at all, by a probe of the same service list. With Developer Mode off it
  turns the switch on itself after a countdown, because the device refuses the
  mount without it (installing the image needs it; using an installed one does
  not). That restarts the iPhone once, so it is also the reason a connect can
  end in "the iPhone is restarting to turn on Developer Mode" rather than a
  mirror. Only a **deliberate** capture may do that, though: the AirPlay
  preview screenshots once a second on its own, and an agent's MCP screenshot
  has nobody behind it, so both prepare images and never touch the switch - a
  phone rebooting unannounced is worse than a screenshot that fails with a
  reason. Two more limits keep a broken device from being worked on in a loop:
  Developer Mode is turned on at most once per device per app run, and a
  device's image is prepared at most once a minute.
- The uxplay fork advertises H.264 only (no H.265 mirror negotiation).
- `TEXT` via the bridge is printable ASCII only (firmware limit); non-ASCII
  input goes through the clipboard (USB tunnel) or AgentInput (see above).

## MCP

The MCP server lists iOS devices with an `ios:` prefix. Connect with
`xphoneuse_connect_device { serial: "<udid>", platform: "ios" }` to start the
session; `xphoneuse_take_screenshot` captures the iPhone, and input tools go
through the HID bridge when it is attached.

Reading an iPhone is what `xphoneuse_ocr` is for: the app ships PaddleOCR
models (PP-OCRv6 small, run on MNN) in its resources, so the screen's text is
recognized on the computer with no image tokens spent. Without `path` it
captures the screen first — the same route `xphoneuse_take_screenshot` takes,
live frame → tunnel → DVT — and `format: "json"` returns per-line confidence
and bounding boxes in screen pixels, which is how an agent turns "tap 允许粘贴"
into normalized `tap` coordinates. `XPHONEUSE_OCR_MODELS` overrides the model
directory (see `crates/xphoneuse-ocr`).

`xphoneuse_send_text` types ASCII directly and delivers non-ASCII text the way
the app is configured (Settings → iOS connection → non-ASCII input, default
`clipboard`). `xphoneuse_unicode_text { text, method, enter }` chooses per
call: `clipboard` sets the device pasteboard and presses Cmd+V; `agentinput`
sends it through the AgentInput keyboard (the MCP server shares the app's
AgentInput client). The planning lives in `xphoneuse_ios::text_input`, so the
app, the MCP server and both CLIs agree on what goes where - an unreachable
AgentInput falls back to the clipboard, and the reply says which route ran.
