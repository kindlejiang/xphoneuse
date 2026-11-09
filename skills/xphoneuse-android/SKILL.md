---
name: xphoneuse-android
description: Control Android devices from AI agents using the XPhoneUse MCP server. Use when asked to interact with, test, automate, or take screenshots of an Android device. Provides tools for touch input, text entry, button presses, screenshots, app launching, shell commands, and swipe gestures.
---

# XPhoneUse - Android Device Control

You have access to an Android device through the XPhoneUse MCP server. Use these tools to see what's on screen, interact with the device, and automate tasks.

## Setup

The XPhoneUse desktop app must be running with MCP Server enabled (on by default in Settings).

Add to your MCP settings (Claude Code, Claude Desktop, Cursor, etc.):

```json
{
  "mcpServers": {
    "xphoneuse": {
      "type": "http",
      "url": "http://localhost:7070/mcp"
    }
  }
}
```

## Workflow

Always follow this order:

1. **List devices** to see what's available
2. **Connect** to establish a control session
3. **Screenshot** to see the current screen state
4. **Act** using touch, text, buttons, swipe, etc.
5. **Screenshot again** to verify the result
6. **Disconnect** when done

## Available Tools

### Device Management

| Tool | What it does |
|------|-------------|
| `xphoneuse_list_devices` | List all connected Android devices |
| `xphoneuse_connect_device` | Connect to a device (starts scrcpy control session) |
| `xphoneuse_disconnect_device` | Disconnect from the current device |

### Observation

| Tool | What it does |
|------|-------------|
| `xphoneuse_take_screenshot` | Capture the screen as a PNG image |

### Input

| Tool | What it does |
|------|-------------|
| `xphoneuse_press_button` | Press a hardware button: `home`, `back`, `recents`, `power`, `volume_up`, `volume_down` |
| `xphoneuse_send_text` | Type text into the focused input field |
| `xphoneuse_send_touch` | Send a touch event (`down`, `up`, `move`) at normalized coordinates (0.0-1.0) |
| `xphoneuse_send_scroll` | Scroll at a position with a given delta |
| `xphoneuse_swipe` | Swipe from one point to another (normalized 0.0-1.0 coordinates) |

### Apps & Shell

| Tool | What it does |
|------|-------------|
| `xphoneuse_launch_app` | Launch an app by package name (e.g. `com.android.chrome`) |
| `xphoneuse_open_url` | Open a URL in the device's default browser |
| `xphoneuse_shell` | Run an arbitrary adb shell command and get the output |

### WiFi

| Tool | What it does |
|------|-------------|
| `xphoneuse_wifi_enable` | Switch a USB device to WiFi debugging mode |
| `xphoneuse_wifi_connect` | Connect to a device by IP address |
| `xphoneuse_wifi_disconnect` | Disconnect a WiFi device |
| `xphoneuse_get_device_ip` | Get the WiFi IP of a USB-connected device |

## Coordinate System

Touch, swipe, and scroll tools use **normalized coordinates** from 0.0 to 1.0:
- `(0.0, 0.0)` = top-left corner
- `(1.0, 1.0)` = bottom-right corner
- `(0.5, 0.5)` = center of screen

## Common Patterns

### Tap on something
```
xphoneuse_send_touch { action: "down", x: 0.5, y: 0.5 }
xphoneuse_send_touch { action: "up", x: 0.5, y: 0.5 }
```

### Tap, then verify
```
1. xphoneuse_take_screenshot  (see what's on screen)
2. xphoneuse_send_touch       (tap where you need to)
3. xphoneuse_take_screenshot  (verify the result)
```

### Type into a field
```
1. Tap the input field with xphoneuse_send_touch
2. xphoneuse_send_text { text: "hello world" }
```

### Scroll down a page
```
xphoneuse_send_scroll { x: 0.5, y: 0.5, dx: 0.0, dy: -1.0 }
```

### Swipe to go back or navigate
```
xphoneuse_swipe { from_x: 0.0, from_y: 0.5, to_x: 0.5, to_y: 0.5 }
```

### Navigate with buttons
```
xphoneuse_press_button { button: "home" }
xphoneuse_press_button { button: "back" }
xphoneuse_press_button { button: "recents" }
```

### Open an app and interact
```
1. xphoneuse_launch_app { package: "com.android.chrome" }
2. Wait briefly, then xphoneuse_take_screenshot
3. Interact as needed
```

### Find an element on screen
Take a screenshot, examine it to identify positions, then use the coordinates to interact. Screenshots are the primary way to understand what's visible on the device.

### Run a shell command
```
xphoneuse_shell { command: "pm list packages" }
xphoneuse_shell { command: "dumpsys battery" }
xphoneuse_shell { command: "settings get system screen_brightness" }
```

## Tips

- **Always screenshot first** before interacting. You need to see the screen to know where to tap.
- **Screenshot after actions** to confirm they worked.
- **Use shell commands** for things that don't need the screen (checking battery, listing packages, getting device info).
- **Swipe duration** defaults to 300ms. Increase `duration_ms` for slower, more deliberate swipes.
- **Connect before acting**. Tools like press_button, send_text, send_touch, swipe require an active connection via `xphoneuse_connect_device`.
- **list_devices, shell, take_screenshot, wifi_* tools** work without a scrcpy connection, they only need adb.
