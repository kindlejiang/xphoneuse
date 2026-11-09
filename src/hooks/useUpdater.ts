import { useState, useCallback, useRef } from "react";
import { check, type Update } from "@tauri-apps/plugin-updater";
import { relaunch } from "@tauri-apps/plugin-process";
import { t } from "../i18n";

type ShowToast = (message: string, type?: "error" | "info", duration?: number) => number;
type UpdateToast = (id: number, message: string, type?: "error" | "info") => void;
type DismissToast = (id: number) => void;

// Dismiss an error toast on its own after this long - long enough to read and
// report, short enough not to sit on screen forever.
const ERROR_TOAST_MS = 30_000;

/** A found update, waiting for the user to say yes. */
export interface UpdatePrompt {
  version: string;
  currentVersion: string;
  /** The release notes as written at publish time, if the feed carried any. */
  notes?: string;
}

export function useUpdater(showToast: ShowToast, updateToast: UpdateToast, dismissToast: DismissToast) {
  const [checking, setChecking] = useState(false);
  const [updating, setUpdating] = useState(false);
  // The prompt is the question, this is the update it is asking about.
  const [prompt, setPrompt] = useState<UpdatePrompt | null>(null);
  const pendingRef = useRef<Update | null>(null);

  /**
   * Download and install the update the prompt is asking about.
   *
   * Only ever runs after the user said yes: the download is tens of megabytes
   * and, on Windows, the installer closes the app - not something to start
   * because a version number looked newer.
   */
  const installUpdate = useCallback(async () => {
    const update = pendingRef.current;
    if (!update || updating) return;
    pendingRef.current = null;
    setPrompt(null);
    setUpdating(true);

    // One toast for the whole update, updated in place: the download runs for a
    // minute or more, and a bare "downloading..." that silently ends is exactly
    // what made a working updater look broken.
    let progressId: number | null = null;
    try {
      progressId = showToast(t("toasts.updateDownloading", { version: update.version }), "info", 0);

      let received = 0;
      let total = 0;
      await update.downloadAndInstall((event) => {
        if (event.event === "Started") {
          total = event.data.contentLength ?? 0;
        } else if (event.event === "Progress") {
          received += event.data.chunkLength;
          updateToast(
            progressId!,
            t("toasts.updateProgress", {
              version: update.version,
              percent: total > 0 ? Math.round((received / total) * 100) : 0,
            }),
            "info",
          );
        }
      });

      // The bundle on disk is the new version now, but this process is still
      // the old binary - the UI version and every fix in the update only
      // appear after a restart, so restart for the user. Windows is left
      // alone: its installer already closes and relaunches the app.
      if (/Windows/i.test(navigator.userAgent)) {
        updateToast(progressId, t("toasts.updateInstalled"), "info");
        return;
      }
      updateToast(progressId, t("toasts.updateRestarting"), "info");
      try {
        await relaunch();
      } catch {
        // Relaunch refused: the update still applies on the next manual start.
        updateToast(progressId, t("toasts.updateInstalled"), "info");
        dismissToastLater(progressId, dismissToast, 8000);
      }
    } catch (e) {
      // A failure must never be invisible: the user asked for this one.
      console.error("[updater] update failed:", e);
      const msg = t("toasts.updateFailed", { error: String(e) });
      if (progressId !== null) {
        updateToast(progressId, msg, "error");
        dismissToastLater(progressId, dismissToast, ERROR_TOAST_MS);
      } else {
        showToast(msg, "error", ERROR_TOAST_MS);
      }
    } finally {
      setUpdating(false);
    }
  }, [updating, showToast, updateToast, dismissToast]);

  /** Not now. The version is not remembered: the next check asks again. */
  const dismissPrompt = useCallback(() => {
    pendingRef.current = null;
    setPrompt(null);
  }, []);

  const checkForUpdates = useCallback(
    async (silent = false) => {
      if (checking || updating) return;
      setChecking(true);
      try {
        const update = await check();
        if (!update) {
          // Only a *successful* check that found nothing means "up to date".
          if (!silent) showToast(t("toasts.latestVersion"), "info");
          return;
        }

        // Held back rather than downloaded: the version and its release notes
        // are the user's to look at before anything moves.
        pendingRef.current = update;
        setPrompt({
          version: update.version,
          currentVersion: update.currentVersion,
          notes: update.body?.trim() || undefined,
        });
      } catch (e) {
        // A failure must never be invisible. Once an update has been announced
        // it always speaks up, even from the silent startup check; only a
        // silent check that never found an update stays quiet (log only).
        console.error("[updater] update failed:", e);
        if (!silent) showToast(t("toasts.updateFailed", { error: String(e) }), "error", ERROR_TOAST_MS);
      } finally {
        setChecking(false);
      }
    },
    [checking, updating, showToast],
  );

  return { checking, updating, prompt, checkForUpdates, installUpdate, dismissPrompt };
}

function dismissToastLater(id: number, dismiss: DismissToast, ms: number) {
  setTimeout(() => dismiss(id), ms);
}
