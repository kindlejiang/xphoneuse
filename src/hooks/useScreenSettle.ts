import { useEffect, useRef, useState } from "react";

/**
 * "The phone's screen has stopped changing."
 *
 * The copilot's trigger. A chat app is quiet between messages and busy while a
 * message arrives or an animation plays, so the useful moment is the one after
 * the motion: wait for the picture to move, then wait for it to stop.
 *
 * The comparison runs on a 32x64 thumbnail of the mirror canvas, not on the
 * canvas itself: `getImageData` on a 1184x2576 canvas every 350ms would copy
 * 12MB a second to answer a yes/no question. The top strip is ignored, because
 * the status bar clock ticks once a minute and would otherwise read as a new
 * message every minute.
 */

/** Thumbnail the frames are compared at. */
const SAMPLE_W = 32;
const SAMPLE_H = 64;
/** How often the canvas is sampled. */
const POLL_MS = 350;
/** How long the picture must hold still before it counts as settled. */
const QUIET_MS = 900;
/** Fraction of the sampled pixels that must move to call the screen "changing". */
const CHANGED_RATIO = 0.02;
/** Fraction of the height ignored at the top (status bar, recording pill). */
const TOP_SKIP = 0.07;
/** Per-channel difference that counts as a changed pixel. */
const CHANNEL_DELTA = 16;

export interface ScreenSettle {
  /** A canvas is present and being sampled. False while the AirPlay preview
   *  is up, since the preview is an `<img>` and there is no canvas at all. */
  watching: boolean;
}

export function useScreenSettle(options: {
  canvasRef: React.RefObject<HTMLCanvasElement | null>;
  /** Must be a plain boolean: anything else re-runs the effect on every render
   *  and the baseline never survives long enough for the screen to settle. */
  enabled: boolean;
  /** Called once each time the screen stops changing. */
  onSettled: () => void;
}): ScreenSettle {
  const { canvasRef, enabled, onSettled } = options;
  const [watching, setWatching] = useState(false);
  const settledRef = useRef(onSettled);
  settledRef.current = onSettled;

  useEffect(() => {
    if (!enabled) {
      setWatching(false);
      return;
    }
    const offscreen = document.createElement("canvas");
    offscreen.width = SAMPLE_W;
    offscreen.height = SAMPLE_H;
    // `willReadFrequently` has to be asked for when the context is first taken:
    // a second `getContext("2d", attrs)` returns the existing context and
    // silently ignores the attributes.
    const ctx = offscreen.getContext("2d", { willReadFrequently: true });
    if (!ctx) return;

    let previous: Uint8ClampedArray | null = null;
    let previousSize = "";
    let changing = false;
    let lastChange = 0;
    let reportedWatching = false;

    const tick = () => {
      const canvas = canvasRef.current;
      if (!canvas || canvas.width === 0) {
        // Nothing to sample: an AirPlay preview replaces the canvas with an
        // <img>, and diffing that image would never settle - it refreshes its
        // data URL every second whether or not the phone moved.
        if (reportedWatching) {
          reportedWatching = false;
          setWatching(false);
        }
        previous = null;
        changing = false;
        return;
      }
      if (!reportedWatching) {
        reportedWatching = true;
        setWatching(true);
      }

      const size = `${canvas.width}x${canvas.height}`;
      if (size !== previousSize) {
        // Rotated, or a new stream: start over rather than compare two
        // different geometries.
        previousSize = size;
        previous = null;
        changing = false;
      }

      ctx.drawImage(canvas, 0, 0, SAMPLE_W, SAMPLE_H);
      const frame = ctx.getImageData(0, 0, SAMPLE_W, SAMPLE_H).data;
      if (!previous) {
        previous = frame;
        return;
      }

      const firstRow = Math.floor(SAMPLE_H * TOP_SKIP);
      let changed = 0;
      let total = 0;
      for (let y = firstRow; y < SAMPLE_H; y++) {
        for (let x = 0; x < SAMPLE_W; x++) {
          const i = (y * SAMPLE_W + x) * 4;
          total++;
          if (
            Math.abs(frame[i] - previous[i]) > CHANNEL_DELTA ||
            Math.abs(frame[i + 1] - previous[i + 1]) > CHANNEL_DELTA ||
            Math.abs(frame[i + 2] - previous[i + 2]) > CHANNEL_DELTA
          ) {
            changed++;
          }
        }
      }
      previous = frame;

      const now = Date.now();
      if (total > 0 && changed / total > CHANGED_RATIO) {
        changing = true;
        lastChange = now;
        return;
      }
      if (changing && now - lastChange >= QUIET_MS) {
        changing = false;
        settledRef.current();
      }
    };

    const timer = setInterval(tick, POLL_MS);
    return () => {
      clearInterval(timer);
      setWatching(false);
    };
  }, [enabled, canvasRef]);

  return { watching };
}
