import { useState, useRef, useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useTranslation, Trans } from "react-i18next";
import {
  DevicePhoneMobileIcon,
  Cog6ToothIcon,
  ArrowPathIcon,
  ChevronRightIcon,
  SunIcon,
  MoonIcon,
  SignalIcon,
  ComputerDesktopIcon,
  WifiIcon,
} from "@heroicons/react/24/outline";
import {
  Dialog,
  DialogOverlay,
  DialogPortal,
  DialogTitle,
} from "@/components/ui/dialog";
import { Dialog as DialogPrimitive } from "@base-ui/react/dialog";
import { Button } from "@/components/ui/button";
import { APP_VERSION, cn, openHomepage, openSitePage, openPlatformTools, openXAccount } from "@/lib/utils";
import type { Device, ThemePreference } from "../types";
import { getDeviceDisplayName, getDeviceKey, getDeviceNickname, setDeviceNickname, isIosDevice, iosNeedsDongle } from "../types";
import appIcon from "../assets/icon.png";
import wechatQr from "../assets/wechat.jpg";

interface WelcomeScreenProps {
  devices: Device[];
  connectingSerial: string | null;
  themePref: ThemePreference;
  onCycleTheme: () => void;
  onOpenSettings: () => void;
  onRefreshDevices: () => void;
  onConnectDevice: (device: Device) => void;
  showToast: (msg: string, type?: "error" | "info") => void;
  /** Null until the backend has answered; false means Android is unusable. */
  adbAvailable: boolean | null;
  adbError?: string;
  onCheckAdb: () => void;
}

function truncateSerial(s: string) {
  return s.length > 16 ? s.slice(0, 6) + "..." + s.slice(-4) : s;
}

function isWifiDevice(serial: string) {
  return serial.includes(":");
}

export function WelcomeScreen({
  devices,
  connectingSerial,
  adbAvailable,
  adbError,
  onCheckAdb,
  themePref,
  onCycleTheme,
  onOpenSettings,
  onRefreshDevices,
  onConnectDevice,
  showToast,
}: WelcomeScreenProps) {
  const { t, i18n } = useTranslation();
  // The contact channel follows the interface language, not the device: a
  // Chinese window gets the WeChat code, an English one the X account.
  const isZh = (i18n.language || "zh").startsWith("zh");
  const [showWifiDialog, setShowWifiDialog] = useState(false);
  const [showContact, setShowContact] = useState(false);
  const [wifiAddress, setWifiAddress] = useState("");
  const [wifiConnecting, setWifiConnecting] = useState(false);
  const [togglingSerial, setTogglingSerial] = useState<string | null>(null);
  const [editingSerial, setEditingSerial] = useState<string | null>(null);
  const [editValue, setEditValue] = useState("");
  const [contextMenu, setContextMenu] = useState<{ x: number; y: number; device: Device } | null>(null);
  const contextMenuRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!contextMenu) return;
    const close = (e: MouseEvent) => {
      if (contextMenuRef.current && !contextMenuRef.current.contains(e.target as Node)) {
        setContextMenu(null);
      }
    };
    window.addEventListener("mousedown", close);
    return () => window.removeEventListener("mousedown", close);
  }, [contextMenu]);

  const handleContextMenu = (e: React.MouseEvent, device: Device) => {
    e.preventDefault();
    e.stopPropagation();
    setContextMenu({ x: e.clientX, y: e.clientY, device });
  };

  const handleWifiConnect = async () => {
    if (!wifiAddress.trim()) return;
    setWifiConnecting(true);
    try {
      const addr = wifiAddress.includes(":") ? wifiAddress : `${wifiAddress}:5555`;
      await invoke("wifi_connect", { address: addr });
      showToast(t("welcome.wifiConnected"), "info");
      setWifiAddress("");
      setShowWifiDialog(false);
      onRefreshDevices();
    } catch (e) {
      showToast(t("welcome.connectionFailed", { error: String(e) }));
    } finally {
      setWifiConnecting(false);
    }
  };

  const handleToggleWifi = async (e: React.MouseEvent, device: Device) => {
    e.stopPropagation();
    if (isIosDevice(device)) return;
    setTogglingSerial(device.serial);
    try {
      if (isWifiDevice(device.serial)) {
        await invoke("wifi_disconnect", { address: device.serial });
        showToast(t("welcome.wifiDisconnected", { name: getDeviceDisplayName(device) }), "info");
      } else if (device.wifi_available) {
        const ip = await invoke<string | null>("get_device_ip", { serial: device.serial });
        if (ip) {
          await invoke("wifi_disconnect", { address: `${ip}:5555` });
          showToast(t("welcome.wifiDisconnected", { name: getDeviceDisplayName(device) }), "info");
        }
      } else {
        const addr = await invoke<string>("wifi_enable", { serial: device.serial });
        showToast(t("welcome.nowAvailableAt", { name: getDeviceDisplayName(device), address: addr }), "info");
      }
      onRefreshDevices();
    } catch (e) {
      showToast(`${e}`);
    } finally {
      setTogglingSerial(null);
    }
  };

  return (
    <div className="h-screen w-screen flex flex-col items-center justify-center bg-background relative animate-in fade-in duration-600 pt-[38px]">
      <div className="absolute top-0 left-0 right-0 h-[38px] drag-region bg-surface border-b border-border flex items-center justify-end" data-tauri-drag-region>
        <div className="flex items-center gap-0.5 pr-2.5 no-drag">
          <Button variant="ghost" size="icon-xs" className="text-text-3 [&_svg]:size-[15px]" onClick={onCycleTheme} title={themePref === "light" ? t("common.light") : themePref === "dark" ? t("common.dark") : t("common.system")}>
            {themePref === "light" ? <SunIcon /> : themePref === "dark" ? <MoonIcon /> : <ComputerDesktopIcon />}
          </Button>
          <Button variant="ghost" size="icon-xs" className="text-text-3 [&_svg]:size-[15px]" onClick={() => setShowWifiDialog(true)} title={t("welcome.connectViaWifi")}>
            <WifiIcon />
          </Button>
          <Button variant="ghost" size="icon-xs" className="text-text-3 [&_svg]:size-[15px]" onClick={onOpenSettings} title={t("common.settings")}>
            <Cog6ToothIcon />
          </Button>
        </div>
      </div>

      <div className="flex flex-col items-center gap-3 mb-2">
        <img src={appIcon} alt={t("common.appName")} className="size-16 rounded-2xl" />
        <h1 className="text-[28px] font-bold tracking-tight">{t("common.appName")}</h1>
      </div>
      <p className="text-sm text-text-2 mb-10">{t("welcome.tagline")}</p>

      <div className="w-[360px] max-w-[90vw]">
        <div className="flex items-center justify-between mb-1.5 px-2">
          <span className="text-[10px] font-semibold uppercase tracking-wider text-text-3">
            {devices.length > 0 ? t("welcome.devices", { count: devices.length }) : t("welcome.searching")}
          </span>
          <Button variant="ghost" size="xs" className="text-text-3 text-[10px] gap-[3px] [&_svg]:size-2.5" onClick={onRefreshDevices}>
            <ArrowPathIcon /> {t("common.refresh")}
          </Button>
        </div>

        {devices.length === 0 ? (
          <div className="text-center py-8 px-5 bg-surface border border-dashed border-border rounded-xl flex flex-col items-center">
            <SignalIcon className="size-8 text-text-3 mb-3 opacity-50" />
            {adbAvailable === false ? (
              // No adb is not "no phone": say which one it is, or the user goes
              // looking at cables and USB debugging forever.
              <>
                <p className="text-xs text-text-3 leading-relaxed whitespace-pre-line">
                  {t("welcome.noAdb")}
                </p>
                {adbError && (
                  <p className="mt-2 text-[10px] text-text-3/80 leading-relaxed break-all">
                    {adbError}
                  </p>
                )}
                <div className="mt-3 flex items-center gap-3">
                  <button
                    type="button"
                    onClick={openPlatformTools}
                    className="text-[11px] text-brand hover:underline underline-offset-2 cursor-pointer bg-transparent border-none"
                  >
                    {t("welcome.getPlatformTools")} →
                  </button>
                  <button
                    type="button"
                    onClick={onCheckAdb}
                    className="text-[11px] text-text-3 hover:underline underline-offset-2 cursor-pointer bg-transparent border-none"
                  >
                    {t("welcome.recheck")}
                  </button>
                </div>
              </>
            ) : (
              <>
                <p className="text-xs text-text-3 leading-relaxed whitespace-pre-line">{t("welcome.noDevices")}</p>
                <button
                  type="button"
                  onClick={openHomepage}
                  className="mt-3 text-[11px] text-brand hover:underline underline-offset-2 cursor-pointer bg-transparent border-none"
                >
                  {t("welcome.tutorial")} →
                </button>
              </>
            )}
          </div>
        ) : (
          devices.map((d) => {
            const deviceKey = getDeviceKey(d);
            return (
            <div
              key={deviceKey}
              className="bg-transparent border-none rounded-lg py-2.5 px-2 mb-0.5 cursor-pointer transition-all duration-200 flex items-center gap-2.5 hover:opacity-60"
              onClick={() => !connectingSerial && onConnectDevice(d)}
              onContextMenu={(e) => handleContextMenu(e, d)}
            >
              <div className="size-[30px] rounded-[7px] bg-surface-2 flex items-center justify-center text-text-2 shrink-0 [&_svg]:size-[15px]">
                {isIosDevice(d) ? (
                  <svg viewBox="0 0 24 24" fill="currentColor"><path d="M17.05 20.28c-.98.95-2.05.8-3.08.35-1.09-.46-2.09-.48-3.24 0-1.44.62-2.2.44-3.06-.35C2.79 15.25 3.51 7.59 9.05 7.31c1.35.07 2.29.74 3.08.8 1.18-.24 2.31-.93 3.57-.84 1.51.12 2.65.72 3.4 1.8-3.12 1.87-2.38 5.98.48 7.13-.57 1.5-1.31 2.99-2.54 4.09zM12.03 7.25c-.15-2.23 1.66-4.07 3.74-4.25.29 2.58-2.34 4.5-3.74 4.25z"/></svg>
                ) : (
                  <DevicePhoneMobileIcon />
                )}
              </div>
              <div className="flex-1 min-w-0">
                {editingSerial === deviceKey ? (
                  <input
                    className="text-[13px] font-semibold leading-tight bg-surface-2 border border-brand rounded w-full text-foreground py-px px-1 outline-none"
                    value={editValue}
                    onChange={(e) => setEditValue(e.target.value)}
                    onBlur={() => { setDeviceNickname(deviceKey, editValue); setEditingSerial(null); }}
                    onKeyDown={(e) => {
                      e.stopPropagation();
                      if (e.key === "Enter") { setDeviceNickname(deviceKey, editValue); setEditingSerial(null); }
                      if (e.key === "Escape") setEditingSerial(null);
                    }}
                    onClick={(e) => e.stopPropagation()}
                    autoFocus
                  />
                ) : (
                  <div
                    className="text-[13px] font-semibold leading-tight truncate cursor-default"
                    onDoubleClick={(e) => { e.stopPropagation(); setEditingSerial(deviceKey); setEditValue(getDeviceDisplayName(d)); }}
                    title={t("welcome.doubleClickRename")}
                  >
                    {getDeviceDisplayName(d)}
                  </div>
                )}
                <div className="text-[10px] font-mono text-text-3 leading-tight truncate flex items-center gap-1">
                  {isIosDevice(d) ? (
                    <>
                      <span className="truncate">iOS {d.ios_version}</span>
                      {d.connection_type === "network" ? (
                        <span
                          className="flex-none px-1 rounded-[3px] border border-brand/40 text-brand text-[9px] font-sans leading-[14px]"
                          title={d.network_ip ? t("welcome.wifiWithIp", { ip: d.network_ip }) : t("welcome.wifi")}
                        >
                          {t("welcome.wifi")}
                        </span>
                      ) : d.connection_type === "usb" ? (
                        <span
                          className="flex-none px-1 rounded-[3px] border border-border text-text-3 text-[9px] font-sans leading-[14px]"
                          title={t("welcome.usb")}
                        >
                          {t("welcome.usb")}
                        </span>
                      ) : null}
                    </>
                  ) : (
                    truncateSerial(d.serial)
                  )}
                </div>
                {isIosDevice(d) && !d.paired && (
                  <div className="text-[10px] text-[#f90] leading-tight mt-0.5">{t("welcome.notTrusted")}</div>
                )}
                {/* iOS keeps its screen-capture session when the cable is
                    yanked mid-stream, and the camera preview stays black
                    until a stop command reaches the phone. The backend clears
                    it automatically when the phone is reachable again; this
                    line tells the user to unplug only after disconnecting. */}
                {isIosDevice(d) && (
                  <div className="text-[10px] leading-tight mt-0.5 text-[#f90]">
                    {t("welcome.iosUsbHint")}
                  </div>
                )}
                {/* No USB tunnel below iOS 27: the screen mirrors, but nothing
                    can drive it without the Bluetooth dongle. The link goes to
                    the hardware page rather than the dongle's own page, so the
                    reader sees what else is on offer while they are there. */}
                {isIosDevice(d) && iosNeedsDongle(d.ios_version) && (
                  <div className="text-[10px] text-text-3 leading-tight mt-0.5">
                    {t("welcome.ios27Hint")}{" "}
                    <button
                      type="button"
                      className="text-brand hover:underline underline-offset-2 cursor-pointer bg-transparent border-none p-0 text-[10px]"
                      onClick={(e) => { e.stopPropagation(); openSitePage("hardware"); }}
                    >
                      {t("welcome.buyDongle")}
                    </button>
                  </div>
                )}
              </div>
              <div className="flex items-center gap-0.5 shrink-0">
                {connectingSerial === deviceKey ? (
                  <div className="size-4 border-2 border-border border-t-brand rounded-full animate-spin" />
                ) : (
                  <>
                    {!isIosDevice(d) && (
                      <Button
                        variant="ghost"
                        className={cn(
                          "flex items-center justify-center size-7 border-transparent bg-transparent text-text-3 rounded-md opacity-40 transition-all duration-150 [&_svg]:size-[15px] shadow-none",
                          (isWifiDevice(d.serial) || d.wifi_available) && "opacity-100 text-brand drop-shadow-[0_0_4px_var(--brand-glow-strong)]",
                          "hover:opacity-100 hover:bg-surface-2",
                          "disabled:opacity-30 disabled:cursor-not-allowed"
                        )}
                        title={isWifiDevice(d.serial) ? t("welcome.disableWifi") : t("welcome.enableWifi")}
                        onClick={(e) => handleToggleWifi(e, d)}
                        disabled={togglingSerial === d.serial}
                      >
                        {togglingSerial === d.serial ? <div className="size-3.5 border-2 border-border border-t-brand rounded-full animate-spin" /> : <WifiIcon />}
                      </Button>
                    )}
                    <ChevronRightIcon className="size-3.5 text-text-3" />
                  </>
                )}
              </div>
            </div>
            );
          })
        )}

      </div>

      {contextMenu && (
        <div
          ref={contextMenuRef}
          className="fixed z-100 min-w-40 bg-surface border border-border rounded-lg p-1 shadow-[0_8px_24px_rgba(0,0,0,0.3)] animate-in fade-in duration-100"
          style={{ top: contextMenu.y, left: contextMenu.x }}
        >
          <Button variant="ghost" className="block w-full h-auto justify-start py-1.5 px-2.5 text-xs font-normal text-foreground bg-transparent border-transparent rounded-[5px] shadow-none hover:bg-surface-hover" onClick={() => {
            onConnectDevice(contextMenu.device);
            setContextMenu(null);
          }}>
            {t("common.connect")}
          </Button>
          <Button variant="ghost" className="block w-full h-auto justify-start py-1.5 px-2.5 text-xs font-normal text-foreground bg-transparent border-transparent rounded-[5px] shadow-none hover:bg-surface-hover" onClick={() => {
            setEditingSerial(getDeviceKey(contextMenu.device));
            setEditValue(getDeviceDisplayName(contextMenu.device));
            setContextMenu(null);
          }}>
            {t("common.rename")}
          </Button>
          {getDeviceNickname(getDeviceKey(contextMenu.device)) && (
            <Button variant="ghost" className="block w-full h-auto justify-start py-1.5 px-2.5 text-xs font-normal text-foreground bg-transparent border-transparent rounded-[5px] shadow-none hover:bg-surface-hover" onClick={() => {
              setDeviceNickname(getDeviceKey(contextMenu.device), "");
              setContextMenu(null);
            }}>
              {t("welcome.resetName")}
            </Button>
          )}
          {!isIosDevice(contextMenu.device) && (
            <Button variant="ghost" className="block w-full h-auto justify-start py-1.5 px-2.5 text-xs font-normal text-foreground bg-transparent border-transparent rounded-[5px] shadow-none hover:bg-surface-hover" onClick={() => {
              handleToggleWifi({ stopPropagation: () => {} } as React.MouseEvent, contextMenu.device);
              setContextMenu(null);
            }}>
              {isWifiDevice(contextMenu.device.serial) ? t("welcome.disableWifi") : t("welcome.enableWifi")}
            </Button>
          )}
          <div className="h-px bg-border mx-1.5 my-1" />
          <Button variant="ghost" className="block w-full h-auto justify-start py-1.5 px-2.5 text-xs font-normal text-foreground bg-transparent border-transparent rounded-[5px] shadow-none hover:bg-surface-hover" onClick={() => {
            const key = getDeviceKey(contextMenu.device);
            navigator.clipboard.writeText(key);
            showToast(isIosDevice(contextMenu.device) ? t("welcome.udidCopied") : t("welcome.serialCopied"), "info");
            setContextMenu(null);
          }}>
            {isIosDevice(contextMenu.device) ? t("welcome.copyUdid") : t("welcome.copySerial")}
          </Button>
        </div>
      )}

      <span className="absolute bottom-3 text-[9px] text-text-3/50 flex items-center gap-1.5">
        v{APP_VERSION}
        <span className="opacity-40">·</span>
        <button
          type="button"
          onClick={openHomepage}
          className="text-text-3/70 hover:text-brand hover:underline underline-offset-2 cursor-pointer bg-transparent border-none p-0 font-inherit"
        >
          {t("welcome.website")}
        </button>
        <span className="opacity-40">·</span>
        {/* The contact link keeps the footer one line: a Chinese window opens
            the WeChat code (a code is scanned, so it gets a box of its own
            rather than a thumbnail in the corner), an English one just goes
            to X. */}
        <button
          type="button"
          onClick={isZh ? () => setShowContact(true) : openXAccount}
          className="text-text-3/70 hover:text-brand hover:underline underline-offset-2 cursor-pointer bg-transparent border-none p-0 font-inherit"
        >
          {t("welcome.contact")}
        </button>
      </span>

      <Dialog open={showWifiDialog} onOpenChange={setShowWifiDialog}>
        <DialogPortal>
          <DialogOverlay />
          <DialogPrimitive.Popup className="fixed top-1/2 left-1/2 -translate-x-1/2 -translate-y-1/2 w-[340px] max-w-[90vw] bg-surface border border-border rounded-[14px] z-50 shadow-lg animate-in fade-in zoom-in-95 duration-100">
            <DialogTitle className="text-[15px] font-semibold px-5 pt-5">{t("welcome.connectByIp")}</DialogTitle>
            <div className="px-5 py-4">
              <p className="text-xs text-muted-foreground leading-relaxed mb-3">
                <Trans
                  i18nKey="welcome.ipHint"
                  components={{ s: <strong className="text-foreground" /> }}
                />
              </p>
              <div className="flex gap-2">
                <input
                  className="flex-1 min-w-0 py-2 px-2.5 bg-surface-2 border border-border rounded-lg text-[13px] text-foreground outline-none focus:border-brand placeholder:text-text-3"
                  type="text"
                  placeholder="192.168.1.100"
                  value={wifiAddress}
                  onChange={(e) => setWifiAddress(e.target.value)}
                  onKeyDown={(e) => e.key === "Enter" && handleWifiConnect()}
                  autoFocus
                />
                <Button
                  className="shrink-0"
                  onClick={handleWifiConnect}
                  disabled={wifiConnecting || !wifiAddress.trim()}
                >
                  {wifiConnecting ? <div className="size-3.5 border-2 border-border border-t-brand rounded-full animate-spin" /> : t("common.connect")}
                </Button>
              </div>
            </div>
          </DialogPrimitive.Popup>
        </DialogPortal>
      </Dialog>

      {/* The WeChat code, big enough for a phone to scan off the screen. It
          lives behind the footer link because on the page itself there is no
          room for a scannable square. */}
      <Dialog open={showContact} onOpenChange={setShowContact}>
        <DialogPortal>
          <DialogOverlay />
          <DialogPrimitive.Popup className="fixed top-1/2 left-1/2 -translate-x-1/2 -translate-y-1/2 w-[300px] max-w-[90vw] bg-surface border border-border rounded-[14px] z-50 shadow-lg animate-in fade-in zoom-in-95 duration-100">
            <div className="flex flex-col items-center gap-3 p-5">
              <DialogTitle className="text-[15px] font-semibold">{t("welcome.contact")}</DialogTitle>
              <img
                src={wechatQr}
                alt={t("welcome.contactWechatAlt")}
                width={240}
                height={306}
                className="rounded-lg border border-border bg-white"
              />
              <p className="text-xs text-text-3 text-center leading-relaxed whitespace-pre-line">
                {t("welcome.contactWechat")}
              </p>
            </div>
          </DialogPrimitive.Popup>
        </DialogPortal>
      </Dialog>
    </div>
  );
}
