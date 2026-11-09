#!/usr/bin/env python3
"""xphoneuse - CLI for controlling Android and iOS devices through the
XPhoneUse MCP server (streamable HTTP). Python twin of the Rust
xphoneuse-cli: same commands, same output. Every command maps to one MCP
tool call; `compare` and screenshot comparison run entirely locally so
agents can verify screen state without spending tokens on image data.

The file commands (`files`, `push`, `pull`) exist only here - the Rust
twin has not been given them.

Standard library only - no pip install needed. Pillow is used
automatically when present (faster image comparison, more PNG variants);
without it a built-in decoder handles the common 8-bit non-interlaced
PNGs the app produces.
"""

import argparse
import base64
import io
import json
import os
import sys
import urllib.error
import urllib.request
import zlib

VERSION = "0.1.0"
DEFAULT_URL = "http://127.0.0.1:7070/mcp"


class CliError(Exception):
    """Fatal error; message is printed and the process exits 1."""


# ---------------------------------------------------------------------------
# Minimal MCP streamable-HTTP client: initialize once per invocation, then a
# single tools/call. The session is discarded when the process exits.
# ---------------------------------------------------------------------------

class Mcp:
    def __init__(self, url, timeout):
        self.url = url
        self.timeout = timeout
        self.session = None
        # The endpoint is always local; never route through a system proxy.
        self.opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))

    @classmethod
    def open(cls, url, timeout):
        mcp = cls(url, timeout)
        body = {
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": {"name": "xphoneuse-cli", "version": VERSION},
            },
        }
        resp = mcp._post(body)
        sid = resp.headers.get("mcp-session-id")
        if sid:
            mcp.session = sid
        mcp._parse_body(resp)  # drain the initialize result
        mcp.notify("notifications/initialized")
        return mcp

    def _post(self, body):
        data = json.dumps(body).encode("utf-8")
        headers = {
            "Accept": "application/json, text/event-stream",
            "Content-Type": "application/json",
        }
        if self.session:
            headers["Mcp-Session-Id"] = self.session
        req = urllib.request.Request(self.url, data=data, headers=headers, method="POST")
        try:
            return self.opener.open(req, timeout=self.timeout)
        except urllib.error.HTTPError as e:
            raise CliError(f"HTTP {e.code} from MCP server")
        except urllib.error.URLError as e:
            raise CliError(
                f"request to {self.url} failed - the XPhoneUse app is not "
                f"running (or its MCP server is off). Download and start it "
                f"from https://xphoneuse.com, then try again. "
                f"({e.reason})"
            )

    def _parse_body(self, resp):
        """The server answers with either a plain JSON object or an SSE
        stream; normalize both to a single dict."""
        content_type = resp.headers.get("Content-Type", "")
        body = resp.read()
        if "text/event-stream" in content_type:
            for line in body.decode("utf-8", "replace").splitlines():
                if not line.startswith("data:"):
                    continue
                data = line[len("data:"):].strip()
                if not data:
                    continue
                try:
                    v = json.loads(data)
                except ValueError:
                    continue
                # Both successful results and JSON-RPC errors count; call()
                # turns the latter into a CliError.
                if "result" in v or "error" in v:
                    return v
            raise CliError("MCP server closed the stream without a response")
        try:
            return json.loads(body)
        except ValueError:
            raise CliError("failed to parse MCP response")

    def notify(self, method):
        self._post({"jsonrpc": "2.0", "method": method}).read()

    def call(self, name, arguments):
        """One tools/call; returns (text_blocks, images, is_error).

        Each image is a (bytes, mime) pair - the screenshot tool answers with
        JPEG or PNG depending on the format asked for, and the caller has to
        know which to name the file and read its size."""
        body = {
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {"name": name, "arguments": arguments},
        }
        v = self._parse_body(self._post(body))
        if "error" in v:
            raise CliError(f"MCP error: {v['error'].get('message', 'unknown')}")
        result = v.get("result")
        if result is None:
            raise CliError("MCP response has no result")
        is_error = bool(result.get("isError", False))
        text, images = [], []
        for item in result.get("content", []):
            if item.get("type") == "text":
                t = item.get("text")
                if t is not None:
                    text.append(t)
            elif item.get("type") == "image":
                data = item.get("data")
                if data:
                    try:
                        images.append((base64.b64decode(data), item.get("mimeType") or "image/png"))
                    except Exception as e:
                        text.append(f"Error: undecodable image data: {e}")
        return text, images, is_error


# ---------------------------------------------------------------------------
# PNG decoding + comparison (local, no MCP involved). Pillow when available,
# otherwise a built-in decoder for 8-bit non-interlaced PNGs.
# ---------------------------------------------------------------------------

def load_png_rgba(source):
    """Decode a PNG (bytes or file path) to (width, height, rgba_bytes).
    RGBA so every pixel compares as exactly 4 channels."""
    try:
        from PIL import Image
    except ImportError:
        return _decode_png_pure(source)
    img = Image.open(io.BytesIO(source) if isinstance(source, (bytes, bytearray)) else source)
    img = img.convert("RGBA")
    return img.width, img.height, img.tobytes()


_PNG_SIG = b"\x89PNG\r\n\x1a\n"


def _decode_png_pure(source):
    if isinstance(source, (bytes, bytearray)):
        buf = bytes(source)
    else:
        with open(source, "rb") as f:
            buf = f.read()
    if not buf.startswith(_PNG_SIG):
        raise ValueError("not a PNG file")

    width = height = bit_depth = color_type = None
    palette = trns = None
    idat = bytearray()
    pos = 8
    while pos + 8 <= len(buf):
        length = int.from_bytes(buf[pos:pos + 4], "big")
        ctype = buf[pos + 4:pos + 8]
        data = buf[pos + 8:pos + 8 + length]
        pos += 12 + length
        if ctype == b"IHDR":
            width = int.from_bytes(data[0:4], "big")
            height = int.from_bytes(data[4:8], "big")
            bit_depth, color_type = data[8], data[9]
            if data[12] != 0:
                raise ValueError("interlaced PNG not supported (install Pillow)")
        elif ctype == b"PLTE":
            palette = data
        elif ctype == b"tRNS":
            trns = data
        elif ctype == b"IDAT":
            idat += data
        elif ctype == b"IEND":
            break
    if width is None:
        raise ValueError("truncated PNG")
    if bit_depth != 8:
        raise ValueError(f"unsupported bit depth {bit_depth} (install Pillow)")
    channels = {0: 1, 2: 3, 3: 1, 4: 2, 6: 4}.get(color_type)
    if channels is None:
        raise ValueError(f"unsupported color type {color_type} (install Pillow)")

    raw = zlib.decompress(bytes(idat))
    stride = width * channels
    out = bytearray(height * stride)
    prev = bytearray(stride)
    p = 0
    for y in range(height):
        filt = raw[p]
        p += 1
        line = bytearray(raw[p:p + stride])
        p += stride
        if filt == 1:  # Sub
            for i in range(channels, stride):
                line[i] = (line[i] + line[i - channels]) & 0xFF
        elif filt == 2:  # Up
            for i in range(stride):
                line[i] = (line[i] + prev[i]) & 0xFF
        elif filt == 3:  # Average
            for i in range(stride):
                left = line[i - channels] if i >= channels else 0
                line[i] = (line[i] + ((left + prev[i]) >> 1)) & 0xFF
        elif filt == 4:  # Paeth
            for i in range(stride):
                a = line[i - channels] if i >= channels else 0
                b = prev[i]
                c = prev[i - channels] if i >= channels else 0
                pr = a + b - c
                pa, pb, pc = abs(pr - a), abs(pr - b), abs(pr - c)
                if pa <= pb and pa <= pc:
                    pred = a
                elif pb <= pc:
                    pred = b
                else:
                    pred = c
                line[i] = (line[i] + pred) & 0xFF
        out[y * stride:(y + 1) * stride] = line
        prev = line

    if color_type == 6:  # already RGBA
        return width, height, bytes(out)
    rgba = bytearray(width * height * 4)
    for i in range(width * height):
        o = i * channels
        if color_type == 2:  # RGB
            rgba[i * 4:i * 4 + 3] = out[o:o + 3]
            rgba[i * 4 + 3] = 255
        elif color_type == 0:  # gray
            g = out[o]
            rgba[i * 4:i * 4 + 3] = bytes((g, g, g))
            rgba[i * 4 + 3] = 255
        elif color_type == 4:  # gray + alpha
            g, a = out[o], out[o + 1]
            rgba[i * 4:i * 4 + 3] = bytes((g, g, g))
            rgba[i * 4 + 3] = a
        else:  # 3: palette
            idx = out[o] * 3
            if idx + 2 >= len(palette):
                raise ValueError("palette index out of range")
            rgba[i * 4:i * 4 + 3] = palette[idx:idx + 3]
            rgba[i * 4 + 3] = trns[o] if trns and o < len(trns) else 255
    return width, height, bytes(rgba)


def compare_buffers(w, h, a, b, tolerance):
    """Per-pixel RGBA comparison. Returns (changed_pct, mean_diff, bbox).
    bbox is (x0, y0, x1, y1) inclusive, or None when nothing changed."""
    if a == b:  # fast path: identical buffers (the common UNCHANGED case)
        return 0.0, 0.0, None
    total = max(w * h, 1)
    changed = 0
    diff_sum = 0
    bbox = None
    row_bytes = w * 4
    for y in range(h):
        ra = a[y * row_bytes:(y + 1) * row_bytes]
        rb = b[y * row_bytes:(y + 1) * row_bytes]
        if ra == rb:  # whole row identical - skip the pixel loop
            continue
        for x in range(w):
            i = x * 4
            d0 = ra[i] - rb[i]
            d1 = ra[i + 1] - rb[i + 1]
            d2 = ra[i + 2] - rb[i + 2]
            d3 = ra[i + 3] - rb[i + 3]
            if d0 < 0:
                d0 = -d0
            if d1 < 0:
                d1 = -d1
            if d2 < 0:
                d2 = -d2
            if d3 < 0:
                d3 = -d3
            max_d = d0 if d0 > d1 else d1
            if d2 > max_d:
                max_d = d2
            if d3 > max_d:
                max_d = d3
            diff_sum += d0 + d1 + d2 + d3
            if max_d > tolerance:
                changed += 1
                if bbox is None:
                    bbox = [x, y, x, y]
                else:
                    if x < bbox[0]:
                        bbox[0] = x
                    if y < bbox[1]:
                        bbox[1] = y
                    if x > bbox[2]:
                        bbox[2] = x
                    if y > bbox[3]:
                        bbox[3] = y
    return 100.0 * changed / total, diff_sum / (total * 4.0), tuple(bbox) if bbox else None


def print_verdict(changed_pct, mean_diff, bbox, dims_a, dims_b, threshold, path_a, path_b):
    """Print the comparison report; returns True when the screen counts as CHANGED."""
    wa, ha = dims_a
    wb, hb = dims_b
    if (wa, ha) != (wb, hb):
        print(f"VERDICT: CHANGED (image sizes differ: {wa}x{ha} vs {wb}x{hb})")
        return True
    changed = changed_pct > threshold
    verdict = "CHANGED" if changed else "UNCHANGED"
    print(f"VERDICT: {verdict} ({changed_pct:.2f}% of pixels differ, threshold {threshold:.2f}%)")
    if bbox:
        x0, y0, x1, y1 = bbox
        print(
            f"changed region: x {x0}-{x1}, y {y0}-{y1} "
            f"(normalized {x0 / wa:.3f}-{(x1 + 1) / wa:.3f}, {y0 / ha:.3f}-{(y1 + 1) / ha:.3f})"
        )
    print(f"mean channel diff: {mean_diff:.2f}")
    print(f"compared: {path_a} vs {path_b}")
    return changed


# ---------------------------------------------------------------------------
# Commands
# ---------------------------------------------------------------------------

def finish(text, images, is_error):
    """Print tool text; exit non-zero when the server reported an error.
    `images` is unused here - only the screenshot command consumes them."""
    failed = is_error or any(
        t.startswith(("Error:", "Failed")) or t == "No device connected" for t in text
    )
    for t in text:
        print(t)
    if failed:
        sys.exit(1)


def cmd_compare(args):
    """Local image comparison - no device or MCP call involved."""
    wa, ha, a = load_png_rgba(args.a)
    wb, hb, b = load_png_rgba(args.b)
    if (wa, ha) != (wb, hb):
        print_verdict(100.0, 255.0, None, (wa, ha), (wb, hb), args.threshold, args.a, args.b)
        sys.exit(0)
    changed_pct, mean_diff, bbox = compare_buffers(wa, ha, a, b, args.tolerance)
    changed = print_verdict(changed_pct, mean_diff, bbox, (wa, ha), (wb, hb),
                            args.threshold, args.a, args.b)
    sys.exit(0)  # verdict is informational; only MCP errors exit non-zero


def image_size(data):
    """(width, height) of a PNG or JPEG, from its headers alone - no decoder,
    so it works for the JPEG the screenshot tool sends by default."""
    if data[:8] == b"\x89PNG\r\n\x1a\n":
        if len(data) >= 24:
            return (int.from_bytes(data[16:20], "big"),
                    int.from_bytes(data[20:24], "big"))
        return 0, 0
    if data[:2] == b"\xff\xd8":  # JPEG: walk the segments to the frame header
        i, n = 2, len(data)
        while i + 9 < n:
            if data[i] != 0xFF:
                i += 1
                continue
            marker = data[i + 1]
            if marker == 0xD8 or marker == 0x01 or 0xD0 <= marker <= 0xD7:
                i += 2
                continue
            if marker in (0xD9, 0xDA):  # end of image / start of scan
                break
            if marker in (0xC0, 0xC1, 0xC2, 0xC3, 0xC5, 0xC6, 0xC7,
                          0xC9, 0xCA, 0xCB, 0xCD, 0xCE, 0xCF):
                return (int.from_bytes(data[i + 7:i + 9], "big"),
                        int.from_bytes(data[i + 5:i + 7], "big"))
            i += 2 + int.from_bytes(data[i + 2:i + 4], "big")
    return 0, 0


# What the model gets by default: scaled to 1568px and JPEG (a phone capture
# is a few megabytes of PNG, far more than looking at it needs).
DEFAULT_SCREENSHOT_SUFFIX = ".jpg"


def cmd_screenshot(mcp, args):
    # Comparison is a pixel question, so it asks for the lossless capture;
    # looking at the screen takes the default - small, and good enough.
    text, images, is_error = mcp.call(
        "xphoneuse_take_screenshot", {"format": "png"} if args.compare else {}
    )
    if not images:
        finish(text, is_error)
        return
    png, mime = images[0]

    # Load the reference BEFORE writing the output file: the common pattern
    # is `screenshot --compare xphoneuse-screenshot.png`, where reference and
    # output are the same path. A missing reference is not an error - it just
    # means this capture starts a new reference.
    reference_img = None
    first_reference = False
    reference_dims = None
    if args.compare:
        if not os.path.exists(args.compare):
            first_reference = True
        else:
            try:
                rw, rh, reference_img = load_png_rgba(args.compare)
                reference_dims = (rw, rh)
            except Exception as e:
                raise CliError(f"failed to open reference {args.compare}: {e}")

    default_name = "xphoneuse-screenshot" + (
        ".png" if mime == "image/png" else DEFAULT_SCREENSHOT_SUFFIX
    )
    path = args.output or default_name
    with open(path, "wb") as f:
        f.write(png)
    w, h = image_size(png)
    print(f"saved: {path} ({w}x{h}, {len(png) // 1024} KB)")
    if text and text[0].strip():
        print(text[0])
    if first_reference:
        print(
            f"no reference {args.compare} yet - this capture starts one; "
            f"view {path} to see the screen"
        )
    if reference_img is not None:
        wn, hn, new_img = load_png_rgba(png)
        changed_pct, mean_diff, bbox = compare_buffers(wn, hn, reference_img, new_img, args.tolerance)
        changed = print_verdict(changed_pct, mean_diff, bbox, reference_dims, (wn, hn),
                                args.threshold, args.compare, path)
        if changed:
            print(f"the screen moved on - view {path} to see the new state")
        else:
            print("screen is the same as the reference - no need to view the new capture")
    if is_error:
        sys.exit(1)


# ---------------------------------------------------------------------------
# Argument parsing - mirrors the Rust clap surface (same names and defaults)
# ---------------------------------------------------------------------------

def build_parser():
    p = argparse.ArgumentParser(
        prog="xphoneuse",
        description="Control Android/iOS devices through the XPhoneUse MCP server",
    )
    p.add_argument("--url", default=os.environ.get("XPHONEUSE_URL", DEFAULT_URL),
                   help="MCP endpoint of the XPhoneUse app")
    p.add_argument("--timeout-secs", type=int, default=120,
                   help="Request timeout in seconds (screenshots can be slow on iOS)")
    sub = p.add_subparsers(dest="command", required=True)

    sub.add_parser("list", help="List connected devices; lines are prefixed android:/ios:")

    c = sub.add_parser("connect", help="Connect to a device and start a control session")
    c.add_argument("serial", help="Device serial (Android) or UDID (iOS)")
    c.add_argument("-p", "--platform", default="android",
                   help="Device platform: android or ios")

    sub.add_parser("disconnect", help="Disconnect from the current device")

    c = sub.add_parser("screenshot", help="Capture the device screen to a file (JPEG by default: the "
                                       "capture is scaled to 1568px and compressed - --compare uses "
                                       "the lossless PNG instead)")
    c.add_argument("-o", "--output", help="Output file (default ./xphoneuse-screenshot.png, overwritten)")
    c.add_argument("--compare", metavar="REF",
                   help="Also compare the capture with this reference PNG and print a CHANGED/UNCHANGED verdict")
    c.add_argument("--threshold", type=float, default=0.5,
                   help="Changed-pixel percentage above which the screen counts as CHANGED")
    c.add_argument("--tolerance", type=int, default=16,
                   help="Per-channel difference tolerated when comparing pixels")

    c = sub.add_parser("ocr", help="Read text off the screen (offline OCR); no image tokens")
    c.add_argument("--path", metavar="FILE",
                   help="Read this image file instead of the device screen. The path is on the "
                        "machine running the app, which is this one unless --url points elsewhere.")
    c.add_argument("--json", action="store_true",
                   help="Print {width, height, lines:[{text, confidence, bbox}]} instead of plain text")
    c.add_argument("--min-confidence", type=float, default=0.5,
                   help="Drop lines recognized with less confidence than this (0-1)")

    c = sub.add_parser("press", help="Press a hardware button: home, back, recents, power, volume_up, volume_down")
    c.add_argument("button")

    c = sub.add_parser("text", help="Type text into the focused field")
    c.add_argument("text")
    # Enter is pressed unless told otherwise: in chat apps it is the send
    # button, and "send this" is the ordinary task. Drafting is the opt-in.
    c.add_argument("--enter", dest="enter", action="store_true", default=True,
                   help="Press Enter after typing (default)")
    c.add_argument("--no-enter", dest="enter", action="store_false",
                   help="Only put the text in the field, do not send it")

    c = sub.add_parser("unicode-text",
                       help="Type text choosing how non-ASCII is delivered on iOS: "
                            "clipboard (device pasteboard + Cmd+V) or agentinput (AgentInput keyboard)")
    c.add_argument("text")
    # No default method: the app's own setting decides, which is what the
    # user configured (Settings -> iOS connection). Pass one to override.
    c.add_argument("--method", "-m", default=None,
                   help="clipboard or agentinput; left unset, the app's own setting "
                        "decides. ASCII is typed either way")
    c.add_argument("--enter", dest="enter", action="store_true", default=True,
                   help="Press Enter after typing (default)")
    c.add_argument("--no-enter", dest="enter", action="store_false",
                   help="Only put the text in the field, do not send it")

    c = sub.add_parser("tap", help="Tap at normalized coordinates (0.0-1.0) - single down+up, no long-press risk")
    c.add_argument("x", type=float)
    c.add_argument("y", type=float)

    c = sub.add_parser("longtap", help="Long-press at normalized coordinates (0.0-1.0): down, hold, up")
    c.add_argument("x", type=float)
    c.add_argument("y", type=float)
    c.add_argument("ms", nargs="?", type=int, default=800,
                   help="Hold duration in milliseconds (default 800)")

    c = sub.add_parser("touch", help="Send a touch event at normalized coordinates (0.0-1.0)")
    c.add_argument("action", help="down, up or move")
    c.add_argument("x", type=float)
    c.add_argument("y", type=float)

    c = sub.add_parser("scroll", help="Scroll at a position by a delta (normalized)")
    c.add_argument("x", type=float)
    c.add_argument("y", type=float)
    c.add_argument("dx", type=float)
    c.add_argument("dy", type=float)

    c = sub.add_parser("swipe", help="Swipe between two points (normalized coordinates)")
    c.add_argument("from_x", type=float)
    c.add_argument("from_y", type=float)
    c.add_argument("to_x", type=float)
    c.add_argument("to_y", type=float)
    c.add_argument("--duration-ms", type=int, dest="duration_ms")

    c = sub.add_parser("shell", help="Run an adb shell command (Android only); all trailing args are joined")
    c.add_argument("command", nargs=argparse.REMAINDER)

    c = sub.add_parser("open-url", help="Open a URL (Android only)")
    # dest must differ from the global --url option (argparse subparsers
    # write into the same namespace).
    c.add_argument("url_arg", metavar="URL")
    c.add_argument("--browser", help="Browser package (default com.android.chrome)")
    c.add_argument("--system", action="store_true", help="Use the system default URL handler")

    c = sub.add_parser("launch", help="Open an app by identifier - package name on Android "
                                       "(com.tencent.mm), bundle id on iOS (com.tencent.xin); see "
                                       "APP-IDS.md in this skill")
    c.add_argument("package", help="App identifier (package name / bundle id)")

    c = sub.add_parser("apps", help="List installed apps with their identifiers (Android: pm list "
                                    "packages; iOS: the installation proxy) - filter it, a full list "
                                    "is hundreds of lines")
    c.add_argument("--filter", "-f", default=None,
                   help="Case-insensitive substring of the identifier, or of the app's name on iOS")
    c.add_argument("--kind", "-k", default="user",
                   help="user (default), system, or all")

    c = sub.add_parser("files", help="List a directory on the phone (iOS: the media area only; "
                                     "Android: anywhere adb reaches)")
    c.add_argument("path", nargs="?", default=None,
                   help="platform:/path (android:/sdcard/DCIM, ios:/DCIM), or a bare path for the "
                        "connected phone. Default: the phone's files root.")
    c.add_argument("--filter", "-f", default=None,
                   help="Only entries whose name contains this (case-insensitive)")

    c = sub.add_parser("push", help="Send a file from this computer to the phone")
    c.add_argument("local", help="The file to send (its name is kept on the phone)")
    c.add_argument("remote", nargs="?", default=None,
                   help="Directory on the phone to put it in (default: the phone's files root). "
                        "An existing name is numbered, never overwritten.")

    c = sub.add_parser("pull", help="Copy a file off the phone onto this computer")
    c.add_argument("remote",
                   help="platform:/path (android:/sdcard/DCIM/123.mp4), or a bare path for the "
                        "connected phone")
    c.add_argument("-o", "--output", default=None,
                   help="Local directory to save it into (default: the current directory). An "
                        "existing name is numbered, never overwritten.")

    sub.add_parser("ui-tree", help="Get the UI accessibility tree (Android only)")

    c = sub.add_parser("find", help="Search UI elements on screen (Android only)")
    c.add_argument("--text", help="Match by text content")
    c.add_argument("--desc", dest="content_desc", help="Match by content description")
    c.add_argument("--id", dest="resource_id", help="Match by resource ID")
    c.add_argument("--class", dest="class_name", help="Match by class name (Button, EditText, TextView, ...)")
    c.add_argument("--clickable", action="store_true", help="Only return clickable elements")

    sub.add_parser("rotate", help="Rotate the device screen (Android only)")

    c = sub.add_parser("macro", help="Record, play and manage macros (Android and iOS)")
    m = c.add_subparsers(dest="macro_action", required=True)
    m.add_parser("record", help="Start recording actions into a named macro").add_argument("name")
    m.add_parser("stop", help="Stop recording and save the macro")
    play = m.add_parser("play", help="Play a recorded macro")
    play.add_argument("name")
    play.add_argument("--repeat", type=int)
    m.add_parser("list", help="List recorded macros")
    m.add_parser("delete", help="Delete a macro").add_argument("name")

    c = sub.add_parser("wifi", help="WiFi debugging helpers (Android only)")
    w = c.add_subparsers(dest="wifi_action", required=True)
    w.add_parser("enable", help="Enable WiFi debugging on a USB-connected device").add_argument("serial")
    w.add_parser("connect", help="Connect to a device by ip:port").add_argument("address")
    w.add_parser("disconnect", help="Disconnect a WiFi device by ip:port").add_argument("address")
    w.add_parser("ip", help="Get the WiFi IP of a USB-connected device").add_argument("serial")

    c = sub.add_parser("compare", help="Compare two PNG files locally; no device or MCP call involved")
    c.add_argument("a")
    c.add_argument("b")
    c.add_argument("--threshold", type=float, default=0.5,
                   help="Changed-pixel percentage above which images count as CHANGED")
    c.add_argument("--tolerance", type=int, default=16,
                   help="Per-channel difference tolerated when comparing pixels")

    return p


def main():
    # Windows consoles often default to GBK; tool text may contain any Unicode.
    for stream in (sys.stdout, sys.stderr):
        if hasattr(stream, "reconfigure"):
            stream.reconfigure(encoding="utf-8", errors="replace")

    args = build_parser().parse_args()

    try:
        if args.command == "compare":
            cmd_compare(args)
            return

        mcp = Mcp.open(args.url, args.timeout_secs)

        if args.command == "list":
            finish(*mcp.call("xphoneuse_list_devices", {}))
        elif args.command == "connect":
            finish(*mcp.call("xphoneuse_connect_device",
                             {"serial": args.serial, "platform": args.platform}))
        elif args.command == "disconnect":
            finish(*mcp.call("xphoneuse_disconnect_device", {}))
        elif args.command == "screenshot":
            cmd_screenshot(mcp, args)
        elif args.command == "ocr":
            # No image handling here on purpose: the server OCRs, the CLI
            # prints text, so nothing has to travel back as bytes.
            finish(*mcp.call("xphoneuse_ocr", {
                "path": args.path,
                "format": "json" if args.json else "text",
                "min_confidence": args.min_confidence,
            }))
        elif args.command == "press":
            finish(*mcp.call("xphoneuse_press_button", {"button": args.button}))
        elif args.command == "text":
            finish(*mcp.call("xphoneuse_send_text", {"text": args.text, "enter": args.enter}))
        elif args.command == "unicode-text":
            finish(*mcp.call("xphoneuse_unicode_text",
                             {"text": args.text, "method": args.method, "enter": args.enter}))
        elif args.command == "tap":
            finish(*mcp.call("xphoneuse_tap", {"x": args.x, "y": args.y}))
        elif args.command == "longtap":
            finish(*mcp.call("xphoneuse_longtap",
                             {"x": args.x, "y": args.y, "duration_ms": args.ms}))
        elif args.command == "touch":
            finish(*mcp.call("xphoneuse_send_touch",
                             {"action": args.action, "x": args.x, "y": args.y}))
        elif args.command == "scroll":
            finish(*mcp.call("xphoneuse_send_scroll",
                             {"x": args.x, "y": args.y, "dx": args.dx, "dy": args.dy}))
        elif args.command == "swipe":
            swipe = {"from_x": args.from_x, "from_y": args.from_y,
                     "to_x": args.to_x, "to_y": args.to_y}
            if args.duration_ms is not None:
                swipe["duration_ms"] = args.duration_ms
            finish(*mcp.call("xphoneuse_swipe", swipe))
        elif args.command == "shell":
            finish(*mcp.call("xphoneuse_shell", {"command": " ".join(args.command)}))
        elif args.command == "open-url":
            open_args = {"url": args.url_arg, "use_system_handler": args.system}
            if args.browser:
                open_args["browser_package"] = args.browser
            finish(*mcp.call("xphoneuse_open_url", open_args))
        elif args.command == "launch":
            finish(*mcp.call("xphoneuse_launch_app", {"package": args.package}))
        elif args.command == "apps":
            args_out = {"kind": args.kind}
            if args.filter:
                args_out["filter"] = args.filter
            finish(*mcp.call("xphoneuse_list_apps", args_out))
        elif args.command == "files":
            call = {}
            if args.path:
                call["path"] = args.path
            if args.filter:
                call["filter"] = args.filter
            finish(*mcp.call("xphoneuse_list_files", call))
        elif args.command == "push":
            # The host path is read by the app, so a relative one would land
            # wherever *it* is running from.
            call = {"host_path": os.path.abspath(args.local)}
            if args.remote:
                call["path"] = args.remote
            finish(*mcp.call("xphoneuse_push_file", call))
        elif args.command == "pull":
            finish(*mcp.call("xphoneuse_pull_file", {
                "path": args.remote,
                "dest": os.path.abspath(args.output or "."),
            }))
        elif args.command == "ui-tree":
            finish(*mcp.call("xphoneuse_get_ui_tree", {}))
        elif args.command == "find":
            find = {}
            if args.text is not None:
                find["text"] = args.text
            if args.content_desc is not None:
                find["content_desc"] = args.content_desc
            if args.resource_id is not None:
                find["resource_id"] = args.resource_id
            if args.class_name is not None:
                find["class_name"] = args.class_name
            if args.clickable:
                find["clickable_only"] = True
            finish(*mcp.call("xphoneuse_find_on_screen", find))
        elif args.command == "rotate":
            finish(*mcp.call("xphoneuse_rotate_device", {}))
        elif args.command == "macro":
            if args.macro_action == "record":
                finish(*mcp.call("xphoneuse_macro_record", {"name": args.name}))
            elif args.macro_action == "stop":
                finish(*mcp.call("xphoneuse_macro_stop", {}))
            elif args.macro_action == "play":
                play = {"name": args.name}
                if args.repeat is not None:
                    play["repeat"] = args.repeat
                finish(*mcp.call("xphoneuse_macro_play", play))
            elif args.macro_action == "list":
                finish(*mcp.call("xphoneuse_macro_list", {}))
            elif args.macro_action == "delete":
                finish(*mcp.call("xphoneuse_macro_delete", {"name": args.name}))
        elif args.command == "wifi":
            if args.wifi_action == "enable":
                finish(*mcp.call("xphoneuse_wifi_enable", {"serial": args.serial}))
            elif args.wifi_action == "connect":
                finish(*mcp.call("xphoneuse_wifi_connect", {"address": args.address}))
            elif args.wifi_action == "disconnect":
                finish(*mcp.call("xphoneuse_wifi_disconnect", {"address": args.address}))
            elif args.wifi_action == "ip":
                finish(*mcp.call("xphoneuse_get_device_ip", {"serial": args.serial}))
    except CliError as e:
        print(f"Error: {e}", file=sys.stderr)
        sys.exit(1)


if __name__ == "__main__":
    main()
