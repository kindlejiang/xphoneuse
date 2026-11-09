import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { invoke } from "@tauri-apps/api/core";
import L from "leaflet";
import "leaflet/dist/leaflet.css";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Collapsible, CollapsibleContent, CollapsibleTrigger } from "@/components/ui/collapsible";
import { ChevronDownIcon } from "@heroicons/react/24/outline";

/** What the backend says about the connected device's ability to be moved. */
interface LocationStatus {
  platform: string;
  supported: boolean;
  active: boolean;
  reason?: string | null;
}

/** The last point the user picked, so the map opens where they left it. */
const PICK_KEY = "location_pick";

/** Where the map starts when there is nothing remembered: Beijing, because
 *  most of this app's users are in China and an arbitrary ocean is a worse
 *  first impression than a city they recognise. */
const DEFAULT_POINT = { lat: 39.90733, lon: 116.39117 };

function readPick(): { lat: number; lon: number } {
  try {
    const raw = localStorage.getItem(PICK_KEY);
    if (raw) {
      const parsed = JSON.parse(raw);
      if (Number.isFinite(parsed?.lat) && Number.isFinite(parsed?.lon)) {
        return { lat: parsed.lat, lon: parsed.lon };
      }
    }
  } catch {
    // A corrupt value is not worth reporting: the default is a fine answer.
  }
  return DEFAULT_POINT;
}

/**
 * Pick a position on a map and put the phone there.
 *
 * The tiles are OpenStreetMap, which needs no key and is WGS-84 - the same
 * frame a GPS reports and both phones expect, so what the user clicks is what
 * the device is told, with no conversion in between. The "open in ..." link is
 * where a Chinese map comes in, and where the coordinates are converted: see
 * `open_map_link` on the Rust side.
 *
 * iOS keeps the position only while the app holds a channel open to the phone
 * (the device falls back to real GPS when it closes), and Android leaves a
 * test provider behind until it is removed - so "stop simulating" exists and
 * means different things on each. The status line says which device is
 * connected and whether it is being simulated right now, rather than guessing.
 */
export function LocationSettings({
  open,
  onOpenChange,
  showToast,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  showToast?: (message: string, type?: "error" | "info") => void;
}) {
  const { t, i18n } = useTranslation();
  const [point, setPoint] = useState(readPick);
  const [status, setStatus] = useState<LocationStatus | null>(null);
  const [busy, setBusy] = useState(false);
  const mapBox = useRef<HTMLDivElement | null>(null);
  const map = useRef<L.Map | null>(null);
  const marker = useRef<L.Marker | null>(null);
  // The click/drag handlers run outside React's render cycle; this keeps the
  // latest setter without rebuilding the map on every pick.
  const applyPick = useRef((_lat: number, _lon: number) => {});
  applyPick.current = (lat, lon) => {
    setPoint({ lat, lon });
    try {
      localStorage.setItem(PICK_KEY, JSON.stringify({ lat, lon }));
    } catch {
      // Storage full or blocked: remembering the point is a convenience.
    }
  };

  const refreshStatus = async () => {
    try {
      setStatus(await invoke<LocationStatus>("location_status"));
    } catch (e) {
      setStatus({ platform: "none", supported: false, active: false, reason: String(e) });
    }
  };

  // Ask on mount and whenever the section is opened: the answer depends on
  // which device is connected, and that changes while the panel is up.
  useEffect(() => {
    if (!open) return;
    void refreshStatus();
  }, [open]);

  // The map is built on open and torn down on close. Leaflet measures its
  // container once, so a map created while the section is collapsed would come
  // out the wrong size; rebuilding per open is simpler than repairing it.
  useEffect(() => {
    if (!open || !mapBox.current || map.current) return;
    const instance = L.map(mapBox.current, {
      center: [point.lat, point.lon],
      zoom: 12,
      worldCopyJump: true,
    });
    L.tileLayer("https://tile.openstreetmap.org/{z}/{x}/{y}.png", {
      maxZoom: 19,
      attribution: "&copy; OpenStreetMap",
    }).addTo(instance);
    const pin = L.marker([point.lat, point.lon], { draggable: true }).addTo(instance);
    pin.on("dragend", () => {
      const { lat, lng } = pin.getLatLng();
      applyPick.current(lat, lng);
    });
    instance.on("click", (e: L.LeafletMouseEvent) => {
      pin.setLatLng(e.latlng);
      applyPick.current(e.latlng.lat, e.latlng.lng);
    });
    map.current = instance;
    marker.current = pin;
    return () => {
      instance.remove();
      map.current = null;
      marker.current = null;
    };
    // Rebuilt only when the section opens; `point` is read for the initial
    // view and then owned by the map.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open]);

  const setFromInput = (lat: number, lon: number) => {
    applyPick.current(lat, lon);
    marker.current?.setLatLng([lat, lon]);
  };

  const send = async () => {
    setBusy(true);
    try {
      await invoke("location_set", { lat: point.lat, lon: point.lon });
      showToast?.(t("settings.locationSent"), "info");
      await refreshStatus();
    } catch (e) {
      showToast?.(String(e), "error");
    } finally {
      setBusy(false);
    }
  };

  const stop = async () => {
    setBusy(true);
    try {
      await invoke("location_clear");
      showToast?.(t("settings.locationCleared"), "info");
      await refreshStatus();
    } catch (e) {
      showToast?.(String(e), "error");
    } finally {
      setBusy(false);
    }
  };

  const openInMap = async () => {
    try {
      // Chinese users get Tencent Maps, everyone else Google - the same split
      // the contact block uses. The coordinate conversion a Chinese map needs
      // happens on the Rust side, next to the URL it belongs to.
      await invoke("open_map_link", {
        lat: point.lat,
        lon: point.lon,
        provider: i18n.language === "zh" ? "tencent" : "google",
      });
    } catch (e) {
      showToast?.(String(e), "error");
    }
  };

  const statusLine = !status
    ? t("settings.locationChecking")
    : status.platform === "none"
      ? t("settings.locationNoDevice")
      : !status.supported
        ? (status.reason ?? t("settings.locationUnsupported"))
        : status.active
          ? t("settings.locationActive")
          : t("settings.locationIdle");

  return (
    <Collapsible open={open} onOpenChange={onOpenChange}>
      <CollapsibleTrigger className="flex w-full items-center justify-between px-5 py-4 border-b border-border/60 text-[10px] font-bold uppercase tracking-widest text-text-3 hover:text-foreground">
        {t("settings.locationSection")}
        <ChevronDownIcon className={`size-3.5 transition-transform ${open ? "rotate-180" : ""}`} />
      </CollapsibleTrigger>
      <CollapsibleContent>
        <div className="px-5 py-4 border-b border-border/60 flex flex-col gap-3">
          <div className="text-[11px] text-text-3">{t("settings.locationDesc")}</div>

          <div ref={mapBox} className="h-[220px] w-full rounded-lg overflow-hidden border border-border" />

          <div className="flex items-center gap-2">
            <Input
              className="h-8 text-[12px] font-mono"
              value={point.lat.toFixed(6)}
              onChange={(e) => {
                const lat = Number(e.target.value);
                if (Number.isFinite(lat) && lat >= -90 && lat <= 90) setFromInput(lat, point.lon);
              }}
            />
            <Input
              className="h-8 text-[12px] font-mono"
              value={point.lon.toFixed(6)}
              onChange={(e) => {
                const lon = Number(e.target.value);
                if (Number.isFinite(lon) && lon >= -180 && lon <= 180) setFromInput(point.lat, lon);
              }}
            />
          </div>

          <div className="flex items-center gap-2">
            <Button size="sm" className="flex-1 text-[11px]" disabled={busy || !status?.supported} onClick={send}>
              {t("settings.locationSend")}
            </Button>
            <Button
              size="sm"
              variant="outline"
              className="flex-1 text-[11px]"
              disabled={busy || status?.platform === "none"}
              onClick={stop}
            >
              {t("settings.locationStop")}
            </Button>
          </div>

          <div className="flex items-center justify-between gap-2">
            <span className={`text-[11px] ${status?.active ? "text-success" : "text-text-3"}`}>{statusLine}</span>
            <Button size="sm" variant="ghost" className="text-[11px] shrink-0" onClick={openInMap}>
              {i18n.language === "zh" ? t("settings.locationOpenTencent") : t("settings.locationOpenGoogle")}
            </Button>
          </div>
        </div>
      </CollapsibleContent>
    </Collapsible>
  );
}
