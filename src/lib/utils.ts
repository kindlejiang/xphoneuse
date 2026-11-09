import { clsx, type ClassValue } from "clsx"
import { twMerge } from "tailwind-merge"
import { invoke } from "@tauri-apps/api/core"
import tauriConf from "../../src-tauri/tauri.conf.json"

// The app version shown in the UI - read from tauri.conf.json so there is a
// single source of truth (the same value the binary and installer report).
export const APP_VERSION: string = tauriConf.version

// The platform's modifier key, as it is written in the UI (shortcut hints,
// the command bar). Windows/Linux use Ctrl, macOS Cmd - and the keyboard
// handler matches the same one (App.tsx).
export const MOD = navigator.userAgent.includes("Mac") ? "⌘" : "Ctrl"

// Height of the mirror's text-input bar (h-[var(--input-h)] in MirrorScreen).
export const INPUT_BAR_HEIGHT = 44

/**
 * Vertical chrome around the mirror viewport: the title bar (38px) plus the
 * text-input bar when the session shows it. Window sizing must account for
 * all of it - sizing the window without the input bar height leaves the
 * stream letterboxed, which paints background slivers down the left and
 * right edges.
 */
export function mirrorChromeHeight(inputBarVisible: boolean): number {
  return 38 + (inputBarVisible ? INPUT_BAR_HEIGHT : 0)
}

// Fraction of the work area the mirror window may fill: kept under 1 so the
// window edges (and its shadow) stay clear of the screen edges.
const MIRROR_AREA_FILL = 0.97

/**
 * Window size (logical px, content area) that gives the mirrored device as
 * many screen pixels as the monitor allows, capped at the stream's own
 * resolution - past 1:1 the compositor would only interpolate.
 *
 * The cap used to be a fixed 860px, which painted a 1179x2556 phone at ~1/3
 * scale; that downscale is what makes the mirror's text look soft next to the
 * same text on the phone. `workArea` is the room the client area may take, in
 * logical px: the monitor's work area less the OS frame. When it is unknown
 * the old 860px cap is used, so a failed query only costs the extra size.
 */
export function mirrorWindowSize(
  deviceWidth: number,
  deviceHeight: number,
  chromeHeight: number,
  workArea?: { width: number; height: number } | null,
): { width: number; height: number } {
  const aspect = deviceWidth / deviceHeight
  const landscape = aspect > 1
  // The view has to leave room for the toolbar and the input bar.
  const availW = workArea ? Math.round(workArea.width * MIRROR_AREA_FILL) : 860
  const availH = Math.max(
    240,
    (workArea ? Math.round(workArea.height * MIRROR_AREA_FILL) : 860 + chromeHeight) - chromeHeight,
  )
  let viewW: number
  let viewH: number
  if (landscape) {
    viewW = Math.min(deviceWidth, availW)
    viewH = Math.round(viewW / aspect)
    if (viewH > availH) {
      viewH = availH
      viewW = Math.round(viewH * aspect)
    }
  } else {
    viewH = Math.min(deviceHeight, availH)
    viewW = Math.round(viewH * aspect)
  }
  return { width: Math.max(viewW, 280), height: viewH + chromeHeight }
}

/**
 * Opens the project website in the system browser (welcome screen, the
 * no-device hint and Settings -> Help & feedback). Routed through the backend
 * so the page opened is fixed in Rust rather than coming from the webview.
 */
export async function openHomepage(): Promise<void> {
  await openSitePage()
}

/** Open a page of the website, e.g. "hardware" for the accessories page
 *  (the Bluetooth dongle an iPhone older than iOS 27 needs lives there). */
export async function openSitePage(page?: string): Promise<void> {
  const lang = (localStorage.getItem("xphoneuse_lang") || "zh").startsWith("en") ? "en" : "zh";
  try {
    await invoke("open_homepage", { path: page ? `${lang}/${page}` : null })
  } catch { /* a browser that will not open is not worth interrupting the user for */ }
}

/**
 * Android's platform-tools (the adb the app falls back to when the machine has
 * none of its own). The URL lives in the backend so the window cannot open an
 * arbitrary one.
 */
export async function openPlatformTools(): Promise<void> {
  try {
    await invoke("open_platform_tools");
  } catch { /* same reasoning as openSitePage */ }
}

/**
 * The author's X account, for the English side of the contact block (the
 * Chinese one shows the WeChat code instead). URL lives in the backend, same
 * reason as `openPlatformTools`.
 */
export async function openXAccount(): Promise<void> {
  try {
    await invoke("open_x_account");
  } catch { /* an unopenable link is not worth interrupting the user for */ }
}

export function cn(...inputs: ClassValue[]) {
  return twMerge(clsx(inputs))
}

// Cached Documents directory from the backend; resolved once per session.
let documentsDir: string | null = null

/**
 * Where screenshots and recordings are saved: the save location configured
 * in Settings, or the system Documents directory by default. Returns null
 * only when neither is available (documents lookup failed) - callers then
 * fall back to a browser download.
 */
export async function resolveSaveDir(): Promise<string | null> {
  const configured = localStorage.getItem("save_path")
  if (configured) return configured
  if (documentsDir === null) {
    try {
      documentsDir = await invoke<string>("default_save_dir")
    } catch {
      documentsDir = ""
    }
  }
  return documentsDir || null
}

/**
 * Local-time filename timestamp: YYYYMMDD-HHmmss (year, month, day, hour,
 * minute, second) - readable at a glance and sorts chronologically.
 */
export function fileTimestamp(d = new Date()): string {
  const p = (n: number) => `${n}`.padStart(2, "0")
  return `${d.getFullYear()}${p(d.getMonth() + 1)}${p(d.getDate())}-${p(d.getHours())}${p(d.getMinutes())}${p(d.getSeconds())}`
}
