import { useState, useCallback, useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { Device, AndroidDevice } from "../types";
import { isIosDevice } from "../types";

/** What the backend says about the adb it would run. */
export interface AdbStatus {
  available: boolean;
  path: string;
  error?: string;
}

export function useDevices(showToast: (msg: string, type?: "error" | "info") => void) {
  const [devices, setDevices] = useState<Device[]>([]);
  // Why the Android side can be empty: no adb and no cable look identical in
  // the list, and only one of them is something the user can go and fix. Read
  // once - it is a property of the machine, not of the phone - and again
  // whenever the user says they have installed it.
  const [adbStatus, setAdbStatus] = useState<AdbStatus | null>(null);

  const checkAdb = useCallback(async () => {
    try {
      setAdbStatus(await invoke<AdbStatus>("adb_status"));
    } catch {
      setAdbStatus(null);
    }
  }, []);

  useEffect(() => {
    void checkAdb();
  }, [checkAdb]);

  const refreshDevices = useCallback(async () => {
    try {
      const devs = await invoke<Device[]>("list_devices");
      const android = devs.filter((d): d is AndroidDevice => !isIosDevice(d) && d.state === "device");
      const ios = devs.filter(isIosDevice);
      const wifiModels = new Set(
        android.filter((d) => d.serial.includes(":")).map((d) => d.model)
      );
      const usbModels = new Set(
        android.filter((d) => !d.serial.includes(":")).map((d) => d.model)
      );
      setDevices([
        ...android
          .filter((d) => !(usbModels.has(d.model) && d.serial.includes(":")))
          .map((d) => ({ ...d, wifi_available: wifiModels.has(d.model) })),
        ...ios,
      ]);
      // An iPhone that comes back after an abrupt disconnect (USB pulled) may
      // still hold the screen-capture session that blacks out its camera; the
      // backend clears it over a live connection. No-op when nothing is left
      // over, so polling it with the device list is cheap.
      for (const d of ios) {
        invoke<boolean>("cleanup_ios_media_session", { udid: d.udid }).catch(() => { });
      }
    } catch (e) {
      showToast(`${e}`);
    }
  }, [showToast]);

  useEffect(() => {
    refreshDevices();
    const interval = setInterval(refreshDevices, 3000);
    return () => clearInterval(interval);
  }, [refreshDevices]);

  return { devices, refreshDevices, adbStatus, checkAdb };
}
