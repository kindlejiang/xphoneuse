//! How text gets onto an iPhone.
//!
//! The HID keyboard - over the CoreDevice tunnel or the BLE bridge - only
//! carries printable ASCII. Anything else has to travel one of two ways:
//!
//! * [`TextInputMethod::Clipboard`] - set the device pasteboard over the
//!   tunnel and press Cmd+V. Nothing to install on the phone; iOS asks the
//!   user to allow the paste the first time an app is pasted into, and the
//!   device clipboard is replaced.
//! * [`TextInputMethod::AgentInput`] - hand the text to the AgentInput
//!   keyboard app, which types it into the focused field. Nothing pops up and
//!   the clipboard is untouched, but the app has to be installed and its
//!   keyboard has to be the active one.
//!
//! Both delivery paths need a live session, and the method only matters for
//! text the HID keyboard cannot type: ASCII (including newlines and tabs, as
//! long as the tunnel is the transport) is always typed, never pasted.
//!
//! [`plan_text_delivery`] is the single place that decides between them, so
//! the app, the MCP server and the CLIs cannot drift apart.

use std::sync::{Arc, Mutex};

use crate::agent_input::AgentInputClient;

/// Which way non-ASCII text should be delivered to an iPhone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TextInputMethod {
    /// Device pasteboard + Cmd+V. The default: it needs nothing installed.
    #[default]
    Clipboard,
    /// The AgentInput keyboard app types the text for us.
    AgentInput,
}

impl TextInputMethod {
    pub fn as_str(self) -> &'static str {
        match self {
            TextInputMethod::Clipboard => "clipboard",
            TextInputMethod::AgentInput => "agentinput",
        }
    }

    /// Parse a caller-supplied method. Case and surrounding space are ignored;
    /// the error names the valid values, because it is what a CLI prints.
    pub fn parse(s: &str) -> Result<Self, String> {
        match s.trim().to_ascii_lowercase().as_str() {
            "clipboard" | "paste" => Ok(TextInputMethod::Clipboard),
            "agentinput" | "agent_input" | "keyboard" => Ok(TextInputMethod::AgentInput),
            other => Err(format!(
                "unknown input method {other:?} (expected \"clipboard\" or \"agentinput\")"
            )),
        }
    }
}

/// What the current session can actually do.
#[derive(Debug, Clone, Copy, Default)]
pub struct TextCapabilities {
    /// The CoreDevice tunnel's input path is ready (full Unicode: types ASCII,
    /// pastes the rest).
    pub tunnel: bool,
    /// A BLE-HID bridge is attached (printable ASCII only).
    pub bridge: bool,
    /// An AgentInput client is configured. Note this says *configured*, not
    /// *reachable*: the client dials lazily, and a phone whose AgentInput
    /// keyboard is not active will refuse the connection.
    pub agent_input: bool,
}

/// The transport that will carry the text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextRoute {
    /// Type it with HID keycodes over the tunnel.
    TunnelKeys,
    /// Set the pasteboard and press Cmd+V.
    TunnelPaste,
    /// Type it with HID keycodes over the BLE bridge.
    Bridge,
    /// Hand it to the AgentInput keyboard.
    AgentInput,
}

impl TextRoute {
    pub fn as_str(self) -> &'static str {
        match self {
            TextRoute::TunnelKeys => "tunnel keys",
            TextRoute::TunnelPaste => "clipboard paste",
            TextRoute::Bridge => "bluetooth bridge",
            TextRoute::AgentInput => "AgentInput",
        }
    }

    /// True when this route puts the text on the device clipboard.
    pub fn uses_clipboard(self) -> bool {
        matches!(self, TextRoute::TunnelPaste)
    }
}

/// The route to take, plus why it is not the one that was asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextPlan {
    pub route: TextRoute,
    /// Set when the preferred route was unavailable and this one is being used
    /// instead - the caller should pass it on to the user.
    pub note: Option<&'static str>,
}

/// Characters the BLE bridge can carry: printable ASCII, one line at a time.
/// `hid::map_text` drops everything else without saying so, which is why the
/// planner checks first instead of letting a newline silently disappear.
pub fn bridge_can_type(text: &str) -> bool {
    text.chars().all(|c| (' '..='~').contains(&c))
}

fn plan(route: TextRoute) -> TextPlan {
    TextPlan { route, note: None }
}

fn noted(route: TextRoute, note: &'static str) -> TextPlan {
    TextPlan { route, note: Some(note) }
}

/// Decide how to deliver `text`.
///
/// The HID path always wins for text it can carry: typing never disturbs the
/// clipboard and never raises the iOS paste alert. `method` therefore only
/// decides what happens to text the keyboard cannot type - which, on iPhone,
/// means anything outside printable ASCII.
pub fn plan_text_delivery(
    text: &str,
    method: TextInputMethod,
    caps: TextCapabilities,
) -> Result<TextPlan, String> {
    // A bare Enter press (empty text) rides the same path as ASCII: there is
    // nothing to deliver, only a key to press.
    if text.is_empty() {
        return Ok(match (caps.tunnel, caps.bridge, caps.agent_input) {
            (true, _, _) => plan(TextRoute::TunnelKeys),
            (_, true, _) => plan(TextRoute::Bridge),
            (_, _, true) => plan(TextRoute::AgentInput),
            _ => return Err(NO_INPUT.to_string()),
        });
    }

    // ASCII over the tunnel is typed, newlines and all. Over the bridge only
    // printable ASCII fits, so anything else falls through to the methods
    // below rather than losing characters.
    if caps.tunnel && text.is_ascii() {
        return Ok(plan(TextRoute::TunnelKeys));
    }
    if !caps.tunnel && text.is_ascii() && bridge_can_type(text) {
        return Ok(plan(TextRoute::Bridge));
    }

    match method {
        TextInputMethod::Clipboard => {
            if caps.tunnel {
                Ok(plan(TextRoute::TunnelPaste))
            } else if caps.agent_input {
                // Older iPhone: the Bluetooth bridge has no pasteboard, so
                // AgentInput is the only way to get non-ASCII onto the device.
                Ok(noted(TextRoute::AgentInput, NEEDS_TUNNEL))
            } else {
                Err(NEEDS_TUNNEL_OR_AGENT_INPUT.to_string())
            }
        }
        TextInputMethod::AgentInput => {
            if caps.agent_input {
                Ok(plan(TextRoute::AgentInput))
            } else if caps.tunnel {
                Ok(noted(TextRoute::TunnelPaste, AGENT_INPUT_UNAVAILABLE))
            } else {
                Err(NEEDS_TUNNEL_OR_AGENT_INPUT.to_string())
            }
        }
    }
}

const NEEDS_TUNNEL: &str = "the clipboard method needs the USB tunnel - used the AgentInput keyboard instead";
const NEEDS_TUNNEL_OR_AGENT_INPUT: &str = "cannot type non-ASCII text: the clipboard method needs the USB tunnel (iOS 17+) and AgentInput is not configured";
const AGENT_INPUT_UNAVAILABLE: &str = "AgentInput is not configured - pasted via the clipboard instead";
const NO_INPUT: &str = "no input available: no USB tunnel, no Bluetooth bridge, no AgentInput. If the device was just connected, give it a moment and retry";

/// Dial AgentInput if it is not already connected.
///
/// Callers use this to decide whether they can fall back: a failure here means
/// the keyboard is not installed or not the active one, and - crucially -
/// nothing has been typed yet. Once a send has started, a failure must be
/// reported rather than retried elsewhere, or the text lands twice.
pub async fn ensure_agent_input(
    slot: &Arc<Mutex<Option<AgentInputClient>>>,
) -> Result<(), String> {
    let slot = slot.clone();
    tokio::task::spawn_blocking(move || {
        let mut guard = slot.lock().unwrap();
        let client = guard
            .as_mut()
            .ok_or_else(|| "AgentInput is not configured".to_string())?;
        if client.is_connected() {
            return Ok(());
        }
        client.connect()
    })
    .await
    .map_err(|e| format!("AgentInput task failed: {e}"))?
}

#[cfg(test)]
mod tests {
    use super::*;

    const TUNNEL: TextCapabilities = TextCapabilities { tunnel: true, bridge: false, agent_input: false };
    const TUNNEL_AI: TextCapabilities = TextCapabilities { tunnel: true, bridge: false, agent_input: true };
    const BRIDGE: TextCapabilities = TextCapabilities { tunnel: false, bridge: true, agent_input: false };
    const BRIDGE_AI: TextCapabilities = TextCapabilities { tunnel: false, bridge: true, agent_input: true };
    const NOTHING: TextCapabilities = TextCapabilities { tunnel: false, bridge: false, agent_input: false };

    fn route(text: &str, method: TextInputMethod, caps: TextCapabilities) -> TextPlan {
        plan_text_delivery(text, method, caps).expect("a route")
    }

    #[test]
    fn parse_accepts_both_spellings() {
        assert_eq!(TextInputMethod::parse(" clipboard ").unwrap(), TextInputMethod::Clipboard);
        assert_eq!(TextInputMethod::parse("AgentInput").unwrap(), TextInputMethod::AgentInput);
        assert_eq!(TextInputMethod::parse("paste").unwrap(), TextInputMethod::Clipboard);
        let err = TextInputMethod::parse("telepathy").unwrap_err();
        assert!(err.contains("clipboard") && err.contains("agentinput"), "{err}");
    }

    #[test]
    fn the_default_is_clipboard() {
        assert_eq!(TextInputMethod::default(), TextInputMethod::Clipboard);
    }

    #[test]
    fn ascii_is_typed_not_pasted() {
        // The clipboard is only for what the keyboard cannot type.
        for caps in [TUNNEL, TUNNEL_AI] {
            for method in [TextInputMethod::Clipboard, TextInputMethod::AgentInput] {
                let p = route("Hello, world!", method, caps);
                assert_eq!(p.route, TextRoute::TunnelKeys, "{method:?} {caps:?}");
                assert!(p.note.is_none());
            }
        }
        assert_eq!(route("Hello", TextInputMethod::Clipboard, BRIDGE_AI).route, TextRoute::Bridge);
    }

    #[test]
    fn newlines_are_ascii_but_the_bridge_cannot_type_them() {
        assert!("\n".is_ascii() && !bridge_can_type("\n"));
        assert!(bridge_can_type("plain text 123 !?"));
        // The tunnel types newlines as the Return key.
        assert_eq!(route("a\nb", TextInputMethod::Clipboard, TUNNEL).route, TextRoute::TunnelKeys);
        // The bridge would drop the newline, so it is not used; with no other
        // route the caller is told instead of losing characters.
        let err = plan_text_delivery("a\nb", TextInputMethod::Clipboard, BRIDGE).unwrap_err();
        assert!(err.contains("USB tunnel"), "{err}");
    }

    #[test]
    fn a_bare_enter_still_works_without_a_hid_path() {
        assert_eq!(route("", TextInputMethod::Clipboard, TUNNEL).route, TextRoute::TunnelKeys);
        assert_eq!(route("", TextInputMethod::Clipboard, BRIDGE).route, TextRoute::Bridge);
        // AgentInput can press Enter too, so it carries an empty text.
        assert_eq!(route("", TextInputMethod::Clipboard, BRIDGE_AI).route, TextRoute::Bridge);
        assert_eq!(route("", TextInputMethod::Clipboard, TextCapabilities { tunnel: false, bridge: false, agent_input: true }).route, TextRoute::AgentInput);
        assert!(plan_text_delivery("", TextInputMethod::Clipboard, NOTHING).is_err());
    }

    #[test]
    fn non_ascii_prefers_the_configured_method() {
        assert_eq!(route("你好", TextInputMethod::Clipboard, TUNNEL).route, TextRoute::TunnelPaste);
        assert_eq!(route("你好", TextInputMethod::AgentInput, TUNNEL_AI).route, TextRoute::AgentInput);
        // Asked for AgentInput with none configured: paste, and say so.
        let p = route("你好", TextInputMethod::AgentInput, TUNNEL);
        assert_eq!(p.route, TextRoute::TunnelPaste);
        assert!(p.note.unwrap().contains("AgentInput"));
    }

    #[test]
    fn an_older_iphone_still_gets_its_text() {
        // Bluetooth bridge only, no tunnel: the clipboard is impossible, so
        // AgentInput carries it - this is what the app has always done.
        let p = route("你好", TextInputMethod::Clipboard, BRIDGE_AI);
        assert_eq!(p.route, TextRoute::AgentInput);
        assert!(p.note.unwrap().contains("USB tunnel"));
    }

    #[test]
    fn nothing_available_is_an_error_that_says_what_to_do() {
        let err = plan_text_delivery("你好", TextInputMethod::Clipboard, NOTHING).unwrap_err();
        assert!(err.contains("AgentInput"), "{err}");
        assert!(err.contains("tunnel"), "{err}");
        // Just after connecting the auth gate is still closed, which looks the
        // same as "nothing available" - mention retrying.
        let err = plan_text_delivery("", TextInputMethod::Clipboard, NOTHING).unwrap_err();
        assert!(err.contains("retry"), "{err}");
    }

    #[test]
    fn falling_back_replans_without_agent_input() {
        // How the callers handle a failed AgentInput dial: clear the
        // capability and ask again, rather than keeping a second table.
        let failed = TextCapabilities { agent_input: false, ..TUNNEL_AI };
        let p = plan_text_delivery("你好", TextInputMethod::AgentInput, failed).unwrap();
        assert_eq!(p.route, TextRoute::TunnelPaste);
        assert!(p.note.is_some());
    }
}
