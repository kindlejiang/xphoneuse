//! The conversation copilot: read a chat off the mirrored screen, judge it with
//! Jev, draft replies with a chat model, and let Jev rank them.
//!
//! The flow, in the order it happens:
//!
//! 1. the frontend notices the mirrored screen stop changing and asks for an
//!    analysis (it owns the only decoded pixels, in the WebCodecs canvas);
//! 2. the app captures a screenshot and this crate OCRs it
//!    ([`read_screen`]) into messages, deciding which side each bubble is on;
//! 3. [`transcript::merge`] folds the screen into the conversation history kept
//!    in memory - nothing here is written to disk;
//! 4. [`jev::judge`] answers seven questions about the conversation in one call
//!    (`questions::judge`), and the panel shows that immediately;
//! 5. [`reply::draft`] writes the candidates, and [`jev::rank`] scores them
//!    ([`draft_and_rank`]).
//!
//! Ported from <https://github.com/jev-chat/jev-chat-jarvis>, whose judgment
//! questions and prompts are the calibrated part and are copied verbatim. What
//! is different here: the reference reads Android's accessibility tree and
//! falls back to whole-screen OCR that cannot tell the speakers apart; this
//! crate reads pixels only, and gets the sides from the OCR boxes.
//!
//! Nothing in this crate sends a message. It produces text; putting it in the
//! phone's input box is the app's business, and pressing send is the user's.

use std::cmp::Ordering;

use serde::{Deserialize, Serialize};

pub mod config;
pub mod conversations;
pub mod http;
pub mod jev;
pub mod questions;
pub mod reply;
pub mod transcript;

pub use config::{CopilotConfig, JevRoute, ReplyRoute, ScreenOptions};
pub use conversations::Conversations;
pub use jev::Analysis;
pub use transcript::{Msg, Side};

/// One drafted reply, with the probability Jev gave it being the best.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Candidate {
    pub text: String,
    /// 0.0-1.0. Exactly 1.0 when there is only one candidate, which is not
    /// worth a ranking call, and 0.0 for every candidate when ranking failed.
    pub score: f32,
}

/// What one screenshot held.
///
/// The name comes out of the header band (see [`transcript::read_title`]) and
/// is `None` whenever the band did not give one up unambiguously - which is
/// most screens, on purpose. The caller treats a `None` as "no name available"
/// and keeps the conversation it was already in.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Reading {
    pub title: Option<String>,
    pub messages: Vec<Msg>,
}

/// A screenshot in, the conversation's name and its messages out. The OCR
/// itself is [`xphoneuse_ocr`], the same offline engine the `xphoneuse_ocr` tool
/// uses.
pub async fn read_screen(png: Vec<u8>, opts: &ScreenOptions) -> Result<Reading, String> {
    read_screen_with_keyboard(png, opts, None).await
}

/// The same, told where the on-screen keyboard is by the device itself: its y
/// in the screenshot's own pixels (see [`transcript::read_screen_with_keyboard`]).
///
/// This is how Android reads are done: the platform knows its input method's
/// frame exactly, and every guess this crate can make from the pixels is a
/// fallback for the platforms that do not say.
pub async fn read_screen_with_keyboard(
    png: Vec<u8>,
    opts: &ScreenOptions,
    keyboard: Option<f32>,
) -> Result<Reading, String> {
    let image = xphoneuse_ocr::recognize_png(png).await?;
    Ok(Reading {
        title: transcript::read_title(&image, opts),
        messages: transcript::read_screen_with_keyboard(&image, opts, keyboard),
    })
}

/// Draft the candidates and rank them with Jev.
///
/// Ranking is treated as a nice-to-have: if the judge route is unreachable or
/// answers with nothing usable, the drafts are still returned, in the order the
/// model wrote them, with a score of 0. The user asked for replies; losing them
/// because the second call failed would be the wrong trade.
pub async fn draft_and_rank(
    config: &CopilotConfig,
    messages: &[Msg],
    analysis: Option<&Analysis>,
) -> Result<Vec<Candidate>, String> {
    let texts = reply::draft(config, messages, analysis).await?;
    if texts.len() < 2 {
        return Ok(texts
            .into_iter()
            .map(|text| Candidate { text, score: 1.0 })
            .collect());
    }
    // Without a judging route there is nothing to rank with, and the drafting
    // prompt has already been asked for its own order (see `reply::draft`).
    // Calling anyway would spend a request to be told there is no key.
    if !config.has_judge_key() {
        return Ok(texts
            .into_iter()
            .map(|text| Candidate { text, score: 0.0 })
            .collect());
    }
    let scores = jev::rank(config, messages, &texts).await.unwrap_or_default();
    Ok(pair_with_scores(texts, &scores))
}

/// Attach the probabilities to the drafts and order them best first.
///
/// A candidate the judge did not score keeps 0.0 rather than being dropped: the
/// model wrote it, so the user gets to see it, just last.
fn pair_with_scores(texts: Vec<String>, scores: &[f64]) -> Vec<Candidate> {
    let mut candidates: Vec<Candidate> = texts
        .into_iter()
        .enumerate()
        .map(|(index, text)| Candidate {
            text,
            score: scores.get(index).copied().unwrap_or(0.0) as f32,
        })
        .collect();
    // A stable sort, so equally-scored candidates stay in the order the model
    // wrote them rather than shuffling between runs.
    candidates.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(Ordering::Equal));
    candidates
}

#[cfg(test)]
mod tests {
    use super::*;
    use xphoneuse_ocr::OcrImage;

    /// The whole reading path, minus the network: OCR boxes in, messages out.
    #[test]
    fn a_chat_screenshot_becomes_a_conversation() {
        let image = OcrImage {
            width: 1000,
            height: 2000,
            lines: vec![
                line("9:41", 450.0, 20.0, 100.0, 0.99),
                line("晚上一起吃饭？", 100.0, 700.0, 300.0, 0.98),
                line("好啊", 700.0, 800.0, 120.0, 0.97),
                line("昨天 20:15", 450.0, 900.0, 120.0, 0.99),
                line("我订了那家川菜", 620.0, 1000.0, 300.0, 0.96),
                line("发消息", 100.0, 1800.0, 150.0, 0.99),
            ],
        };
        let messages = transcript::read_screen(&image, &ScreenOptions::default());
        assert_eq!(
            messages.iter().map(|m| m.text.as_str()).collect::<Vec<_>>(),
            ["晚上一起吃饭？", "好啊", "我订了那家川菜"]
        );
        assert_eq!(transcript::latest_from(&messages), Side::Me);

        // And it survives a merge with what an earlier screen already gave us.
        let mut history = vec![messages[0].clone()];
        assert!(transcript::merge(&mut history, &messages));
        assert_eq!(history.len(), 3);
        assert!(!transcript::merge(&mut history, &messages));
    }

    fn line(text: &str, x: f32, y: f32, width: f32, confidence: f32) -> xphoneuse_ocr::OcrLine {
        xphoneuse_ocr::OcrLine {
            text: text.to_string(),
            confidence,
            bbox: [x, y, width, 40.0],
        }
    }

    #[test]
    fn candidates_come_back_best_first() {
        let texts = vec!["甲".to_string(), "乙".to_string(), "丙".to_string()];
        let candidates = pair_with_scores(texts, &[0.2, 0.7, 0.1]);
        assert_eq!(
            candidates.iter().map(|c| c.text.as_str()).collect::<Vec<_>>(),
            ["乙", "甲", "丙"]
        );
        assert_eq!(candidates[0].score, 0.7);
    }

    #[test]
    fn a_failed_ranking_keeps_the_drafts_in_the_order_they_were_written() {
        let texts = vec!["甲".to_string(), "乙".to_string(), "丙".to_string()];
        let candidates = pair_with_scores(texts, &[]);
        assert_eq!(
            candidates.iter().map(|c| c.text.as_str()).collect::<Vec<_>>(),
            ["甲", "乙", "丙"]
        );
        assert!(candidates.iter().all(|c| c.score == 0.0));
    }
}
