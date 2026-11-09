mod audio;
mod commands;
mod copilot;
mod files;
mod ios;
mod location;
mod state;
mod video;

use state::AppState;
use tauri_plugin_aptabase::EventTracker;
use tauri::{
    Manager, RunEvent,
    menu::{Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    WebviewUrl, WebviewWindowBuilder,
};

/// Label of the settings window. The frontend branches on the window label in
/// src/main.tsx: this label renders the settings panel instead of the mirror.
const SETTINGS_WINDOW_LABEL: &str = "settings";

/// Label of the file manager window, branched on in src/main.tsx the same way.
/// Its progress events are addressed to this label too (see src/files.rs).
pub(crate) const FILES_WINDOW_LABEL: &str = "file-manager";

/// Aptabase (self-hosted) analytics, supplied by the build environment
/// (`XPHONEUSE_APTABASE_KEY` and `XPHONEUSE_APTABASE_HOST`). A build without
/// them simply sends nothing - the plugin disables itself on an empty key -
/// so a fork cannot report into the project's own instance and the key stays
/// out of the source. A-SH-* keys are instance-specific, which is why the host
/// is passed explicitly. Events are sent in the background and never block the
/// app, and tracking is deliberately coarse - no device names, serials or any
/// other identifier ever goes in an event.
const APTABASE_KEY: &str = match option_env!("XPHONEUSE_APTABASE_KEY") {
    Some(key) => key,
    None => "",
};
const APTABASE_HOST: &str = match option_env!("XPHONEUSE_APTABASE_HOST") {
    Some(host) => host,
    None => "",
};

/// Browser args for every webview in the process.
///
/// Two things make this a single, shared constant:
///   - wry's defaults (`--disable-features=msWebOOUI,msPdfOOUI,
///     msSmartScreenProtection`) are REPLACED, not extended, so they are
///     repeated here.
///   - WebView2 fixes the environment options once per process/user-data
///     folder, and a webview built with DIFFERENT options than the main one
///     fails with HRESULT 0x8007139F (invalid state). The window config in
///     tauri.conf.json must carry the exact same string.
/// See `additionalBrowserArgs` in src-tauri/tauri.conf.json.
const BROWSER_ARGS: &str =
    "--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection,msWebView2EnableTrackingPrevention";

/// Opens the settings window, or brings it forward when it is already open.
///
/// Settings used to be a bottom sheet inside the mirror window, which is tall
/// and narrow: pulling the sheet up covered most of the phone, so the settings
/// could not be read next to the thing they change. A second OS window can be
/// moved aside, minimised or sent to another display, and it keeps its own
/// scroll position while the mirror keeps running.
///
/// The window is created with BROWSER_ARGS like every other webview in the
/// process: WebView2 fixes the environment options once per user-data folder,
/// and a webview built with different ones fails with 0x8007139F (see the
/// constant). The title is a placeholder — the frontend localises it.
///
/// This has to be an async command. On Windows, building a webview from a
/// *synchronous* command deadlocks: the command runs on the event-loop thread
/// and the webview waits for it (wry#583). The window then exists but never
/// loads anything — a blank rectangle, which is exactly what it looked like
/// the first time. An async command runs off that thread.
#[tauri::command]
async fn open_settings_window(app: tauri::AppHandle) -> Result<(), String> {
    if let Some(win) = app.get_webview_window(SETTINGS_WINDOW_LABEL) {
        let _ = win.show();
        let _ = win.unminimize();
        let _ = win.set_focus();
        return Ok(());
    }
    WebviewWindowBuilder::new(
        &app,
        SETTINGS_WINDOW_LABEL,
        WebviewUrl::App("index.html".into()),
    )
    .title("XPhoneUse")
    // The panel is a stack of label/control rows, laid out for the 380px-wide
    // mirror window it used to be a sheet in: wider than this only stretches
    // every row. It is resizable for anyone who wants the extra width.
    .inner_size(460.0, 800.0)
    .min_inner_size(380.0, 420.0)
    .center()
    .additional_browser_args(BROWSER_ARGS)
    .build()
    .map_err(|e| e.to_string())?;
    Ok(())
}

/// Opens the file manager window, or brings it forward when it is already
/// open.
///
/// A window rather than a panel for the same reason settings are: it is a
/// two-dimensional thing - a path bar, a list, and a drop target for files
/// dragged in from the desktop - and none of that fits a 380px-wide mirror.
/// It is created exactly like the settings window and for exactly the same
/// reasons (async command, BROWSER_ARGS; see `open_settings_window` above).
#[tauri::command]
async fn open_file_manager_window(app: tauri::AppHandle) -> Result<(), String> {
    if let Some(win) = app.get_webview_window(FILES_WINDOW_LABEL) {
        let _ = win.show();
        let _ = win.unminimize();
        let _ = win.set_focus();
        return Ok(());
    }
    WebviewWindowBuilder::new(
        &app,
        FILES_WINDOW_LABEL,
        WebviewUrl::App("index.html".into()),
    )
    .title("XPhoneUse")
    // Wide enough for a name, a size and a date on one line, which is what a
    // listing is; the minimum keeps the columns from collapsing.
    .inner_size(760.0, 560.0)
    .min_inner_size(520.0, 320.0)
    .center()
    .additional_browser_args(BROWSER_ARGS)
    .build()
    .map_err(|e| e.to_string())?;
    Ok(())
}

/// Bring the main window back up (tray click / "show" menu item).
fn show_main_window(app: &tauri::AppHandle) {
    #[cfg(target_os = "macos")]
    let _ = app.show(); // unhide the app if it was hidden (Cmd+H)
    if let Some(win) = app.get_webview_window("main") {
        let _ = win.show();
        let _ = win.unminimize();
        let _ = win.set_focus();
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
/// Where a packaged build keeps its log. Tauri's data directory, named after
/// the bundle identifier; computed here because the redirect runs before the
/// app (and its path resolver) exists.
pub(crate) fn log_dir() -> std::path::PathBuf {
    #[cfg(windows)]
    {
        std::env::var_os("APPDATA")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(std::env::temp_dir)
    }
    #[cfg(target_os = "macos")]
    {
        std::env::var_os("HOME")
            .map(|home| std::path::PathBuf::from(home).join("Library/Application Support"))
            .unwrap_or_else(std::env::temp_dir)
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        std::env::var_os("XDG_DATA_HOME")
            .map(std::path::PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME").map(|home| std::path::PathBuf::from(home).join(".local/share"))
            })
            .unwrap_or_else(std::env::temp_dir)
    }
    .join("com.aovik.xphoneuse")
}

/// The switch's persistence: present = write the log to a file. Off by
/// default - a troubleshooting tool, not something every customer machine
/// should keep writing.
pub(crate) fn logging_marker() -> std::path::PathBuf {
    log_dir().join("logging-enabled")
}

/// A packaged build is a Windows GUI process (or a `.app`/AppImage): it has
/// no console, so everything the app writes to stderr - every `[tunnel]`,
/// `[ddi]`, `[ddi-cryptex]` line - goes nowhere. Point stderr at
/// `xphoneuse.log` in the data directory so a report from a customer's
/// machine can actually be read.
///
/// Development builds keep the terminal: `XPHONEUSE_LOG_STDERR=1` forces that
/// even for a packaged build.
fn redirect_stderr_to_log() {
    if cfg!(debug_assertions)
        || std::env::var_os("XPHONEUSE_LOG_STDERR").is_some()
        || !logging_marker().is_file()
    {
        return;
    }
    let dir = log_dir();
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let path = dir.join("xphoneuse.log");
    // A log from months ago is not worth the space; a recent one is the whole
    // point, so append until it gets big.
    if std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0) > 4 * 1024 * 1024 {
        let _ = std::fs::remove_file(&path);
    }
    let Ok(file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    else {
        return;
    };

    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        use windows::Win32::Foundation::HANDLE;
        use windows::Win32::System::Console::{SetStdHandle, STD_ERROR_HANDLE};
        let handle = HANDLE(file.as_raw_handle() as _);
        // Rust reads the stderr handle on first write; this runs before the
        // runtime can write anything, so the file is what it finds.
        unsafe {
            let _ = SetStdHandle(STD_ERROR_HANDLE, handle);
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::io::AsRawFd;
        unsafe {
            let _ = libc::dup2(file.as_raw_fd(), libc::STDERR_FILENO);
        }
    }
    // The handle must outlive the redirect.
    std::mem::forget(file);
}

pub fn run() {
    redirect_stderr_to_log();
    tauri::Builder::default()
        // One instance only, and it has to be this plugin: it is the first to
        // register, so a second launch is turned away before it can take the
        // MCP port, start its own scrcpy or spawn a second ad webview. The
        // callback runs *in the instance that already owns the app*, which is
        // where bringing the window up has to happen.
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            show_main_window(app);
        }))
        // Start on login. Registered here and driven from Settings (on by
        // default - see the first-run step in `setup`), so the app is already
        // running when a phone is plugged in.
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        // The frontend relaunches into the new version after an update installs.
        .plugin(tauri_plugin_process::init())
        .plugin(
            tauri_plugin_aptabase::Builder::new(APTABASE_KEY)
                .with_options(tauri_plugin_aptabase::InitOptions {
                    // A-SH-* keys live on our own instance, not aptabase.com.
                    host: Some(APTABASE_HOST.into()),
                    flush_interval: None, // 60s release / 2s debug
                })
                .build(),
        )
        .manage(AppState::new())
        .setup(|app| {
            let _ = app.track_event("app_started", None);

            // Start on login, on by default. The first run registers the app
            // and leaves a marker; after that the setting is the user's, so
            // turning it off sticks. A failure here is not a reason to refuse
            // to start (macOS can decline it by policy) - it is logged, the
            // marker is still written so the app does not retry on every
            // launch, and the toggle in Settings reports the real state.
            //
            // Never from a development build. Those load the frontend from the
            // dev server (`tauri::is_dev`), so an entry registered by
            // `cargo build` + `cargo run` starts at login with nothing to load
            // and shows "localhost refused to connect" - which looks exactly
            // like the app being broken. A packaged build (tauri build) has
            // the frontend embedded and is the only one worth starting at
            // login.
            // Where the "this iPhone may still hold a screen session" set
            // lives across restarts (see `tunnel::cleanup_media_session`).
            if let Ok(dir) = app.path().app_data_dir() {
                let _ = std::fs::create_dir_all(&dir);
                xphoneuse_ios::tunnel::set_state_dir(dir);
            }
            if !tauri::is_dev() {
                use tauri_plugin_autostart::ManagerExt;
                if let Ok(dir) = app.path().app_data_dir() {
                    let marker = dir.join("autostart-initialized");
                    if !marker.exists() {
                        match app.autolaunch().enable() {
                            Ok(()) => eprintln!("[autostart] registered on first run"),
                            Err(e) => eprintln!("[autostart] first-run registration failed: {e}"),
                        }
                        let _ = std::fs::create_dir_all(&dir);
                        let _ = std::fs::write(&marker, b"1");
                    }
                }
            }

            if let Ok(dir) = app.path().resource_dir() {
                xphoneuse_core::adb::set_resource_dir(dir.clone());
                xphoneuse_ios::uxplay::set_resource_dir(dir.clone());
                // The OCR models ship in resources/ocr; the crate joins the
                // rest of the path itself.
                xphoneuse_ocr::set_resource_dir(dir);
            }

            // Developer images are downloaded once per iOS build and shared by
            // every device, so they live in the app's data directory rather
            // than next to a session. The progress sink is what turns the work
            // into the UI's line, whichever path asked for the image (a tunnel
            // start, a screenshot that came up short).
            xphoneuse_ios::developer_image::use_cache_dir(commands::developer_images_dir(app.handle()));
            xphoneuse_ios::developer_image::set_progress_sink(commands::image_progress_sink(
                app.handle().clone(),
            ));

            // Hide the macOS traffic-light zoom (fullscreen) button: the
            // window is a phone-shaped mirror, fullscreen makes no sense.
            #[cfg(target_os = "macos")]
            if let Some(win) = app.get_webview_window("main") {
                use objc2::runtime::AnyObject;
                if let Ok(ns) = win.ns_window() {
                    let ns = ns as *mut AnyObject;
                    unsafe {
                        // standardWindowButton: NSWindowZoomButton (2)
                        let button: *mut AnyObject =
                            objc2::msg_send![&*ns, standardWindowButton: 2i64];
                        if !button.is_null() {
                            let _: () = objc2::msg_send![&*button, setHidden: true];
                        }
                    }
                }
            }

            // The settings and file-manager windows are satellites of the
            // mirror: with the main window gone they have nothing left to
            // configure or to browse, and on Windows the process would stay
            // alive with only those windows on screen.
            #[cfg(desktop)]
            if let Some(main) = app.get_webview_window("main") {
                let handle = app.handle().clone();
                main.on_window_event(move |event| {
                    if matches!(event, tauri::WindowEvent::CloseRequested { .. }) {
                        for label in [SETTINGS_WINDOW_LABEL, FILES_WINDOW_LABEL] {
                            if let Some(window) = handle.get_webview_window(label) {
                                let _ = window.close();
                            }
                        }
                    }
                });
            }

            let show =
                MenuItem::with_id(app, "show", "显示主界面", true, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&show, &quit])?;

            TrayIconBuilder::with_id("main-tray")
                .icon(app.default_window_icon().expect("app icon").clone())
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "show" => show_main_window(app),
                    "quit" => app.exit(0), // ExitRequested cleanup still runs
                    _ => {}
                })
                .on_tray_icon_event(|tray, event| {
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } = event
                    {
                        show_main_window(tray.app_handle());
                    }
                })
                .build(app)?;

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            open_settings_window,
            open_file_manager_window,
            files::fm_list,
            files::fm_download,
            files::fm_upload,
            files::fm_cancel,
            files::fm_start_drag,
            commands::open_homepage,
            commands::list_devices,
            commands::adb_status,
            commands::open_platform_tools,
            commands::open_x_account,
            commands::connect_device,
            commands::connect_ios_device,
            commands::enable_ios_developer_mode,
            commands::reveal_ios_developer_mode,
            commands::set_ios_transport_modes,
            commands::disconnect_device,
            commands::file_logging_enabled,
            commands::set_file_logging,
            commands::file_log_path,
            commands::send_touch,
            commands::send_key,
            commands::send_text,
            commands::send_scroll,
            commands::set_agent_input,
            commands::clear_agent_input,
            commands::get_agent_input_configured,
            commands::get_unicode_text_default,
            commands::set_unicode_text_default,
            commands::autostart_enabled,
            location::location_status,
            location::location_set,
            location::location_clear,
            location::open_map_link,
            commands::set_autostart,
            commands::take_screenshot,
            commands::press_button,
            commands::rotate_device,
            commands::update_screen_size,
            commands::set_muted,
            commands::wake_screen,
            commands::play_macro,
            commands::stop_macro,
            commands::get_default_macros_dir,
            commands::list_macro_files,
            commands::load_macro_file,
            commands::save_macro_file,
            commands::delete_macro_file,
            commands::rename_macro_file,
            commands::save_macros_order,
            commands::save_file,
            commands::default_save_dir,
            commands::wifi_connect,
            commands::wifi_disconnect,
            commands::wifi_enable,
            commands::get_device_ip,
            commands::start_mcp_server,
            commands::stop_mcp_server,
            commands::get_mcp_status,
            commands::mcp_frame_reply,
            commands::cleanup_ios_media_session,
            copilot::copilot_transcript,
            copilot::copilot_analyze,
            copilot::copilot_draft,
            copilot::copilot_reset,
            copilot::copilot_test_route,
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|app, event| {
            if let RunEvent::ExitRequested { .. } = &event {
                let state = app.state::<AppState>();
                let session = state.session.clone();
                let ios_session = state.ios_session.clone();
                let mcp = state.mcp.clone();
                // Blocking here would be "block_on within a runtime" (main is
                // async now), so the cleanup runs as a spawned task and the
                // exit waits for it on a plain channel.
                let (tx, rx) = std::sync::mpsc::channel::<()>();
                tauri::async_runtime::spawn(async move {
                    if let Some(s) = session.lock().await.take() {
                        s.shutdown.notify_one();
                        xphoneuse_core::scrcpy::stop_server(&s.device_serial, 27183).await;
                    }
                    if let Some(mut s) = ios_session.lock().await.take() {
                        s.shutdown.notify_one();
                        if let Some(mut uxplay) = s.uxplay.take() {
                            let _ = tokio::task::spawn_blocking(move || uxplay.stop()).await;
                        }
                    }
                    if let Some(ct) = mcp.lock().await.cancel.take() {
                        ct.cancel();
                    }
                    xphoneuse_core::adb::kill_server().await;
                    let _ = tx.send(());
                });
                let _ = rx.recv_timeout(std::time::Duration::from_secs(10));
            }
            if let RunEvent::Exit = &event {
                // Last chance to ship what is still queued: the process is
                // about to end and the flush is asynchronous otherwise.
                let _ = app.track_event("app_exited", None);
                app.flush_events_blocking();
            }
        });
}
