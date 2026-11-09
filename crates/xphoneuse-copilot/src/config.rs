//! What the frontend sends with every copilot call.
//!
//! Two routes, deliberately separate, because they are different kinds of
//! service: `judge` is a Jev/TypeSafe decision endpoint (it answers
//! multiple-choice questions and generates nothing), and `reply` is any
//! OpenAI-compatible chat endpoint (it writes the drafts). Mixing them into one
//! "model" setting would make the provider presets meaningless - Jev is not
//! served from `/chat/completions`, and a chat model cannot answer `noul`.
//!
//! The keys live here only for the length of a call. Nothing in this crate
//! logs them, and error text is built from status codes and response bodies
//! only.

use serde::{Deserialize, Serialize};

/// Jev providers, with the base URL and model the reference implementation
/// ships as defaults (`core/Prefs.kt:283-306`).
pub const JEV_PROVIDERS: &[(&str, &str, &str)] = &[
    ("typesafe", "https://api.typesafe.ai", "jev-latest"),
    ("openrouter", "https://openrouter.ai/api", "typesafe/jev-1.13"),
    ("bocha", "https://jev.bocha.cn", "bocha-jev-v1"),
    ("vercel", "https://ai-gateway.vercel.sh/typesafe", "typesafe-ai/jev"),
    ("zen", "https://opencode.ai/zen", "jev-1.13"),
    // The user supplies the whole URL; nothing is appended.
    ("custom", "", ""),
];

fn preset(provider: &str) -> Option<(&'static str, &'static str)> {
    JEV_PROVIDERS
        .iter()
        .find(|(id, _, _)| *id == provider)
        .map(|(_, base, model)| (*base, *model))
}

/// Where the Jev decision calls go.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct JevRoute {
    /// One of [`JEV_PROVIDERS`]. Unknown values behave like `openrouter`,
    /// which is what the reference implementation's `else` branch does.
    #[serde(default)]
    pub provider: String,
    /// Empty = the provider's default.
    #[serde(default)]
    pub base_url: String,
    #[serde(default)]
    pub api_key: String,
    /// Empty = the provider's default.
    #[serde(default)]
    pub model: String,
}

impl JevRoute {
    /// The provider actually used: an unknown or unset one behaves like
    /// OpenRouter, which is the reference's `else` branch
    /// (`core/Prefs.kt:234`). Base URL, model and path all have to agree on
    /// this, so they all read it from here.
    fn provider_id(&self) -> &str {
        if preset(&self.provider).is_some() {
            self.provider.as_str()
        } else {
            "openrouter"
        }
    }

    fn preset_of(&self) -> (&'static str, &'static str) {
        preset(self.provider_id()).expect("a resolved provider id is a preset")
    }

    pub fn base(&self) -> String {
        let configured = self.base_url.trim();
        if configured.is_empty() {
            self.preset_of().0.to_string()
        } else {
            configured.trim_end_matches('/').to_string()
        }
    }

    pub fn model(&self) -> String {
        let configured = self.model.trim();
        if configured.is_empty() {
            self.preset_of().1.to_string()
        } else {
            configured.to_string()
        }
    }

    /// The full POST URL.
    ///
    /// Every provider but OpenRouter serves the TypeSafe protocol at
    /// `/v1/systemone`; OpenRouter proxies it at `/alpha/decisions`; `custom`
    /// is taken verbatim (`core/Prefs.kt:225-236`).
    pub fn endpoint(&self) -> String {
        let base = self.base();
        match self.provider_id() {
            "custom" => self.base_url.trim().to_string(),
            "openrouter" => format!("{base}/alpha/decisions"),
            _ => format!("{base}/v1/systemone"),
        }
    }
}

/// Where the drafting calls go: any OpenAI-compatible chat endpoint.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ReplyRoute {
    #[serde(default = "default_reply_base")]
    pub base_url: String,
    /// Empty = reuse the judge key, the way `Prefs.effectiveReplyKey()` does.
    /// Only useful when both routes are the same vendor (OpenRouter), which is
    /// exactly the reference's default setup.
    #[serde(default)]
    pub api_key: String,
    #[serde(default = "default_reply_model")]
    pub model: String,
}

fn default_reply_base() -> String {
    "https://openrouter.ai/api/v1".to_string()
}

fn default_reply_model() -> String {
    "deepseek/deepseek-chat-v3.1".to_string()
}

impl Default for ReplyRoute {
    fn default() -> Self {
        Self {
            base_url: default_reply_base(),
            api_key: String::new(),
            model: default_reply_model(),
        }
    }
}

impl ReplyRoute {
    pub fn endpoint(&self) -> String {
        format!("{}/chat/completions", self.base_url.trim().trim_end_matches('/'))
    }

    /// The key actually sent: the reply key, or the judge key when blank.
    pub fn effective_key<'a>(&'a self, judge: &'a JevRoute) -> &'a str {
        if self.api_key.trim().is_empty() {
            judge.api_key.trim()
        } else {
            self.api_key.trim()
        }
    }
}

/// Reading the screen: how much of it is chat, and how to tell the two sides
/// apart.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ScreenOptions {
    /// Fraction of the height dropped from the top (status bar, title bar).
    #[serde(default = "default_top_crop")]
    pub top_crop: f32,
    /// Fraction of the height kept from the top (the rest is the input area).
    #[serde(default = "default_bottom_crop")]
    pub bottom_crop: f32,
    /// OCR lines below this confidence are dropped before grouping.
    #[serde(default = "default_min_confidence")]
    pub min_confidence: f32,
    /// Vertical gap, as a multiple of the previous line's height, that starts a
    /// new bubble.
    ///
    /// 1.2 is the reference's value, calibrated on Android chat apps read
    /// through a bitmap crop. It is a setting rather than a constant because
    /// the line spacing of a real chat screen is the one number in this crate
    /// that has not been measured against a real XPhoneUse screenshot yet - and
    /// it is the difference between reading a conversation and reading one
    /// enormous message.
    #[serde(default = "default_line_gap")]
    pub line_gap: f32,
    /// The band a chat app prints the name of the conversation in, as
    /// fractions of the height: `title_top`..`title_bottom`.
    ///
    /// Read only for the name (`transcript::read_title`), never for messages.
    /// The defaults land inside the strip `top_crop` already throws away, so
    /// this costs no OCR - those lines were read and dropped. It is a setting
    /// because header layouts differ: the band has to clear the status bar,
    /// whose height is the one thing here that is not the same on both
    /// platforms. Setting `title_bottom` below `title_top` turns the name off
    /// and leaves the continuity rule alone.
    #[serde(default = "default_title_top")]
    pub title_top: f32,
    #[serde(default = "default_title_bottom")]
    pub title_bottom: f32,
    /// Which side of the screen my own bubbles sit on. Most chat apps put mine
    /// on the right; the panel offers a switch for the ones that do not.
    #[serde(default = "default_true")]
    pub me_on_right: bool,
    /// Give up on telling the sides apart: file everything as the other
    /// person's, the way the reference's whole-screen OCR mode does.
    #[serde(default)]
    pub all_from_other: bool,
}

fn default_top_crop() -> f32 {
    0.12
}
/// Status bars run about 2% on iOS and 4% on Android, and the name sits just
/// under them.
fn default_title_top() -> f32 {
    0.045
}
fn default_title_bottom() -> f32 {
    0.11
}
fn default_bottom_crop() -> f32 {
    0.84
}
fn default_min_confidence() -> f32 {
    0.5
}
fn default_line_gap() -> f32 {
    1.2
}
fn default_true() -> bool {
    true
}

impl Default for ScreenOptions {
    fn default() -> Self {
        Self {
            top_crop: default_top_crop(),
            bottom_crop: default_bottom_crop(),
            title_top: default_title_top(),
            title_bottom: default_title_bottom(),
            min_confidence: default_min_confidence(),
            line_gap: default_line_gap(),
            me_on_right: true,
            all_from_other: false,
        }
    }
}

/// The whole copilot configuration, one object per call.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct CopilotConfig {
    /// The judging route. This is the one that must be configured.
    #[serde(default)]
    pub judge: JevRoute,
    #[serde(default)]
    pub reply: ReplyRoute,
    /// Who the other person is, in the user's own words. Goes into the Jev
    /// state and into the drafting prompt.
    #[serde(default)]
    pub relationship: String,
    /// How many candidates to draft (the reference fixes this at 3).
    #[serde(default = "default_candidate_count")]
    pub candidate_count: usize,
    #[serde(default)]
    pub screen: ScreenOptions,
}

fn default_candidate_count() -> usize {
    3
}

impl CopilotConfig {
    /// Whether the judging route can be called at all. The reply key may fall
    /// back to the judge key, so only this one is required.
    pub fn has_judge_key(&self) -> bool {
        !self.judge.api_key.trim().is_empty()
    }

    pub fn candidates(&self) -> usize {
        self.candidate_count.clamp(1, 5)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_presets_pick_the_right_path() {
        let cases = [
            ("typesafe", "https://api.typesafe.ai/v1/systemone", "jev-latest"),
            ("bocha", "https://jev.bocha.cn/v1/systemone", "bocha-jev-v1"),
            ("vercel", "https://ai-gateway.vercel.sh/typesafe/v1/systemone", "typesafe-ai/jev"),
            ("zen", "https://opencode.ai/zen/v1/systemone", "jev-1.13"),
            ("openrouter", "https://openrouter.ai/api/alpha/decisions", "typesafe/jev-1.13"),
        ];
        for (provider, endpoint, model) in cases {
            let route = JevRoute {
                provider: provider.to_string(),
                ..Default::default()
            };
            assert_eq!(route.endpoint(), endpoint, "{provider}");
            assert_eq!(route.model(), model, "{provider}");
        }
    }

    #[test]
    fn an_unset_or_unknown_provider_behaves_like_openrouter() {
        // Base URL, model and path have to agree: a config that names no
        // provider must not take OpenRouter's model to TypeSafe's path.
        for provider in ["", "who-knows"] {
            let route = JevRoute {
                provider: provider.to_string(),
                ..Default::default()
            };
            assert_eq!(route.endpoint(), "https://openrouter.ai/api/alpha/decisions", "{provider:?}");
            assert_eq!(route.model(), "typesafe/jev-1.13", "{provider:?}");
        }
    }

    #[test]
    fn custom_takes_the_url_verbatim() {
        let route = JevRoute {
            provider: "custom".into(),
            base_url: "  http://localhost:8080/decide  ".into(),
            ..Default::default()
        };
        assert_eq!(route.endpoint(), "http://localhost:8080/decide");
    }

    #[test]
    fn a_configured_base_url_overrides_the_preset() {
        let route = JevRoute {
            provider: "typesafe".into(),
            base_url: "https://proxy.example.com/jev/".into(),
            ..Default::default()
        };
        assert_eq!(route.endpoint(), "https://proxy.example.com/jev/v1/systemone");
    }

    #[test]
    fn the_reply_route_falls_back_to_the_judge_key() {
        let judge = JevRoute {
            api_key: "judge-key".into(),
            ..Default::default()
        };
        let blank = ReplyRoute::default();
        assert_eq!(blank.effective_key(&judge), "judge-key");
        let own = ReplyRoute {
            api_key: "reply-key".into(),
            ..Default::default()
        };
        assert_eq!(own.effective_key(&judge), "reply-key");
        assert_eq!(own.endpoint(), "https://openrouter.ai/api/v1/chat/completions");
    }

    #[test]
    fn a_partial_config_still_deserializes() {
        // The frontend sends whatever the user has filled in so far; every
        // field but the routes themselves must survive being absent.
        let config: CopilotConfig = serde_json::from_str(r#"{"relationship":"同事"}"#).unwrap();
        assert_eq!(config.relationship, "同事");
        assert_eq!(config.candidates(), 3);
        assert_eq!(config.screen.top_crop, 0.12);
        assert!(config.screen.me_on_right);
        assert_eq!(config.judge.endpoint(), "https://openrouter.ai/api/alpha/decisions");
        assert_eq!(config.reply.model, "deepseek/deepseek-chat-v3.1");
    }
}
