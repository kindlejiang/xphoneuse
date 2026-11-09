import { useState, useRef, useCallback, useEffect } from "react";
import { invoke, Channel } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow, currentMonitor, LogicalSize, PhysicalPosition } from "@tauri-apps/api/window";
import type { Device, IosDevice, Settings, FrameEvent, Screen, MacroEvent } from "../types";
import { isIosDevice, getDeviceKey, resolveIosScreenMode, resolveIosInputMode } from "../types";
import { t } from "../i18n";
import { resolveSaveDir, fileTimestamp, mirrorChromeHeight, mirrorWindowSize } from "../lib/utils";

/**
 * Move the window back inside its monitor's work area so that `target` fits
 * there. The mirror's input bar sits at the bottom, so a window that grew past
 * the screen would look like a missing control rather than a too-big window.
 *
 * The target size is passed in rather than read back from the window: this
 * runs before the resize lands, and the OS may still report the old size.
 *
 * Callers must treat this as best-effort and keep it out of the way of the
 * resize itself: moving a window needs `core:window:allow-set-position`, and a
 * rejection here once took the resize down with it (see fitWindowToMirror).
 */
async function keepWindowOnScreen(
  win: ReturnType<typeof getCurrentWindow>,
  target: { width: number; height: number },
  scale: number,
): Promise<void> {
  const [pos, mon] = await Promise.all([win.outerPosition(), currentMonitor()]);
  if (!mon) return;
  const area = mon.workArea;
  const targetW = Math.round(target.width * scale);
  const targetH = Math.round(target.height * scale);
  const x = Math.round(
    Math.max(area.position.x, Math.min(pos.x, area.position.x + area.size.width - targetW)),
  );
  const y = Math.round(
    Math.max(area.position.y, Math.min(pos.y, area.position.y + area.size.height - targetH)),
  );
  if (x !== pos.x || y !== pos.y) {
    await win.setPosition(new PhysicalPosition(x, y));
  }
}

/** Resolves once the webview reports a size different from `before`, or when
 *  the budget runs out - the resize is the compositor's to apply, so there is
 *  nothing to await but the result. */
async function waitForWebviewResize(
  before: { width: number; height: number },
  budgetMs: number,
): Promise<void> {
  const deadline = performance.now() + budgetMs;
  while (performance.now() < deadline) {
    if (window.innerWidth !== before.width || window.innerHeight !== before.height) return;
    await new Promise((r) => setTimeout(r, 25));
  }
}

/**
 * Size the window for a viewport of `request` minus the chrome, then correct
 * for what the window manager actually handed over.
 *
 * `setSize` is a request about the *window*, not a promise about the webview
 * inside it. On Wayland/GTK the client-side header bar comes out of the height
 * - measured on Ubuntu 22.04, 2026-10-10: asking for 534x1240 left a 534x1204
 * webview - while the width arrives untouched, and Tauri reports
 * outerSize == innerSize there, so the frame cannot be subtracted up front the
 * way it is on Windows. 36px of unaccounted height makes the viewport wider
 * than the device aspect, which is what paints background slivers down the
 * left and right of the mirror.
 *
 * The shortfall is therefore measured off the webview and paid back out of the
 * width: the window keeps the height it was granted (that budget is already
 * spent against the work area), and the mirror loses a few pixels of scale
 * instead of the window growing off the screen. Where the request is honoured
 * exactly - Windows, macOS - the measurement agrees and nothing is re-issued.
 */
async function sizeWindowForViewport(
  win: ReturnType<typeof getCurrentWindow>,
  request: { width: number; height: number },
  chromeHeightFor: (windowWidth: number) => number,
  viewportAspect: number,
  programmatic: { current: boolean },
): Promise<void> {
  programmatic.current = true;
  try {
    const before = { width: window.innerWidth, height: window.innerHeight };
    await win.setSize(new LogicalSize(request.width, request.height));
    await waitForWebviewResize(before, 400);
    const gotW = window.innerWidth;
    const gotH = window.innerHeight - chromeHeightFor(gotW);
    if (gotW <= 0 || gotH <= 0) return;
    const wantW = Math.round(gotH * viewportAspect);
    // One pixel counts. The canvas is centred in the viewport, so a viewport
    // one pixel wider than the device's aspect leaves half a pixel of
    // background on each side - which the compositor blends into a pale
    // column down the left edge, exactly the hairline this correction exists
    // to remove (measured on a real window, 2026-10-11: client 569, canvas
    // 568, one background pixel at x=2). A tolerance of two let that through.
    if (wantW === gotW) return;
    await win.setSize(new LogicalSize(wantW, request.height));
  } catch (e) {
    // Logged rather than swallowed: a resize that fails silently looks
    // exactly like a window that was never meant to be resized.
    console.error(`[fit] could not size the window to ${request.width}x${request.height}`, e);
  } finally {
    programmatic.current = false;
  }
}

/**
 * Size the window to the device: as large as the monitor's work area allows
 * (see mirrorWindowSize), then nudged back on screen. Called on connect and
 * again whenever the stream's dimensions change.
 *
 * `chromeHeightFor` gives the chrome around the viewport: the title bar plus
 * the text-input bar when the session shows one.
 *
 * The budget is the monitor's work area (taskbar excluded) minus the OS frame
 * - the resize must leave both the taskbar and the frame their space.
 */
async function fitWindowToMirror(
  deviceWidth: number,
  deviceHeight: number,
  chromeHeightFor: (windowWidth: number) => number,
  programmatic: { current: boolean },
): Promise<void> {
  const win = getCurrentWindow();
  let monitor: { width: number; height: number } | null = null;
  try {
    const mon = await currentMonitor();
    if (mon) {
      const scale = mon.scaleFactor || 1;
      monitor = { width: mon.workArea.size.width / scale, height: mon.workArea.size.height / scale };
    }
  } catch {
    // No monitor information (or the query failed): mirrorWindowSize falls
    // back to the previous fixed cap, so the window still fits the device.
  }
  let frameW = 0;
  let frameH = 0;
  let scale = 1;
  if (monitor) {
    // setSize below sizes the CLIENT area, so the OS frame has to come out of
    // the budget as well: without it the window's bottom - the text input bar
    // - lands under the taskbar even though the work area was respected.
    try {
      const [outer, inner, factor] = await Promise.all([
        win.outerSize(),
        win.innerSize(),
        win.scaleFactor(),
      ]);
      scale = factor || 1;
      frameW = Math.max(0, Math.round((outer.width - inner.width) / scale));
      frameH = Math.max(0, Math.round((outer.height - inner.height) / scale));
      monitor = { width: monitor.width - frameW, height: monitor.height - frameH };
    } catch {
      // No frame information: MIRROR_AREA_FILL still keeps a few px free.
    }
  }
  let size = mirrorWindowSize(deviceWidth, deviceHeight, chromeHeightFor(window.innerWidth), monitor);
  size = mirrorWindowSize(deviceWidth, deviceHeight, chromeHeightFor(size.width), monitor);
  // The resize goes first, alone, and in its own try. It used to be the second
  // half of a try shared with the repositioning below, and moving a window
  // needs `core:window:allow-set-position` - which this app's capability list
  // did not grant. The clamp wants the window in the top ~3% of the work area
  // (the target is 97% of it), so for nearly every window position the move was
  // attempted, refused, and swallowed - taking the resize with it. The window
  // then kept the 380x750 from tauri.conf.json and the mirror sat letterboxed
  // with background down both sides, with nothing anywhere saying why. Sizing
  // and moving are separate concerns: a refused move must never cost the size.
  await sizeWindowForViewport(win, size, chromeHeightFor, deviceWidth / deviceHeight, programmatic);
  try {
    // Position after the resize: the top-left corner does not move when a
    // window is resized, so the clamped position is the one the finished
    // window keeps. Best-effort - a window that could not be nudged back on
    // screen is cosmetic, and the mirror is the right size either way.
    await keepWindowOnScreen(win, { width: size.width + frameW, height: size.height + frameH }, scale);
  } catch (e) {
    console.warn("[fit] could not move the window back onto the monitor", e);
  }
}

function b64ToBytes(b64: string): Uint8Array {
  const bin = atob(b64);
  const bytes = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) {
    bytes[i] = bin.charCodeAt(i);
  }
  return bytes;
}

/**
 * The backend's marker, mirrored from `xphoneuse_ios::developer_image` where the
 * wording IS the contract: this failure means the device's Developer Mode is
 * off, and installing the developer image needs it. Nothing on the host turns
 * it on by itself - the phone restarts when it happens - so the answer is a
 * question for the user, not an error message.
 */
const IOS_DEVMODE_OFF_MARK = "developer mode is off on the device";

/** Whether this failure is the device saying its Developer Mode is off. */
export function isDeveloperModeOff(e: unknown): boolean {
  return String(e).includes(IOS_DEVMODE_OFF_MARK);
}

/**
 * Mirrors `xphoneuse_ios::developer_image::PASSCODE_MARK`: the device refused
 * the request because it has a passcode set (iOS 27 does; iOS 16 accepts the
 * same request). Nothing to retry - the switch has to be flipped by hand, and
 * Settings asks for the passcode there.
 */
const IOS_PASSCODE_MARK = "the device has a passcode set";

/** Whether this failure is that refusal. */
export function isPasscodeRefusal(e: unknown): boolean {
  return String(e).includes(IOS_PASSCODE_MARK);
}

/** How long the dialog waits before it flips the switch by itself. */
const DEV_MODE_COUNTDOWN_SECONDS = 10;

/**
 * The "this iPhone needs Developer Mode" question, and where the user is in it.
 *
 * `ask` counts down to doing it (and can be stopped), `enabling` is the request
 * in flight, `waiting` is the phone rebooting (the user unlocks it and taps
 * through), and `manual` is the app admitting it could not and handing over.
 */
export interface IosDevModePrompt {
  stage: "ask" | "enabling" | "waiting" | "manual";
  /** The device the question is about, kept whole so "continue" can reconnect. */
  device: IosDevice;
  /** Seconds left before the switch is flipped; `ask` only. */
  seconds?: number;
  /** Why the manual route is manual. `manual` only. */
  error?: string;
}

/** What the backend reports while it prepares the iPhone's developer image. */
export interface IosImagePrep {
  stage: "checking" | "downloading" | "mounting" | "developer-mode" | "done";
  got?: number;
  total?: number;
  build?: string;
  /** Countdown for the "developer-mode" stage. */
  seconds?: number;
}

interface UseConnectionOptions {
  settings: Settings;
  showToast: (msg: string, type?: "error" | "info") => void;
  takeScreenshot: () => void;
  setThemePref: (fn: (p: "light" | "dark" | "auto") => "light" | "dark" | "auto") => void;
  onToggleAlwaysOnTop: () => void;
  onFrameReceived?: () => void;
  onCodecFallback?: (codec: string) => void;
  onRecordEvent?: (event: MacroEvent) => void;
}

export function useConnection(opts: UseConnectionOptions) {
  const { showToast, settings } = opts;
  const [screen, setScreen] = useState<Screen>("welcome");
  const [connectedDevice, setConnectedDevice] = useState<Device | null>(null);
  const [connectingSerial, setConnectingSerial] = useState<string | null>(null);
  const [deviceSize, setDeviceSize] = useState({ width: 1080, height: 1920 });
  const [muted, setMutedState] = useState(false);
  const [recording, setRecording] = useState(false);
  const [streamStarted, setStreamStarted] = useState(false);
  const streamStartedRef = useRef(false);
  // iOS sessions without the HID bridge are view-only (AirPlay is
  // receive-only); with the bridge, input handlers route through it.
  const [iosHidAvailable, setIosHidAvailable] = useState(false);
  // Screen transport of the current iOS session ("airplay" | "tunnel");
  // suppresses the Control-Center AirPlay hint in tunnel mode.
  const [iosScreen, setIosScreen] = useState("airplay");
  // Set while the backend is preparing the iPhone's developer image, which it
  // has to do when the device is not offering the tunnel's services (an iOS
  // update invalidates the mounted image). It downloads ~16 MB, so the wait
  // gets a line of its own instead of looking like a hang.
  const [iosImagePrep, setIosImagePrep] = useState<IosImagePrep | null>(null);
  // Whether the current iOS session's input rides the USB tunnel — the tunnel
  // delivers non-ASCII text via the pasteboard + Cmd+V, so the ASCII-only
  // warning (below) does not apply to it.
  const iosTunnelInputRef = useRef(false);
  const viewOnlyRef = useRef(false);
  const isIosRef = useRef(false);
  const inputErrWarnedRef = useRef(false);
  const iosAsciiWarnedRef = useRef(false);
  const canvasRef = useRef<HTMLCanvasElement>(null);
  /// `performance.now()` of the last canvas paint. The MCP screenshot path
  /// reports it, so a stale frame can be told apart from a stale stream.
  const lastDrawAtRef = useRef(0);
  /// `performance.now()` of the last decoded frame — unlike the paint age it
  /// keeps advancing while the window is hidden and rAF is parked.
  const lastDecodeAtRef = useRef(0);
  /// Newest decoded frame, kept alive so a screenshot can paint it on demand.
  /// The rAF path only runs while the window is on screen (WebView2 parks it
  /// when the window is minimized or fully covered), so without this the
  /// canvas - and every screenshot taken from it - freezes at whatever was
  /// last visible, while the stream itself is perfectly healthy.
  const latestFrameRef = useRef<VideoFrame | null>(null);
  /// Bumped on every connect/disconnect; channel callbacks from a session
  /// whose token is outdated are ignored (the old channel can deliver a
  /// `disconnected` event after a NEW session already started, which would
  /// otherwise reset the new session back to the welcome screen).
  const sessionTokenRef = useRef(0);
  const decoderRef = useRef<VideoDecoder | null>(null);
  /// Packets that arrived while the decoder was still being configured: the
  /// stream sends Config and its first keyframe back-to-back, but
  /// `isConfigSupported` resolves asynchronously — without buffering, that
  /// first (and on a static screen, only) keyframe is dropped.
  const pendingPacketsRef = useRef<{ key: boolean; timestamp: number; bytes: Uint8Array }[]>([]);
  /// Last decoder config — needed to rebuild the decoder after a decode
  /// error (a closed decoder would otherwise drop every later frame).
  const videoConfigRef = useRef<VideoDecoderConfig | null>(null);
  /// Set by the decoder error callback; deltas are dropped until the next
  /// keyframe, where the decoder is rebuilt (the decoder's reference state
  /// may be silently stale after feeding it a truncated picture).
  const needsResyncRef = useRef(false);
  /// Decoder factory from the latest config event — lets the packet path
  /// rebuild the decoder at a keyframe after a decode error.
  const buildDecoderRef = useRef<(() => VideoDecoder) | null>(null);
  const pendingFrame = useRef<VideoFrame | null>(null);
  const rafId = useRef<number>(0);
  const isMouseDownRef = useRef(false);
  const reconnectTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const isReconnecting = useRef(false);
  const recorderRef = useRef<MediaRecorder | null>(null);
  const recordedChunks = useRef<Blob[]>([]);
  const nativeSize = useRef({ width: 1080, height: 1920 });
  const displaySizeRef = useRef({ width: 1080, height: 1920 });
  /// Visible (border-free) frame size from the config event: the iOS tunnel
  /// encodes macroblock-aligned dimensions with black padding on the right
  /// and bottom, so the decoder output is cropped to this size when set.
  const visibleSizeRef = useRef({ width: 0, height: 0 });
  const resizeSnapTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const isProgrammaticResize = useRef(false);

  const cleanupDecoder = useCallback(() => {
    if (rafId.current) {
      cancelAnimationFrame(rafId.current);
      rafId.current = 0;
    }
    if (pendingFrame.current) {
      pendingFrame.current.close();
      pendingFrame.current = null;
    }
    if (latestFrameRef.current) {
      latestFrameRef.current.close();
      latestFrameRef.current = null;
    }
    if (decoderRef.current && decoderRef.current.state !== "closed") {
      decoderRef.current.close();
      decoderRef.current = null;
    }
    pendingPacketsRef.current = [];
    needsResyncRef.current = false;
    buildDecoderRef.current = null;
    videoConfigRef.current = null;
    setMutedState(false);
    streamStartedRef.current = false;
    setStreamStarted(false);
    if (recorderRef.current && recorderRef.current.state === "recording") {
      recorderRef.current.stop();
    }
  }, []);

  /// Paint the newest decoded frame immediately, without waiting for rAF.
  /// The rAF route only runs while the window is on screen; a screenshot
  /// taken while it is minimized or covered would otherwise read whatever
  /// frame was last visible. Returns whether a frame was drawn.
  const paintLatestFrame = useCallback((): boolean => {
    const canvas = canvasRef.current;
    const frame = latestFrameRef.current;
    if (!canvas || !frame) return false;
    const vis = visibleSizeRef.current;
    const cropped =
      vis.width > 0 && vis.height > 0 &&
      vis.width <= frame.displayWidth && vis.height <= frame.displayHeight;
    const cw = cropped ? vis.width : frame.displayWidth;
    const ch = cropped ? vis.height : frame.displayHeight;
    if (canvas.width !== cw || canvas.height !== ch) {
      canvas.width = cw;
      canvas.height = ch;
    }
    const ctx = canvas.getContext("2d");
    if (!ctx) return false;
    ctx.drawImage(frame, 0, 0, cw, ch, 0, 0, cw, ch);
    lastDrawAtRef.current = performance.now();
    return true;
  }, []);

  const setMuted = useCallback(async (m: boolean) => {
    setMutedState(m);
    if (viewOnlyRef.current) return;
    try { await invoke("set_muted", { muted: m }); } catch { }
  }, []);

  // The "this iPhone needs Developer Mode" conversation, driven here because
  // the connect flow and a failed screenshot both land in it.
  const [iosDevModePrompt, setIosDevModePrompt] = useState<IosDevModePrompt | null>(null);
  /// Devices that refused the request because they have a passcode set, with
  /// the refusal to quote back. Per run: the passcode is the user's to remove.
  const passcodeRefusedRef = useRef<Map<string, string>>(new Map());

  /**
   * Ask about turning Developer Mode on, and say whether that is what this
   * failure was. The phone restarts when it happens, so it is a question with
   * a countdown rather than something to report - and the caller should skip
   * its own error message when this answers yes.
   */
  const askForDeveloperMode = useCallback((device: Device, e: unknown): boolean => {
    // Only an iPhone: Developer Mode and developer images are an iOS story.
    if (!isIosDevice(device) || !isDeveloperModeOff(e)) return false;
    // A device that already refused this run is not offered the countdown
    // again: the request would fail the same way, and the user is told what to
    // do about it instead of watching ten seconds tick by first.
    // The row is hidden on a device that has never been used for development,
    // and any route that ends with the user going to Settings needs it there.
    void invoke("reveal_ios_developer_mode", { udid: device.udid }).catch(() => { });
    const refused = passcodeRefusedRef.current.get(device.udid);
    setIosDevModePrompt(
      refused
        ? { stage: "manual", device, error: refused }
        : { stage: "ask", device, seconds: DEV_MODE_COUNTDOWN_SECONDS }
    );
    return true;
  }, []);

  /** Send the request. The countdown ran out, or the user said to. */
  const enableDeveloperMode = useCallback(async (device: IosDevice) => {
    setIosDevModePrompt({ stage: "enabling", device });
    try {
      await invoke("enable_ios_developer_mode", { udid: device.udid });
      console.log(`[ios-dev-mode] enable requested; waiting for ${device.udid} to come back`);
      setIosDevModePrompt({ stage: "waiting", device });
    } catch (e) {
      // The refusal is per device and lasts as long as the passcode does: a
      // retry would be another failed request, so this run stops asking.
      if (isPasscodeRefusal(e)) passcodeRefusedRef.current.set(device.udid, String(e));
      // The manual route is where this is going, and the switch has to be
      // findable when the user gets to Settings.
      void invoke("reveal_ios_developer_mode", { udid: device.udid }).catch(() => { });
      setIosDevModePrompt({ stage: "manual", device, error: String(e) });
    }
  }, []);

  // The countdown before the switch is flipped on its own. Cancelling, or the
  // prompt moving on, takes the timer with it.
  useEffect(() => {
    if (iosDevModePrompt?.stage !== "ask") return;
    const { device, seconds = 0 } = iosDevModePrompt;
    if (seconds <= 0) {
      void enableDeveloperMode(device);
      return;
    }
    const timer = setTimeout(() => {
      setIosDevModePrompt((p) =>
        p?.stage === "ask" ? { ...p, seconds: (p.seconds ?? 1) - 1 } : p
      );
    }, 1000);
    return () => clearTimeout(timer);
  }, [iosDevModePrompt, enableDeveloperMode]);

  // 取消 — nothing was sent (or the wait is over), back to the device list.
  const cancelDevModePrompt = useCallback(() => setIosDevModePrompt(null), []);

  const connectToDevice = useCallback(async (device: Device, s: Settings, silent = false) => {
    const tConnect = performance.now();
    console.log(`[connect] connectToDevice ${getDeviceKey(device)}`);
    setConnectingSerial(getDeviceKey(device));
    cleanupDecoder();
    // The crop size is remembered across a stream restart within one
    // connection (see the config handler), but never across devices: a stale
    // size from a connection that is over could otherwise crop the next
    // device's stream, whenever the new frame happens to be larger.
    visibleSizeRef.current = { width: 0, height: 0 };
    const token = ++sessionTokenRef.current;
    try {
      const channel = new Channel<FrameEvent>();
      channel.onmessage = (msg) => {
        if (sessionTokenRef.current !== token) return; // stale session event
        if (msg.event === "config") {
          cleanupDecoder();
          // Only a config that carries dimensions may replace them. The actor
          // probes the visible size once per tunnel, not once per stream, so
          // the config that follows a stream restart - a reconnect, which can
          // happen mid-session - has none, and overwriting the known size with
          // 0 costs the whole session its crop: the coded picture is padded
          // 1178x2556 -> 1184x2576 with black (the encoder drops the screen's
          // odd last column and the SPS carries no crop to recover either
          // number), and that padding then sits on screen down the right and
          // bottom edges until the next reconnect that still has it.
          if (msg.data.width != null && msg.data.height != null) {
            visibleSizeRef.current = { width: msg.data.width, height: msg.data.height };
          }
          const descBytes = b64ToBytes(msg.data.description);
          console.log(`[video] config: ${msg.data.codec} (desc ${descBytes.length}B)`);
          const config: VideoDecoderConfig = {
            codec: msg.data.codec,
            description: descBytes.buffer,
            optimizeForLatency: true,
          };
          videoConfigRef.current = config;
          const buildDecoder = () => new VideoDecoder({
            output: (frame: VideoFrame) => {
              opts.onFrameReceived?.();
              if (!streamStartedRef.current) {
                streamStartedRef.current = true;
                setStreamStarted(true);
              }
              lastDecodeAtRef.current = performance.now();
              if (latestFrameRef.current) latestFrameRef.current.close();
              latestFrameRef.current = frame.clone();
              if (pendingFrame.current) pendingFrame.current.close();
              pendingFrame.current = frame;
              if (!rafId.current) {
                rafId.current = requestAnimationFrame(() => {
                  rafId.current = 0;
                  const f = pendingFrame.current;
                  if (!f) return;
                  pendingFrame.current = null;
                  const canvas = canvasRef.current;
                  if (!canvas) { f.close(); return; }
                  // Crop the encoder's macroblock padding (black borders on
                  // the right/bottom) to the probed visible size. The fit
                  // check also covers a rotated stream, whose coded size
                  // would no longer contain the portrait probe.
                  const vis = visibleSizeRef.current;
                  const cropped =
                    vis.width > 0 && vis.height > 0 &&
                    vis.width <= f.displayWidth && vis.height <= f.displayHeight;
                  const cw = cropped ? vis.width : f.displayWidth;
                  const ch = cropped ? vis.height : f.displayHeight;
                  if (canvas.width !== cw || canvas.height !== ch) {
                    canvas.width = cw;
                    canvas.height = ch;
                    setDeviceSize((prev) => {
                      if (prev.width === cw && prev.height === ch) return prev;
                      const prevLandscape = prev.width > prev.height;
                      const nowLandscape = cw > ch;
                      if (prevLandscape !== nowLandscape) {
                        nativeSize.current = { width: nativeSize.current.height, height: nativeSize.current.width };
                      }
                      invoke("update_screen_size", { width: nativeSize.current.width, height: nativeSize.current.height }).catch(() => { });
                      displaySizeRef.current = { width: cw, height: ch };
                      const chromeHeightFor = (_w: number) =>
                        mirrorChromeHeight(!viewOnlyRef.current || agentInputReadyRef.current);
                      void fitWindowToMirror(cw, ch, chromeHeightFor, isProgrammaticResize);
                      return { width: cw, height: ch };
                    });
                  }
                  const ctx = canvas.getContext("2d");
                  if (ctx) ctx.drawImage(f, 0, 0, cw, ch, 0, 0, cw, ch);
                  lastDrawAtRef.current = performance.now();
                  f.close();
                });
              }
            },
            // Decode errors close the decoder asynchronously. Flag the state
            // so the packet path rebuilds it at the next keyframe — without
            // this, one bad chunk freezes the stream forever.
            error: (e: DOMException) => {
              console.error("Decoder error:", e);
              needsResyncRef.current = true;
            },
          });
          buildDecoderRef.current = buildDecoder;
          VideoDecoder.isConfigSupported(config).then((result) => {
            if (!result.supported) {
              // Android auto-reconnects with H.264; an iOS tunnel session has
              // no fallback, so surface the failure instead of spinning.
              if (isIosRef.current) {
                showToast(t("toasts.hevcUnsupported"), "error");
              }
              opts.onCodecFallback?.("h264");
              return;
            }
            const decoder = buildDecoder();
            decoder.configure(config);
            decoderRef.current = decoder;
            // Flush packets buffered while the config check was in flight.
            for (const p of pendingPacketsRef.current) {
              decoder.decode(new EncodedVideoChunk({
                type: p.key ? "key" : "delta",
                timestamp: p.timestamp,
                data: p.bytes,
              }));
            }
            pendingPacketsRef.current = [];
          });
        } else if (msg.event === "packet") {
          const decoder = decoderRef.current;
          if (!decoder || decoder.state !== "configured") {
            // Decoder still configuring — hold the packet until it is ready
            // (bounded: a stuck stream must not grow the buffer forever).
            if (decoder === null && pendingPacketsRef.current.length < 60) {
              pendingPacketsRef.current.push({
                key: msg.data.key,
                timestamp: msg.data.timestamp,
                bytes: b64ToBytes(msg.data.data),
              });
            }
            return;
          }
          if (needsResyncRef.current) {
            // Reference state is suspect after a decode error: deltas are
            // worthless without it, so wait for the next keyframe and rebuild.
            if (!msg.data.key) return;
            needsResyncRef.current = false;
            console.log("[video] resyncing at keyframe after decoder error");
            decoder.close();
            const cfg = videoConfigRef.current;
            const build = buildDecoderRef.current;
            if (!cfg || !build) return;
            const rebuilt = build();
            rebuilt.configure(cfg);
            decoderRef.current = rebuilt;
          }
          const bytes = b64ToBytes(msg.data.data);
          decoderRef.current!.decode(new EncodedVideoChunk({
            type: msg.data.key ? "key" : "delta",
            timestamp: msg.data.timestamp,
            data: bytes,
          }));
        } else if (msg.event === "agent_input") {
          // Auto-configured from the mirroring phone's IP (no PIN by default).
          setAgentInputReady(true);
          //showToast(t("toasts.chineseInputReady", { host: msg.data.host }), "info");
        } else if (msg.event === "disconnected") {
          cleanupDecoder();
          if (!isReconnecting.current) {
            viewOnlyRef.current = false;
            isIosRef.current = false;
            setIosHidAvailable(false);
            setConnectedDevice(null);
            setScreen("welcome");
            showToast(t("toasts.deviceDisconnected"), "info");
          }
        }
      };

      let width: number, height: number;
      isIosRef.current = isIosDevice(device);
      inputErrWarnedRef.current = false;
      iosAsciiWarnedRef.current = false;
      viewOnlyRef.current = isIosDevice(device);
      if (isIosDevice(device)) {
        // Starts uxplay and waits for the iPhone to start AirPlay mirroring.
        // Returns whether the BLE-HID bridge was detected (controllable).
        const tInvoke = performance.now();
        console.log(`[connect] connect_ios_device invoke (${device.udid})`);
        // Transport modes: stored Settings preference, else the default —
        // USB tunnel on iOS >= 27, AirPlay + BLE-HID bridge below that.
        // The backend falls back to AirPlay/bridge when the USB tunnel
        // cannot start and reports it via the "-fallback" suffixes.
        const screenMode = resolveIosScreenMode(device.ios_version);
        const inputMode = resolveIosInputMode(device.ios_version);
        let res: {
          hid: boolean;
          port: string | null;
          input_available?: boolean;
          screen?: string;
          input?: string;
        };
        try {
          res = await invoke("connect_ios_device", {
            udid: device.udid,
            onFrame: channel,
            screenMode,
            inputMode,
          });
        } finally {
          // Whatever the outcome, the image work (if any) is over by now.
          setIosImagePrep(null);
        }
        console.log(
          `[connect] connect_ios_device returned in ${Math.round(performance.now() - tInvoke)}ms (hid=${res.hid})`
        );
        const inputAvailable = res.input_available ?? res.hid;
        setIosScreen((res.screen ?? "airplay").split("-")[0]);
        iosTunnelInputRef.current = (res.input ?? "bridge").split("-")[0] === "tunnel";
        viewOnlyRef.current = !inputAvailable;
        setIosHidAvailable(inputAvailable);
        if (!inputAvailable && inputMode === "bridge") {
          showToast(t("toasts.bridgeNotFound"), "info");
        }
        if (res.screen === "airplay-fallback") {
          showToast(t("toasts.tunnelScreenFallback"), "info");
        }
        if (res.input === "none") {
          // Tunnel input mode never probes the COM ports, so a failed tunnel
          // leaves the session view-only rather than falling back.
          showToast(t("toasts.tunnelInputFailed"), "info");
        }
        // Stream dimensions are unknown until the first decoded frame
        // arrives; use a phone-ish default, the frame-driven resize takes over.
        width = 390;
        height = 844;
      } else {
        // `copilot` carries API keys and has nothing to do with scrcpy; it is
        // dropped here rather than shipped to Rust with every connect (serde
        // would silently ignore it, so nothing would break - the keys would
        // just be crossing a boundary they have no business crossing).
        const { adaptive: _, copilot: __, ...streamSettings } = s;
        const tInvoke = performance.now();
        console.log(`[connect] connect_device invoke (${device.serial})`);
        [width, height] = await invoke<[number, number]>("connect_device", {
          serial: device.serial,
          onFrame: channel,
          settings: streamSettings,
        });
        console.log(
          `[connect] connect_device returned in ${Math.round(performance.now() - tInvoke)}ms (${width}x${height})`
        );
      }
      nativeSize.current = { width, height };
      displaySizeRef.current = { width, height };
      setDeviceSize({ width, height });
      setConnectedDevice(device);
      setScreen("another");
      console.log(`[connect] mirror screen state set (${Math.round(performance.now() - tConnect)}ms from click)`);

      const chromeHeightFor = (_w: number) =>
        mirrorChromeHeight(!viewOnlyRef.current || agentInputReadyRef.current);
      await fitWindowToMirror(width, height, chromeHeightFor, isProgrammaticResize);
      console.log(`[connect] window resized, connect flow done (${Math.round(performance.now() - tConnect)}ms from click)`);
    } catch (e) {
      // An iPhone missing Developer Mode is a question, not an error: the
      // dialog offers to turn it on (and says the phone will restart), and
      // "could not connect" underneath that would only be noise.
      if (!askForDeveloperMode(device, e) && !silent) {
        showToast(t("toasts.failedToConnect", { error: String(e) }));
      }
    } finally {
      setConnectingSerial(null);
      isReconnecting.current = false;
    }
  }, [showToast, askForDeveloperMode, cleanupDecoder]);

  /**
   * 继续 — the phone has restarted and is unlocked: connect again, which now
   * finds the switch on and gets on with preparing the developer image before
   * the mirror opens. Lives here rather than with the rest of the prompt
   * because it is the one action that needs the connect flow.
   */
  const continueDevModePrompt = useCallback(() => {
    const device = iosDevModePrompt?.device;
    setIosDevModePrompt(null);
    if (device) void connectToDevice(device, settings);
  }, [iosDevModePrompt, connectToDevice, settings]);

  const disconnect = useCallback(async () => {
    sessionTokenRef.current++;
    cleanupDecoder();
    viewOnlyRef.current = false;
    isIosRef.current = false;
    iosTunnelInputRef.current = false;
    setIosHidAvailable(false);
    try { await invoke("disconnect_device"); } catch { }
    setConnectedDevice(null);
    setScreen("welcome");
    try {
      await getCurrentWindow().setSize(new LogicalSize(380, 750));
    } catch { }
  }, [cleanupDecoder]);

  // Switch the iOS input transport live (Settings): "tunnel" adopts/starts a
  // headless CoreDevice tunnel on the current session, "bridge" stops it and
  // falls back to the BLE-HID bridge. Persisted for future connects; returns
  // false when the switch failed (settings keep the previous mode).
  const setIosInputMode = useCallback(async (mode: "bridge" | "tunnel"): Promise<boolean> => {
    localStorage.setItem("ios_input_mode", mode);
    if (!connectedDevice || !isIosDevice(connectedDevice)) return true;
    try {
      const actual = await invoke<string>("set_ios_transport_modes", { inputMode: mode });
      iosTunnelInputRef.current = actual.split("-")[0] === "tunnel";
      if (actual === "tunnel") {
        setIosHidAvailable(true);
        viewOnlyRef.current = false;
        showToast(t("toasts.tunnelConnected"), "info");
      } else {
        // "bridge": the backend attaches a bridge on demand (tunnel input
        // mode skips COM probing at connect) and errors when none is found,
        // so an Ok means input is controllable again.
        setIosHidAvailable(true);
        viewOnlyRef.current = false;
      }
      return true;
    } catch (e) {
      // Switching to the tunnel can be the thing that discovers the missing
      // developer image; the same dialog answers for it.
      if (!askForDeveloperMode(connectedDevice, e)) {
        showToast(t("toasts.tunnelInputSwitchFailed", { error: String(e) }), "error");
      }
      return false;
    }
  }, [connectedDevice, showToast, askForDeveloperMode]);

  const scheduleReconnect = useCallback((s: Settings) => {
    if (reconnectTimer.current) clearTimeout(reconnectTimer.current);
    reconnectTimer.current = setTimeout(() => {
      if (connectedDevice) {
        isReconnecting.current = true;
        connectToDevice(connectedDevice, s, true);
      }
    }, 800);
  }, [connectedDevice, connectToDevice]);

  const toggleRecording = useCallback(() => {
    if (recorderRef.current && recorderRef.current.state === "recording") {
      recorderRef.current.stop();
      return;
    }
    const canvas = canvasRef.current;
    if (!canvas) return;
    const stream = canvas.captureStream(30);
    recordedChunks.current = [];
    const recorder = new MediaRecorder(stream, { mimeType: "video/webm;codecs=vp9" });
    recorder.ondataavailable = (e) => {
      if (e.data.size > 0) recordedChunks.current.push(e.data);
    };
    recorder.onstop = async () => {
      const blob = new Blob(recordedChunks.current, { type: "video/webm" });
      const filename = `recording-${fileTimestamp()}.webm`;
      // Settings save location, else the system Documents directory; the
      // browser download is only the fallback when writing fails or neither
      // is available.
      const savePath = await resolveSaveDir();
      let savedTo: string | null = null;
      if (savePath) {
        try {
          const buffer = await blob.arrayBuffer();
          const bytes = new Uint8Array(buffer);
          let binary = "";
          for (let i = 0; i < bytes.length; i++) binary += String.fromCharCode(bytes[i]);
          const base64 = btoa(binary);
          savedTo = `${savePath}/${filename}`;
          await invoke("save_file", { path: savedTo, data: base64 });
        } catch {
          savedTo = null;
        }
      }
      if (!savedTo) {
        const url = URL.createObjectURL(blob);
        const link = document.createElement("a");
        link.href = url;
        link.download = filename;
        link.click();
        URL.revokeObjectURL(url);
      }
      recordedChunks.current = [];
      recorderRef.current = null;
      setRecording(false);
      showToast(
        savedTo ? t("toasts.recordingSaved", { path: savedTo }) : t("toasts.recordingDownloaded"),
        "info"
      );
    };
    recorder.start(100);
    recorderRef.current = recorder;
    setRecording(true);
    showToast(t("toasts.recordingStarted"), "info");
  }, [showToast]);

  // Surface the first input failure of an iOS session (e.g. Bluetooth not
  // paired with the bridge) without spamming a toast per event.
  const warnInputError = useCallback((e: unknown) => {
    if (isIosRef.current && !inputErrWarnedRef.current) {
      inputErrWarnedRef.current = true;
      showToast(t("toasts.iphoneInputFailed", { error: String(e) }), "error");
    }
  }, [showToast]);

  // iOS sessions can type non-English text through the AgentInput keyboard
  // (ws://phone:8787) even though the HID bridge is ASCII-only - including
  // view-only sessions that have no bridge at all.
  const [agentInputReady, setAgentInputReadyState] = useState(false);
  const agentInputReadyRef = useRef(false);
  const setAgentInputReady = useCallback((ready: boolean) => {
    agentInputReadyRef.current = ready;
    setAgentInputReadyState(ready);
  }, []);

  // Restore a AgentInput configuration saved from a previous session.
  useEffect(() => {
    invoke<boolean>("get_agent_input_configured")
      .then(setAgentInputReady)
      .catch(() => { });
  }, [setAgentInputReady]);

  // The developer-image work runs inside connect_ios_device, so its progress
  // arrives as events rather than through the connect result.
  useEffect(() => {
    const pending = listen<{
      stage: string;
      got?: number;
      total?: number;
      seconds?: number;
      build?: string;
      error?: string;
      code?: string;
    }>("ios-image-progress", (event) => {
      const p = event.payload;
      if (p.stage === "failed") {
        setIosImagePrep(null);
        // "developer-mode-off" says nothing here: that failure is what the
        // connect returns, and the dialog answers it with the device in hand.
        if (p.code !== "developer-mode-off") {
          showToast(t("iosImage.failed", { error: p.error ?? "" }));
        }
        return;
      }
      setIosImagePrep({
        stage: p.stage as IosImagePrep["stage"],
        got: p.got,
        total: p.total,
        seconds: p.seconds,
        build: p.build,
      });
    });
    return () => {
      void pending.then((off) => off());
    };
    // showToast is stable (useCallback in useToasts), so subscribing once is
    // what keeps a re-render from dropping mid-prep events.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // The HID bridge's TEXT command only accepts printable ASCII; without
  // AgentInput, non-English characters are dropped by the backend. Warn once
  // per session instead of failing silently. The USB tunnel is exempt: it
  // delivers non-ASCII through the device pasteboard + Cmd+V.
  const warnIosAscii = useCallback((text: string) => {
    if (!isIosRef.current || iosAsciiWarnedRef.current || agentInputReadyRef.current || iosTunnelInputRef.current) return;
    if (/[^\x20-\x7E]/.test(text)) {
      iosAsciiWarnedRef.current = true;
      showToast(t("toasts.asciiWarning"), "error");
    }
  }, [showToast]);

  const sendTextToDevice = useCallback(async (text: string, enter = false) => {
    if (viewOnlyRef.current && !agentInputReadyRef.current) return;
    warnIosAscii(text);
    opts.onRecordEvent?.({ type: "text", text });
    try { await invoke("send_text", { text, enter }); } catch (err) { warnInputError(err); }
  }, [warnInputError, warnIosAscii]);

  const pressButton = useCallback(async (button: string) => {
    if (viewOnlyRef.current) return;
    opts.onRecordEvent?.({ type: "button", button });
    try { await invoke("press_button", { button }); } catch (e) { warnInputError(e); }
  }, [warnInputError]);

  // Works for both the video canvas and the AirPlay-wait screenshot preview
  // (both render the phone screen in the same spot): coordinates are taken
  // relative to whichever element the event landed on.
  const handleCanvasMouseEvent = async (e: React.MouseEvent<HTMLElement>, action: string) => {
    if (!connectedDevice || viewOnlyRef.current) return;
    const rect = e.currentTarget.getBoundingClientRect();
    const x = (e.clientX - rect.left) / rect.width;
    const y = (e.clientY - rect.top) / rect.height;
    opts.onRecordEvent?.({ type: "touch", action, x, y });
    try { await invoke("send_touch", { action, x, y }); } catch (err) { warnInputError(err); }
  };

  const handleWheel = async (e: React.WheelEvent<HTMLElement>) => {
    if (!connectedDevice || viewOnlyRef.current) return;
    const rect = e.currentTarget.getBoundingClientRect();
    const x = (e.clientX - rect.left) / rect.width;
    const y = (e.clientY - rect.top) / rect.height;
    const dy = e.deltaY > 0 ? -1 : 1;
    opts.onRecordEvent?.({ type: "scroll", x, y, dx: 0, dy });
    try { await invoke("send_scroll", { x, y, dx: 0, dy }); } catch (err) { warnInputError(err); }
  };

  const composingRef = useRef(false);

  const handleCompositionStart = () => { composingRef.current = true; };
  const handleCompositionEnd = async (e: React.CompositionEvent) => {
    composingRef.current = false;
    if (!connectedDevice || !e.data || (viewOnlyRef.current && !agentInputReadyRef.current)) return;
    warnIosAscii(e.data);
    opts.onRecordEvent?.({ type: "text", text: e.data });
    try { await invoke("send_text", { text: e.data }); } catch (err) { warnInputError(err); }
  };

  const handleKeyDown = async (e: React.KeyboardEvent) => {
    if (!connectedDevice || (viewOnlyRef.current && !agentInputReadyRef.current)) return;
    if (composingRef.current || e.key === "Dead") return;
    e.preventDefault();
    if (e.key.length === 1 && !e.ctrlKey && !e.metaKey) {
      warnIosAscii(e.key);
      opts.onRecordEvent?.({ type: "text", text: e.key });
      try { await invoke("send_text", { text: e.key }); } catch (err) { warnInputError(err); }
    } else if (!viewOnlyRef.current) {
      const keyMap: Record<string, number> = {
        Enter: 66, Backspace: 67, Delete: 112,
        ArrowUp: 19, ArrowDown: 20, ArrowLeft: 21, ArrowRight: 22,
        Escape: 111, Tab: 61,
      };
      const keycode = keyMap[e.key];
      if (keycode) {
        opts.onRecordEvent?.({ type: "key", keycode, action: "down" });
        try {
          await invoke("send_key", { keycode, action: "down" });
          await invoke("send_key", { keycode, action: "up" });
        } catch (err) { warnInputError(err); }
      }
    }
  };

  useEffect(() => {
    if (screen !== "another") return;
    let cancelled = false;
    const win = getCurrentWindow();
    // Title bar plus the input bar when the session shows one; read fresh on
    // both ends of the resize.
    const chromeFor = (_windowWidth: number) =>
      mirrorChromeHeight(!viewOnlyRef.current || agentInputReadyRef.current);
    const resizeUnlisten = win.onResized(() => {
      if (isProgrammaticResize.current || cancelled) return;
      if (resizeSnapTimer.current) clearTimeout(resizeSnapTimer.current);
      resizeSnapTimer.current = setTimeout(() => {
        void (async () => {
          if (cancelled || isProgrammaticResize.current) return;
          // Measure the webview, not win.innerSize(): the size a window is
          // given is not the size the webview inside it gets (see
          // sizeWindowForViewport - on Wayland the GTK header bar comes out of
          // the height, and the window's own numbers do not show it).
          const cw = window.innerWidth;
          const ch = window.innerHeight - chromeFor(cw);
          if (ch < 200 || cw < 240) return;
          const { width: dw, height: dh } = displaySizeRef.current;
          const targetAspect = dw / dh;
          const currentAspect = cw / ch;
          // Tolerance as it always was: a manual drag that lands within ~1% of
          // the device aspect is left alone rather than fought. The sizing at
          // connect lands the aspect exactly (sizeWindowForViewport measures
          // what the window really got), so nothing here depends on a tight
          // threshold - and keeping it loose keeps the behaviour on Windows
          // and macOS identical to what it has always been.
          if (Math.abs(currentAspect - targetAspect) < 0.01) return;
          let newW: number;
          let newH: number;
          if (currentAspect > targetAspect) {
            newH = ch;
            newW = Math.floor(ch * targetAspect);
          } else {
            newW = cw;
            newH = Math.floor(cw / targetAspect);
          }
          newW = Math.min(Math.max(newW, 280), cw);
          newH = Math.min(Math.max(newH, 240), ch);
          await sizeWindowForViewport(
            win,
            { width: newW, height: newH + chromeFor(newW) },
            chromeFor,
            targetAspect,
            isProgrammaticResize,
          );
        })();
      }, 120);
    });
    return () => {
      cancelled = true;
      if (resizeSnapTimer.current) clearTimeout(resizeSnapTimer.current);
      void resizeUnlisten.then((fn) => fn());
    };
  }, [screen]);

  return {
    screen,
    connectedDevice,
    connectingSerial,
    deviceSize,
    canvasRef,
    lastDrawAtRef,
    lastDecodeAtRef,
    paintLatestFrame,
    decoderRef,
    isMouseDownRef,
    muted,
    recording,
    streamStarted,
    iosHidAvailable,
    iosScreen,
    iosImagePrep,
    iosDevModePrompt,
    askForDeveloperMode,
    enableDeveloperMode: () => {
      if (iosDevModePrompt) void enableDeveloperMode(iosDevModePrompt.device);
    },
    cancelDevModePrompt,
    continueDevModePrompt,
    setIosInputMode,
    agentInputReady,
    setAgentInputReady,
    setMuted,
    toggleRecording,
    connectToDevice,
    disconnect,
    scheduleReconnect,
    pressButton,
    handleCanvasMouseEvent,
    handleWheel,
    handleKeyDown,
    handleCompositionStart,
    handleCompositionEnd,
    sendTextToDevice,
  };
}
