import { useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { useTranslation } from "react-i18next";
import { open as openDialog, save as saveDialog } from "@tauri-apps/plugin-dialog";
import { join } from "@tauri-apps/api/path";
import { revealItemInDir } from "@tauri-apps/plugin-opener";
import {
  ArrowDownTrayIcon,
  ArrowPathIcon,
  ArrowUpTrayIcon,
  ChevronUpIcon,
  DocumentIcon,
  FolderIcon,
  XMarkIcon,
} from "@heroicons/react/24/outline";
import { Button } from "@/components/ui/button";
import { ToastContainer } from "./ToastContainer";
import { useToasts } from "../hooks/useToasts";
import { resolveSaveDir } from "../lib/utils";

/**
 * What the backend says a directory contains. Field names are as they cross
 * the boundary (snake_case), which is the convention every other command in
 * this app follows - see FsListing in xphoneuse-core/src/files.rs.
 */
interface FsEntry {
  name: string;
  location: string;
  kind: string;
  size: number;
  modified: number | null;
}
interface FsRoot {
  kind: string;
  label: string;
  location: string;
}
interface FsCrumb {
  label: string;
  location: string;
  kind: string | null;
}
interface FsListing {
  platform: string;
  device: string;
  location: string;
  path: string;
  parent: string | null;
  crumbs: FsCrumb[];
  roots: FsRoot[];
  entries: FsEntry[];
  truncated: number;
  warnings: string[];
}

/** A transfer, as the `fm-progress` events report it (src-tauri/src/files.rs). */
interface Transfer {
  id: string;
  direction: string;
  name: string;
  transferred: number;
  total: number;
  state: string;
  local_path?: string | null;
  error?: string | null;
}

/** What a finished transfer gives back. */
interface FmTransfer {
  name: string;
  location: string;
  local_path: string | null;
  size: number;
}

function humanSize(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  const units = ["KB", "MB", "GB"];
  let value = bytes / 1024;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit += 1;
  }
  return `${value.toFixed(1)} ${units[unit]}`;
}

function formatTime(unix: number | null): string {
  if (!unix) return "-";
  const d = new Date(unix * 1000);
  const pad = (n: number) => `${n}`.padStart(2, "0");
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())}`;
}

/**
 * The file manager window (a third OS window - see `open_file_manager_window`
 * in src-tauri/src/lib.rs and src/main.tsx, which mounts this).
 *
 * The two directions of file movement are not symmetrical:
 *
 * * **Files dragged in from the desktop** are a drop, which tauri hands over
 *   as ordinary events carrying the files' real paths - straight to an upload.
 * * **Files dragged out** have to exist on this computer first. The OS drag
 *   needs a real path and has to start while the mouse button is down, so a
 *   file that is only on the phone is downloaded before it can be dragged.
 *   That is what `localPaths` is for: it remembers which of the phone's files
 *   have a copy here (in this session's save directory), and only those rows
 *   are draggable. It is the cloud-storage gesture - fetch, then drag.
 */
export function FileManagerWindow() {
  const { t } = useTranslation();
  const { toasts, showToast } = useToasts();
  const [listing, setListing] = useState<FsListing | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(true);
  const [dropActive, setDropActive] = useState(false);
  const [transfers, setTransfers] = useState<Transfer[]>([]);
  // A phone location -> the copy of it on this computer, for this session.
  const [localPaths, setLocalPaths] = useState<Record<string, string>>({});
  // Which rows are picked out. The toolbar's buttons act on these; a row's own
  // button acts on that row alone.
  const [selected, setSelected] = useState<string[]>([]);
  // Locations being fetched because someone tried to drag them out.
  const fetching = useRef(new Set<string>());
  // True while a drag we started is in flight. A drag that ends over our own
  // window - the user let go inside it - comes back to us as a drop *onto*
  // this window, and the drop handler would take it for an upload of the very
  // file that was being dragged out. Nothing good comes of acting on it.
  const draggingOut = useRef(false);
  // The press that may or may not become a drag. A click selects a row and
  // must stay a click: nothing is fetched and no drag starts until the
  // pointer has actually moved (see the mousemove listener below).
  const press = useRef<{ entry: FsEntry; x: number; y: number; acted: boolean } | null>(null);

  // The listeners below are registered once, but they need the listing as it
  // is now (a drop lands in whatever directory is on screen).
  const listingRef = useRef<FsListing | null>(null);
  listingRef.current = listing;

  const open = useCallback(async (location?: string) => {
    setBusy(true);
    setError(null);
    try {
      const next = await invoke<FsListing>("fm_list", { location: location ?? null });
      setListing(next);
      setSelected([]);
    } catch (e) {
      setError(String(e));
      setListing(null);
    } finally {
      setBusy(false);
    }
  }, []);

  // The first listing. No location: the backend picks the connected phone's
  // root, which is also how the window learns which phone it is looking at.
  useEffect(() => {
    void open();
  }, [open]);

  // Transfers report themselves as they go. The window's own id comes back in
  // the event, which is what lets a row find its transfer.
  useEffect(() => {
    const unlisten = listen<Transfer>("fm-progress", (event) => {
      const update = event.payload;
      setTransfers((rows) => {
        const index = rows.findIndex((row) => row.id === update.id);
        if (index === -1) {
          if (update.state !== "running") return rows; // nothing to show for work that is over
          return [...rows, update];
        }
        const next = [...rows];
        next[index] = update;
        return next;
      });
      if (update.state === "done" && update.local_path) {
        // From here the row can be dragged out.
        const entry = listingRef.current?.entries.find((item) => item.name === update.name);
        if (entry) {
          setLocalPaths((paths) => ({ ...paths, [entry.location]: update.local_path! }));
        }
      }
    });
    return () => {
      void unlisten.then((fn) => fn());
    };
  }, []);

  // Files dropped from the desktop are an upload into the directory on screen.
  useEffect(() => {
    const unlisten = getCurrentWebview().onDragDropEvent((event) => {
      if (draggingOut.current) {
        setDropActive(false);
        return;
      }
      if (event.payload.type === "enter" || event.payload.type === "over") {
        setDropActive(true);
        return;
      }
      if (event.payload.type === "leave") {
        setDropActive(false);
        return;
      }
      setDropActive(false);
      const current = listingRef.current;
      if (!current) return;
      void upload(event.payload.paths, current.location);
    });
    return () => {
      void unlisten.then((fn) => fn());
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const upload = useCallback(
    async (paths: string[], directory: string) => {
      let uploaded = 0;
      for (const [index, path] of paths.entries()) {
        const id = `up-${Date.now()}-${index}`;
        try {
          await invoke("fm_upload", { directory, hostPath: path, id });
          uploaded += 1;
        } catch (e) {
          showToast(String(e), "error");
        }
      }
      if (uploaded > 0) {
        showToast(t("files.uploadedCount", { n: uploaded }), "info");
        void open(directory);
      }
    },
    [open, showToast, t],
  );

  /**
   * Fetch one file; returns where it landed, or null when it did not.
   *
   * `dest_path` is what the save dialog answered - the file goes exactly
   * there. Without it the copy lands in the save directory under a numbered
   * name, which is the drag prefetch: a gesture must not stop to ask where.
   */
  const download = useCallback(
    async (
      entry: FsEntry,
      dest?: { path?: string; dir?: string; quiet?: boolean },
    ): Promise<string | null> => {
      const id = `dl-${Date.now()}-${Math.random().toString(36).slice(2, 7)}`;
      try {
        const result = await invoke<FmTransfer>("fm_download", {
          location: entry.location,
          id,
          destPath: dest?.path ?? null,
          destDir: dest?.dir ?? null,
        });
        if (!result.local_path) return null;
        setLocalPaths((paths) => ({ ...paths, [entry.location]: result.local_path! }));
        if (!dest?.quiet) showToast(t("files.saved", { path: result.local_path }), "info");
        return result.local_path;
      } catch (e) {
        // A cancelled transfer is not a failure worth a red toast; the row
        // shows it.
        if (!String(e).toLowerCase().includes("cancel")) showToast(String(e), "error");
        return null;
      }
    },
    [showToast, t],
  );

  /** Ask where the file should go, starting in the save directory. */
  const askWhereToSave = useCallback(
    async (entry: FsEntry): Promise<string | null> => {
      const dir = await resolveSaveDir();
      const defaultPath = dir ? await join(dir, entry.name).catch(() => entry.name) : entry.name;
      return (await saveDialog({ defaultPath, title: t("files.saveAs") })) ?? null;
    },
    [t],
  );

  /** The download button and the row's own arrow: every one asks first. */
  const downloadAsking = useCallback(
    async (entry: FsEntry) => {
      const path = await askWhereToSave(entry);
      if (!path) return;
      await download(entry, { path });
    },
    [askWhereToSave, download],
  );

  /** The toolbar's download button: everything picked out, one file at a time. */
  const downloadSelected = useCallback(async () => {
    const entries = (listingRef.current?.entries ?? []).filter(
      (entry) => selected.includes(entry.location) && entry.kind !== "dir",
    );
    if (entries.length === 0) {
      showToast(t("files.selectFiles"), "info");
      return;
    }
    if (entries.length === 1) {
      await downloadAsking(entries[0]);
      return;
    }
    // Several files: one destination for all of them, so ask for the folder
    // rather than interrupting a save dialog per file.
    const dir = await openDialog({ directory: true, title: t("files.saveInto") });
    if (!dir || Array.isArray(dir)) return;
    const landed: string[] = [];
    for (const entry of entries) {
      const path = await download(entry, { dir, quiet: true });
      if (path) landed.push(path);
    }
    if (landed.length > 0) {
      showToast(t("files.savedMany", { n: landed.length, dir }), "info");
    }
  }, [download, downloadAsking, selected, showToast, t]);

  /**
   * Dragging a file that is not on this computer yet: fetch it, then hand the
   * gesture to the OS if the hand is still on the button - one drag, not two.
   *
   * The OS drag needs a real file, which is why this cannot simply start the
   * drag and copy in the background. The button check is the backend's (it
   * asks the OS; see `left_button_down`): by the time the fetch lands the
   * user may have let go, and a drag started then would drop the file
   * wherever the cursor happened to be.
   */
  const prepareDrag = useCallback(
    async (entry: FsEntry) => {
      if (fetching.current.has(entry.location)) return;
      fetching.current.add(entry.location);
      showToast(t("files.preparingDrag", { name: entry.name }), "info");
      const path = await download(entry, { quiet: true });
      fetching.current.delete(entry.location);
      if (!path) return;
      if (press.current?.entry.location === entry.location) {
        draggingOut.current = true;
        try {
          const dragged = await invoke<boolean>("fm_start_drag", { localPath: path });
          if (!dragged) showToast(t("files.readyToDrag", { name: entry.name }), "info");
        } catch (e) {
          showToast(String(e), "error");
        } finally {
          window.setTimeout(() => {
            draggingOut.current = false;
          }, 800);
        }
      } else {
        showToast(t("files.readyToDrag", { name: entry.name }), "info");
      }
    },
    [download, showToast, t],
  );

  const toggleSelect = (entry: FsEntry, additive: boolean) => {
    setSelected((current) =>
      additive
        ? current.includes(entry.location)
          ? current.filter((location) => location !== entry.location)
          : [...current, entry.location]
        : [entry.location],
    );
  };

  useEffect(() => {
    getCurrentWindow()
      .setTitle(t("files.title"))
      .catch(() => {});
  }, [t]);

  /** A row, clicked: directories open, local files are revealed, the rest is fetched. */
  const entryActivate = (entry: FsEntry) => {
    if (entry.kind === "dir") {
      void open(entry.location);
      return;
    }
    const path = localPaths[entry.location];
    if (path) {
      void revealItemInDir(path);
      return;
    }
    void downloadAsking(entry);
  };

  /**
   * A drag the user actually made: hand an existing local copy to the OS, or
   * fetch one first (the OS drag needs a real file - see the module comment).
   */
  const beginDrag = useCallback(
    (entry: FsEntry) => {
      const path = localPaths[entry.location];
      if (!path) {
        void prepareDrag(entry);
        return;
      }
      draggingOut.current = true;
      void invoke<boolean>("fm_start_drag", { localPath: path })
        .catch((e) => showToast(String(e), "error"))
        .finally(() => {
          // The drop event (when the user let go over this window) may still
          // be on its way; give it a moment before listening again.
          window.setTimeout(() => {
            draggingOut.current = false;
          }, 800);
        });
    },
    [localPaths, prepareDrag, showToast],
  );

  // The listener below is registered once, so it reaches these through refs.
  const beginDragRef = useRef(beginDrag);
  beginDragRef.current = beginDrag;

  useEffect(() => {
    const onMove = (event: MouseEvent) => {
      const pending = press.current;
      if (!pending || pending.acted) return;
      // A few pixels of slack: a click with a shaky hand is still a click.
      if (Math.hypot(event.clientX - pending.x, event.clientY - pending.y) < 5) return;
      pending.acted = true;
      beginDragRef.current(pending.entry);
    };
    const onUp = () => {
      press.current = null;
    };
    window.addEventListener("mousemove", onMove);
    window.addEventListener("mouseup", onUp);
    return () => {
      window.removeEventListener("mousemove", onMove);
      window.removeEventListener("mouseup", onUp);
    };
  }, []);

  const pickAndUpload = async () => {
    if (!listing) return;
    const picked = await openDialog({ multiple: true });
    if (!picked) return;
    const paths = Array.isArray(picked) ? picked : [picked];
    await upload(paths as string[], listing.location);
  };

  const rootLabel = (root: { kind: string }) => {
    switch (root.kind) {
      case "sdcard":
        return t("files.rootSdcard");
      case "system":
        return t("files.rootSystem");
      default:
        return t("files.rootMedia");
    }
  };

  return (
    <div className="h-screen flex flex-col bg-background text-foreground">
      {/* Which phone, where in it, and the two ways of putting files in. */}
      <div className="h-[44px] shrink-0 flex items-center gap-2 px-3 border-b border-border/60 bg-surface/90">
        <Button
          size="sm"
          variant="ghost"
          className="size-7 min-h-0 shrink-0 gap-0 rounded-md border-transparent p-0 shadow-none text-text-2 hover:text-foreground"
          title={t("files.up")}
          disabled={!listing?.parent}
          onClick={() => listing?.parent && void open(listing.parent)}
        >
          <ChevronUpIcon className="size-4" />
        </Button>
        <Button
          size="sm"
          variant="ghost"
          className="size-7 min-h-0 shrink-0 gap-0 rounded-md border-transparent p-0 shadow-none text-text-2 hover:text-foreground"
          title={t("files.refresh")}
          onClick={() => void open(listing?.location)}
        >
          <ArrowPathIcon className="size-4" />
        </Button>

        <div className="flex-1 min-w-0 flex items-center gap-1 text-[12px] overflow-hidden">
          {listing?.crumbs.map((crumb, index) => (
            <span key={crumb.location} className="flex items-center gap-1 min-w-0">
              {index > 0 && <span className="text-text-3">/</span>}
              <button
                className="truncate px-1 py-0.5 rounded hover:bg-surface-3 hover:text-foreground text-text-2"
                onClick={() => void open(crumb.location)}
              >
                {crumb.kind ? rootLabel({ kind: crumb.kind }) : crumb.label}
              </button>
            </span>
          ))}
        </div>

        <Button
          size="sm"
          variant="ghost"
          className="h-7 shrink-0 gap-1.5 rounded-md px-2 text-[11px] text-text-2 hover:text-foreground"
          title={t("files.uploadHint")}
          disabled={!listing}
          onClick={() => void pickAndUpload()}
        >
          <ArrowUpTrayIcon className="size-4" />
          {t("files.upload")}
        </Button>
        <Button
          size="sm"
          variant="ghost"
          className="h-7 shrink-0 gap-1.5 rounded-md px-2 text-[11px] text-text-2 hover:text-foreground"
          title={t("files.downloadHint")}
          disabled={!listing || selected.length === 0}
          onClick={() => void downloadSelected()}
        >
          <ArrowDownTrayIcon className="size-4" />
          {t("files.download")}
        </Button>
      </div>

      {/* Roots, for when the path bar is a long way from the top. */}
      {listing && listing.roots.length > 1 && (
        <div className="shrink-0 flex items-center gap-2 px-3 py-1.5 border-b border-border/60 text-[11px]">
          {listing.roots.map((root) => (
            <button
              key={root.location}
              className={`px-2 py-0.5 rounded-full border ${
                listing.location === root.location ||
                listing.location.startsWith(`${root.location}/`)
                  ? "border-primary/60 text-foreground bg-surface-3"
                  : "border-border text-text-3 hover:text-foreground"
              }`}
              onClick={() => void open(root.location)}
            >
              {rootLabel(root)}
            </button>
          ))}
          <span className="ml-auto text-text-3 truncate">{listing.device}</span>
        </div>
      )}

      {/* On iOS this window looks at the media area, which the phone's own
          apps do not read: files come out of it fine, but a file put in is
          invisible to Photos, Files and everything built on them. Saying so
          here is the difference between a limitation and a puzzle. */}
      {listing?.platform === "ios" && (
        <div className="shrink-0 px-3 py-1.5 border-b border-border/60 text-[11px] text-text-3">
          {t("files.iosMediaNote")}
        </div>
      )}

      <div className="flex-1 min-h-0 overflow-y-auto">
        {busy && !listing && <div className="p-6 text-[12px] text-text-3">{t("files.loading")}</div>}
        {error && (
          <div className="p-6 flex flex-col items-start gap-3">
            <div className="text-[12px] text-error whitespace-pre-wrap">{error}</div>
            <Button size="sm" variant="outline" className="text-[11px]" onClick={() => void open()}>
              {t("files.retry")}
            </Button>
          </div>
        )}
        {listing && listing.entries.length === 0 && !busy && (
          <div className="p-6 text-[12px] text-text-3">{t("files.empty")}</div>
        )}
        {listing && listing.entries.length > 0 && (
          <table
            className="w-full text-[12px] border-collapse select-none"
            // The browser's own drag - a selected-text drag - would compete
            // with the OS one the row's mousedown starts, and on Windows it
            // wins the gesture. Nothing here is meant to be selected or
            // dragged as text.
            onDragStart={(event) => event.preventDefault()}
            draggable={false}
          >
            <tbody>
              {listing.entries.map((entry) => {
                return (
                  <tr
                    key={entry.location}
                    className={`border-b border-border/40 ${
                      selected.includes(entry.location)
                        ? "bg-primary/10"
                        : "hover:bg-surface-2/60"
                    } ${entry.kind === "dir" ? "cursor-default" : "cursor-grab"}`}
                    // One click picks the row out (the toolbar works on the
                    // selection). A drag begins further along, once the pointer
                    // has moved - which is also when the OS drag has to start,
                    // so the press is remembered here and acted on there.
                    onMouseDown={(event) => {
                      if (event.button !== 0) return;
                      toggleSelect(entry, event.ctrlKey || event.metaKey);
                      press.current =
                        entry.kind === "dir"
                          ? null
                          : { entry, x: event.clientX, y: event.clientY, acted: false };
                    }}
                    onDoubleClick={() => entryActivate(entry)}
                  >
                    <td className="w-[24px] pl-3 py-1.5 align-middle">
                      {entry.kind === "dir" ? (
                        <FolderIcon className="size-4 text-text-3" />
                      ) : (
                        <DocumentIcon className="size-4 text-text-3" />
                      )}
                    </td>
                    <td className="py-1.5 pr-3 align-middle">
                      <span className="truncate max-w-full text-left" title={entry.name}>
                        {entry.name}
                      </span>
                    </td>
                    <td className="w-[90px] py-1.5 pr-3 text-right text-text-3 align-middle">
                      {entry.kind === "dir" ? "-" : humanSize(entry.size)}
                    </td>
                    <td className="w-[130px] py-1.5 pr-3 text-text-3 align-middle">
                      {formatTime(entry.modified)}
                    </td>
                    <td className="w-[70px] pr-3 py-1.5 text-right align-middle">
                      {entry.kind !== "dir" && (
                        <button
                          className="text-text-3 hover:text-foreground"
                          title={t("files.download")}
                          onMouseDown={(event) => event.stopPropagation()}
                          onClick={() => void downloadAsking(entry)}
                        >
                          <ArrowDownTrayIcon className="size-4" />
                        </button>
                      )}
                    </td>
                  </tr>
                );
              })}
            </tbody>
          </table>
        )}
        {listing && listing.truncated > 0 && (
          <div className="px-3 py-2 text-[11px] text-text-3">
            {t("files.truncated", { n: listing.truncated })}
          </div>
        )}
      </div>

      {/* Transfers: one line each, with a cancel for the ones still running. */}
      {transfers.length > 0 && (
        <div className="shrink-0 max-h-[120px] overflow-y-auto border-t border-border/60">
          {transfers.map((row) => (
            <div key={row.id} className="px-3 py-1.5 text-[11px] flex items-center gap-2">
              <span className="w-[52px] shrink-0 text-text-3">
                {row.direction === "download" ? t("files.download") : t("files.upload")}
              </span>
              <span className="truncate flex-1">{row.name}</span>
              <span className="text-text-3 shrink-0">
                {row.state === "running"
                  ? `${humanSize(row.transferred)}${row.total ? ` / ${humanSize(row.total)}` : ""}`
                  : row.state === "done"
                    ? t("files.done")
                    : row.state === "cancelled"
                      ? t("files.cancelled")
                      : (row.error ?? t("files.failed"))}
              </span>
              {row.state === "running" && (
                <button
                  className="text-text-3 hover:text-foreground shrink-0"
                  title={t("files.cancel")}
                  onClick={() => void invoke("fm_cancel", { id: row.id })}
                >
                  <XMarkIcon className="size-3.5" />
                </button>
              )}
            </div>
          ))}
        </div>
      )}

      {/* The whole window is the drop target while a drag is over it. */}
      {dropActive && (
        <div className="fixed inset-0 z-50 bg-background/80 border-2 border-dashed border-primary flex items-center justify-center pointer-events-none">
          <div className="text-[13px] text-foreground">
            {t("files.dropHere", { dir: listing?.path ?? "" })}
          </div>
        </div>
      )}

      <ToastContainer toasts={toasts} />
    </div>
  );
}
