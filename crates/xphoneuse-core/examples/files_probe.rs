//! Probe: the Android half of the file manager, without the app.
//!
//! Everything the window and the CLI do to an Android phone goes through
//! `xphoneuse_core::files` and three adb verbs. This runs them against the
//! first connected device so the shapes can be checked on real hardware - in
//! particular the listing, which is one `adb shell` script whose output is
//! parsed (`stat -c '%F|%s|%Y|%n'`), and the permission gate that tells an
//! unreadable directory apart from an empty one.
//!
//! Usage:
//!   cargo run -p xphoneuse-core --example files_probe -- list [PATH]
//!   cargo run -p xphoneuse-core --example files_probe -- push LOCAL [REMOTE_DIR]
//!   cargo run -p xphoneuse-core --example files_probe -- pull REMOTE [LOCAL_DIR]

use std::time::Instant;

use xphoneuse_core::files;
use xphoneuse_core::adb;

#[tokio::main]
async fn main() {
    let mut args = std::env::args().skip(1);
    let command = args.next().unwrap_or_else(|| "list".to_string());

    let serial = match adb::list_devices().await {
        Ok(devices) if !devices.is_empty() => devices[0].serial.clone(),
        Ok(_) => {
            eprintln!("[files] no Android device is connected");
            std::process::exit(1);
        }
        Err(e) => {
            eprintln!("[files] adb failed: {e}");
            std::process::exit(1);
        }
    };
    eprintln!("[files] device {serial}");

    match command.as_str() {
        "list" => {
            let path = args.next().unwrap_or_else(|| "/sdcard".to_string());
            let started = Instant::now();
            match files::list_dir(&serial, &path).await {
                Ok(directory) => {
                    eprintln!(
                        "[files] {} entries in {:?} ({} truncated, {} warnings)",
                        directory.entries.len(),
                        started.elapsed(),
                        directory.truncated,
                        directory.warnings.len()
                    );
                    for entry in directory.entries.iter().take(25) {
                        println!(
                            "{:>10}  {:<5}  {}  {}",
                            if entry.kind == "dir" {
                                "<dir>".to_string()
                            } else {
                                files::human_size(entry.size)
                            },
                            entry.kind,
                            entry
                                .modified
                                .map(files::format_time)
                                .unwrap_or_else(|| "-".to_string()),
                            entry.location
                        );
                    }
                }
                Err(e) => {
                    eprintln!("[files] list {path} failed: {e}");
                    std::process::exit(1);
                }
            }
        }
        "push" => {
            let (Some(local), Some(dir)) = (args.next(), args.next().or(Some("/sdcard/Download".to_string())))
            else {
                eprintln!("usage: files_probe push LOCAL [REMOTE_DIR]");
                std::process::exit(2);
            };
            let name = std::path::Path::new(&local)
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("file");
            let remote = match files::unique_remote_path(&serial, &dir, name).await {
                Ok(remote) => remote,
                Err(e) => {
                    eprintln!("[files] could not pick a remote name: {e}");
                    std::process::exit(1);
                }
            };
            let started = Instant::now();
            match adb::push_file(&serial, &local, &remote).await {
                Ok(()) => eprintln!("[files] pushed {local} -> {remote} in {:?}", started.elapsed()),
                Err(e) => {
                    eprintln!("[files] push failed: {e}");
                    std::process::exit(1);
                }
            }
            // Read it back to prove it landed: the size the device reports is
            // the only evidence that is not adb's own word.
            match files::stat(&serial, &remote).await {
                Ok(entry) => eprintln!(
                    "[files] device says {} is {} bytes, modified {}",
                    entry.location,
                    entry.size,
                    entry.modified.map(files::format_time).unwrap_or_else(|| "-".into())
                ),
                Err(e) => eprintln!("[files] stat after push failed: {e}"),
            }
        }
        "pull" => {
            let (Some(remote), Some(dir)) = (args.next(), args.next().or(Some(".".to_string())))
            else {
                eprintln!("usage: files_probe pull REMOTE [LOCAL_DIR]");
                std::process::exit(2);
            };
            let name = remote.rsplit('/').next().unwrap_or("file");
            let dir = std::path::PathBuf::from(&dir);
            let local = match files::unique_local_path(&dir, name) {
                Ok(local) => local,
                Err(e) => {
                    eprintln!("[files] could not pick a local name: {e}");
                    std::process::exit(1);
                }
            };
            let started = Instant::now();
            match adb::pull_file(&serial, &remote, &local.display().to_string()).await {
                Ok(()) => {
                    let size = std::fs::metadata(&local).map(|m| m.len()).unwrap_or(0);
                    eprintln!(
                        "[files] pulled {remote} ({}) -> {} in {:?}",
                        files::human_size(size),
                        local.display(),
                        started.elapsed()
                    );
                }
                Err(e) => {
                    eprintln!("[files] pull failed: {e}");
                    std::process::exit(1);
                }
            }
        }
        other => {
            eprintln!("unknown command {other:?}; try list, push, pull");
            std::process::exit(2);
        }
    }
}
