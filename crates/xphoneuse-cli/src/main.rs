//! `xphoneuse` - CLI for controlling Android and iOS devices through the
//! XPhoneUse MCP server (streamable HTTP). Every command maps to one MCP
//! tool call; `compare` and screenshot comparison run entirely locally so
//! agents can verify screen state without spending tokens on image data.

use anyhow::{bail, Context, Result};
use base64::Engine;
use clap::{Args, Parser, Subcommand};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Parser)]
#[command(
    name = "xphoneuse",
    about = "Control Android/iOS devices through the XPhoneUse MCP server",
    disable_help_subcommand = true
)]
struct Cli {
    /// MCP endpoint of the XPhoneUse app
    #[arg(long, env = "XPHONEUSE_URL", default_value = "http://127.0.0.1:7070/mcp")]
    url: String,
    /// Request timeout in seconds (screenshots can be slow on iOS)
    #[arg(long, default_value_t = 120)]
    timeout_secs: u64,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// List connected devices; lines are prefixed android:/ios:
    List,
    /// Connect to a device and start a control session
    Connect {
        /// Device serial (Android) or UDID (iOS)
        serial: String,
        /// Device platform: android or ios
        #[arg(short, long, default_value = "android")]
        platform: String,
    },
    /// Disconnect from the current device
    Disconnect,
    /// Capture the device screen to a file (a scaled JPEG by default; --compare
    /// captures the lossless PNG instead)
    Screenshot {
        /// Output file (default ./xphoneuse-screenshot.jpg, overwritten)
        #[arg(short, long)]
        output: Option<PathBuf>,
        /// Also compare the capture with this reference PNG and print a
        /// CHANGED/UNCHANGED verdict (avoids re-reading the image)
        #[arg(long)]
        compare: Option<PathBuf>,
        /// Changed-pixel percentage above which the screen counts as CHANGED
        #[arg(long, default_value_t = 0.5)]
        threshold: f64,
        /// Per-channel difference tolerated when comparing pixels
        #[arg(long, default_value_t = 16)]
        tolerance: u8,
    },
    /// Read text off the screen with offline OCR (no image tokens spent)
    Ocr {
        /// Read this image file instead of the device screen. The path is on
        /// the machine running the app (this one, unless --url points elsewhere)
        #[arg(long)]
        path: Option<PathBuf>,
        /// Print {width, height, lines:[{text, confidence, bbox}]} instead of plain text
        #[arg(long)]
        json: bool,
        /// Drop lines recognized with less confidence than this (0-1)
        #[arg(long, default_value_t = 0.5)]
        min_confidence: f32,
    },
    /// Press a hardware button: home, back, recents, power, volume_up, volume_down
    Press { button: String },
    /// Type text into the focused field
    Text {
        text: String,
        /// Press Enter after typing
        #[arg(long)]
        enter: bool,
    },
    /// Type text choosing how non-ASCII is delivered on iOS: clipboard (device
    /// pasteboard + Cmd+V) or agentinput (AgentInput keyboard)
    UnicodeText {
        text: String,
        /// clipboard (default) or agentinput; ASCII is typed either way
        #[arg(short, long, default_value = "clipboard")]
        method: String,
        /// Press Enter after typing
        #[arg(long)]
        enter: bool,
    },
    /// Tap at normalized coordinates (0.0-1.0) - single down+up, no long-press risk
    Tap { x: f64, y: f64 },
    /// Send a touch event at normalized coordinates (0.0-1.0)
    Touch {
        /// down, up or move
        action: String,
        x: f64,
        y: f64,
    },
    /// Scroll at a position by a delta (normalized)
    Scroll {
        #[arg(allow_negative_numbers = true)]
        x: f64,
        #[arg(allow_negative_numbers = true)]
        y: f64,
        #[arg(allow_negative_numbers = true)]
        dx: f64,
        #[arg(allow_negative_numbers = true)]
        dy: f64,
    },
    /// Swipe between two points (normalized coordinates)
    Swipe {
        #[arg(allow_negative_numbers = true)]
        from_x: f64,
        #[arg(allow_negative_numbers = true)]
        from_y: f64,
        #[arg(allow_negative_numbers = true)]
        to_x: f64,
        #[arg(allow_negative_numbers = true)]
        to_y: f64,
        #[arg(long)]
        duration_ms: Option<u64>,
    },
    /// Run an adb shell command (Android only); all trailing args are joined
    Shell {
        #[arg(trailing_var_arg = true, num_args = 1..)]
        command: Vec<String>,
    },
    /// Open a URL (Android only)
    OpenUrl {
        url: String,
        /// Browser package (default com.android.chrome)
        #[arg(long)]
        browser: Option<String>,
        /// Use the system default URL handler
        #[arg(long)]
        system: bool,
    },
    /// Open an app by identifier: package name on Android (com.tencent.mm),
    /// bundle id on iOS (com.tencent.xin) - see APP-IDS.md in the skill
    Launch { package: String },
    /// List installed apps with their identifiers - filter it, a full list is
    /// hundreds of lines
    Apps(AppsArgs),
    /// Get the UI accessibility tree (Android only)
    UiTree,
    /// Search UI elements on screen (Android only)
    Find(FindArgs),
    /// Rotate the device screen (Android only)
    Rotate,
    /// Record, play and manage macros (Android and iOS)
    Macro {
        #[command(subcommand)]
        action: MacroAction,
    },
    /// WiFi debugging helpers (Android only)
    Wifi {
        #[command(subcommand)]
        action: WifiAction,
    },
    /// Compare two PNG files locally; no device or MCP call involved
    Compare {
        a: PathBuf,
        b: PathBuf,
        /// Changed-pixel percentage above which images count as CHANGED
        #[arg(long, default_value_t = 0.5)]
        threshold: f64,
        /// Per-channel difference tolerated when comparing pixels
        #[arg(long, default_value_t = 16)]
        tolerance: u8,
    },
}

#[derive(Args)]
struct AppsArgs {
    /// Case-insensitive substring of the identifier, or of the app's name on iOS
    #[arg(long, short)]
    filter: Option<String>,
    /// user (default), system, or all
    #[arg(long, short, default_value = "user")]
    kind: String,
}

#[derive(Args)]
struct FindArgs {
    /// Match by text content
    #[arg(long)]
    text: Option<String>,
    /// Match by content description
    #[arg(long = "desc")]
    content_desc: Option<String>,
    /// Match by resource ID
    #[arg(long = "id")]
    resource_id: Option<String>,
    /// Match by class name (Button, EditText, TextView, ...)
    #[arg(long)]
    class: Option<String>,
    /// Only return clickable elements
    #[arg(long)]
    clickable: bool,
}

#[derive(Subcommand)]
enum MacroAction {
    /// Start recording actions into a named macro
    Record { name: String },
    /// Stop recording and save the macro
    Stop,
    /// Play a recorded macro
    Play {
        name: String,
        #[arg(long)]
        repeat: Option<u32>,
    },
    /// List recorded macros
    List,
    /// Delete a macro
    Delete { name: String },
}

#[derive(Subcommand)]
enum WifiAction {
    /// Enable WiFi debugging on a USB-connected device
    Enable { serial: String },
    /// Connect to a device by ip:port
    Connect { address: String },
    /// Disconnect a WiFi device by ip:port
    Disconnect { address: String },
    /// Get the WiFi IP of a USB-connected device
    Ip { serial: String },
}

/// One MCP tool response: text blocks plus embedded images, each with the
/// mime type it arrived as - the screenshot is JPEG or PNG depending on the
/// format the caller asked the tool for.
struct ToolOutput {
    text: Vec<String>,
    images: Vec<(Vec<u8>, String)>,
    is_error: bool,
}

impl ToolOutput {
    fn print_text(&self) {
        for t in &self.text {
            println!("{t}");
        }
    }
}

/// Minimal MCP streamable-HTTP client: initialize once per invocation, then
/// a single tools/call. The session is discarded when the process exits.
struct Mcp {
    url: String,
    client: reqwest::Client,
    session: Option<String>,
}

impl Mcp {
    async fn open(url: &str, timeout: Duration) -> Result<Self> {
        let client = reqwest::Client::builder()
            .timeout(timeout)
            // The endpoint is always local; never route through a system proxy.
            .no_proxy()
            .build()
            .context("failed to build HTTP client")?;
        let mut mcp = Mcp {
            url: url.to_string(),
            client,
            session: None,
        };
        let body = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": {"name": "xphoneuse-cli", "version": env!("CARGO_PKG_VERSION")}
            }
        });
        let resp = mcp.post(&body).await?;
        if let Some(sid) = resp
            .headers()
            .get("mcp-session-id")
            .and_then(|v| v.to_str().ok())
        {
            mcp.session = Some(sid.to_string());
        }
        // Drain the initialize result; the handshake notification is fire-and-forget.
        let _ = mcp.parse_body(resp).await?;
        mcp.notify("notifications/initialized").await?;
        Ok(mcp)
    }

    async fn post(&self, body: &Value) -> Result<reqwest::Response> {
        let mut req = self
            .client
            .post(&self.url)
            .header("Accept", "application/json, text/event-stream")
            .header("Content-Type", "application/json")
            .json(body);
        if let Some(sid) = &self.session {
            req = req.header("Mcp-Session-Id", sid);
        }
        let resp = req.send().await.with_context(|| format!("request to {} failed - is the XPhoneUse app running with the MCP server enabled?", self.url))?;
        if !resp.status().is_success() {
            bail!("HTTP {} from MCP server", resp.status());
        }
        Ok(resp)
    }

    /// The server answers with either a plain JSON object or an SSE stream;
    /// normalize both to a single JSON value.
    async fn parse_body(&self, resp: reqwest::Response) -> Result<Value> {
        let content_type = resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_string();
        let bytes = resp.bytes().await?;
        if content_type.contains("text/event-stream") {
            for line in String::from_utf8_lossy(&bytes).lines() {
                if let Some(data) = line.strip_prefix("data:") {
                    let data = data.trim();
                    if data.is_empty() {
                        continue;
                    }
                    if let Ok(v) = serde_json::from_str::<Value>(data) {
                        // Both successful results and JSON-RPC errors count;
                        // the call() layer turns the latter into a bail!().
                        if v.get("result").is_some() || v.get("error").is_some() {
                            return Ok(v);
                        }
                    }
                }
            }
            bail!("MCP server closed the stream without a response");
        }
        serde_json::from_slice(&bytes).context("failed to parse MCP response")
    }

    async fn notify(&self, method: &str) -> Result<()> {
        let body = json!({"jsonrpc": "2.0", "method": method});
        self.post(&body).await?.bytes().await?;
        Ok(())
    }

    async fn call(&self, name: &str, arguments: Value) -> Result<ToolOutput> {
        let body = json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {"name": name, "arguments": arguments}
        });
        let resp = self.post(&body).await?;
        let v = self.parse_body(resp).await?;
        if let Some(err) = v.get("error") {
            bail!("MCP error: {}", err.get("message").and_then(Value::as_str).unwrap_or("unknown"));
        }
        let result = v
            .get("result")
            .cloned()
            .context("MCP response has no result")?;
        let is_error = result.get("isError").and_then(Value::as_bool).unwrap_or(false);
        let mut text = Vec::new();
        let mut images = Vec::new();
        for item in result
            .get("content")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or_default()
        {
            match item.get("type").and_then(Value::as_str) {
                Some("text") => {
                    if let Some(t) = item.get("text").and_then(Value::as_str) {
                        text.push(t.to_string());
                    }
                }
                Some("image") => {
                    if let Some(data) = item.get("data").and_then(Value::as_str) {
                        match base64::engine::general_purpose::STANDARD.decode(data) {
                            Ok(bytes) => images.push((
                                bytes,
                                item.get("mimeType")
                                    .and_then(Value::as_str)
                                    .unwrap_or("image/png")
                                    .to_string(),
                            )),
                            Err(e) => text.push(format!("Error: undecodable image data: {e}")),
                        }
                    }
                }
                _ => {}
            }
        }
        Ok(ToolOutput { text, images, is_error })
    }
}

struct DiffStats {
    changed_pct: f64,
    mean_diff: f64,
    /// Bounding box of changed pixels (x0, y0, x1, y1), inclusive.
    bbox: Option<(u32, u32, u32, u32)>,
    dims_a: (u32, u32),
    dims_b: (u32, u32),
}

fn compare_images(a_path: &Path, b_path: &Path, tolerance: u8) -> Result<DiffStats> {
    let a = image::open(a_path)
        .with_context(|| format!("failed to open {}", a_path.display()))?
        .to_rgba8();
    let b = image::open(b_path)
        .with_context(|| format!("failed to open {}", b_path.display()))?
        .to_rgba8();
    Ok(compare_buffers(a, b, tolerance))
}

fn compare_buffers(a: image::RgbaImage, b: image::RgbaImage, tolerance: u8) -> DiffStats {
    let (wa, ha) = a.dimensions();
    let (wb, hb) = b.dimensions();
    if (wa, ha) != (wb, hb) {
        return DiffStats {
            changed_pct: 100.0,
            mean_diff: 255.0,
            bbox: None,
            dims_a: (wa, ha),
            dims_b: (wb, hb),
        };
    }
    let tol = tolerance as i32;
    let total = (wa as usize * ha as usize).max(1);
    let mut changed = 0usize;
    let mut diff_sum = 0u64;
    let mut bbox: Option<(u32, u32, u32, u32)> = None;
    for (y, (ra, rb)) in a.rows().zip(b.rows()).enumerate() {
        for (x, (pa, pb)) in ra.zip(rb).enumerate() {
            let d = pa.0.iter().zip(&pb.0).map(|(c1, c2)| (i32::from(*c1) - i32::from(*c2)).abs());
            let mut max_d = 0i32;
            let mut sum_d = 0i32;
            for dv in d {
                max_d = max_d.max(dv);
                sum_d += dv;
            }
            diff_sum += sum_d as u64;
            if max_d > tol {
                changed += 1;
                let x = x as u32;
                let y = y as u32;
                bbox = Some(match bbox {
                    None => (x, y, x, y),
                    Some((x0, y0, x1, y1)) => (x0.min(x), y0.min(y), x1.max(x), y1.max(y)),
                });
            }
        }
    }
    DiffStats {
        changed_pct: 100.0 * changed as f64 / total as f64,
        mean_diff: diff_sum as f64 / (total as f64 * 4.0),
        bbox,
        dims_a: (wa, ha),
        dims_b: (wb, hb),
    }
}

fn print_verdict(stats: &DiffStats, threshold: f64, a: &Path, b: &Path) -> bool {
    let changed = stats.changed_pct > threshold;
    let (wa, ha) = stats.dims_a;
    let (wb, hb) = stats.dims_b;
    if (wa, ha) != (wb, hb) {
        println!("VERDICT: CHANGED (image sizes differ: {}x{} vs {}x{})", wa, ha, wb, hb);
        return true;
    }
    if changed {
        println!(
            "VERDICT: CHANGED ({:.2}% of pixels differ, threshold {:.2}%)",
            stats.changed_pct, threshold
        );
    } else {
        println!(
            "VERDICT: UNCHANGED ({:.2}% of pixels differ, threshold {:.2}%)",
            stats.changed_pct, threshold
        );
    }
    if let Some((x0, y0, x1, y1)) = stats.bbox {
        println!(
            "changed region: x {}-{}, y {}-{} (normalized {:.3}-{:.3}, {:.3}-{:.3})",
            x0, x1, y0, y1,
            x0 as f64 / wa as f64,
            (x1 + 1) as f64 / wa as f64,
            y0 as f64 / ha as f64,
            (y1 + 1) as f64 / ha as f64
        );
    }
    println!("mean channel diff: {:.2}", stats.mean_diff);
    println!("compared: {} vs {}", a.display(), b.display());
    changed
}

/// Print tool text; exit non-zero when the server reported an error.
fn finish(out: &ToolOutput) -> Result<()> {
    let failed = out.is_error
        || out.text.iter().any(|t| {
            t.starts_with("Error:")
                || t.starts_with("Failed")
                || t == "No device connected"
        });
    out.print_text();
    if failed {
        std::process::exit(1);
    }
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let timeout = Duration::from_secs(cli.timeout_secs);

    // Local-only commands that never touch the MCP server.
    if let Command::Compare { a, b, threshold, tolerance } = &cli.command {
        let stats = compare_images(a, b, *tolerance)?;
        print_verdict(&stats, *threshold, a, b);
        return Ok(());
    }

    let mcp = Mcp::open(&cli.url, timeout).await?;

    match &cli.command {
        Command::Compare { .. } => unreachable!(),
        Command::List => {
            let out = mcp.call("xphoneuse_list_devices", json!({})).await?;
            finish(&out)
        }
        Command::Connect { serial, platform } => {
            let out = mcp
                .call("xphoneuse_connect_device", json!({"serial": serial, "platform": platform}))
                .await?;
            finish(&out)
        }
        Command::Disconnect => {
            let out = mcp.call("xphoneuse_disconnect_device", json!({})).await?;
            finish(&out)
        }
        Command::Screenshot { output, compare, threshold, tolerance } => {
            // Comparison is a pixel question, so it asks for the lossless
            // capture; looking at the screen takes the default - scaled and
            // JPEG, a fraction of the size for a model to look at.
            let args = if compare.is_some() {
                json!({"format": "png"})
            } else {
                json!({})
            };
            let out = mcp.call("xphoneuse_take_screenshot", args).await?;
            let (png, mime) = match out.images.first() {
                Some(image) => image.clone(),
                None => return finish(&out),
            };
            // Load the reference BEFORE writing the output file: the common
            // pattern is `screenshot --compare xphoneuse-screenshot.png`,
            // where reference and output are the same path. A missing
            // reference is not an error - it just means this capture starts
            // a new reference.
            let mut first_reference = false;
            let reference_img = match compare {
                Some(reference) => match image::open(reference) {
                    Ok(img) => Some(img.to_rgba8()),
                    Err(e) if reference.exists() => {
                        return Err(e).with_context(|| {
                            format!("failed to open reference {}", reference.display())
                        });
                    }
                    Err(_) => {
                        first_reference = true;
                        None
                    }
                },
                None => None,
            };
            let path = output.clone().unwrap_or_else(|| {
                let suffix = if mime == "image/png" { "png" } else { "jpg" };
                PathBuf::from(format!("xphoneuse-screenshot.{suffix}"))
            });
            std::fs::write(&path, &png)
                .with_context(|| format!("failed to write {}", path.display()))?;
            let (w, h) = image::load_from_memory(&png)
                .map(|img| (img.width(), img.height()))
                .unwrap_or((0, 0));
            println!(
                "saved: {} ({}x{}, {} KB)",
                path.display(),
                w,
                h,
                png.len() / 1024
            );
            if let Some(text) = out.text.first() {
                if !text.trim().is_empty() {
                    println!("{text}");
                }
            }
            if first_reference {
                if let Some(reference) = compare {
                    println!(
                        "no reference {} yet - this capture starts one; view {} to see the screen",
                        reference.display(),
                        path.display()
                    );
                }
            }
            if let (Some(reference), Some(reference_img)) = (compare, reference_img) {
                let new_img = image::load_from_memory(&png)
                    .context("failed to decode captured screenshot")?
                    .to_rgba8();
                let stats = compare_buffers(reference_img, new_img, *tolerance);
                let changed = print_verdict(&stats, *threshold, reference, &path);
                if changed {
                    println!("the screen moved on - view {} to see the new state", path.display());
                } else {
                    println!("screen is the same as the reference - no need to view the new capture");
                }
            }
            if out.is_error {
                std::process::exit(1);
            }
            Ok(())
        }
        Command::Ocr { path, json: as_json, min_confidence } => {
            // No image handling here on purpose: the server OCRs and the CLI
            // prints text, so nothing has to travel back as bytes.
            let out = mcp
                .call(
                    "xphoneuse_ocr",
                    json!({
                        "path": path.as_ref().map(|p| p.to_string_lossy()),
                        "format": if *as_json { "json" } else { "text" },
                        "min_confidence": min_confidence,
                    }),
                )
                .await?;
            finish(&out)
        }
        Command::Press { button } => {
            let out = mcp
                .call("xphoneuse_press_button", json!({"button": button}))
                .await?;
            finish(&out)
        }
        Command::Text { text, enter } => {
            let out = mcp
                .call("xphoneuse_send_text", json!({"text": text, "enter": enter}))
                .await?;
            finish(&out)
        }
        Command::UnicodeText { text, method, enter } => {
            let out = mcp
                .call(
                    "xphoneuse_unicode_text",
                    json!({"text": text, "method": method, "enter": enter}),
                )
                .await?;
            finish(&out)
        }
        Command::Tap { x, y } => {
            let out = mcp.call("xphoneuse_tap", json!({"x": x, "y": y})).await?;
            finish(&out)
        }
        Command::Touch { action, x, y } => {
            let out = mcp
                .call("xphoneuse_send_touch", json!({"action": action, "x": x, "y": y}))
                .await?;
            finish(&out)
        }
        Command::Scroll { x, y, dx, dy } => {
            let out = mcp
                .call("xphoneuse_send_scroll", json!({"x": x, "y": y, "dx": dx, "dy": dy}))
                .await?;
            finish(&out)
        }
        Command::Swipe { from_x, from_y, to_x, to_y, duration_ms } => {
            let mut args = json!({"from_x": from_x, "from_y": from_y, "to_x": to_x, "to_y": to_y});
            if let Some(ms) = duration_ms {
                args["duration_ms"] = json!(ms);
            }
            let out = mcp.call("xphoneuse_swipe", args).await?;
            finish(&out)
        }
        Command::Shell { command } => {
            let command = command.join(" ");
            let out = mcp.call("xphoneuse_shell", json!({"command": command})).await?;
            finish(&out)
        }
        Command::OpenUrl { url, browser, system } => {
            let mut args = json!({"url": url, "use_system_handler": system});
            if let Some(pkg) = browser {
                args["browser_package"] = json!(pkg);
            }
            let out = mcp.call("xphoneuse_open_url", args).await?;
            finish(&out)
        }
        Command::Launch { package } => {
            let out = mcp.call("xphoneuse_launch_app", json!({"package": package})).await?;
            finish(&out)
        }
        Command::Apps(args) => {
            let mut call = json!({"kind": args.kind});
            if let Some(filter) = &args.filter {
                call["filter"] = json!(filter);
            }
            let out = mcp.call("xphoneuse_list_apps", call).await?;
            finish(&out)
        }
        Command::UiTree => {
            let out = mcp.call("xphoneuse_get_ui_tree", json!({})).await?;
            finish(&out)
        }
        Command::Find(f) => {
            let mut args = json!({});
            if let Some(v) = &f.text {
                args["text"] = json!(v);
            }
            if let Some(v) = &f.content_desc {
                args["content_desc"] = json!(v);
            }
            if let Some(v) = &f.resource_id {
                args["resource_id"] = json!(v);
            }
            if let Some(v) = &f.class {
                args["class_name"] = json!(v);
            }
            if f.clickable {
                args["clickable_only"] = json!(true);
            }
            let out = mcp.call("xphoneuse_find_on_screen", args).await?;
            finish(&out)
        }
        Command::Rotate => {
            let out = mcp.call("xphoneuse_rotate_device", json!({})).await?;
            finish(&out)
        }
        Command::Macro { action } => {
            let (tool, args) = match action {
                MacroAction::Record { name } => {
                    ("xphoneuse_macro_record", json!({"name": name}))
                }
                MacroAction::Stop => ("xphoneuse_macro_stop", json!({})),
                MacroAction::Play { name, repeat } => {
                    let mut args = json!({"name": name});
                    if let Some(r) = repeat {
                        args["repeat"] = json!(r);
                    }
                    ("xphoneuse_macro_play", args)
                }
                MacroAction::List => ("xphoneuse_macro_list", json!({})),
                MacroAction::Delete { name } => ("xphoneuse_macro_delete", json!({"name": name})),
            };
            let out = mcp.call(tool, args).await?;
            finish(&out)
        }
        Command::Wifi { action } => {
            let (tool, args) = match action {
                WifiAction::Enable { serial } => {
                    ("xphoneuse_wifi_enable", json!({"serial": serial}))
                }
                WifiAction::Connect { address } => {
                    ("xphoneuse_wifi_connect", json!({"address": address}))
                }
                WifiAction::Disconnect { address } => {
                    ("xphoneuse_wifi_disconnect", json!({"address": address}))
                }
                WifiAction::Ip { serial } => {
                    ("xphoneuse_get_device_ip", json!({"serial": serial}))
                }
            };
            let out = mcp.call(tool, args).await?;
            finish(&out)
        }
    }
}
