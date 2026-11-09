//! Runs the copilot over a screenshot that is already on disk.
//!
//! This exists because the interesting half of the feature - reading a chat out
//! of OCR boxes - is the half that has to be tuned against a real screen, and
//! doing that through the app means connecting a phone, opening a chat and
//! watching a panel. Here it is one command, and the network is optional.
//!
//! Not part of the app: a development aid, run by hand.
//!
//! ```text
//! # What the parser made of a screenshot, no API keys, no network:
//! XPHONEUSE_OCR_MODELS=<dir> cargo run -p xphoneuse-copilot --example copilot_probe -- \
//!     shot.png --lines
//!
//! # The whole chain, judged and drafted:
//! XPHONEUSE_OCR_MODELS=<dir> \
//! XPHONEUSE_COPILOT_JUDGE_KEY=... XPHONEUSE_COPILOT_REPLY_KEY=... \
//! cargo run -p xphoneuse-copilot --example copilot_probe -- shot.png
//! ```
//!
//! Env: `XPHONEUSE_COPILOT_JUDGE_KEY` / `_JUDGE_PROVIDER` / `_JUDGE_BASE` /
//! `_JUDGE_MODEL`, `XPHONEUSE_COPILOT_REPLY_KEY` / `_REPLY_BASE` / `_REPLY_MODEL`,
//! `XPHONEUSE_COPILOT_RELATIONSHIP`, `XPHONEUSE_COPILOT_ME_ON_LEFT=1`,
//! `XPHONEUSE_COPILOT_ALL_OTHER=1`, `XPHONEUSE_COPILOT_TOP_CROP`,
//! `XPHONEUSE_COPILOT_BOTTOM_CROP`, `XPHONEUSE_COPILOT_MIN_CONFIDENCE`,
//! `XPHONEUSE_COPILOT_LINE_GAP`, `XPHONEUSE_COPILOT_CANDIDATES`,
//! `XPHONEUSE_COPILOT_SHOW_PROMPT`.

use std::path::PathBuf;
use std::process::ExitCode;

use xphoneuse_copilot::{config::JevRoute, reply, CopilotConfig, ReplyRoute, ScreenOptions, Side};

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    let mut path: Option<PathBuf> = None;
    let mut show_lines = false;
    let mut keyboard_hint: Option<f32> = None;
    let mut transcript_only = false;
    let mut no_judge = false;
    // An iterator rather than a `for`, because --keyboard takes a value.
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--lines" => show_lines = true,
            // Where the device said its keyboard is, in the screenshot's own
            // pixels (see the app's `dumpsys window` read). For checking what
            // the hint does without a phone in front of you.
            "--keyboard" => {
                keyboard_hint = args.next().and_then(|value| value.parse().ok());
            }
            "--transcript-only" => transcript_only = true,
            "--no-judge" => no_judge = true,
            other => path = Some(PathBuf::from(other)),
        }
    }
    let Some(path) = path else {
        eprintln!("usage: copilot_probe <screenshot.png> [--lines] [--transcript-only] [--no-judge]");
        return ExitCode::from(2);
    };
    let Ok(bytes) = std::fs::read(&path) else {
        eprintln!("could not read {}", path.display());
        return ExitCode::from(2);
    };

    let config = config_from_env();
    let screen = config.screen.clone();
    let image = match xphoneuse_ocr::recognize_png(bytes).await {
        Ok(image) => image,
        Err(err) => {
            eprintln!("OCR failed: {err}");
            return ExitCode::FAILURE;
        }
    };
    println!("{}: {}x{}", path.display(), image.width, image.height);
    println!("OCR: {} lines", image.lines.len());
    if show_lines {
        for line in &image.lines {
            let [x, y, w, h] = line.bbox;
            println!(
                "    {:.2}  x={x:>6.0} y={y:>6.0} w={w:>6.0} h={h:>4.0}  {}",
                line.confidence,
                line.text.replace('\n', " ")
            );
        }
    }

    // The name first: it is what a conversation is keyed by when it is read,
    // and it is the first thing to look at when tuning the header band.
    match xphoneuse_copilot::transcript::read_title(&image, &screen) {
        Some(title) => println!("\ntitle: {title}"),
        None => println!("\ntitle: (none read - the header band gave nothing up)"),
    }

    // What the bottom crop came out as. With a keyboard on screen it is the
    // keypad's top rather than the setting, and that is the first thing to look
    // at when keys are being read as messages.
    match keyboard_hint.or_else(|| xphoneuse_copilot::transcript::keyboard_top(&image, &screen)) {
        Some(cut) => println!(
            "\nkeyboard: yes, reading stops at y={cut:.0} ({:.0}% of the height)",
            cut / image.height as f32 * 100.0
        ),
        None => println!("\nkeyboard: none detected - the crop setting stands"),
    }

    let messages = xphoneuse_copilot::transcript::read_screen_with_keyboard(
        &image,
        &screen,
        keyboard_hint,
    );
    println!("\ntranscript ({} messages):", messages.len());
    for msg in &messages {
        let who = match msg.from {
            Side::Me => "我  ",
            Side::Other => "对方",
        };
        println!("    {who}  {}", msg.text);
    }
    if messages.is_empty() {
        eprintln!("\nnothing was read as a message - try --lines and tune the crop and the side rule");
    }
    if transcript_only || messages.is_empty() {
        return ExitCode::SUCCESS;
    }

    // One judgement, used for both the printout and the drafting prompt: it is
    // the expensive call, and asking twice would double the bill.
    let mut analysis = None;
    if !no_judge {
        println!("\njudging...");
        match xphoneuse_copilot::jev::judge(&config, &messages).await {
            Ok(judged) => {
                let danger = match (judged.danger_level, judged.danger_max) {
                    (Some(level), Some(max)) => format!("{level:.0}/{max:.0}"),
                    _ => "-".to_string(),
                };
                println!("    true_intent    {:?}", judged.true_intent);
                println!("    best_action    {:?}", judged.best_action);
                println!("    other_needs    {:?}", judged.other_needs);
                println!("    danger_level   {danger}");
                println!("    should_reply   {:?}", judged.should_reply_now);
                println!("    literal        {:?}", judged.literal_question);
                println!("    tension_done   {:?}", judged.tension_resolved);
                analysis = Some(judged);
            }
            Err(err) => println!("    failed: {err}"),
        }
    }

    println!("\ndrafting...");
    match xphoneuse_copilot::draft_and_rank(&config, &messages, analysis.as_ref()).await {
        Ok(candidates) => {
            for (index, candidate) in candidates.iter().enumerate() {
                println!("    #{}  {:.0}%  {}", index + 1, candidate.score * 100.0, candidate.text);
            }
        }
        Err(err) => println!("    failed: {err}"),
    }

    // The system half of the drafting prompt, so a bad draft can be told apart
    // from a bad prompt without a debugger. The user half carries the chat.
    if std::env::var_os("XPHONEUSE_COPILOT_SHOW_PROMPT").is_some() {
        println!("\n--- reply prompt (system) ---\n{}", reply::system_prompt(config.candidates(), !config.has_judge_key()));
    }
    ExitCode::SUCCESS
}

fn config_from_env() -> CopilotConfig {
    let env = |key: &str| std::env::var(key).unwrap_or_default();
    let float = |key: &str| env(key).trim().parse::<f32>().ok();
    let flag = |key: &str| {
        matches!(
            env(key).trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes"
        )
    };
    let mut screen = ScreenOptions::default();
    if let Some(value) = float("XPHONEUSE_COPILOT_TOP_CROP") {
        screen.top_crop = value;
    }
    if let Some(value) = float("XPHONEUSE_COPILOT_BOTTOM_CROP") {
        screen.bottom_crop = value;
    }
    if let Some(value) = float("XPHONEUSE_COPILOT_MIN_CONFIDENCE") {
        screen.min_confidence = value;
    }
    if let Some(value) = float("XPHONEUSE_COPILOT_LINE_GAP") {
        screen.line_gap = value;
    }
    if flag("XPHONEUSE_COPILOT_ME_ON_LEFT") {
        screen.me_on_right = false;
    }
    if flag("XPHONEUSE_COPILOT_ALL_OTHER") {
        screen.all_from_other = true;
    }
    CopilotConfig {
        judge: JevRoute {
            provider: env("XPHONEUSE_COPILOT_JUDGE_PROVIDER"),
            base_url: env("XPHONEUSE_COPILOT_JUDGE_BASE"),
            api_key: env("XPHONEUSE_COPILOT_JUDGE_KEY"),
            model: env("XPHONEUSE_COPILOT_JUDGE_MODEL"),
        },
        reply: ReplyRoute {
            base_url: {
                let base = env("XPHONEUSE_COPILOT_REPLY_BASE");
                if base.is_empty() {
                    ReplyRoute::default().base_url
                } else {
                    base
                }
            },
            api_key: env("XPHONEUSE_COPILOT_REPLY_KEY"),
            model: {
                let model = env("XPHONEUSE_COPILOT_REPLY_MODEL");
                if model.is_empty() {
                    ReplyRoute::default().model
                } else {
                    model
                }
            },
        },
        relationship: {
            let relationship = env("XPHONEUSE_COPILOT_RELATIONSHIP");
            if relationship.is_empty() {
                "对方是我的伴侣；我发的在右边，对方发的在左边。".to_string()
            } else {
                relationship
            }
        },
        candidate_count: float("XPHONEUSE_COPILOT_CANDIDATES").map(|v| v as usize).unwrap_or(3),
        screen,
    }
}
