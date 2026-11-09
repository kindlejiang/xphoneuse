//! The generative route: any OpenAI-compatible `/chat/completions` endpoint.
//! Drafts the candidate replies.
//!
//! Ported from the reference's `jev/ReplyClient.kt`, with one addition: the Jev
//! judgement is handed to the drafting model as well. The reference uses the
//! judgement only to rank what was drafted, so the drafts are written blind to
//! it - they can come back proposing a concrete time for a message the judge
//! said should carry no substance at all. One short block fixes that, and it is
//! what "use Jev to decide" has to mean if it is to mean anything.

use serde_json::{json, Value};

use crate::config::CopilotConfig;
use crate::http::{self, ROUTE_REPLY};
use crate::jev::Analysis;
use crate::transcript::{Msg, Side};

/// How many messages the drafting prompt carries.
const PROMPT_WINDOW: usize = 10;

/// Used to top up a short answer, so the panel always has something to show.
///
/// The reference's placeholder is a Chinese sentence ("（稍等，我看下）"), which
/// is the right language only when the conversation is Chinese. A bare ellipsis
/// claims nothing and cannot be in the wrong language: what is missing here is a
/// candidate, not a reply, and the drafts above it are the ones meant to be
/// sent.
const PLACEHOLDER: &str = "…";

/// Draft the candidate replies. `analysis` is the judgement that was already
/// shown to the user, or `None` when only drafting was asked for.
pub async fn draft(
    config: &CopilotConfig,
    messages: &[Msg],
    analysis: Option<&Analysis>,
) -> Result<Vec<String>, String> {
    let count = config.candidates();
    let key = config.reply.effective_key(&config.judge);
    if key.is_empty() {
        return Err("the reply route has no API key".to_string());
    }

    let conversation = conversation_block(messages);
    let mut user = String::new();
    if let Some(analysis) = analysis {
        user.push_str(&judgement_block(analysis));
    }
    if !config.relationship.trim().is_empty() {
        user.push_str(&format!("关系：{}\n\n", config.relationship.trim()));
    }
    // The language rule is in the system prompt too; it is repeated here, next
    // to the task, because this is the instruction the model applies to the
    // text directly above it.
    user.push_str(&format!(
        "最近对话：\n{conversation}\n\n请给出 {count} 条候选回复（用上面这段对话的语言写）。"
    ));

    let body = json!({
        "model": config.reply.model,
        "messages": [
            {
                "role": "system",
                "content": system_prompt(count, !config.has_judge_key())
            },
            { "role": "user", "content": user },
        ],
        // The reference's 0.8: high enough that the candidates differ from each
        // other, which is the whole point of asking for several.
        "temperature": 0.8,
    });

    let response = http::post_json(&config.reply.endpoint(), key, &body, ROUTE_REPLY).await?;
    let content = response
        .get("choices")
        .and_then(|choices| choices.get(0))
        .and_then(|choice| choice.get("message"))
        .and_then(|message| message.get("content"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    if content.trim().is_empty() {
        return Err("the reply model returned no content".to_string());
    }
    Ok(parse_candidates(content, count))
}

/// The settings page's connectivity test: one ordinary chat round trip, so the
/// test exercises the path the drafts actually take.
pub async fn ping(config: &CopilotConfig) -> Result<String, String> {
    let key = config.reply.effective_key(&config.judge);
    if key.is_empty() {
        return Err("the reply route has no API key".to_string());
    }
    let body = json!({
        "model": config.reply.model,
        "messages": [
            { "role": "system", "content": "你是连通性测试助手，只按要求回答，不要解释。" },
            { "role": "user", "content": "请只回复两个字：收到" },
        ],
        "temperature": 0.0,
    });
    let response = http::post_json(&config.reply.endpoint(), key, &body, ROUTE_REPLY).await?;
    let content = response
        .get("choices")
        .and_then(|choices| choices.get(0))
        .and_then(|choice| choice.get("message"))
        .and_then(|message| message.get("content"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string();
    if content.is_empty() {
        return Err("the reply model returned no content".to_string());
    }
    Ok(content)
}

/// The reference's system prompt, with the count made variable and the language
/// left to the conversation rather than assumed.
///
/// The reference is a Chinese app talking to Chinese chats, so its prompt says
/// so ("你是中文即时通讯回复助手"). Copied here, that turned every English
/// conversation into Chinese drafts: the model saw one language in the system
/// message and another in the chat, and followed the system message. The
/// language is now a rule about the conversation instead, which the model reads
/// off the messages themselves - better than any guess this side could make,
/// and it keeps working for the languages nobody thought to list.
///
/// `orders` is set when there is no judging route to rank the candidates
/// afterwards: the model is then the only one who can say which of its own
/// replies it would send first, so it is asked to put them in that order. With
/// a judging route the order here does not matter - Jev re-ranks them - and
/// telling the model otherwise would only bias what it writes.
pub fn system_prompt(count: usize, orders: bool) -> String {
    let strategies = match count {
        1 => "一条稳妥承接".to_string(),
        2 => "例如：一条稳妥承接、一条给具体行动或承诺".to_string(),
        3 => "例如：一条稳妥承接、一条给具体行动或承诺、一条简短低姿态".to_string(),
        _ => "例如：一条稳妥承接、一条给具体行动或承诺、一条简短低姿态、一条追问细节、一条明确拒绝"
            .to_string(),
    };
    // Only asked for when nobody else will rank them: with a judging route Jev
    // re-orders the candidates anyway, and an order asked for here would only
    // bias what the model writes.
    let order = if orders {
        "数组顺序就是推荐顺序：最该发的那条放第一个，其余按此排列。"
    } else {
        ""
    };
    format!(
        "你是即时通讯回复助手。只输出一个 JSON 数组，含且仅含 {count} 条候选回复文本，\
         {count} 条策略要有区别（{strategies}）。{order}\
         这些回复要用对话本身的语言写：对话用英文就写英文，用日文就写日文，\
         不要翻译成别的语言（这个界面的语言和它无关）。\
         每条都短，像平时打字那样（中文 40 字以内，其他语言是相当的长度），\
         口语、自然、像真人在聊天软件里发消息。\
         不要解释，不要加引号以外的内容，直接输出 JSON 数组。"
    )
}

/// The conversation as the drafting model sees it.
fn conversation_block(messages: &[Msg]) -> String {
    let start = messages.len().saturating_sub(PROMPT_WINDOW);
    messages[start..]
        .iter()
        .map(|msg| {
            let who = match msg.from {
                Side::Me => "我",
                Side::Other => "对方",
            };
            format!("{who}：{}", msg.text)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The judgement, in the drafting prompt. Only the lines that carry an answer
/// are included, and the two guidance lines fire only when they apply.
fn judgement_block(analysis: &Analysis) -> String {
    let mut lines: Vec<String> = Vec::new();
    if let Some(intent) = &analysis.true_intent {
        lines.push(format!("- 对方真实意图：{intent}"));
    }
    if let Some(level) = analysis.danger_level {
        let max = analysis.danger_max.unwrap_or(9.0);
        lines.push(format!("- 危险程度：{level:.0}/{max:.0}"));
    }
    if let Some(needs) = &analysis.other_needs {
        lines.push(format!("- 对方需要：{needs}"));
    }
    if let Some(action) = &analysis.best_action {
        lines.push(format!("- 建议动作：{action}"));
    }
    if let Some(probability) = analysis.should_reply_now {
        lines.push(format!("- 现在就该给实质内容：{}", yes_no(probability >= 0.5)));
    }
    if let Some(probability) = analysis.literal_question {
        lines.push(format!("- 只是字面意思：{}", yes_no(probability >= 0.5)));
    }
    if let Some(probability) = analysis.tension_resolved {
        lines.push(format!("- 紧张已化解：{}", yes_no(probability >= 0.5)));
    }
    if lines.is_empty() {
        return String::new();
    }

    let mut block = format!(
        "判定模型对这段对话的结论（英文键名按字面理解，起草必须与之一致）：\n{}\n",
        lines.join("\n")
    );
    if analysis.best_action.as_deref() == Some("check_history") {
        block.push_str("建议动作是 check_history：不要编造记忆或具体往事，改为先确认或询问细节。\n");
    }
    if analysis.should_reply_now.is_some_and(|probability| probability < 0.5) {
        block.push_str("现在不该给实质内容：不要承诺具体时间，不要编造事实，一条承接的话就够。\n");
    }
    block.push('\n');
    block
}

fn yes_no(value: bool) -> &'static str {
    if value {
        "是"
    } else {
        "否"
    }
}

/// Read the drafted candidates out of whatever the model returned.
///
/// The reference's two-step parse: find the JSON array first, and only if that
/// fails fall back to one candidate per line. Both paths top up to `count` with
/// [`PLACEHOLDER`], so the panel is never left with nothing to show.
pub fn parse_candidates(content: &str, count: usize) -> Vec<String> {
    let count = count.max(1);

    if let (Some(start), Some(end)) = (content.find('['), content.rfind(']')) {
        if end > start {
            if let Ok(Value::Array(items)) = serde_json::from_str::<Value>(&content[start..=end]) {
                let out = items
                    .iter()
                    .filter_map(Value::as_str)
                    .map(|text| text.trim().to_string())
                    .filter(|text| !text.is_empty())
                    .take(count)
                    .collect();
                return pad(out, count);
            }
        }
    }

    let out = content
        .lines()
        .map(|line| strip_list_marker(line).trim_matches('"').trim().to_string())
        .filter(|line| !line.is_empty())
        .take(count)
        .collect();
    pad(out, count)
}

/// Tops a short answer up with [`PLACEHOLDER`].
fn pad(mut out: Vec<String>, count: usize) -> Vec<String> {
    if out.len() < count {
        out.extend(std::iter::repeat_n(PLACEHOLDER.to_string(), count - out.len()));
    }
    out
}

/// Drops a leading `- `, `* ` or `1. ` from a line of the fallback path.
///
/// The reference trims the characters `-*123. "` off the front, which eats the
/// first digit of a candidate that legitimately starts with one ("3点见" comes
/// back as "点见"). Matching a marker instead costs three lines.
fn strip_list_marker(line: &str) -> &str {
    let trimmed = line.trim();
    for marker in ['-', '*'] {
        if let Some(rest) = trimmed.strip_prefix(marker) {
            return rest.trim_start();
        }
    }
    let digits = trimmed
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(trimmed.len());
    if digits > 0 {
        if let Some(rest) = trimmed[digits..].strip_prefix(['.', '、', ')', '）']) {
            return rest.trim_start();
        }
    }
    trimmed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_json_array_is_read_as_is() {
        let content = r#"["好的，我看下","我下午三点前发你","抱歉，是我疏忽了"]"#;
        assert_eq!(
            parse_candidates(content, 3),
            ["好的，我看下", "我下午三点前发你", "抱歉，是我疏忽了"]
        );
    }

    #[test]
    fn a_json_array_wrapped_in_prose_is_still_found() {
        let content = "好的，这是三条回复：\n[\"甲\", \"乙\", \"丙\"]\n希望有帮助。";
        assert_eq!(parse_candidates(content, 3), ["甲", "乙", "丙"]);
    }

    #[test]
    fn a_short_array_is_topped_up() {
        let content = r#"["只有一条"]"#;
        assert_eq!(parse_candidates(content, 3), ["只有一条", PLACEHOLDER, PLACEHOLDER]);
    }

    #[test]
    fn a_non_json_answer_falls_back_to_one_candidate_per_line() {
        let content = "1. 好的，我看下\n2. 我下午三点前发你\n3. 抱歉，是我疏忽了";
        assert_eq!(
            parse_candidates(content, 3),
            ["好的，我看下", "我下午三点前发你", "抱歉，是我疏忽了"]
        );
    }

    #[test]
    fn the_fallback_keeps_a_candidate_that_starts_with_a_digit() {
        let content = "- 3点见吧\n- 行，没问题";
        assert_eq!(parse_candidates(content, 2), ["3点见吧", "行，没问题"]);
    }

    #[test]
    fn an_empty_answer_becomes_placeholders_rather_than_nothing() {
        assert_eq!(parse_candidates("", 2), [PLACEHOLDER, PLACEHOLDER]);
        assert_eq!(parse_candidates("   \n  ", 2), [PLACEHOLDER, PLACEHOLDER]);
    }

    #[test]
    fn the_prompt_asks_for_the_number_of_candidates_the_user_set() {
        assert!(system_prompt(3, false).contains("含且仅含 3 条"));
        assert!(system_prompt(5, false).contains("含且仅含 5 条"));
        assert!(system_prompt(1, false).contains("一条稳妥承接"));
        // The reply language follows the conversation, not this prompt or the
        // app's own language setting.
        assert!(system_prompt(3, false).contains("用对话本身的语言写"));
        // Without a judging route the model is asked to order its own answers,
        // and with one it is not: Jev re-ranks them anyway, and an order asked
        // for here would only bias what it writes.
        assert!(system_prompt(3, true).contains("数组顺序就是推荐顺序"));
        assert!(!system_prompt(3, false).contains("推荐顺序"));
    }

    #[test]
    fn the_judgement_reaches_the_prompt_and_fires_the_guidance_lines() {
        let analysis = Analysis {
            true_intent: Some("confirm_you_care".into()),
            danger_level: Some(5.0),
            danger_max: Some(10.0),
            best_action: Some("check_history".into()),
            other_needs: Some("care".into()),
            should_reply_now: Some(0.2),
            tension_resolved: Some(0.1),
            literal_question: Some(0.05),
            ..Default::default()
        };
        let block = judgement_block(&analysis);
        assert!(block.contains("对方真实意图：confirm_you_care"));
        assert!(block.contains("危险程度：5/10"));
        assert!(block.contains("对方需要：care"));
        assert!(block.contains("建议动作：check_history"));
        assert!(block.contains("现在就该给实质内容：否"));
        assert!(block.contains("紧张已化解：否"));
        assert!(block.contains("不要编造记忆"));
        assert!(block.contains("不要承诺具体时间"));
    }

    #[test]
    fn an_empty_judgement_contributes_nothing() {
        assert_eq!(judgement_block(&Analysis::default()), "");
    }

    #[test]
    fn the_conversation_block_keeps_the_last_ten_messages() {
        let messages: Vec<Msg> = (0..14)
            .map(|i| Msg {
                from: if i % 2 == 0 { Side::Other } else { Side::Me },
                text: i.to_string(),
            })
            .collect();
        let block = conversation_block(&messages);
        let lines: Vec<&str> = block.lines().collect();
        assert_eq!(lines.len(), PROMPT_WINDOW);
        assert!(lines[0].starts_with("对方：4"));
        assert!(lines[1].starts_with("我：5"));
    }
}
