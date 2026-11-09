import { useState, useRef, useCallback, useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { MacroEvent, MacroTimedEvent, SavedMacro } from "../types";
import { t } from "../i18n";
import { MOD } from "../lib/utils";

export interface MacroInfo {
  name: string;
  event_count: number;
}

interface UseMacroOptions {
  showToast: (msg: string, type?: "error" | "info") => void;
  onRecordingStopped?: () => void;
}

export function useMacro({ showToast, onRecordingStopped }: UseMacroOptions) {
  const [macroRecording, setMacroRecording] = useState(false);
  const [playingMacro, setPlayingMacro] = useState<string | null>(null);
  /** The macro playing on a loop right now, if any. */
  const [loopingMacro, setLoopingMacro] = useState<string | null>(null);
  const [macros, setMacros] = useState<MacroInfo[]>([]);
  const [macrosDir, setMacrosDirState] = useState<string>("");

  const recordingRef = useRef(false);
  const eventsRef = useRef<MacroTimedEvent[]>([]);
  const startTimeRef = useRef(0);
  /**
   * The running loop's id, or null when none is running. An id rather than the
   * name: stopping and restarting the same macro would otherwise let the old
   * loop mistake the new one for itself and carry on beside it.
   */
  const loopRef = useRef<number | null>(null);
  const loopSeqRef = useRef(0);

  const refreshMacros = useCallback(async (dir?: string) => {
    const d = dir || macrosDir;
    if (!d) return;
    try {
      const list = await invoke<MacroInfo[]>("list_macro_files", { dir: d });
      setMacros(list);
    } catch {
      setMacros([]);
    }
  }, [macrosDir]);

  useEffect(() => {
    (async () => {
      const stored = localStorage.getItem("macros_dir");
      if (stored) {
        setMacrosDirState(stored);
        refreshMacros(stored);
      } else {
        try {
          const defaultDir = await invoke<string>("get_default_macros_dir");
          setMacrosDirState(defaultDir);
          localStorage.setItem("macros_dir", defaultDir);
          refreshMacros(defaultDir);
        } catch {}
      }
    })();
  }, []);

  const setMacrosDir = useCallback(
    (dir: string) => {
      setMacrosDirState(dir);
      localStorage.setItem("macros_dir", dir);
      refreshMacros(dir);
    },
    [refreshMacros],
  );

  const startRecording = useCallback(() => {
    eventsRef.current = [];
    startTimeRef.current = Date.now();
    recordingRef.current = true;
    setMacroRecording(true);
    showToast(t("toasts.macroRecordingStarted"), "info");
  }, [showToast]);

  const stopRecording = useCallback(
    async (name?: string) => {
      recordingRef.current = false;
      setMacroRecording(false);
      const macroName = name || `Macro ${macros.length + 1}`;
      const macro: SavedMacro = {
        name: macroName,
        events: eventsRef.current,
      };
      eventsRef.current = [];
      try {
        await invoke("save_macro_file", { dir: macrosDir, macroData: macro });
        showToast(t("toasts.macroSaved", { name: macroName, count: macro.events.length }), "info");
        refreshMacros();
        onRecordingStopped?.();
      } catch (e) {
        showToast(t("toasts.macroSaveFailed", { error: String(e) }));
      }
    },
    [macros.length, macrosDir, showToast, refreshMacros, onRecordingStopped],
  );

  const toggleRecording = useCallback(() => {
    if (recordingRef.current) {
      stopRecording();
    } else {
      startRecording();
    }
  }, [startRecording, stopRecording]);

  const recordEvent = useCallback((event: MacroEvent) => {
    if (!recordingRef.current) return;
    eventsRef.current.push({
      timestamp_ms: Date.now() - startTimeRef.current,
      event,
    });
  }, []);

  const playMacro = useCallback(
    async (name: string) => {
      setPlayingMacro(name);
      try {
        await invoke("wake_screen");
        const m = await invoke<SavedMacro>("load_macro_file", {
          dir: macrosDir,
          name,
        });
        await invoke("play_macro", { events: m.events });
        showToast(t("toasts.macroPlayed", { name }), "info");
      } catch (e) {
        showToast(t("toasts.macroPlayFailed", { error: String(e) }));
      } finally {
        setPlayingMacro(null);
      }
    },
    [macrosDir, showToast],
  );

  /**
   * Start playing `name` again and again until [`stopLoop`] is called.
   *
   * The rounds are this loop's, not Rust's, so a round can be cut short
   * (`stop_macro`) without losing the macro: the loop then sees the flag gone
   * and returns instead of asking for another round.
   */
  const startLoop = useCallback(
    async (name: string) => {
      if (loopRef.current !== null) return;
      const id = (loopSeqRef.current += 1);
      loopRef.current = id;
      setLoopingMacro(name);
      try {
        const m = await invoke<SavedMacro>("load_macro_file", {
          dir: macrosDir,
          name,
        });
        showToast(t("toasts.macroLoopStarted", { name, mod: MOD }), "info");
        while (loopRef.current === id) {
          // A sleeping screen would swallow the whole macro; the same wake the
          // single play does, once per round.
          await invoke("wake_screen");
          await invoke("play_macro", { events: m.events });
        }
      } catch (e) {
        // A round that failed must not loop forever on the same error.
        showToast(t("toasts.macroPlayFailed", { error: String(e) }));
      } finally {
        if (loopRef.current === id) {
          loopRef.current = null;
          setLoopingMacro(null);
        }
      }
    },
    [macrosDir, showToast],
  );

  /** End the loop and cut the round playing now short. Harmless when idle. */
  const stopLoop = useCallback(() => {
    const was = loopRef.current !== null;
    loopRef.current = null;
    setLoopingMacro(null);
    invoke("stop_macro").catch(() => {});
    if (was) showToast(t("toasts.macroLoopStopped"), "info");
  }, [showToast]);

  const deleteMacro = useCallback(
    async (name: string) => {
      try {
        await invoke("delete_macro_file", { dir: macrosDir, name });
        showToast(t("toasts.macroDeleted", { name }), "info");
        refreshMacros();
      } catch (e) {
        showToast(t("toasts.deleteFailed", { error: String(e) }));
      }
    },
    [macrosDir, showToast, refreshMacros],
  );

  const renameMacro = useCallback(
    async (oldName: string, newName: string) => {
      try {
        await invoke("rename_macro_file", {
          dir: macrosDir,
          oldName,
          newName,
        });
        refreshMacros();
      } catch (e) {
        showToast(t("toasts.renameFailed", { error: String(e) }));
      }
    },
    [macrosDir, showToast, refreshMacros],
  );

  const reorderMacros = useCallback(
    async (order: string[]) => {
      try {
        await invoke("save_macros_order", { dir: macrosDir, order });
        refreshMacros();
      } catch {}
    },
    [macrosDir, refreshMacros],
  );

  const exportMacro = useCallback(
    async (name: string) => {
      try {
        const m = await invoke<SavedMacro>("load_macro_file", {
          dir: macrosDir,
          name,
        });
        const blob = new Blob([JSON.stringify(m, null, 2)], {
          type: "application/json",
        });
        const url = URL.createObjectURL(blob);
        const link = document.createElement("a");
        link.href = url;
        link.download = `${name.replace(/\s+/g, "_")}.macro.json`;
        link.click();
        URL.revokeObjectURL(url);
        showToast(t("toasts.macroExported", { name }), "info");
      } catch (e) {
        showToast(t("toasts.exportFailed", { error: String(e) }));
      }
    },
    [macrosDir, showToast],
  );

  const exportAllMacros = useCallback(async () => {
    if (macros.length === 0) return;
    try {
      const all: SavedMacro[] = [];
      for (const info of macros) {
        const m = await invoke<SavedMacro>("load_macro_file", {
          dir: macrosDir,
          name: info.name,
        });
        all.push(m);
      }
      const blob = new Blob([JSON.stringify(all, null, 2)], {
        type: "application/json",
      });
      const url = URL.createObjectURL(blob);
      const link = document.createElement("a");
      link.href = url;
      link.download = `macros-${Date.now()}.json`;
      link.click();
      URL.revokeObjectURL(url);
      showToast(t("toasts.exportedCount", { count: macros.length }), "info");
    } catch (e) {
      showToast(t("toasts.exportFailed", { error: String(e) }));
    }
  }, [macros, macrosDir, showToast]);

  const importMacros = useCallback(() => {
    const input = document.createElement("input");
    input.type = "file";
    input.accept = ".json";
    input.onchange = async () => {
      const file = input.files?.[0];
      if (!file) return;
      try {
        const text = await file.text();
        const data = JSON.parse(text);
        const imported: SavedMacro[] = Array.isArray(data) ? data : [data];
        const valid = imported.filter(
          (m) => m.name && Array.isArray(m.events),
        );
        if (valid.length === 0) {
          showToast(t("toasts.noValidMacros"));
          return;
        }
        const existingNames = new Set(macros.map((m) => m.name));
        for (const m of valid) {
          const name = existingNames.has(m.name)
            ? `${m.name} (imported)`
            : m.name;
          await invoke("save_macro_file", {
            dir: macrosDir,
            macroData: { ...m, name },
          });
        }
        showToast(t("toasts.importedCount", { count: valid.length }), "info");
        refreshMacros();
      } catch {
        showToast(t("toasts.parseFailed"));
      }
    };
    input.click();
  }, [macros, macrosDir, showToast, refreshMacros]);

  return {
    macroRecording,
    playingMacro,
    loopingMacro,
    macros,
    macrosDir,
    setMacrosDir,
    startRecording,
    stopRecording,
    toggleRecording,
    recordEvent,
    playMacro,
    startLoop,
    stopLoop,
    deleteMacro,
    renameMacro,
    reorderMacros,
    exportMacro,
    exportAllMacros,
    importMacros,
    refreshMacros,
  };
}
