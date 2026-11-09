//! The Jev question set, ported verbatim from the reference implementation
//! (`jev/JevQuestions.kt`, itself ported from `tools/jev/questions.py` - this is
//! the wording that passed calibration, so it is not paraphrased).
//!
//! Two rules that look like style but are not:
//!
//! * instructions and criteria stay in English - Jev's primary training
//!   language - while the chat text inside `state` stays in the language the
//!   user actually types;
//! * every question carries [`BACKGROUND_NOTE`], so that context handed to the
//!   model through `background` reads as given rather than as an off-topic
//!   digression to be penalized.

use serde_json::{json, Map, Value};

use crate::transcript::{Msg, Side};

/// Appended to every question.
pub const BACKGROUND_NOTE: &str = " Facts given in background are provided context, not off-topic.";

/// The keys the ranking question uses, in order. Three is the reference's
/// count; the settings allow up to five.
const RANK_KEYS: &[&str] = &["reply_a", "reply_b", "reply_c", "reply_d", "reply_e"];

/// The criterion key for candidate `index`.
pub fn rank_key(index: usize) -> &'static str {
    RANK_KEYS.get(index).copied().unwrap_or("reply_a")
}

fn noul(instructions: &str, yes: &str, no: &str) -> Value {
    json!({
        "type": "noul",
        "instructions": format!("{instructions}{BACKGROUND_NOTE}"),
        "criteria": { "true": yes, "false": no },
    })
}

fn choice(instructions: &str, criteria: &[(&str, &str)]) -> Value {
    let criteria: Map<String, Value> = criteria
        .iter()
        .map(|(key, value)| ((*key).to_string(), Value::String((*value).to_string())))
        .collect();
    json!({
        "type": "choice",
        "instructions": format!("{instructions}{BACKGROUND_NOTE}"),
        "criteria": Value::Object(criteria),
    })
}

fn score(instructions: &str, levels: &[&str]) -> Value {
    json!({
        "type": "score",
        "instructions": format!("{instructions}{BACKGROUND_NOTE}"),
        "criteria": levels,
    })
}

/// The seven judgment questions, asked in one call (~1s in the reference).
pub fn judge() -> Value {
    json!({
        "literal_question": noul(
            "Is the other person's latest message meant purely literally, with no subtext? \
             Judge from the whole thread, not one sentence in isolation.",
            "The latest message is a straightforward statement, question, or plan \
             with no implied accusation, test, sarcasm, hint, or unsaid request.",
            "There is subtext: a test of whether you remember or care, sarcasm, \
             an implied complaint, a hint they will not say outright, a trap question, \
             an accusation dressed as a question, or a cold/short line that really means blame.",
        ),
        "true_intent": choice(
            "What is the other person's true intent in the latest message, given the full conversation? \
             Prefer tone and context over surface wording. \
             If they are checking whether you remember something or still care, choose confirm_you_care \
             even if the words look like a request to 'say it' or to do something. \
             If they already accepted and closed the matter peacefully, choose close_topic. \
             Ending the relationship, deleting you, or 'don't talk to me' is vent_anger, never close_topic.",
            &[
                ("confirm_you_care", "They are testing whether you remember, pay attention, or still care. \
                    Signals: 'did you forget again', 'then say it', 'you better', sarcastic 'busy person', \
                    asking you to prove you know a past conversation. \
                    If they mainly want a new deliverable or a yes on a time, do not use this."),
                ("vent_anger", "They are angry or hurt and mainly want the feeling acknowledged. \
                    They are blaming or raising the temperature; a specific plan is not the main point yet."),
                ("request_action", "They want a concrete action, time, deliverable, or commitment from you now, \
                    and this is a real ask, not a loyalty test."),
                ("seek_explanation", "They want a factual explanation of why something happened. \
                    They asked why or what is going on, not mainly for an apology or a new plan."),
                ("casual_chat", "Light talk, banter, sharing, teasing with a laugh, or friendly logistics \
                    with no emotional test and no conflict. A friend suggesting a meal time can be this \
                    if the thread is warm."),
                ("close_topic", "Peaceful wrap-up only: they accepted an apology, confirmed a happy plan, said thanks, \
                    or clearly signaled they need nothing more. \
                    Not a breakup, not 'don't contact me', not sarcastic 'I'm used to it'."),
            ],
        ),
        "danger_level": score(
            "How close is this conversation to a fight or to hurting the relationship? \
             Match the current scene. \
             If they genuinely accepted an apology or confirmed a happy plan, score the cooled-down present, \
             not an earlier complaint. \
             If an ultimatum (break up, report to the boss, stop covering for you) is still in force \
             and has not been withdrawn, stay in that high bin even if the latest line names a specific task.",
            &[
                "Light chat or joking; no complaint, no test, no deadline.",
                "Mild tease or a small reminder that is easy to laugh off; a clumsy reply would only feel slightly awkward.",
                "A mild complaint or 'please remember next time' said without heat; they still send warm or practical follow-ups.",
                "Noticeable unhappiness; they mention being forgotten, ignored, or kept waiting, but still give you a chance to make it right.",
                "Sarcasm, cold short replies, or 'you better'; they are testing you, and a sloppy or fake-confident reply will escalate.",
                "Openly upset; they accuse you of not listening or not caring; they expect a real response, not a joke.",
                "Clearly angry and blaming you; a wrong reply will turn this into a fight.",
                "Last-chance warning. They will not cover for you, do not want to keep talking unless this changes, \
                 or tell you to finish a named checklist yourself because trust is almost gone.",
                "An ultimatum is already on the table even if they also give a practical next step: \
                 break up if you forget again, report you tonight, or stop working together if you miss this.",
                "Active rupture: they said it is over, told you not to reply, deleted you, or are exploding.",
            ],
        ),
        "should_reply_now": noul(
            "Should your next message contain substantive content? \
             Substantive means: admitting a specific known fault, giving a concrete time/plan/deliverable, \
             explaining facts you actually know, or reciting the recalled content they asked you to say. \
             This is NOT 'should you send any message'. Timing is irrelevant. \
             Answer FALSE if the thing they want you to recite or prove is not present in this snippet \
             (you would be guessing). 'Then say it' / 'you better' while you are stalling is FALSE. \
             Answer FALSE if they already accepted and closed the topic. \
             Answer true only if the needed fact, plan, or named fault is already in this snippet.",
            "The needed fact, named fault, or named time/place is already in this snippet, \
             and they are waiting for that substance now.",
            "Do not put substance in the next message: the recalled content is not in this snippet, \
             they are testing whether you remember, a holding line is enough, \
             saying less is safer, or they already closed the topic.",
        ),
        "best_action": choice(
            "What type of next action is best? Do not decide whether to send a message immediately. \
             Ignore timing. Choose only the action type. \
             If they asked you to recall a specific past message or event and you have not shown that you actually remember it, \
             choose check_history - do not apologize or invent a plan instead.",
            &[
                ("check_history", "Look up prior chat or facts before taking a position. \
                    Use when they ask you to repeat, recall, or prove you remember something specific."),
                ("apologize", "Lead with a sincere apology for a real mistake or hurt already identified. \
                    Not for an unnamed forgotten thing when you should first find out what it was."),
                ("give_commitment", "Give a concrete promise, deadline, or arrangement they asked for \
                    in a conflict or work-pressure setting."),
                ("explain", "Explain what happened or why, without leading with apology or a new plan."),
                ("acknowledge", "Show you heard them and care, without new facts, an apology, or a plan. \
                    Use for light chat or when they mainly need to feel seen."),
                ("say_less", "Keep it short or add nothing. Extra words would over-explain, reopen a closed topic, \
                    or pour fuel on an ultimatum that told you not to talk."),
                ("make_plan", "Propose or confirm logistics (time, place, task) for a non-conflict request \
                    such as a meal or a meeting."),
            ],
        ),
        "she_needs": choice(
            "What does the other person need from you right now? Judge the LATEST message first. \
             If they genuinely accepted (thanks / got it / 没事了 / 那就这样 / 收到了 / 过去了), \
             you MUST choose nothing, even if earlier they wanted action or an apology. \
             Sarcastic 'I'm used to it', 'whatever', 'I don't want to hear it', 'don't bother coming' \
             is NOT genuine satisfaction - do not choose nothing. \
             If they asked you to recap a named time/place/date, choose action. \
             If they are testing whether you remember or still care, and the content is unnamed, choose care.",
            &[
                ("apology", "They need a sincere apology for hurt or a mistake, and they have not accepted one yet."),
                ("action", "They need a concrete action, time, commitment, recap of a named fact, or follow-through, \
                    and they have not yet accepted one."),
                ("explanation", "They need a clear explanation of what happened or why, and have not received it."),
                ("care", "They need proof you remember, listen, or care - a loyalty or attention test - \
                    not yet a plan or an apology. Sarcastic 'I am used to it' belongs here, not nothing."),
                ("nothing", "They need nothing further. Genuine acceptance, a peaceful closed topic, \
                    warm casual chat with no ask, or a rupture where they told you not to reply. \
                    Not sarcasm pretending to be fine."),
            ],
        ),
        "tension_resolved": noul(
            "Has interpersonal tension already been resolved? \
             Answer true only if there was never tension, or the other person has clearly accepted, \
             cooled down, joked again, or said it is fine. \
             A sarcastic 'you better', an unanswered test, leftover blame, or an open ultimatum means false.",
            "No remaining tension: they accepted, joked again, said it's fine, \
             confirmed a happy plan, or the chat was never tense.",
            "Tension is still present: they are waiting, testing, angry, sarcastic, \
             issuing an ultimatum, or the issue is open.",
        ),
    })
}

/// How many messages of the conversation Jev sees.
pub const STATE_WINDOW: usize = 10;

/// The `state` object: who the other person is and the last few messages.
///
/// The reference also carries an optional knowledge base (`background`,
/// `history`); XPhoneUse has none, so the body stays exactly what the reference
/// sends for a user who never turned it on.
pub fn state(messages: &[Msg], relationship: &str) -> Value {
    let start = messages.len().saturating_sub(STATE_WINDOW);
    let recent = &messages[start..];
    let messages: Vec<Value> = recent
        .iter()
        .map(|msg| json!({ "from": msg.from.key(), "text": msg.text }))
        .collect();
    json!({
        "chat": {
            "relationship": relationship,
            "messages": messages,
            "latest_from": recent.last().map(|msg| msg.from).unwrap_or(Side::Other).key(),
        }
    })
}

/// The ranking question: which of the drafted candidates is the best next
/// message. The candidate texts stay in the language they were drafted in.
pub fn rank(candidates: &[String]) -> Value {
    let criteria: Map<String, Value> = candidates
        .iter()
        .enumerate()
        .map(|(index, text)| (rank_key(index).to_string(), Value::String(text.clone())))
        .collect();
    json!({
        "best_reply": {
            "type": "choice",
            "instructions": format!(
                "Which candidate reply is the most appropriate next message, \
                 given the conversation and the other person's true need? \
                 Prefer a reply that matches the best action type. \
                 Penalize dismissive, over-promising, or off-topic replies. \
                 If the facts are not yet confirmed, prefer the candidate that looks them up \
                 instead of faking memory or a vague apology.{BACKGROUND_NOTE}"
            ),
            "criteria": Value::Object(criteria),
        }
    })
}

/// The probability Jev gave each candidate, in the order they were sent.
pub fn rank_probabilities(answers: &Value, count: usize) -> Vec<f64> {
    let probabilities = answers.get("best_reply").and_then(|v| v.get("probabilities"));
    (0..count)
        .map(|index| {
            probabilities
                .and_then(|p| p.get(rank_key(index)))
                .and_then(Value::as_f64)
                .unwrap_or(0.0)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transcript::Msg;

    #[test]
    fn every_question_carries_the_background_note() {
        let questions = judge();
        let map = questions.as_object().expect("the questions are an object");
        assert_eq!(map.len(), 7);
        for (key, question) in map {
            let instructions = question["instructions"].as_str().expect("instructions");
            assert!(instructions.ends_with(BACKGROUND_NOTE), "{key}");
        }
    }

    #[test]
    fn the_question_types_match_the_reference() {
        let questions = judge();
        let expected = [
            ("literal_question", "noul"),
            ("true_intent", "choice"),
            ("danger_level", "score"),
            ("should_reply_now", "noul"),
            ("best_action", "choice"),
            ("she_needs", "choice"),
            ("tension_resolved", "noul"),
        ];
        for (key, kind) in expected {
            assert_eq!(questions[key]["type"], kind, "{key}");
        }
    }

    #[test]
    fn the_choice_and_score_options_are_all_there() {
        let questions = judge();
        assert_eq!(questions["true_intent"]["criteria"].as_object().unwrap().len(), 6);
        assert_eq!(questions["best_action"]["criteria"].as_object().unwrap().len(), 7);
        assert_eq!(questions["she_needs"]["criteria"].as_object().unwrap().len(), 5);
        // The score levels are an ordered array, not a map: their order is the
        // scale, so it has to survive serialization.
        let levels = questions["danger_level"]["criteria"].as_array().unwrap();
        assert_eq!(levels.len(), 10);
        assert!(levels[0].as_str().unwrap().starts_with("Light chat"));
        assert!(levels[9].as_str().unwrap().starts_with("Active rupture"));
        assert_eq!(questions["literal_question"]["criteria"]["true"].as_str().is_some(), true);
    }

    #[test]
    fn the_state_carries_the_last_ten_messages_and_the_latest_side() {
        let messages: Vec<Msg> = (0..14)
            .map(|i| Msg {
                from: if i % 2 == 0 { Side::Other } else { Side::Me },
                text: i.to_string(),
            })
            .collect();
        let state = state(&messages, "同事");
        let chat = &state["chat"];
        assert_eq!(chat["relationship"], "同事");
        let carried = chat["messages"].as_array().unwrap();
        assert_eq!(carried.len(), STATE_WINDOW);
        assert_eq!(carried[0]["text"], "4");
        assert_eq!(carried[0]["from"], "other");
        assert_eq!(chat["latest_from"], "me");
    }

    #[test]
    fn an_empty_conversation_still_builds_a_state() {
        let state = state(&[], "伴侣");
        assert_eq!(state["chat"]["messages"].as_array().unwrap().len(), 0);
        assert_eq!(state["chat"]["latest_from"], "other");
    }

    #[test]
    fn the_ranking_question_names_the_candidates_in_order() {
        let candidates = vec!["甲".to_string(), "乙".to_string(), "丙".to_string()];
        let question = &rank(&candidates)["best_reply"];
        assert_eq!(question["type"], "choice");
        assert_eq!(question["criteria"]["reply_a"], "甲");
        assert_eq!(question["criteria"]["reply_c"], "丙");
        assert!(question["instructions"].as_str().unwrap().ends_with(BACKGROUND_NOTE));
    }

    #[test]
    fn the_probabilities_come_back_in_the_order_they_were_sent() {
        let answers = json!({
            "best_reply": { "choice": "reply_b", "confidence": 0.9,
                            "probabilities": { "reply_a": 0.1, "reply_b": 0.7, "reply_c": 0.2 } }
        });
        assert_eq!(rank_probabilities(&answers, 3), vec![0.1, 0.7, 0.2]);
        // A missing or malformed answer must not lose the candidates.
        assert_eq!(rank_probabilities(&json!({}), 2), vec![0.0, 0.0]);
    }
}
