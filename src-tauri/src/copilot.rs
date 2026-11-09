//! The conversation copilot's app-side half: capture a screenshot, read the
//! chat out of it, and keep the conversation in memory between calls.
//!
//! Everything interesting happens in `xphoneuse-copilot` (pure, testable, no
//! Tauri). What is here is the three things that need the app: the device
//! session, the screenshot, and the state that survives from one screen to the
//! next.
//!
//! Nothing here sends anything to the phone. The drafts go back to the frontend
//! and the user decides what to do with them.
//!
//! **Known limitation, deliberate for now.** Pixels cannot tell a chat from
//! anything else, so a screen that settles on the home screen is read as a
//! conversation too (the probe turns a home screen into 39 "messages" out of
//! app icon labels). Two things keep that from being expensive: an automatic
//! run only judges a screen whose newest message is the other person's, and the
//! panel shows what was read. It is still wrong, and the fix is a foreground-app
//! check - which is what the reference implementation uses, and which XPhoneUse
//! has no source for.

use std::sync::Arc;
use std::time::Duration;

use xphoneuse_copilot::config::CopilotConfig;
use xphoneuse_copilot::transcript;
use xphoneuse_copilot::{Conversations, Reading};
use xphoneuse_copilot::{Analysis, Candidate, Msg};
use serde::Serialize;
use tauri::State;

use crate::commands::take_screenshot_png;
use crate::state::AppState;

/// What the copilot remembers between calls.
///
/// The conversations live here and nowhere else: nothing is written to disk,
/// and they go away when the app does.
#[derive(Default)]
pub struct CopilotState {
    /// The device the messages came from. A different device starts over rather
    /// than splicing two people's chats together.
    pub session_key: String,
    /// One history per conversation, keyed by the name in the app's header when
    /// there is one. See `xphoneuse_copilot::conversations`.
    pub conversations: Conversations,
    /// The change signature of the current conversation at the last analysis,
    /// so an unchanged screen does not buy a second judgement.
    pub signature: String,
    /// The last judgement, reused by the drafting call so the panel's analysis
    /// and the drafts agree.
    pub analysis: Option<Analysis>,
}

/// The copilot state, as the app holds it.
pub type SharedCopilot = Arc<std::sync::Mutex<CopilotState>>;

/// What the panel shows after an analysis.
#[derive(Debug, Serialize)]
pub struct Outcome {
    /// `ok`, `no_chat`, `waiting`, `unchanged` or `error`. Everything but `ok`
    /// and `error` is a non-event rather than a failure - there was nothing new
    /// to judge - and the transcript is still returned so the panel can show
    /// what was read.
    pub status: &'static str,
    /// What the conversation is called, when the header gave a name up. `None`
    /// means the name was not readable, not that the chat has no name.
    pub title: Option<String>,
    /// The conversation so far.
    pub transcript: Vec<Msg>,
    pub analysis: Option<Analysis>,
    pub message: Option<String>,
}

/// What the tuning path returns: what this screen held, and what the history
/// looks like after folding it in.
#[derive(Debug, Serialize)]
pub struct Transcript {
    /// What this one screen held, name and messages.
    pub screen: Vec<Msg>,
    pub title: Option<String>,
    pub messages: Vec<Msg>,
}

/// A conversation as it stands: the name it goes by, if any, and everything
/// read of it.
struct Chat {
    title: Option<String>,
    messages: Vec<Msg>,
}

fn outcome(status: &'static str, chat: Chat, analysis: Option<Analysis>) -> Outcome {
    Outcome {
        status,
        title: chat.title,
        transcript: chat.messages,
        analysis,
        message: None,
    }
}

fn failure(status: &'static str, chat: Chat, message: impl Into<String>) -> Outcome {
    Outcome {
        status,
        title: chat.title,
        transcript: chat.messages,
        analysis: None,
        message: Some(message.into()),
    }
}

/// Which device the session belongs to, or `None` when nothing is connected.
async fn session_key(state: &AppState) -> Option<String> {
    let android = state.session.lock().await;
    if let Some(session) = android.as_ref() {
        return Some(format!("android:{}", session.device_serial));
    }
    drop(android);
    let ios = state.ios_session.lock().await;
    ios.as_ref().map(|session| format!("ios:{}", session.udid))
}

/// A screenshot of whatever is connected.
///
/// On iOS the tunnel's one-shot capture is tried first: it is ~750ms, and the
/// DVT path can wedge for tens of seconds on rich content. Same order as the
/// embedded MCP server's `capture_screen_png`. A timeout only abandons this
/// request - the tunnel actor keeps serving.
async fn capture_png(state: &AppState) -> Result<Vec<u8>, String> {
    let udid = state
        .ios_session
        .lock()
        .await
        .as_ref()
        .map(|session| session.udid.clone());
    if let Some(udid) = udid {
        let attempt = tokio::time::timeout(
            Duration::from_secs(6),
            xphoneuse_ios::tunnel::tunnel_screenshot(&udid, xphoneuse_ios::tunnel::ImageFormat::Png),
        )
        .await;
        if let Ok(Some(Ok(png))) = attempt {
            return Ok(png);
        }
    }
    take_screenshot_png(state).await
}

/// Where Android says its keyboard is, in the screenshot's own pixels.
///
/// The platform reports its input method's frame while it is showing, and that
/// is exact: every keyboard, every language, docked or floating - which no
/// reading of the pixels can promise. It costs one adb round trip on a
/// connection scrcpy is already using.
///
/// `None` for everything else: an iPhone, a device too old to report insets, or
/// a query that fails. The parser then recognises a keyboard by its shape,
/// which is what it did before this existed.
async fn android_keyboard_top(state: &AppState) -> Option<f32> {
    let serial = {
        let session = state.session.lock().await;
        session.as_ref()?.device_serial.clone()
    };
    let output = xphoneuse_core::adb::shell(&serial, "dumpsys window | grep type=ime")
        .await
        .ok()?
        .wait_with_output()
        .await
        .ok()?;
    ime_top_in(&String::from_utf8_lossy(&output.stdout))
}

/// The keyboard's top edge out of the insets `dumpsys window` prints.
///
/// The line looks like
/// `InsetsSource id=3 type=ime frame=[0,0][0,0] visibleFrame=[0,1505][1080,2340] visible=false`.
/// Only a line that says `visible=true` means a keyboard is on screen: the
/// frame is zeroed and the *visible* frame keeps the last shape it had, so
/// reading it without the flag would cut a conversation in half with no
/// keyboard anywhere near it.
///
/// The first `type=ime` line in that dump has no `visibleFrame` at all, which is
/// why the search is for the line that has both.
fn ime_top_in(dumpsys: &str) -> Option<f32> {
    let line = dumpsys
        .lines()
        .find(|line| line.contains("type=ime") && line.contains("visibleFrame="))?;
    if !line.contains("visible=true") {
        return None;
    }
    let frame = line.split("visibleFrame=").nth(1)?.split(' ').next()?;
    // "[0,1505][1080,2340]": the leading number of the second pair.
    let top = frame.strip_prefix('[')?.split(',').nth(1)?.split(']').next()?;
    let top: f32 = top.trim().parse().ok()?;
    (top > 0.0).then_some(top)
}

/// Fold a fresh screen into the conversation it belongs to, starting over when
/// the device changed.
///
/// The name decides which conversation that is when the header gave one up; the
/// merge rule decides when it did not (see `xphoneuse_copilot::conversations`).
fn absorb(state: &SharedCopilot, key: &str, reading: &Reading) -> Chat {
    let mut copilot = state.lock().unwrap_or_else(|e| e.into_inner());
    if copilot.session_key != key {
        copilot.session_key = key.to_string();
        copilot.conversations.clear();
        copilot.signature.clear();
        copilot.analysis = None;
    }
    let messages = copilot
        .conversations
        .absorb(reading.title.clone(), &reading.messages)
        .to_vec();
    Chat {
        title: copilot.conversations.title().map(str::to_string),
        messages,
    }
}

/// Read the screen and report what the parser made of it. No model calls, no
/// state changes beyond folding the screen into the history: this is the
/// command to run against a real phone when the parsing needs tuning.
#[tauri::command]
pub async fn copilot_transcript(
    state: State<'_, AppState>,
    config: CopilotConfig,
) -> Result<Transcript, String> {
    let key = session_key(&state).await.ok_or("Not connected")?;
    let png = capture_png(&state).await?;
    // Asked after the capture, so the answer describes the screen that was just
    // taken. A keyboard dismissed in between costs one read its tail, and the
    // next one is right again.
    let keyboard = android_keyboard_top(&state).await;
    let reading =
        xphoneuse_copilot::read_screen_with_keyboard(png, &config.screen, keyboard).await?;
    let chat = absorb(&state.copilot, &key, &reading);
    Ok(Transcript {
        screen: reading.messages,
        title: chat.title,
        messages: chat.messages,
    })
}

/// Capture, read, and judge. The judgement is what the panel shows first.
///
/// `auto` is the difference between "the screen settled" and "the user pressed
/// the button". An automatic run only judges a screen whose newest message is
/// the other person's - there is nothing to reply to when the last word was
/// mine, and paying a decision model to say so is how a feature gets turned
/// off. The manual run judges whatever is on screen, because the user asked.
#[tauri::command]
pub async fn copilot_analyze(
    state: State<'_, AppState>,
    config: CopilotConfig,
    auto: bool,
) -> Result<Outcome, String> {
    let key = session_key(&state).await.ok_or("Not connected")?;
    let png = capture_png(&state).await?;
    // Asked after the capture, so the answer describes the screen that was just
    // taken. A keyboard dismissed in between costs one read its tail, and the
    // next one is right again.
    let keyboard = android_keyboard_top(&state).await;
    let reading =
        xphoneuse_copilot::read_screen_with_keyboard(png, &config.screen, keyboard).await?;
    let chat = absorb(&state.copilot, &key, &reading);

    if chat.messages.is_empty() {
        return Ok(outcome("no_chat", chat, None));
    }
    if auto && transcript::latest_from(&chat.messages) != xphoneuse_copilot::Side::Other {
        return Ok(outcome("waiting", chat, None));
    }
    let signature = transcript::signature(&chat.messages);
    {
        let copilot = state.copilot.lock().unwrap_or_else(|e| e.into_inner());
        if copilot.signature == signature {
            let analysis = copilot.analysis.clone();
            drop(copilot);
            return Ok(outcome("unchanged", chat, analysis));
        }
    }

    // A judging route is optional. Without one there is no judgement to show -
    // the panel says so in words - and the reply model is asked to order its own
    // candidates instead (see `reply::draft`). The signature above is still
    // worth keeping: it is the drafting call, not the judgement, that costs a
    // request here, and an unchanged screen must not buy a second one.
    let analysis = if config.has_judge_key() {
        match xphoneuse_copilot::jev::judge(&config, &chat.messages).await {
            Ok(analysis) => Some(analysis),
            Err(err) => return Ok(failure("error", chat, err)),
        }
    } else {
        None
    };
    {
        let mut copilot = state.copilot.lock().unwrap_or_else(|e| e.into_inner());
        copilot.signature = signature;
        copilot.analysis = analysis.clone();
    }
    Ok(outcome("ok", chat, analysis))
}

/// Draft the candidate replies for the conversation as it stands, and rank them
/// with Jev.
#[tauri::command]
pub async fn copilot_draft(
    state: State<'_, AppState>,
    config: CopilotConfig,
) -> Result<Vec<Candidate>, String> {
    let (messages, analysis) = {
        let copilot = state.copilot.lock().unwrap_or_else(|e| e.into_inner());
        (
            copilot.conversations.messages().to_vec(),
            copilot.analysis.clone(),
        )
    };
    if messages.is_empty() {
        return Err("there is no conversation to draft for yet".to_string());
    }
    xphoneuse_copilot::draft_and_rank(&config, &messages, analysis.as_ref()).await
}

/// Forget the conversation. Called when the panel is closed or the device
/// changes: the history is per-session by design.
#[tauri::command]
pub async fn copilot_reset(state: State<'_, AppState>) -> Result<(), String> {
    let mut copilot = state.copilot.lock().unwrap_or_else(|e| e.into_inner());
    copilot.conversations.clear();
    copilot.signature.clear();
    copilot.analysis = None;
    Ok(())
}

/// The settings page's connectivity test. `route` is `"judge"` or `"reply"`;
/// anything else is a reply test, because the drafting route is the one that
/// has a key of its own to check.
#[tauri::command]
pub async fn copilot_test_route(
    config: CopilotConfig,
    route: String,
) -> Result<String, String> {
    if route == "judge" {
        xphoneuse_copilot::jev::ping(&config).await
    } else {
        xphoneuse_copilot::reply::ping(&config).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shape `dumpsys window` prints on a Huawei (Android 10), keyboard up.
    const UP: &str = "        InsetsSource id=3 type=ime frame=[0,0][0,0] visible=false flags=         
        mSource=InsetsSource id=3 type=ime frame=[0,0][0,0] visible=false flags=         
        InsetsSource id=3 type=ime frame=[0,0][0,0] visibleFrame=[0,1505][1080,2340] visible=true flags= insetsRoundedCornerFrame=false";

    #[test]
    fn the_input_methods_frame_is_read_when_it_is_showing() {
        assert_eq!(ime_top_in(UP), Some(1505.0));
    }

    #[test]
    fn a_hidden_keyboard_has_no_frame_to_read() {
        // Same dump with the keyboard put away: the visible frame keeps the
        // last shape it had, and believing it would cut the conversation in
        // half with no keyboard on screen at all.
        let hidden = UP.replace("visible=true", "visible=false");
        assert_eq!(ime_top_in(&hidden), None);
    }

    #[test]
    fn a_device_that_reports_no_insets_is_not_a_failure() {
        // Older devices, and anything that is not Android: the parser falls
        // back to recognising a keyboard by its shape.
        assert_eq!(ime_top_in(""), None);
        assert_eq!(ime_top_in("  mInputShown=false
"), None);
        assert_eq!(
            ime_top_in("InsetsSource id=3 type=ime frame=[0,0][0,0] visible=false"),
            None
        );
    }

    #[test]
    fn a_floating_keyboard_reports_its_own_corner() {
        let line = "InsetsSource id=3 type=ime frame=[0,0][0,0] visibleFrame=[120,900][960,1500] visible=true flags=";
        assert_eq!(ime_top_in(line), Some(900.0));
    }
}
