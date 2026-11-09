import { useState, useEffect, useRef } from "react";
import type React from "react";
import { invoke } from "@tauri-apps/api/core";
import { useTranslation } from "react-i18next";
import {
  Cog6ToothIcon,
  CameraIcon,
  ChevronLeftIcon,
  XMarkIcon,
  HomeIcon,
  Square2StackIcon,
  CommandLineIcon,
  StopIcon,
  VideoCameraIcon,
  SpeakerWaveIcon,
  SpeakerXMarkIcon,
  PowerIcon,
  ArrowPathIcon,
  FolderIcon,
} from "@heroicons/react/24/outline";
import type { Device } from "../types";
import { getDeviceDisplayName, isIosDevice, type QuickActionId } from "../types";
import { Button } from "@/components/ui/button";
import { MOD } from "../lib/utils";
import { CopilotPanel } from "./CopilotPanel";
import type { CopilotHandle } from "../hooks/useCopilot";

const QUICK_ACTION_ICONS: Record<QuickActionId, (props: { muted?: boolean; recording?: boolean; macroRecording?: boolean }) => React.ReactNode> = {
  record: ({ recording }) => recording ? <StopIcon className="text-[#f44]" /> : <VideoCameraIcon />,
  mute: ({ muted }) => muted ? <SpeakerXMarkIcon /> : <SpeakerWaveIcon />,
  volume_up: () => <span className="text-[11px] font-bold">V+</span>,
  volume_down: () => <span className="text-[11px] font-bold">V-</span>,
  power: () => <PowerIcon />,
  "macro-toggle": ({ macroRecording }) => macroRecording ? <StopIcon className="text-[#f90]" /> : (
    <svg width="15" height="15" viewBox="0 0 24 25" fill="none" className="text-current">
      <path d="M11.41 2.068c-.57 0-1.08 0-1.55.17-.1.04-.19.08-.29.12-.46.22-.81.58-1.21.98L3.58 8.148c-.47.47-.88.88-1.11 1.43-.22.54-.22 1.13-.22 1.8v3.47c0 1.78 0 3.22.15 4.35.16 1.17.49 2.16 1.27 2.95.78.78 1.76 1.12 2.93 1.28 1.12.15 2.55.15 4.33.15s3.31 0 4.43-.15c-.49-1.1-1.51-2.09-2.61-2.52-1.66-.65-1.66-3.01 0-3.66 1.16-.46 2.22-1.52 2.67-2.67.66-1.66 3.01-1.66 3.66 0 .16.41.39.81.67 1.17V14.858c0-1.53 0-2.77-.11-3.75-.12-1.02-.37-1.89-.96-2.63-.22-.27-.46-.52-.73-.74-.73-.6-1.6-.85-2.61-.97-1.18-.11-2.4-.11-3.92-.11z" fill="currentColor" opacity="0.5"/>
      <path fillRule="evenodd" clipRule="evenodd" d="M9.569 2.358c.09-.05.19-.09.29-.12.21-.07.42-.12.65-.14v1.99c0 1.36 0 2.01-.12 2.88-.12.9-.38 1.66-.98 2.26s-1.36.86-2.26.98c-.87.12-1.52.12-2.88.12H2.289c.03-.26.09-.51.18-.75.22-.54.64-.96 1.11-1.43l4.78-4.81c.4-.4.76-.77 1.21-.98zM17.919 23.118c-.24.61-1.09.61-1.33 0l-.04-.1a5.73 5.73 0 00-3.23-3.23l-.11-.04c-.6-.24-.6-1.1 0-1.33l.11-.04a5.73 5.73 0 003.23-3.23l.04-.1c.24-.61 1.09-.61 1.33 0l.04.1a5.73 5.73 0 003.23 3.23l.11.04c.6.24.6 1.1 0 1.33l-.11.04a5.73 5.73 0 00-3.23 3.23l-.04.1z" fill="currentColor"/>
    </svg>
  ),
  rotate: () => <ArrowPathIcon />,
};

/** Quick actions that send input to the device; hidden for view-only (iOS) sessions. */
const INPUT_QUICK_ACTIONS: QuickActionId[] = ["mute", "volume_up", "volume_down", "power", "macro-toggle", "rotate"];

interface MirrorScreenProps {
  connectedDevice: Device;
  connecting?: boolean;
  viewOnly?: boolean;
  /** Show the text input bar; defaults to !viewOnly (AgentInput keeps it usable for view-only iOS). */
  textInputEnabled?: boolean;
  /** Shown under the canvas until the first frame arrives (e.g. AirPlay hint). */
  waitingHint?: string;
  streamStarted?: boolean;
  deviceSize: { width: number; height: number };
  canvasRef: React.RefObject<HTMLCanvasElement | null>;
  isMouseDownRef: React.MutableRefObject<boolean>;
  recording: boolean;
  muted: boolean;
  macroRecording: boolean;
  /** The macro on a loop right now, if any - shown so it can be stopped. */
  macroLooping: string | null;
  pinnedActions: QuickActionId[];
  alwaysOnTop: boolean;
  adaptiveInfo?: { enabled: boolean; tierName: string; fps: number };
  onToggleRecording: () => void;
  onToggleMacroRecording: () => void;
  onStopMacroLoop: () => void;
  onPressButton: (button: string) => void;
  onTakeScreenshot: () => void;
  onOpenSettings: () => void;
  onOpenFileManager: () => void;
  onOpenCommandBar: () => void;
  onToggleAlwaysOnTop: () => void;
  onDisconnect: () => void;
  onRotate: () => void;
  onToggleMute: () => void;
  onCanvasMouseEvent: (e: React.MouseEvent<HTMLElement>, action: string) => void;
  onWheel: (e: React.WheelEvent<HTMLElement>) => void;
  onKeyDown: (e: React.KeyboardEvent) => void;
  onCompositionStart: () => void;
  onCompositionEnd: (e: React.CompositionEvent) => void;
  onSendText: (text: string, enter?: boolean) => void;
  /** The conversation copilot, when the feature is on. */
  copilot?: CopilotHandle;
}

function formatTime(seconds: number) {
  const m = Math.floor(seconds / 60).toString().padStart(2, "0");
  const s = (seconds % 60).toString().padStart(2, "0");
  return `${m}:${s}`;
}

/** Compact chrome toolbar hits (title bar); overrides default Button height and ghost hover fill. */
const mirrorToolbarBtnClass =
  "size-7 min-h-0 shrink-0 gap-0 rounded-md border-transparent p-0 shadow-none text-text-2 transition-all duration-100 hover:bg-transparent dark:hover:bg-transparent hover:text-foreground active:scale-[0.92] [&_svg]:!size-4";

/** macOS draws the window's traffic lights over the top-left of the content
 *  (`titleBarStyle: "Overlay"`), so the toolbar starts past them. Windows keeps
 *  its own title bar above the toolbar - Overlay is macOS-only - and reserving
 *  the same 72px there is 72px of dead space in the row with the least of it. */
const isMac = navigator.userAgent.includes("Mac");

// Below 341px the toolbar stops showing its extras and keeps the controls the
// mirror cannot be used without - the close button above all, which used to be
// the first thing pushed out of the window. Everything that goes is in the
// command bar (⌘K), which is itself one of the buttons that stay. The width is
// written into the class names rather than kept in a constant: the stylesheet
// is built by scanning this file for class strings, so an interpolated one
// would generate nothing.

export function MirrorScreen({
  connectedDevice,
  connecting,
  viewOnly,
  textInputEnabled,
  waitingHint,
  streamStarted,
  deviceSize,
  canvasRef,
  isMouseDownRef,
  recording,
  muted,
  macroRecording,
  macroLooping,
  onStopMacroLoop,
  pinnedActions,
  alwaysOnTop,
  adaptiveInfo,
  onToggleRecording,
  onToggleMacroRecording,
  onPressButton,
  onTakeScreenshot,
  onOpenSettings,
  onOpenFileManager,
  onOpenCommandBar,
  onToggleAlwaysOnTop,
  onDisconnect,
  onRotate,
  onToggleMute,
  onCanvasMouseEvent,
  onWheel,
  onKeyDown,
  onCompositionStart,
  onCompositionEnd,
  onSendText,
  copilot,
}: MirrorScreenProps) {
  const { t } = useTranslation();
  const isIos = isIosDevice(connectedDevice);
  // The input bar is hidden for a view-only iOS session (no way to type into
  // the phone). The copilot panel shares the condition: it fills that same
  // input box, and with no box to fill it would have nothing to do.
  const inputBarVisible = (textInputEnabled ?? !viewOnly) && !connecting;
  const quickActionHandlers: Record<QuickActionId, () => void> = {
    record: onToggleRecording,
    mute: onToggleMute,
    volume_up: () => onPressButton("volume_up"),
    volume_down: () => onPressButton("volume_down"),
    power: () => onPressButton("power"),
    "macro-toggle": onToggleMacroRecording,
    rotate: onRotate,
  };

  const quickActionTitles: Record<QuickActionId, string> = {
    record: recording ? t("commands.labels.stopRecording") : t("mirror.record"),
    mute: muted ? t("mirror.unmute") : t("mirror.mute"),
    volume_up: t("mirror.volumeUp"),
    volume_down: t("mirror.volumeDown"),
    power: t("mirror.power"),
    "macro-toggle": macroRecording ? t("mirror.stopMacro") : t("mirror.recordMacro"),
    rotate: t("mirror.rotate"),
  };
  const [elapsed, setElapsed] = useState(0);
  const [macroElapsed, setMacroElapsed] = useState(0);
  const [shiftHeld, setShiftHeld] = useState(false);
  const intervalRef = useRef<ReturnType<typeof setInterval> | null>(null);
  const macroIntervalRef = useRef<ReturnType<typeof setInterval> | null>(null);
  const [textInput, setTextInput] = useState("");
  /** When on, submitting also presses Enter on the phone (send a chat message, etc.). On by default. */
  const [sendEnter, setSendEnter] = useState(true);
  const textInputRef = useRef<HTMLInputElement>(null);

  // Snap the canvas box to whole device pixels. An aspect-preserving fit of
  // the stream into the viewport (e.g. 1184x2576 into 396x860) yields a
  // fractional CSS size; the compositor rounds the painted area down, which
  // leaves ~1px background slivers on the right/bottom edges. Sizing the
  // element to an integer device-pixel width/height removes them.
  const videoViewportRef = useRef<HTMLDivElement | null>(null);
  useEffect(() => {
    const vp = videoViewportRef.current;
    const canvas = canvasRef.current;
    if (!vp || !canvas) return;
    const apply = () => {
      const { width: W, height: H } = deviceSize;
      if (!W || !H) return;
      const dpr = window.devicePixelRatio || 1;
      const rect = vp.getBoundingClientRect();
      // Rounded up, both of them: the viewport's own width can be fractional
      // (a window is whole pixels, the box inside it need not be), and
      // flooring the measurement makes the canvas half a pixel short - which
      // the compositor blends with the background into a hairline along
      // whichever edge it lands on. Covering the measurement costs at most a
      // pixel of overflow, which the viewport clips.
      const availW = Math.max(1, Math.ceil(rect.width * dpr));
      const availH = Math.max(1, Math.ceil(rect.height * dpr));
      // The canvas has to cover the viewport, never fall short of it. Falling
      // short leaves the page background showing along whichever edge the
      // browser's centring snaps the remainder to, and that hairline is what
      // the eye finds first - measured on a real window (2026-10-11): a 568px
      // viewport with a 566px canvas painted background down the left side.
      //
      // So the axis that runs out of room first takes the viewport's own size,
      // and the other is rounded up: both end up at least as large as the box
      // they sit in. Overshooting costs at most a pixel of overflow, which the
      // viewport's `overflow-hidden` clips (and `max-w-full`/`max-h-full`
      // clamp, which can only shrink a too-large canvas, never a too-small
      // one) - and the window sizing keeps the mismatch to that one pixel.
      const widthLimited = availW / W <= availH / H;
      let pw = Math.max(1, widthLimited ? availW : Math.ceil(W * (availH / H)));
      let ph = Math.max(1, widthLimited ? Math.ceil(H * (availW / W)) : availH);

      // And where the fit lands within a pixel or two of the viewport - the
      // normal case, since the window was sized from this same aspect - take
      // the viewport's own size instead. The window manager does not always
      // hand back the width it was asked for (measured 2026-10-11: 569 for a
      // request of 568), and chasing that last pixel with another setSize does
      // not converge; letting the canvas be that one pixel wider stretches the
      // picture by 0.08%, which is invisible, while the aspect-preserving
      // alternative leaves a pale column of background down one edge, which is
      // not. A window dragged far from the device's shape still letterboxes:
      // past this tolerance the aspect fit is used as it always was.
      if (Math.abs(pw - availW) <= 2 && Math.abs(ph - availH) <= 2) {
        pw = availW;
        ph = availH;
      }
      canvas.style.width = `${(pw / dpr).toFixed(4)}px`;
      canvas.style.height = `${(ph / dpr).toFixed(4)}px`;
    };
    apply();
    const ro = new ResizeObserver(apply);
    ro.observe(vp);
    return () => ro.disconnect();
  }, [deviceSize.width, deviceSize.height, canvasRef]);

  /** A copilot candidate into the input box. Deliberately not a send: the
   *  user reads it, edits it, and presses send. `sendEnter` is left alone. */
  const fillFromCopilot = (text: string) => {
    setTextInput(text);
    const input = textInputRef.current;
    if (!input) return;
    input.focus();
    // Put the caret at the end so the user can keep typing; the value is set
    // through React, so it lands in the DOM on the next frame.
    requestAnimationFrame(() => {
      input.setSelectionRange(text.length, text.length);
    });
  };

  const submitText = () => {
    const text = textInput.trim();
    // Empty input + ↵ on = send a bare Enter press.
    if (!text && !sendEnter) return;
    onSendText(text, sendEnter);
    setTextInput("");
    textInputRef.current?.focus();
  };

  useEffect(() => {
    const down = (e: KeyboardEvent) => { if (e.key === "Shift") setShiftHeld(true); };
    const up = (e: KeyboardEvent) => { if (e.key === "Shift") setShiftHeld(false); };
    const blur = () => setShiftHeld(false);
    window.addEventListener("keydown", down);
    window.addEventListener("keyup", up);
    window.addEventListener("blur", blur);
    return () => {
      window.removeEventListener("keydown", down);
      window.removeEventListener("keyup", up);
      window.removeEventListener("blur", blur);
    };
  }, []);

  useEffect(() => {
    if (recording) {
      setElapsed(0);
      intervalRef.current = setInterval(() => setElapsed((s) => s + 1), 1000);
    } else {
      if (intervalRef.current) clearInterval(intervalRef.current);
      intervalRef.current = null;
    }
    return () => { if (intervalRef.current) clearInterval(intervalRef.current); };
  }, [recording]);

  useEffect(() => {
    if (macroRecording) {
      setMacroElapsed(0);
      macroIntervalRef.current = setInterval(() => setMacroElapsed((s) => s + 1), 1000);
    } else {
      if (macroIntervalRef.current) clearInterval(macroIntervalRef.current);
      macroIntervalRef.current = null;
    }
    return () => { if (macroIntervalRef.current) clearInterval(macroIntervalRef.current); };
  }, [macroRecording]);

  // While waiting for AirPlay to start (any iOS session, no frames yet),
  // keep a live-ish preview of the phone on screen: poll the device
  // screenshot (idevice over USB) and show it where the video will land.
  const showAirplayHint = isIos && !connecting && !streamStarted && !!waitingHint;
  const [previewShot, setPreviewShot] = useState<string | null>(null);
  // The hint banner stays at the top of the viewport until dismissed with
  // its X (or until mirroring starts); it returns on the next wait.
  const [hintChipVisible, setHintChipVisible] = useState(true);
  useEffect(() => {
    if (showAirplayHint) setHintChipVisible(true);
  }, [showAirplayHint]);
  useEffect(() => {
    console.log("[connect] MirrorScreen mounted");
  }, []);
  useEffect(() => {
    if (!showAirplayHint) return;
    console.log("[airplay-preview] hint shown, starting screenshot poll");
    let cancelled = false;
    let timer: ReturnType<typeof setTimeout> | null = null;
    const fetchShot = async () => {
      const t0 = performance.now();
      try {
        const b64 = await invoke<string>("take_screenshot");
        console.log(
          `[airplay-preview] invoke round-trip ${Math.round(performance.now() - t0)}ms (${b64.length} b64 chars)`
        );
        if (!cancelled) setPreviewShot(`data:image/png;base64,${b64}`);
      } catch (e) {
        // device busy/unplugged - keep the last shot
        console.warn(`[airplay-preview] invoke failed after ${Math.round(performance.now() - t0)}ms:`, e);
      }
      if (!cancelled) timer = setTimeout(fetchShot, 1000);
    };
    fetchShot();
    return () => {
      cancelled = true;
      if (timer) clearTimeout(timer);
    };
  }, [showAirplayHint]);

  return (
    <div className="flex flex-col h-screen w-screen animate-in fade-in zoom-in-[0.98] duration-300">
      <div className={`h-[38px] shrink-0 flex items-center ${isMac ? "pl-[72px]" : "pl-2"} pr-2 bg-surface/90 backdrop-blur-md drag-region`} data-tauri-drag-region>
        <div className="flex items-center gap-1.5 min-w-0 mr-auto drag-region" data-tauri-drag-region>
          <span className="text-[11px] font-medium text-foreground truncate max-w-[120px] data-tauri-drag-region">{getDeviceDisplayName(connectedDevice)}</span>
        </div>
        {!viewOnly && (
          <div className="flex items-center gap-0.5 no-drag">
            {isIos || !shiftHeld
              ? <Button variant="ghost" className={mirrorToolbarBtnClass} onClick={() => onPressButton("home")} title={t("mirror.home")}><HomeIcon /></Button>
              : <Button variant="ghost" className={mirrorToolbarBtnClass} onClick={() => onPressButton("back")} title={t("mirror.back")}><ChevronLeftIcon /></Button>}
            {isIos || !shiftHeld
              ? <Button variant="ghost" className={mirrorToolbarBtnClass} onClick={() => onPressButton("recents")} title={t("mirror.recents")}><Square2StackIcon /></Button>
              : <Button variant="ghost" className={mirrorToolbarBtnClass} onClick={() => onPressButton("power")} title={t("mirror.power")}><PowerIcon /></Button>}
          </div>
        )}
        {(() => {
          // View-only iOS: record only. Controllable iOS: volume (home/recents
          // have dedicated buttons, power is intentionally not shown).
          // Android: every pinned action.
          const visiblePinned = pinnedActions.filter((id) => {
            if (viewOnly) return !INPUT_QUICK_ACTIONS.includes(id);
            if (isIos) return ["volume_up", "volume_down"].includes(id);
            return true;
          });
          if (visiblePinned.length === 0) return null;
          return (
            <>
              {/* Gone on a narrow window, where the row cannot hold the
                  extras and the buttons at once. */}
              <div className="w-px h-3.5 bg-border/30 mx-1 shrink-0 hidden min-[341px]:block" />
              {/* The group that gives way: it scrolls when the row is tight,
                  so the fixed controls to its right - the close button first -
                  stay on screen instead of being pushed out of the window. */}
              <div className="flex items-center gap-0.5 no-drag min-w-0 overflow-x-auto [scrollbar-width:none] [&::-webkit-scrollbar]:hidden hidden min-[341px]:flex">
                {visiblePinned.map((id) => (
                  <Button key={id} variant="ghost" className={mirrorToolbarBtnClass} onClick={quickActionHandlers[id]} title={quickActionTitles[id]}>
                    {QUICK_ACTION_ICONS[id]({ muted, recording, macroRecording })}
                  </Button>
                ))}
              </div>
            </>
          );
        })()}
        <div className="w-px h-3.5 bg-border/30 mx-1 shrink-0 hidden min-[341px]:block" />
        <div className="flex items-center gap-0.5 no-drag shrink-0">
          {shiftHeld && !viewOnly && !isIos
            ? <Button variant="ghost" className={mirrorToolbarBtnClass} onClick={onRotate} title={t("mirror.rotate")}><ArrowPathIcon /></Button>
            : <Button variant="ghost" className={mirrorToolbarBtnClass} onClick={onOpenCommandBar} title={t("mirror.commands")}><CommandLineIcon /></Button>}
          <Button variant="ghost" className={mirrorToolbarBtnClass} onClick={onToggleRecording} title={recording ? t("commands.labels.stopRecording") : t("mirror.record")}>{recording ? <StopIcon className="text-[#f44]" /> : <VideoCameraIcon />}</Button>
          <Button variant="ghost" className={mirrorToolbarBtnClass} onClick={onTakeScreenshot} title={t("mirror.screenshot")}><CameraIcon /></Button>
          <Button variant="ghost" className={mirrorToolbarBtnClass} onClick={onOpenFileManager} title={t("mirror.files")}><FolderIcon /></Button>
          <Button variant="ghost" className={mirrorToolbarBtnClass} onClick={onOpenSettings} title={t("mirror.settings")}><Cog6ToothIcon /></Button>
          <Button variant="ghost" className={`${mirrorToolbarBtnClass} hidden min-[341px]:inline-flex ${alwaysOnTop ? "text-brand" : ""}`} onClick={onToggleAlwaysOnTop} title={alwaysOnTop ? t("commands.labels.disableAlwaysOnTop") : t("commands.labels.alwaysOnTop")}>
            <svg viewBox="0 0 24 24" fill={alwaysOnTop ? "currentColor" : "none"} stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" strokeLinejoin="round"><path d="M12 17v5" /><path d="M5 17h14" /><path d="M15 3H9v7l-2.5 7h11L15 10V3z" /></svg>
          </Button>
          <Button variant="ghost" className={mirrorToolbarBtnClass} onClick={onDisconnect} title={t("commands.labels.disconnect")}><XMarkIcon /></Button>
        </div>
      </div>


      {/* Canvas viewport */}
      <div className="flex-1 flex flex-col overflow-hidden outline-none relative" tabIndex={0} onKeyDown={onKeyDown} onCompositionStart={onCompositionStart} onCompositionEnd={onCompositionEnd}>
        <div ref={videoViewportRef} className="flex-1 flex items-center justify-center overflow-hidden relative">
        {connecting ? (
          <div className="flex flex-col items-center gap-4 text-text-3">
            <div className="size-6 border-2 border-border border-t-brand rounded-full animate-spin" />
            <p className="text-[13px] font-medium">{t("mirror.connecting")}</p>
          </div>
        ) : showAirplayHint && previewShot ? (
          // Placeholder for the AirPlay video: the phone's live screenshot,
          // refreshed every second until mirroring starts. Input goes through
          // the same path as the canvas (HID bridge for iOS) — the preview is
          // fully clickable while waiting for AirPlay.
          <img
            src={previewShot}
            alt=""
            data-role="mirror-preview"
            draggable={false}
            onLoad={(e) => {
              const el = e.currentTarget;
              if (el.dataset.timed !== "1") {
                el.dataset.timed = "1";
                console.log("[airplay-preview] image decoded and rendered");
              }
            }}
            className="max-w-full max-h-full object-contain rounded-lg border border-border/50 shadow-lg select-none cursor-default"
            onMouseDown={(e) => { isMouseDownRef.current = true; onCanvasMouseEvent(e, "down"); }}
            onMouseMove={(e) => { if (isMouseDownRef.current) onCanvasMouseEvent(e, "move"); }}
            onMouseUp={(e) => { isMouseDownRef.current = false; onCanvasMouseEvent(e, "up"); }}
            onMouseLeave={(e) => { if (isMouseDownRef.current) { isMouseDownRef.current = false; onCanvasMouseEvent(e, "up"); } }}
            onWheel={onWheel}
            onContextMenu={(e) => e.preventDefault()}
          />
        ) : (
          <canvas
            ref={canvasRef}
            data-role="mirror-canvas"
            width={deviceSize.width}
            height={deviceSize.height}
            className="max-w-full max-h-full cursor-default block"
            onMouseDown={(e) => { isMouseDownRef.current = true; onCanvasMouseEvent(e, "down"); }}
            onMouseMove={(e) => { if (isMouseDownRef.current) onCanvasMouseEvent(e, "move"); }}
            onMouseUp={(e) => { isMouseDownRef.current = false; onCanvasMouseEvent(e, "up"); }}
            onMouseLeave={(e) => { if (isMouseDownRef.current) { isMouseDownRef.current = false; onCanvasMouseEvent(e, "up"); } }}
            onWheel={onWheel}
            onContextMenu={(e) => e.preventDefault()}
          />
        )}

        {isIos && !connecting && !streamStarted && !showAirplayHint && (
          // USB-tunnel screen sessions need no Control Center interaction;
          // overlay a spinner on the (still blank) canvas until the first
          // frame arrives. AirPlay sessions show the hint chip instead.
          <div className="absolute inset-0 flex flex-col items-center justify-center gap-4 text-text-3 z-10 pointer-events-none">
            <div className="size-6 border-2 border-border border-t-brand rounded-full animate-spin" />
            <p className="text-[13px] font-medium">{t("mirror.tunnelStarting")}</p>
          </div>
        )}

        {recording && (
          <div className="absolute bottom-4 left-1/2 -translate-x-1/2 flex items-center gap-2.5 py-1.5 px-2.5 pl-3.5 bg-black/75 backdrop-blur-xl rounded-full z-10 animate-in fade-in slide-in-from-bottom-2 duration-200">
            <span className="size-2 rounded-full bg-[#f44] shrink-0 animate-pulse" />

            <span className="text-[13px] font-semibold font-mono
            text-white min-w-10">{formatTime(elapsed)}</span>
            <Button variant="ghost" className="flex items-center gap-1 px-2.5 py-1 h-auto bg-[#f44] border-transparent rounded-xl text-white text-xs font-semibold hover:bg-[#e33] [&_svg]:size-3" onClick={onToggleRecording}>
              <StopIcon />
              {t("common.stop")}
            </Button>
          </div>
        )}

        {/* A loop keeps tapping the phone, so it has to be visible from the
            screen it is running on - and the way out is written on it. */}
        {macroLooping && (
          <div className="absolute bottom-[52px] left-1/2 -translate-x-1/2 flex items-center gap-2.5 py-1.5 px-2.5 pl-3.5 bg-black/75 backdrop-blur-xl rounded-full z-10 animate-in fade-in slide-in-from-bottom-2 duration-200">
            <ArrowPathIcon className="size-3.5 text-brand shrink-0 animate-spin" />
            <span className="text-[13px] font-semibold text-white max-w-[160px] truncate">{macroLooping}</span>
            <span className="text-[10px] font-mono text-white/50">{MOD}+E</span>
            <Button variant="ghost" className="flex items-center gap-1 px-2.5 py-1 h-auto bg-brand border-transparent rounded-xl text-white text-xs font-semibold hover:bg-brand/90 [&_svg]:size-3" onClick={onStopMacroLoop}>
              <StopIcon />
              {t("common.stop")}
            </Button>
          </div>
        )}

        {macroRecording && (
          <div className="absolute bottom-[52px] left-1/2 -translate-x-1/2 flex items-center gap-2.5 py-1.5 px-2.5 pl-3.5 bg-black/75 backdrop-blur-xl rounded-full z-10 animate-in fade-in slide-in-from-bottom-2 duration-200">
            <svg width="12" height="12" viewBox="0 0 24 25" fill="none" className="text-[#f90] shrink-0 animate-pulse">
              <path d="M11.41 2.068c-.57 0-1.08 0-1.55.17-.1.04-.19.08-.29.12-.46.22-.81.58-1.21.98L3.58 8.148c-.47.47-.88.88-1.11 1.43-.22.54-.22 1.13-.22 1.8v3.47c0 1.78 0 3.22.15 4.35.16 1.17.49 2.16 1.27 2.95.78.78 1.76 1.12 2.93 1.28 1.12.15 2.55.15 4.33.15s3.31 0 4.43-.15c-.49-1.1-1.51-2.09-2.61-2.52-1.66-.65-1.66-3.01 0-3.66 1.16-.46 2.22-1.52 2.67-2.67.66-1.66 3.01-1.66 3.66 0 .16.41.39.81.67 1.17V14.858c0-1.53 0-2.77-.11-3.75-.12-1.02-.37-1.89-.96-2.63-.22-.27-.46-.52-.73-.74-.73-.6-1.6-.85-2.61-.97-1.18-.11-2.4-.11-3.92-.11z" fill="currentColor" opacity="0.5"/>
              <path fillRule="evenodd" clipRule="evenodd" d="M9.569 2.358c.09-.05.19-.09.29-.12.21-.07.42-.12.65-.14v1.99c0 1.36 0 2.01-.12 2.88-.12.9-.38 1.66-.98 2.26s-1.36.86-2.26.98c-.87.12-1.52.12-2.88.12H2.289c.03-.26.09-.51.18-.75.22-.54.64-.96 1.11-1.43l4.78-4.81c.4-.4.76-.77 1.21-.98zM17.919 23.118c-.24.61-1.09.61-1.33 0l-.04-.1a5.73 5.73 0 00-3.23-3.23l-.11-.04c-.6-.24-.6-1.1 0-1.33l.11-.04a5.73 5.73 0 003.23-3.23l.04-.1c.24-.61 1.09-.61 1.33 0l.04.1a5.73 5.73 0 003.23 3.23l.11.04c.6.24.6 1.1 0 1.33l-.11.04a5.73 5.73 0 00-3.23 3.23l-.04.1z" fill="currentColor"/>
            </svg>
            <span className="text-[13px] font-semibold font-mono text-white min-w-10">{formatTime(macroElapsed)}</span>
            <Button variant="ghost" className="flex items-center gap-1 px-2.5 py-1 h-auto bg-[#f90] border-transparent rounded-xl text-white text-xs font-semibold hover:bg-[#e80] [&_svg]:size-3" onClick={onToggleMacroRecording}>
              <StopIcon />
              {t("common.stop")}
            </Button>
          </div>
        )}

        {showAirplayHint && hintChipVisible && (
          // Above the copilot panel (z-20), which now shares the top of the
          // canvas: the chip is the one thing that says the mirror is not
          // running yet, so it must not end up behind a panel.
          <div className="absolute inset-x-0 top-3 flex justify-center z-30 pointer-events-none">
            <div className="flex items-center gap-2 pl-3 pr-1.5 py-1.5 bg-black/70 backdrop-blur-lg rounded-full text-white text-[11px] font-medium animate-in fade-in duration-200 pointer-events-auto">
              <div className="size-3 border-2 border-white/30 border-t-white rounded-full animate-spin shrink-0" />
              {waitingHint}
              <button
                className="flex items-center justify-center size-5 rounded-full text-white/60 hover:text-white hover:bg-white/10 transition-colors cursor-pointer"
                onClick={() => setHintChipVisible(false)}
                title={t("common.close")}
                aria-label={t("common.close")}
              >
                <XMarkIcon className="size-3" />
              </button>
            </div>
          </div>
        )}

        {adaptiveInfo?.enabled && (
          <div className="absolute bottom-2 left-2 px-2 py-0.5 bg-black/60 backdrop-blur-lg rounded-md text-text-2 font-mono text-[10px] z-5 pointer-events-none">
            {adaptiveInfo.tierName} &middot; {adaptiveInfo.fps} FPS
          </div>
        )}
        </div>

        {/* The copilot floats over the top of the canvas, well clear of the
            input bar it fills. Absolutely positioned on purpose: the window
            height is computed from the chrome around the canvas, and a panel
            in the flex flow would make that computation wrong. */}
        {inputBarVisible && copilot && (
          <CopilotPanel copilot={copilot} onFill={fillFromCopilot} />
        )}

        {/* Text input bar: type directly into the phone */}
        {inputBarVisible && (
          <div
            className="shrink-0 flex items-center gap-2 px-3 h-[var(--input-h)] bg-surface-2 border-t border-border"
            onMouseDown={(e) => e.stopPropagation()}
            onWheel={(e) => e.stopPropagation()}
          >
            <input
              ref={textInputRef}
              className="flex-1 min-w-0 h-8 px-2.5 bg-surface border border-border-strong rounded-lg text-[13px] text-foreground outline-none focus:border-brand placeholder:text-text-2"
              placeholder={t("mirror.textPlaceholder")}
              value={textInput}
              onChange={(e) => setTextInput(e.target.value)}
              onKeyDown={(e) => {
                e.stopPropagation();
                if (e.key === "Enter" && !e.nativeEvent.isComposing && textInput.trim()) submitText();
              }}
              onCompositionStart={(e) => e.stopPropagation()}
              onCompositionEnd={(e) => e.stopPropagation()}
              onContextMenu={(e) => e.stopPropagation()}
              spellCheck={false}
            />
            <Button
              size="sm"
              variant={sendEnter ? "default" : "outline"}
              className="shrink-0 h-8 px-2.5 font-mono"
              title={sendEnter ? t("mirror.enterAfter") : t("mirror.enterOff")}
              onClick={() => setSendEnter((v) => !v)}
            >
              ↵
            </Button>
            <Button
              size="sm"
              className="shrink-0 h-8"
              onClick={submitText}
              disabled={!textInput.trim() && !sendEnter}
              title={sendEnter ? t("mirror.sendEnterOn") : t("common.send")}
            >
              {t("common.send")}
            </Button>
          </div>
        )}
      </div>
    </div>
  );
}
