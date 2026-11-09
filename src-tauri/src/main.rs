// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

// The aptabase plugin spawns its flush loop from the setup hook, which runs
// on the main thread: without a tokio context entered there it panics with
// "there is no reactor running". An async main provides one.
#[tokio::main]
async fn main() {
    // The AppImage's AppRun forces GDK_BACKEND=x11: linuxdeploy's gtk plugin
    // sets it to dodge a crash it hit on the Wayland backend
    // (tauri-apps/tauri#8541). On this build that trade goes the wrong way -
    // under X11, connecting a device aborts the process in an XCB assertion
    // ("multi-threaded client and XInitThreads has not been called"), while
    // the session's own Wayland backend connects fine. So inside an AppImage,
    // undo the hook's value and let GDK pick the backend for the session it is
    // actually running in. Only that exact value is undone: a GDK_BACKEND the
    // user set themselves is left alone, and a machine with no Wayland still
    // ends up on X11, as before.
    #[cfg(target_os = "linux")]
    if std::env::var_os("APPDIR").is_some() && std::env::var("GDK_BACKEND").as_deref() == Ok("x11") {
        std::env::remove_var("GDK_BACKEND");
    }

    tauri_app_lib::run()
}
