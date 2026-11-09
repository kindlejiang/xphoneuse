//! The Jev judgment route: seven questions in one call, plus the ranking
//! question over already-drafted candidates. Nothing generative here.
//!
//! Ported from the reference's `jev/JudgeClient.kt`. The parsing is deliberately
//! forgiving - every field is optional - because a decision model that answers
//! six of seven questions is still worth showing, and a single missing key must
//! not cost the user the whole analysis.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::config::CopilotConfig;
use crate::http::{self, ROUTE_JUDGE};
use crate::questions;
use crate::transcript::Msg;

/// What the seven answers say, in the panel's terms.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Analysis {
    /// Probability that the latest message is purely literal (no subtext).
    pub literal_question: Option<f32>,
    /// One of `confirm_you_care`, `vent_anger`, `request_action`,
    /// `seek_explanation`, `casual_chat`, `close_topic`.
    pub true_intent: Option<String>,
    pub true_intent_confidence: Option<f32>,
    /// Where the conversation sits on the danger scale, and the scale's top.
    pub danger_level: Option<f32>,
    pub danger_max: Option<f32>,
    pub danger_confidence: Option<f32>,
    /// Probability that the next message should carry substance (a fact, a
    /// time, a named fault) rather than hold the line.
    pub should_reply_now: Option<f32>,
    /// One of `check_history`, `apologize`, `give_commitment`, `explain`,
    /// `acknowledge`, `say_less`, `make_plan`.
    pub best_action: Option<String>,
    /// What the other person needs: `apology`, `action`, `explanation`,
    /// `care`, `nothing`. The question key on the wire is `she_needs` - kept
    /// verbatim, because the criteria wording is what was calibrated.
    pub other_needs: Option<String>,
    pub tension_resolved: Option<f32>,
}

/// The default top of the danger scale, used when the answer carries no legend
/// (the reference does the same: `JudgeClient.kt:114`).
const DEFAULT_DANGER_MAX: f32 = 9.0;

/// Ask the seven judgment questions.
pub async fn judge(config: &CopilotConfig, messages: &[Msg]) -> Result<Analysis, String> {
    let state = questions::state(messages, &config.relationship);
    let answers = decisions(config, &state, questions::judge()).await?;
    Ok(parse_analysis(&answers))
}

/// Ask which of the drafted candidates is the best next message. Returns one
/// probability per candidate, in the order they were sent.
pub async fn rank(
    config: &CopilotConfig,
    messages: &[Msg],
    candidates: &[String],
) -> Result<Vec<f64>, String> {
    let state = questions::state(messages, &config.relationship);
    let answers = decisions(config, &state, questions::rank(candidates)).await?;
    Ok(questions::rank_probabilities(&answers, candidates.len()))
}

/// The settings page's connectivity test for the judging route: one trivial
/// question, so the test exercises the real protocol rather than a health
/// endpoint that might not exist.
pub async fn ping(config: &CopilotConfig) -> Result<String, String> {
    let state = serde_json::json!({ "chat": { "messages": [], "latest_from": "other" } });
    let questions = serde_json::json!({
        "ping": {
            "type": "noul",
            "instructions": format!("Is this a connectivity test?{}", questions::BACKGROUND_NOTE),
            "criteria": {
                "true": "This is a test of the connection.",
                "false": "This is a real conversation.",
            },
        }
    });
    let answers = decisions(config, &state, questions).await?;
    let value = noul(&answers, "ping");
    Ok(match value {
        Some(probability) => format!("{:.0}%", probability * 100.0),
        None => "answered, but without a usable value".to_string(),
    })
}

/// One decisions round trip; returns the `answers` object.
async fn decisions(config: &CopilotConfig, state: &Value, questions: Value) -> Result<Value, String> {
    if !config.has_judge_key() {
        return Err("the judge route has no API key".to_string());
    }
    let body = serde_json::json!({
        "model": config.judge.model(),
        "state": state,
        "questions": questions,
    });
    let response = http::post_json(
        &config.judge.endpoint(),
        config.judge.api_key.trim(),
        &body,
        ROUTE_JUDGE,
    )
    .await?;
    Ok(response.get("answers").cloned().unwrap_or(Value::Null))
}

/// Read the seven answers into [`Analysis`]. Never fails: an answer that is
/// missing or of the wrong shape stays `None`.
pub fn parse_analysis(answers: &Value) -> Analysis {
    let (true_intent, true_intent_confidence) = choice(answers, "true_intent");
    let (best_action, _) = choice(answers, "best_action");
    let (other_needs, _) = choice(answers, "she_needs");
    let (danger_level, danger_max, danger_confidence) = score(answers, "danger_level");
    Analysis {
        literal_question: noul(answers, "literal_question"),
        true_intent,
        true_intent_confidence,
        danger_level,
        danger_max,
        danger_confidence,
        should_reply_now: noul(answers, "should_reply_now"),
        best_action,
        other_needs,
        tension_resolved: noul(answers, "tension_resolved"),
    }
}

/// `noul` answers: the probability that the statement is true.
fn noul(answers: &Value, key: &str) -> Option<f32> {
    answers.get(key)?.get("noul")?.as_f64().map(|v| v as f32)
}

/// `choice` answers: the chosen key and its confidence.
fn choice(answers: &Value, key: &str) -> (Option<String>, Option<f32>) {
    let Some(answer) = answers.get(key) else {
        return (None, None);
    };
    let chosen = answer
        .get("choice")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let confidence = answer.get("confidence").and_then(Value::as_f64).map(|v| v as f32);
    (chosen, confidence)
}

/// `score` answers: the score, the top of the scale (from the returned legend,
/// so a different scale needs no code change) and the confidence.
fn score(answers: &Value, key: &str) -> (Option<f32>, Option<f32>, Option<f32>) {
    let Some(answer) = answers.get(key) else {
        return (None, None, None);
    };
    let level = answer.get("score").and_then(Value::as_f64).map(|v| v as f32);
    let max = answer
        .get("legend")
        .and_then(Value::as_object)
        .and_then(|legend| {
            legend
                .keys()
                .filter_map(|level| level.parse::<f32>().ok())
                .fold(None, |max: Option<f32>, level| Some(max.map_or(level, |m| m.max(level))))
        })
        .or(if level.is_some() { Some(DEFAULT_DANGER_MAX) } else { None });
    let confidence = answer.get("confidence").and_then(Value::as_f64).map(|v| v as f32);
    (level, max, confidence)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_full_answer_set_is_read_into_every_field() {
        let answers = json!({
            "literal_question": { "noul": 0.2, "confidence": 0.9 },
            "true_intent": { "choice": "request_action", "confidence": 0.88,
                             "probabilities": { "request_action": 0.88, "casual_chat": 0.12 } },
            "danger_level": { "score": 4.0, "confidence": 0.8,
                              "legend": { "1": "a", "2": "b", "3": "c", "4": "d", "5": "e",
                                          "6": "f", "7": "g", "8": "h", "9": "i", "10": "j" } },
            "should_reply_now": { "noul": 0.71 },
            "best_action": { "choice": "give_commitment", "confidence": 0.7 },
            "she_needs": { "choice": "action", "confidence": 0.66 },
            "tension_resolved": { "noul": 0.15 }
        });
        let analysis = parse_analysis(&answers);
        assert_eq!(analysis.literal_question, Some(0.2));
        assert_eq!(analysis.true_intent.as_deref(), Some("request_action"));
        assert_eq!(analysis.true_intent_confidence, Some(0.88));
        assert_eq!(analysis.danger_level, Some(4.0));
        assert_eq!(analysis.danger_max, Some(10.0));
        assert_eq!(analysis.danger_confidence, Some(0.8));
        assert_eq!(analysis.should_reply_now, Some(0.71));
        assert_eq!(analysis.best_action.as_deref(), Some("give_commitment"));
        assert_eq!(analysis.other_needs.as_deref(), Some("action"));
        assert_eq!(analysis.tension_resolved, Some(0.15));
    }

    #[test]
    fn a_partial_answer_set_leaves_the_rest_empty() {
        let analysis = parse_analysis(&json!({ "she_needs": { "choice": "nothing" } }));
        assert_eq!(analysis.other_needs.as_deref(), Some("nothing"));
        assert_eq!(analysis.true_intent, None);
        assert_eq!(analysis.danger_level, None);
        assert_eq!(analysis.danger_max, None);
    }

    #[test]
    fn a_score_without_a_legend_falls_back_to_the_reference_scale() {
        let analysis = parse_analysis(&json!({ "danger_level": { "score": 7.0 } }));
        assert_eq!(analysis.danger_level, Some(7.0));
        assert_eq!(analysis.danger_max, Some(DEFAULT_DANGER_MAX));
    }

    #[test]
    fn a_non_numeric_answer_is_ignored_rather_than_guessed() {
        let analysis = parse_analysis(&json!({
            "danger_level": { "score": "high" },
            "true_intent": { "choice": "" },
        }));
        assert_eq!(analysis.danger_level, None);
        assert_eq!(analysis.danger_max, None);
        assert_eq!(analysis.true_intent, None);
    }

    #[test]
    fn an_empty_answers_object_produces_an_empty_analysis() {
        assert_eq!(parse_analysis(&Value::Null), Analysis::default());
        assert_eq!(parse_analysis(&json!({})), Analysis::default());
    }
}
