# "XPhoneUse" fork of UxPlay

Base commit: `f897541` (rewrite of adjust_yt_condensed_playlist), UxPlay 1.74,
from https://github.com/FDH2/UxPlay.

## What changed

- **GStreamer removed entirely.** No video/audio renderers, no HLS/playbin,
  no cover art, no local window. Deleted `renderers/`; trimmed `uxplay.cpp`
  to a headless mirror server.
- **Video delivered through a C ABI** (`uxplay_api.h`): decrypted Annex-B
  NAL data with stream timestamps via `on_video_data`, plus flush / size /
  codec / connect / disconnect / log callbacks. The buffer from
  `video_renderer_render_buffer` (AU-grouped NALs + ntp_time) is forwarded
  directly instead of being pushed into an appsrc.
- **`on_connected` carries the client's source IP** (the host app uses it to
  auto-configure the AgentInput text-input keyboard at that address). This
  required a new `client_ip` parameter on `raop_callbacks.register_client`;
  `raop_handlers.h` fills it from `conn->remote` via
  `utils_ipaddress_to_string` (same helper the SETUP handler already uses).
  The callback now fires on **every** admitted SETUP: upstream only called
  it during a fresh pair-setup (when the session has a client PK), so
  already-paired clients reconnecting via pair-verify were never reported.
  The PK argument may now be NULL — `register_client` skips the optional
  registration list in that case.
- **Built as a shared library** (`uxplay.dll` on Windows; the same sources
  still build `uxplay.exe` as a debug CLI). Exports exactly three symbols:
  `uxplay_start`, `uxplay_stop`, `uxplay_is_running`.
- `main()` is now a thin CLI wrapper around `uxplay_start()`; the server
  runs on a background glib thread owned by the DLL session.
- AirPlay discovery uses the built-in mDNS responder (`lib/mdnsd`);
  Bonjour optional via `-DUSE_DNS_SD=1` (unchanged from upstream 1.74).
- **mDNS interface selection patched** (`lib/mdnsd/mdnsd.c`): upstream picked
  the advertisement address with the default-route trick, which breaks when
  a proxy/VPN TUN adapter (e.g. Clash Meta's 198.18.0.1) owns the default
  route — all AirPlay announcements then go into the tunnel and the iPhone
  can't discover the receiver. The fork enumerates interfaces
  (`GetAdaptersAddresses` on Windows, `getifaddrs` elsewhere), skips
  loopback/APIPA/198.18-19, and prefers a private LAN address, falling back
  to the old default-route behavior only when nothing else exists.
- Callbacks carry a `user_data` pointer (set by the host) to avoid global
  state in the embedding application.
- Removed: `-vrtp`, `-vdmp` kept (debug dump), audio options, D-Bus, X11,
  HLS options, cover art, mux recording.

## Runtime dependencies (Windows)

`uxplay.dll` + these MinGW runtime DLLs (copy all from
`C:\msys64\ucrt64\bin` next to the app):

```
libgcc_s_seh-1.dll  libglib-2.0-0.dll  libwinpthread-1.dll  libstdc++-6.dll
libcrypto-3-x64.dll  libplist-2.0.dll
libintl-8.dll  libiconv-2.dll  libpcre2-8-0.dll  libffi-8.dll  zlib1.dll
```

No GStreamer, no Bonjour runtime.

## macOS

Build: `scripts/build-uxplay-mac.sh` (Homebrew: cmake ninja glib libplist
openssl@3). Produces `src-tauri/resources/uxplay.dylib` — CMake's
`OUTPUT_NAME uxplay` + `PREFIX ""` names it `uxplay.dylib`, which is what
`uxplay_ffi.rs` looks for on macOS. libplist and openssl are linked
statically; the only dynamic deps are Homebrew's glib + libintl (gettext),
referenced by absolute install path — fine for dev on a machine with the
same Homebrew prefix, but a release .app bundle needs an `install_name_tool`
fixup (or a static glib) before distribution. Service discovery uses the
Apple dns_sd (system mDNSResponder); the fork's mdnsd interface patch is
not compiled in on macOS.

**macOS stop-fix**: `gmainloop` was originally assigned only under
`#ifdef _WIN32`, so on any other platform `uxplay_stop()` never quit the
loop and `g_thread_join` hung forever. It is now set unconditionally in
`main_loop()`.

## Rebuilding (MSYS2 UCRT64)

```sh
# in an MSYS2 UCRT64 shell, from the repo root:
/ucrt64/bin/bash scripts/build-uxplay.sh
```

The script runs cmake + ninja in `build/`, then copies `uxplay.dll` and the
dependency DLLs into `src-tauri/resources/` for bundling and dev use.

## Re-syncing upstream

The upstream tree moved on (`git log` since f897541). To re-sync: clone
upstream, re-apply the changes above by hand (the diff is large: the fork
removes ~1/3 of uxplay.cpp and rewrites the entry point). This file lists
everything that changed so the port can be redone against a new base.
