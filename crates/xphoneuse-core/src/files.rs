//! The phone's files, and the vocabulary for talking about where they are.
//!
//! Two very different backends sit behind one model:
//!
//! * **Android** - `adb shell` to list, `adb push`/`adb pull` to move bytes.
//!   The whole of adb's world is reachable, so the roots are `/sdcard` (what
//!   the user thinks of as the phone) and `/`.
//! * **iOS** - AFC, which is chrooted into `/var/mobile/Media` (see
//!   `xphoneuse_ios::files`). There is no `/` above it and no app containers
//!   beside it, so that namespace has exactly one root.
//!
//! A *location* is the opaque string both frontends (the file-manager window
//! and the CLI) pass around: `platform:/path`, e.g. `android:/sdcard/DCIM` or
//! `ios:/DCIM/100APPLE`. It is self-describing, so a location in a log line
//! says which phone's namespace it means - `/DCIM` exists on both and would
//! otherwise be ambiguous - and a mix-up is a clear error rather than a
//! silent wrong listing.
//!
//! Everything here that can be pure is pure and tested: the parsing, the
//! breadcrumbs, the name handling. The one impure thing is the Android
//! listing, which is a single `adb shell` away from the parser it feeds.

use std::path::{Path, PathBuf};

use serde::Serialize;

/// The platform half of a location.
pub const PLATFORM_ANDROID: &str = "android";
pub const PLATFORM_IOS: &str = "ios";

/// How many entries one listing may carry. A directory with more is reported
/// as truncated rather than refused: the answer is still useful, and
/// `truncated` says so instead of letting the UI pretend it saw everything.
///
/// The number is a guard rail, not a size limit - it is what a phone's DCIM
/// looks like after years, and on iOS the listing costs one round trip per
/// entry (see `xphoneuse_ios::files`).
pub const MAX_ENTRIES: usize = 2000;

/// One entry of a directory.
#[derive(Debug, Clone, Serialize)]
pub struct FsEntry {
    pub name: String,
    /// The location to hand back for a download, a drag or a descent.
    pub location: String,
    /// `"dir"`, `"file"`, `"link"` or `"other"`.
    pub kind: String,
    pub size: u64,
    /// Unix seconds; `None` when the device did not say.
    pub modified: Option<i64>,
}

/// One starting point, for the sidebar.
#[derive(Debug, Clone, Serialize)]
pub struct FsRoot {
    /// `"sdcard"`, `"system"` or `"media"` - the window localises by this, so
    /// a root reads "内部存储" in Chinese and "Internal storage" in English;
    /// `label` below is the English one the MCP text output uses.
    pub kind: String,
    pub label: String,
    pub location: String,
}

/// One step of the path bar.
#[derive(Debug, Clone, Serialize)]
pub struct FsCrumb {
    pub label: String,
    pub location: String,
    /// Set on the crumb that *is* a root (`"sdcard"` …), which is how the
    /// window knows to translate its label.
    pub kind: Option<String>,
}

/// A directory, its entries, and everything needed to draw the view around
/// them: the roots, the path bar and the way up. One call, one snapshot - the
/// window never stitches state together from separate requests, and a stale
/// breadcrumb cannot survive a listing.
#[derive(Debug, Clone, Serialize)]
pub struct FsListing {
    pub platform: String,
    /// What the device calls itself (a model name or serial), for the status line.
    pub device: String,
    pub location: String,
    pub path: String,
    /// `None` at a root: the way up is closed there, and the UI disables it.
    pub parent: Option<String>,
    pub crumbs: Vec<FsCrumb>,
    pub roots: Vec<FsRoot>,
    pub entries: Vec<FsEntry>,
    /// How many entries did not fit in [`MAX_ENTRIES`].
    pub truncated: usize,
    /// Non-fatal notes from the device (an entry that could not be described).
    pub warnings: Vec<String>,
}

/// What a backend hands back before the view around it is built.
pub struct FsDirectory {
    pub entries: Vec<FsEntry>,
    pub truncated: usize,
    pub warnings: Vec<String>,
}

/// Wrap a backend's directory in the view around it. Shared so the window and
/// the MCP server cannot disagree about what a listing contains.
pub fn build_listing(platform: &str, device: &str, path: &str, directory: FsDirectory) -> FsListing {
    FsListing {
        platform: platform.to_string(),
        device: device.to_string(),
        location: format_location(platform, path),
        path: path.to_string(),
        parent: parent_location(platform, path),
        crumbs: crumbs(platform, path),
        roots: roots(platform),
        entries: directory.entries,
        truncated: directory.truncated,
        warnings: directory.warnings,
    }
}

/// Split a location into its platform and path halves.
///
/// A prefix counts as a platform only when it is exactly `android` or `ios`,
/// which is what keeps a Windows path from a stray argument (`C:\Users\...`)
/// from parsing as platform `C`. Without a prefix the path belongs to
/// `default_platform` - the connected device, which is what someone typing
/// `ls /sdcard` by hand means.
pub fn parse_location(input: &str, default_platform: &str) -> Result<(String, String), String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err("a location is required".to_string());
    }
    let split = trimmed.find(':').filter(|colon| {
        let prefix = &trimmed[..*colon];
        prefix.eq_ignore_ascii_case(PLATFORM_ANDROID) || prefix.eq_ignore_ascii_case(PLATFORM_IOS)
    });
    let (platform, path) = match split {
        Some(colon) => (trimmed[..colon].to_ascii_lowercase(), &trimmed[colon + 1..]),
        None => (default_platform.to_string(), trimmed),
    };
    Ok((platform, normalize_path(path)?))
}

/// The inverse of [`parse_location`]'s split.
pub fn format_location(platform: &str, path: &str) -> String {
    format!("{platform}:{path}")
}

/// The platform a location names, if it names one - `Some("ios")` for
/// `ios:/DCIM`, `None` for a bare path. Callers that need to decide *which
/// phone* a request is about before they can parse a bare path use this.
pub fn platform_of(input: &str) -> Option<&'static str> {
    let trimmed = input.trim();
    let colon = trimmed.find(':')?;
    let prefix = &trimmed[..colon];
    if prefix.eq_ignore_ascii_case(PLATFORM_ANDROID) {
        Some(PLATFORM_ANDROID)
    } else if prefix.eq_ignore_ascii_case(PLATFORM_IOS) {
        Some(PLATFORM_IOS)
    } else {
        None
    }
}

/// Where a listing starts when the caller did not say: the place the user
/// thinks of as the phone's files.
pub fn default_path(platform: &str) -> &'static str {
    match platform {
        PLATFORM_IOS => "/",
        _ => "/sdcard",
    }
}

/// A timestamp the way a listing shows it, in this machine's timezone -
/// the one the user is looking at the phone from. Sizes are one thing and
/// "which of these photos is from today" is another.
pub fn format_time(unix: i64) -> String {
    use chrono::{Local, TimeZone};
    match Local.timestamp_opt(unix, 0).single() {
        Some(time) => time.format("%Y-%m-%d %H:%M").to_string(),
        None => unix.to_string(),
    }
}

/// The path half of a location string.
fn location_path(location: &str) -> &str {
    match location.find(':') {
        Some(colon) => &location[colon + 1..],
        None => location,
    }
}

/// Clean a device path and refuse the shapes that are not one.
///
/// Absolute only: a relative path would depend on the shell's or the service's
/// idea of a working directory, which is not something the user picked. `..`
/// is refused rather than resolved - AFC is chrooted so it could not escape
/// anyway, and a path that tries is a mistake worth reporting.
pub fn normalize_path(path: &str) -> Result<String, String> {
    let path = path.trim();
    if path.is_empty() {
        return Ok("/".to_string());
    }
    if !path.starts_with('/') {
        return Err(format!(
            "{path:?} is not a phone path: locations are absolute, like /sdcard or android:/sdcard"
        ));
    }
    let mut segments: Vec<&str> = Vec::new();
    for segment in path.split('/') {
        match segment {
            "" | "." => continue,
            ".." => return Err(format!("{path:?} is not a phone path: `..` is not allowed")),
            other => segments.push(other),
        }
    }
    if segments.is_empty() {
        return Ok("/".to_string());
    }
    Ok(format!("/{}", segments.join("/")))
}

/// The location one level up, or `None` when already at a root.
pub fn parent_location(platform: &str, path: &str) -> Option<String> {
    if path == "/" || roots(platform).iter().any(|root| location_path(&root.location) == path) {
        return None;
    }
    let parent = match path.rfind('/') {
        Some(0) | None => "/".to_string(),
        Some(index) => path[..index].to_string(),
    };
    Some(format_location(platform, &parent))
}

/// Where browsing can start on this platform.
pub fn roots(platform: &str) -> Vec<FsRoot> {
    match platform {
        PLATFORM_IOS => vec![FsRoot {
            kind: "media".to_string(),
            label: "Media".to_string(),
            location: format_location(PLATFORM_IOS, "/"),
        }],
        _ => vec![
            FsRoot {
                kind: "sdcard".to_string(),
                label: "Internal storage".to_string(),
                location: format_location(PLATFORM_ANDROID, "/sdcard"),
            },
            FsRoot {
                kind: "system".to_string(),
                label: "System".to_string(),
                location: format_location(PLATFORM_ANDROID, "/"),
            },
        ],
    }
}

/// The path bar: the longest root that contains `path`, then one crumb per
/// segment below it.
///
/// Longest-first matters - `/` contains everything, so without it every
/// Android path would start at "System" and `/sdcard` would never show as the
/// root it is.
pub fn crumbs(platform: &str, path: &str) -> Vec<FsCrumb> {
    let root = roots(platform)
        .into_iter()
        .filter(|root| {
            let root_path = location_path(&root.location);
            path == root_path || root_path == "/" || path.starts_with(&format!("{root_path}/"))
        })
        .max_by_key(|root| location_path(&root.location).len());

    let mut out = Vec::new();
    let mut current = String::new();
    if let Some(root) = root {
        current = location_path(&root.location).to_string();
        out.push(FsCrumb {
            label: root.label,
            location: root.location,
            kind: Some(root.kind),
        });
    }
    for segment in path[current.len().min(path.len())..]
        .split('/')
        .filter(|s| !s.is_empty())
    {
        current = format!("{}/{}", current.trim_end_matches('/'), segment);
        out.push(FsCrumb {
            label: segment.to_string(),
            location: format_location(platform, &current),
            kind: None,
        });
    }
    out
}

/// Directories first, then case-insensitive by name - the order every file
/// manager uses, and the one that keeps a list stable as it is re-read.
pub fn sort_entries(entries: &mut [FsEntry]) {
    entries.sort_by(|a, b| {
        let a_dir = a.kind == "dir";
        let b_dir = b.kind == "dir";
        b_dir
            .cmp(&a_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            .then_with(|| a.name.cmp(&b.name))
    });
}

/// The `kind` for a file type as the two backends spell it: toybox's
/// `stat -c %F` on Android, AFC's `st_ifmt` on iOS.
pub fn kind_of(file_type: &str) -> &'static str {
    match file_type {
        "directory" | "S_IFDIR" => "dir",
        "regular file" | "regular empty file" | "S_IFREG" => "file",
        "symbolic link" | "S_IFLNK" => "link",
        _ => "other",
    }
}

/// A human-readable size, for the CLI and the MCP's text output.
pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// A filename that is safe to create on this machine.
///
/// The phone's names are not this filesystem's names: a Windows host refuses
/// `\ / : * ? " < > |` and trailing dots or spaces, and an iOS photo is free
/// to be called `IMG:0001.HEIC`. Replacing the offenders keeps the download
/// from failing on a name the user cannot even see.
pub fn sanitize_host_name(name: &str) -> String {
    let mut out: String = name
        .chars()
        .map(|c| match c {
            '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|' | '\0' => '_',
            c if (c as u32) < 0x20 => '_',
            c => c,
        })
        .collect();
    while out.ends_with('.') || out.ends_with(' ') {
        out.pop();
    }
    if out.is_empty() || out == "." || out == ".." {
        out = "unnamed".to_string();
    }
    out
}

/// A path in `dir` that does not exist yet, never overwriting what is there.
///
/// The phone is the source of truth for a download's name, but the host
/// already having a file by that name is normal (the same photo pulled twice).
/// The suffix goes before the extension, `IMG_0001 (1).HEIC`, because that is
/// what Explorer and Finder do and the name then stays recognisable.
pub fn unique_local_path(dir: &Path, name: &str) -> Result<PathBuf, String> {
    let clean = sanitize_host_name(name);
    let first = dir.join(&clean);
    if !first.exists() {
        return Ok(first);
    }
    let (stem, extension) = match clean.rfind('.') {
        Some(index) if index > 0 => (&clean[..index], Some(&clean[index + 1..])),
        _ => (clean.as_str(), None),
    };
    for n in 1..1000u32 {
        let candidate = match extension {
            Some(ext) => dir.join(format!("{stem} ({n}).{ext}")),
            None => dir.join(format!("{stem} ({n})")),
        };
        if !candidate.exists() {
            return Ok(candidate);
        }
    }
    Err(format!(
        "{clean} already exists 1000 times over in {}",
        dir.display()
    ))
}

/// One argument for the device's shell: wrapped in single quotes, with any
/// single quote of its own closed and reopened.
///
/// Every path that reaches `adb shell` goes through this. The paths come from
/// a phone's filesystem and from an agent, so "it will not contain a quote" is
/// not an assumption worth making.
pub fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', r"'\''"))
}

/// One line of the Android listing, as produced by the `stat` loop in
/// [`list_dir`]: `%F|%s|%Y|%n`.
///
/// `splitn(4, '|')` on purpose: a `|` inside the *name* is legal on the device
/// and lands in the fourth field, where it cannot be mistaken for a separator.
pub fn parse_stat_line(line: &str, platform: &str) -> Option<FsEntry> {
    let line = line.trim_end_matches('\n').trim_end_matches('\r');
    if line.is_empty() {
        return None;
    }
    let mut parts = line.splitn(4, '|');
    let file_type = parts.next()?;
    let size = parts.next().and_then(|v| v.parse::<u64>().ok()).unwrap_or(0);
    let modified = parts
        .next()
        .and_then(|v| v.parse::<i64>().ok())
        .filter(|v| *v > 0);
    let path = parts.next().unwrap_or_default();
    let name = path.rsplit('/').next().unwrap_or(path).to_string();
    if name.is_empty() {
        return None;
    }
    // A failed stat comes back as `?|0|0|path`: the entry still exists (the
    // glob saw it), it just could not be described. Better an "other"-kind
    // row the user can still download or ignore than a file that silently
    // vanishes from the listing.
    let kind = if file_type == "?" {
        "other"
    } else {
        kind_of(file_type)
    };
    Some(FsEntry {
        name,
        location: format_location(platform, path),
        kind: kind.to_string(),
        size,
        modified,
    })
}

/// List a directory on an Android device.
///
/// One `adb shell` for the whole directory. The first command is the
/// permission gate: a directory the shell user cannot read makes the glob
/// below match nothing, which is indistinguishable from an empty directory -
/// so `ls -a` is asked first, and its own error ("Permission denied", "No such
/// file or directory") is what comes back.
pub async fn list_dir(serial: &str, path: &str) -> Result<FsDirectory, String> {
    let dir = shell_quote(path);
    let script = format!(
        "d={dir}; ls -a \"$d\" >/dev/null || exit 3; \
         for f in \"$d\"/* \"$d\"/.[!.]*; do [ -e \"$f\" ] || continue; \
         stat -L -c '%F|%s|%Y|%n' \"$f\" 2>/dev/null || echo \"?|0|0|$f\"; done"
    );
    let output = crate::adb::shell_output(serial, &script)
        .await
        .map_err(|e| e.to_string())?;

    let mut directory = FsDirectory {
        entries: Vec::new(),
        truncated: 0,
        warnings: Vec::new(),
    };
    for line in output.lines() {
        let Some(entry) = parse_stat_line(line, PLATFORM_ANDROID) else {
            continue;
        };
        if entry.kind == "other" {
            directory
                .warnings
                .push(format!("could not read the properties of {}", entry.name));
        }
        if directory.entries.len() >= MAX_ENTRIES {
            directory.truncated += 1;
            continue;
        }
        directory.entries.push(entry);
    }
    sort_entries(&mut directory.entries);
    Ok(directory)
}

/// What `path` is on an Android device.
pub async fn stat(serial: &str, path: &str) -> Result<FsEntry, String> {
    let script = format!("stat -L -c '%F|%s|%Y|%n' {} || exit 3", shell_quote(path));
    let output = crate::adb::shell_output(serial, &script)
        .await
        .map_err(|e| e.to_string())?;
    parse_stat_line(output.trim(), PLATFORM_ANDROID)
        .ok_or_else(|| format!("{path} could not be described"))
}

/// Whether a path exists on an Android device.
pub async fn exists(serial: &str, path: &str) -> Result<bool, String> {
    let script = format!("[ -e {} ] && echo yes || echo no", shell_quote(path));
    let output = crate::adb::shell_output(serial, &script)
        .await
        .map_err(|e| e.to_string())?;
    Ok(output.trim() == "yes")
}

/// A remote path in `dir` that does not exist yet, for an upload that must not
/// clobber what is there.
pub async fn unique_remote_path(serial: &str, dir: &str, name: &str) -> Result<String, String> {
    let dir = dir.trim_end_matches('/');
    let base = format!("{dir}/{name}");
    if !exists(serial, &base).await? {
        return Ok(base);
    }
    let (stem, extension) = match name.rfind('.') {
        Some(index) if index > 0 => (&name[..index], Some(&name[index + 1..])),
        _ => (name, None),
    };
    for n in 1..1000u32 {
        let candidate = match extension {
            Some(ext) => format!("{dir}/{stem} ({n}).{ext}"),
            None => format!("{dir}/{stem} ({n})"),
        };
        if !exists(serial, &candidate).await? {
            return Ok(candidate);
        }
    }
    Err(format!("{name} already exists 1000 times over in {dir}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The prefix is only a platform when it is one: a Windows path must not
    /// parse as platform `C`, and a bare path belongs to the connected device.
    #[test]
    fn a_location_is_a_platform_and_a_path() {
        assert_eq!(
            parse_location("android:/sdcard/DCIM", PLATFORM_IOS).unwrap(),
            ("android".to_string(), "/sdcard/DCIM".to_string())
        );
        assert_eq!(
            parse_location("IOS:/DCIM", PLATFORM_ANDROID).unwrap(),
            ("ios".to_string(), "/DCIM".to_string())
        );
        assert_eq!(
            parse_location("/sdcard/DCIM", PLATFORM_ANDROID).unwrap(),
            ("android".to_string(), "/sdcard/DCIM".to_string())
        );
        assert!(parse_location(r"C:\Users\me", PLATFORM_ANDROID).is_err());
        assert!(parse_location("sdcard/DCIM", PLATFORM_ANDROID).is_err());
        assert!(parse_location("/sdcard/../../etc", PLATFORM_ANDROID).is_err());
        assert!(parse_location("", PLATFORM_ANDROID).is_err());
    }

    /// Cleaning is quiet about the shapes it accepts - `//`, `.`, a trailing
    /// slash - and loud about `..`.
    #[test]
    fn paths_are_normalised_the_same_way_everywhere() {
        assert_eq!(normalize_path("/sdcard//DCIM/").unwrap(), "/sdcard/DCIM");
        assert_eq!(normalize_path("/./sdcard/.").unwrap(), "/sdcard");
        assert_eq!(normalize_path("/").unwrap(), "/");
        assert_eq!(normalize_path("").unwrap(), "/");
        assert!(normalize_path("/a/../b").is_err());
        assert!(normalize_path("relative").is_err());
    }

    /// The way up stops at a root: `/sdcard` is a top, not a step below `/`.
    #[test]
    fn the_way_up_closes_at_a_root() {
        assert_eq!(
            parent_location(PLATFORM_ANDROID, "/sdcard/DCIM/Camera").unwrap(),
            "android:/sdcard/DCIM"
        );
        assert_eq!(parent_location(PLATFORM_ANDROID, "/sdcard"), None);
        assert_eq!(parent_location(PLATFORM_ANDROID, "/"), None);
        assert_eq!(
            parent_location(PLATFORM_ANDROID, "/system/bin").unwrap(),
            "android:/system"
        );
        assert_eq!(parent_location(PLATFORM_ANDROID, "/system").unwrap(), "android:/");
        assert_eq!(parent_location(PLATFORM_IOS, "/"), None);
        assert_eq!(parent_location(PLATFORM_IOS, "/DCIM").unwrap(), "ios:/");
    }

    /// Longest root wins, or every Android breadcrumb would start at "System".
    #[test]
    fn crumbs_start_at_the_root_that_contains_the_path() {
        let labels = |platform: &str, path: &str| -> Vec<String> {
            crumbs(platform, path).iter().map(|c| c.label.clone()).collect()
        };

        assert_eq!(
            labels(PLATFORM_ANDROID, "/sdcard/DCIM/Camera"),
            ["Internal storage", "DCIM", "Camera"]
        );
        let trail = crumbs(PLATFORM_ANDROID, "/sdcard/DCIM/Camera");
        assert_eq!(trail[0].kind.as_deref(), Some("sdcard"));
        assert_eq!(trail[2].location, "android:/sdcard/DCIM/Camera");

        assert_eq!(labels(PLATFORM_ANDROID, "/system/bin"), ["System", "system", "bin"]);
        assert_eq!(
            crumbs(PLATFORM_ANDROID, "/system/bin")[0].kind.as_deref(),
            Some("system")
        );

        assert_eq!(labels(PLATFORM_IOS, "/DCIM/100APPLE"), ["Media", "DCIM", "100APPLE"]);
        assert_eq!(crumbs(PLATFORM_IOS, "/DCIM")[0].kind.as_deref(), Some("media"));

        // A root is a single crumb.
        assert_eq!(labels(PLATFORM_IOS, "/"), ["Media"]);
        assert_eq!(labels(PLATFORM_ANDROID, "/sdcard"), ["Internal storage"]);
    }

    /// The device's own listing format, including the shapes that break naive
    /// parsers: spaces and parentheses in names, a pipe in a name, a failed
    /// stat, and a CRLF line ending.
    #[test]
    fn the_android_listing_round_trips_through_its_format() {
        let entry = parse_stat_line(
            "regular file|396731|1779935235|/sdcard/DCIM/123.mp4",
            PLATFORM_ANDROID,
        )
        .unwrap();
        assert_eq!(entry.name, "123.mp4");
        assert_eq!(entry.kind, "file");
        assert_eq!(entry.size, 396731);
        assert_eq!(entry.modified, Some(1779935235));
        assert_eq!(entry.location, "android:/sdcard/DCIM/123.mp4");

        let entry = parse_stat_line(
            "directory|3444|1718785045|/sdcard/ 我的文件",
            PLATFORM_ANDROID,
        )
        .unwrap();
        assert_eq!(entry.name, " 我的文件");
        assert_eq!(entry.kind, "dir");

        let entry =
            parse_stat_line("regular file|10|1|/sdcard/a|b.txt", PLATFORM_ANDROID).unwrap();
        assert_eq!(entry.name, "a|b.txt");

        let entry = parse_stat_line("?|0|0|/sdcard/odd\r", PLATFORM_ANDROID).unwrap();
        assert_eq!(entry.kind, "other");
        assert_eq!(entry.modified, None);

        assert!(parse_stat_line("", PLATFORM_ANDROID).is_none());
    }

    /// A download never lands on top of a file that is already there, and the
    /// name it does get is still recognisable.
    #[test]
    fn a_download_never_overwrites_and_keeps_its_name() {
        let dir = std::env::temp_dir().join(format!("xphoneuse-files-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        let first = unique_local_path(&dir, "IMG_0001.HEIC").unwrap();
        assert!(first.ends_with("IMG_0001.HEIC"));
        std::fs::write(&first, b"one").unwrap();

        let second = unique_local_path(&dir, "IMG_0001.HEIC").unwrap();
        assert!(second.ends_with("IMG_0001 (1).HEIC"), "{second:?}");
        std::fs::write(&second, b"two").unwrap();

        let third = unique_local_path(&dir, "IMG_0001.HEIC").unwrap();
        assert!(third.ends_with("IMG_0001 (2).HEIC"), "{third:?}");

        // A name with no extension is still numbered.
        std::fs::write(dir.join("notes"), b"x").unwrap();
        assert!(unique_local_path(&dir, "notes")
            .unwrap()
            .ends_with("notes (1)"));

        // The phone's name may not even be legal here.
        let odd = unique_local_path(&dir, "IMG:0001?.HEIC").unwrap();
        assert!(odd.ends_with("IMG_0001_.HEIC"), "{odd:?}");

        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Every path that reaches the device's shell is quoted, and a quote in
    /// the name does not end the quoting.
    #[test]
    fn device_paths_are_quoted_for_the_shell() {
        assert_eq!(shell_quote("/sdcard/Download"), "'/sdcard/Download'");
        assert_eq!(shell_quote("/sdcard/my file.txt"), "'/sdcard/my file.txt'");
        assert_eq!(shell_quote("/sdcard/$(rm -rf /)"), "'/sdcard/$(rm -rf /)'");
        assert_eq!(shell_quote("/sdcard/it's.txt"), r"'/sdcard/it'\''s.txt'");
    }

    /// Sizes read the way a person reads them.
    #[test]
    fn sizes_are_human() {
        assert_eq!(human_size(0), "0 B");
        assert_eq!(human_size(999), "999 B");
        assert_eq!(human_size(1024), "1.0 KB");
        assert_eq!(human_size(396731), "387.4 KB");
        assert_eq!(human_size(238623275), "227.6 MB");
    }
}
