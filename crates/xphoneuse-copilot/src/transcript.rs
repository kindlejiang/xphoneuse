//! Reading a chat out of OCR lines.
//!
//! The reference implementation gets the message list from Android's
//! accessibility tree and only falls back to OCR for apps that expose no text;
//! its OCR path cannot tell the two speakers apart at all, so it files every
//! bubble as the other person and says so on the panel
//! (`ChatCaptureService.kt:451-453`). XPhoneUse has no tree to read, but it has
//! the pixel boxes OCR returns, and those do carry the side: a bubble sits
//! against the left or the right margin.
//!
//! Everything here is pure so the parsing can be tuned against a real
//! screenshot without a phone, an app or an API key (`examples/copilot_probe`).

use std::cmp::Ordering;

use xphoneuse_ocr::{OcrImage, OcrLine};
use serde::{Deserialize, Serialize};

use crate::config::ScreenOptions;

/// Who sent a message. The wire spelling matches the Jev state's `from` field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Side {
    Me,
    Other,
}

impl Side {
    pub fn key(self) -> &'static str {
        match self {
            Side::Me => "me",
            Side::Other => "other",
        }
    }
}

/// One message, as it goes into the Jev state and the drafting prompt.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Msg {
    pub from: Side,
    pub text: String,
}

/// How many trailing messages the change signature covers. Six is the
/// reference's window (`ChatModels.kt:36`): long enough that any new message
/// changes it, short enough to survive the tail scrolling.
const SIGNATURE_WINDOW: usize = 6;

/// How many messages are kept. The Jev state carries the last 10 and so does
/// the drafting prompt, so this is only about how far back a merge can look.
pub const MAX_HISTORY: usize = 60;

/// Fraction of the width a bubble must hug to be decided by that edge alone.
const EDGE: f32 = 0.12;

/// How close two lines' left edges must be to read as one bubble's text
/// column.
///
/// A bubble lays its wrapped lines out from its own left edge, so a line that
/// starts where the line above starts - and sits within the line spacing - is
/// that bubble's next line, whatever its right edge says. That matters because
/// a wrapped line is short: measured on a real screen, a long message of mine
/// (x=107 w=329 of 542) wrapped to a second line (x=106 w=239), whose right
/// edge sat 36% of the width from the right margin - closer to the left margin
/// than to its own, so the nearest-margin rule below handed the second half of
/// my own sentence to the other person.
///
/// Two percent of the width clears OCR jitter (a pixel or two) and stays well
/// under the difference between two bubbles of different lengths.
const LINE_ALIGN_SLACK: f32 = 0.02;

/// OCR lines in, messages out: crop to the chat area, drop the chrome, group
/// lines into bubbles, and decide which side each bubble is on.
pub fn read_screen(image: &OcrImage, opts: &ScreenOptions) -> Vec<Msg> {
    read_screen_with_keyboard(image, opts, None)
}

/// The same, told where the keyboard is by the device itself.
///
/// Android reports its input method's frame whenever it is up, which is exact
/// and layout-independent - every keyboard, in any language, floating or
/// docked. [`keyboard_top`] guesses the same thing from the shapes the OCR
/// returned, and is what runs when nobody can tell us: an iPhone, a device too
/// old to report it, or a read that does not go through adb at all.
pub fn read_screen_with_keyboard(
    image: &OcrImage,
    opts: &ScreenOptions,
    keyboard: Option<f32>,
) -> Vec<Msg> {
    let width = image.width as f32;
    let height = image.height as f32;
    if width <= 0.0 || height <= 0.0 {
        return Vec::new();
    }
    let top = height * opts.top_crop.clamp(0.0, 0.9);
    // The fixed crop is measured with the keyboard away. With it up, the
    // conversation only reaches halfway down the screen and the crop leaves the
    // whole keypad inside the chat area, where it reads as messages - the left
    // half of the keys as mine, the right half as theirs.
    let bottom = match keyboard.or_else(|| keyboard_top(image, opts)) {
        Some(keyboard) => (keyboard - height * KEYBOARD_CUT_MARGIN)
            .min(height * opts.bottom_crop.clamp(0.1, 1.0)),
        None => height * opts.bottom_crop.clamp(0.1, 1.0),
    };

    let mut lines: Vec<OcrLine> = image
        .lines
        .iter()
        .filter(|line| {
            let text = line.text.trim();
            if text.is_empty() || line.confidence < opts.min_confidence || is_time_only(text) {
                return false;
            }
            // The reference crops the bitmap before OCR; the same crop here is
            // a filter on the line's vertical centre.
            let centre = line.bbox[1] + line.bbox[3] / 2.0;
            centre >= top && centre <= bottom
        })
        .cloned()
        .collect();
    lines.sort_by(|a, b| a.bbox[1].partial_cmp(&b.bbox[1]).unwrap_or(Ordering::Equal));

    // A new bubble starts on a gap larger than `line_gap` times the previous
    // line's height - the reference's rule (`ChatCaptureService.kt:453-474`) -
    // and on a change of side, which the reference's OCR mode cannot use
    // because it does not know the sides. The second half matters: two short
    // messages from different people ("在吗" / "在的") sit closer together than
    // one bubble's own line spacing, so the gap alone would read them as a
    // single message from one person.
    //
    // The side half is not asked of a line that continues the one above it
    // (same left edge, within the line spacing): that line is the rest of a
    // wrapped bubble, and a wrapped line's own right edge - the only thing the
    // side rule has to go on - is short and says nothing about who wrote it.
    let mut out: Vec<Msg> = Vec::new();
    let mut group: Vec<OcrLine> = Vec::new();
    let mut group_left = true;
    for line in lines {
        let left = is_left(line.bbox[0], line.bbox[0] + line.bbox[2], width);
        let starts_new = match group.last() {
            None => false,
            Some(prev) => {
                let gap = line.bbox[1] - (prev.bbox[1] + prev.bbox[3]);
                let within = gap <= prev.bbox[3].max(1.0) * opts.line_gap;
                let continues =
                    within && (line.bbox[0] - prev.bbox[0]).abs() <= width * LINE_ALIGN_SLACK;
                !continues && (!within || left != group_left)
            }
        };
        if starts_new {
            out.push(finish(&group, group_left, opts));
            group.clear();
        }
        if group.is_empty() {
            group_left = left;
        }
        group.push(line);
    }
    if !group.is_empty() {
        out.push(finish(&group, group_left, opts));
    }
    out.retain(|msg| !msg.text.is_empty());
    out
}

/// Labels per row before the row looks like a keyboard rather than a message.
///
/// Three, not ten: a telephone pad's rows hold three keys ("ABC DEF GHI",
/// "1 2 3", "符号 中/英 123"), and those are the rows a count tuned to a QWERTY
/// keyboard would miss. A chat bubble is alone on its line, sitting at its own
/// height; keys share a row by construction, and in threes at the fewest.
const KEYBOARD_MIN_KEYS_PER_ROW: usize = 3;

/// A key's label. "ABC", "符号", "123", "换行" - not a sentence.
const KEYBOARD_MAX_LABEL_CHARS: usize = 6;

/// Tolerated difference in label height. Every key is the same size as every
/// other key; the lines of a conversation are not - and that one difference
/// survives every keyboard layout.
///
/// Not quite 1.0, because some keys print two lines in one box: iOS puts 中 over
/// 英 on its language key, which reads as a single label half again as tall as
/// its neighbours. A tolerance that insisted on 1.0 would throw that key out,
/// and on iOS's pad that is the difference between four rows of keys and three
/// - and three is below the bar this needs to clear.
const KEYBOARD_HEIGHT_TOLERANCE: f32 = 1.8;

/// Rows of keys needed before this is believed, for a keypad: a telephone pad
/// has four or five rows of three. Cutting a conversation short costs messages,
/// so the bar is high.
const KEYBOARD_MIN_ROWS: usize = 4;

/// The other keyboard shape: a 26-key board's letter rows carry nine or ten
/// keys each, but it has only three of them plus a short bottom row - so a rule
/// that asked for four rows would walk straight past it.
///
/// Three rows of six or more keys each is not something a conversation
/// produces; three rows of two or three (a stack of transfer cards, say) is,
/// which is why the count is part of the test rather than the row count alone.
const KEYBOARD_WIDE_KEYS_PER_ROW: usize = 6;
const KEYBOARD_MIN_WIDE_ROWS: usize = 3;

/// How much of a row's pitch its labels take up.
///
/// A key's label fills its key: the row pitch *is* the key's height, and the
/// label is about 40% of it. An app icon is the other way round - a large glyph
/// with a line of small print under it, so the labels are a quarter of the
/// pitch and the row is twice as tall as its text.
///
/// This is what tells a keypad from a home screen. Without it the shape rules
/// below fire on a grid of app icons, which is the same shape as a keypad in
/// every other respect, and cut half the picture away.
const KEYBOARD_LABEL_FILLS_ROW: f32 = 0.30;

/// Only the bottom of the screen is searched, because that is where a keyboard
/// is. The conversation above it is never touched.
const KEYBOARD_SEARCH_FROM: f32 = 0.45;

/// A vertical gap, as a fraction of the screen height, that ends one run of
/// key-like rows and starts another. A keyboard's rows are about half this far
/// apart; anything above it is a different piece of the screen.
const KEYBOARD_RUN_GAP: f32 = 0.12;

/// How close to the bottom edge the lowest row of keys has to be.
///
/// A keyboard is pinned to the bottom of the screen - that is what makes it a
/// keyboard and not a toolbar. Without this, any run of short labels in the
/// middle of the screen (a stack of transfer cards, a panel of suggestions,
/// anything showing through a translucent overlay) can pass for a keypad, and
/// the cut then lands *above* it and eats part of the conversation. Cutting too
/// little costs a few key labels read as messages; cutting too much costs the
/// messages themselves.
const KEYBOARD_REACHES_BOTTOM: f32 = 0.85;

/// How far above the keypad's first row to stop reading, as a fraction of the
/// screen height.
///
/// Deliberately small. The app's input bar (field, microphone, emoji button)
/// sits between the keyboard and the conversation, and its icons are sometimes
/// misread as text - but the newest message sits just above that bar, and
/// cutting it off would cost the one line the whole feature is about. A couple
/// of percent clears the keys themselves, which is where the damage was.
const KEYBOARD_CUT_MARGIN: f32 = 0.02;

/// One row of key-like labels.
struct KeyRow {
    top: f32,
    bottom: f32,
    /// Every label's height: keys are all the same size, and that is measured
    /// against the middle of the row rather than its extremes, because a pad's
    /// digit sits above its letters and one of the two is often misread.
    heights: Vec<f32>,
}

impl KeyRow {
    fn keys(&self) -> usize {
        self.heights.len()
    }

    /// The middle label height of the row: see `keys_like` on why the middle.
    fn median_height(&self) -> f32 {
        let mut heights = self.heights.clone();
        heights.sort_by(|a, b| a.partial_cmp(b).unwrap_or(Ordering::Equal));
        heights[heights.len() / 2]
    }

    fn keys_like(&self) -> bool {
        if self.keys() < KEYBOARD_MIN_KEYS_PER_ROW {
            return false;
        }
        let mut heights = self.heights.clone();
        heights.sort_by(|a, b| a.partial_cmp(b).unwrap_or(Ordering::Equal));
        let median = heights[heights.len() / 2];
        if median <= 0.0 {
            return false;
        }
        heights
            .iter()
            .filter(|h| **h <= median * KEYBOARD_HEIGHT_TOLERANCE && **h * KEYBOARD_HEIGHT_TOLERANCE >= median)
            .count()
            >= KEYBOARD_MIN_KEYS_PER_ROW
    }
}

/// Where the on-screen keyboard begins, if one is open.
///
/// The keyboard is the one thing on a chat screen made of short labels laid out
/// in rows across the whole width - "ABC DEF GHI", "1 2 3", "符号 中/英" - and
/// that shape is what this looks for: at least two such rows, close together,
/// in the bottom half of the screen. Chat bubbles are nothing like it: they are
/// long, they hug one side, and they are not the same size as each other.
///
/// Returns the y to stop reading at - above the keypad's first row by about the
/// height of the input bar, so a half-typed draft in the field is not read as
/// something that was said.
pub fn keyboard_top(image: &OcrImage, opts: &ScreenOptions) -> Option<f32> {
    let width = image.width as f32;
    let height = image.height as f32;
    if width <= 0.0 || height <= 0.0 {
        return None;
    }
    let from = height * KEYBOARD_SEARCH_FROM;
    let mut labels: Vec<&OcrLine> = image
        .lines
        .iter()
        .filter(|line| {
            let text = line.text.trim();
            !text.is_empty()
                && line.confidence >= opts.min_confidence
                && !is_time_only(text)
                && text.chars().count() <= KEYBOARD_MAX_LABEL_CHARS
                && line.bbox[1] + line.bbox[3] / 2.0 >= from
        })
        .collect();
    labels.sort_by(|a, b| a.bbox[1].partial_cmp(&b.bbox[1]).unwrap_or(Ordering::Equal));

    // Lines sharing a baseline are one row of keys.
    let mut rows: Vec<KeyRow> = Vec::new();
    for line in labels {
        let [_, y, _, h] = line.bbox;
        match rows.last_mut() {
            // Tight on purpose: on a telephone pad the digit is printed just
            // above its letters and the two are separate lines, so a tolerance
            // as wide as a label would glue a key's halves to each other and
            // the row's sizes would no longer look alike.
            Some(row) if (y - row.top).abs() <= row.heights[0].min(h) * 0.6 => {
                row.top = row.top.min(y);
                row.bottom = row.bottom.max(y + h);
                row.heights.push(h);
            }
            _ => rows.push(KeyRow {
                top: y,
                bottom: y + h,
                heights: vec![h],
            }),
        }
    }

    let key_rows: Vec<&KeyRow> = rows.iter().filter(|row| row.keys_like()).collect();

    // Split the rows into runs wherever there is a real gap, and keep the last
    // run: the one on the bottom edge is the keyboard, and whatever else looked
    // like keys (a strip of suggestions, a row of cards, text showing through a
    // translucent panel) is above it and must not move the cut up into the
    // conversation. A keyboard's own rows are much closer together than this.
    let mut runs: Vec<Vec<&KeyRow>> = Vec::new();
    for row in key_rows {
        match runs.last_mut() {
            Some(run) if row.top - run[run.len() - 1].bottom <= height * KEYBOARD_RUN_GAP => run.push(row),
            _ => runs.push(vec![row]),
        }
    }
    let run = match runs.last() {
        Some(run) => run,
        None => return None,
    };
    let wide = run
        .iter()
        .filter(|row| row.keys() >= KEYBOARD_WIDE_KEYS_PER_ROW)
        .count();
    // Four rows of three (a telephone pad) or three rows of six (a 26-key
    // board's letter rows): see the two thresholds above.
    if run.len() < KEYBOARD_MIN_ROWS && wide < KEYBOARD_MIN_WIDE_ROWS {
        return None;
    }
    let first = run[0];
    let last = run[run.len() - 1];
    if last.bottom - first.top > height * 0.5 {
        return None;
    }
    // ... and the run has to sit on the bottom edge, where a keyboard is.
    if last.bottom < height * KEYBOARD_REACHES_BOTTOM {
        return None;
    }
    // ... and its labels have to fill their rows, which is what a key does and
    // what an app icon does not.
    let mut pitches: Vec<f32> = run
        .windows(2)
        .map(|pair| pair[1].top - pair[0].top)
        .filter(|pitch| *pitch > 0.0)
        .collect();
    if pitches.is_empty() {
        return None;
    }
    pitches.sort_by(|a, b| a.partial_cmp(b).unwrap_or(Ordering::Equal));
    let pitch = pitches[pitches.len() / 2];
    let mut labels: Vec<f32> = run.iter().map(|row| row.median_height()).collect();
    labels.sort_by(|a, b| a.partial_cmp(b).unwrap_or(Ordering::Equal));
    if labels[labels.len() / 2] < pitch * KEYBOARD_LABEL_FILLS_ROW {
        return None;
    }
    let cut = first.top - height * KEYBOARD_CUT_MARGIN;
    // Never above the top crop: a cut that climbs past the conversation would
    // drop messages to remove a keyboard.
    Some(cut.max(height * opts.top_crop.clamp(0.0, 0.9)))
}

/// A name is short: a contact or a group, not a sentence.
const TITLE_MAX_CHARS: usize = 24;

/// A wrong name files two people's messages under one key, so only lines the
/// reader is sure of are taken.
const TITLE_MIN_CONFIDENCE: f32 = 0.80;

/// How much taller than the next line in the band the name has to be. The name
/// is the largest text on that row of every chat app there is; when nothing
/// stands out, the screen is not a chat header and there is no name to read.
const TITLE_HEIGHT_MARGIN: f32 = 1.25;

/// How far a line's centre may sit from the middle of the screen, as a fraction
/// of its width. Chat apps centre the name, and the status bar's clock and
/// battery - which share the band - live at the edges.
const TITLE_CENTRE_SLACK: f32 = 0.15;

/// The name of the conversation on screen, if the header gives one up.
///
/// Every line considered here has already been read: `read_screen` filters the
/// OCR's lines instead of cropping the bitmap, so the strip above the messages
/// is read and thrown away on every capture. Reading a name out of it is
/// therefore free - being *sure* of it is what this function spends its care
/// on.
///
/// The caller keys a conversation's history by this string, and a wrong key
/// puts two chats in one buffer, which is the failure that keying exists to
/// prevent. So anything ambiguous answers `None` - a status bar, a button, a
/// headline, a line that is not clearly the biggest thing in the band - and the
/// caller falls back to matching the screen against the history it already has
/// (`merge`). A screen that is not a chat at all can still produce a name
/// (a page's search field reads as one); the cost is a conversation nobody
/// talks in, not a mixed one, and the panel shows what was read.
pub fn read_title(image: &OcrImage, opts: &ScreenOptions) -> Option<String> {
    let width = image.width as f32;
    let height = image.height as f32;
    if width <= 0.0 || height <= 0.0 {
        return None;
    }
    let top = height * opts.title_top.clamp(0.0, 0.9);
    let bottom = height * opts.title_bottom.clamp(0.0, 1.0);
    if !top.is_finite() || !bottom.is_finite() || bottom <= top {
        return None; // an empty band: reading names is turned off
    }

    let mut candidates: Vec<&OcrLine> = image
        .lines
        .iter()
        .filter(|line| {
            let text = line.text.trim();
            if text.is_empty() || line.confidence < TITLE_MIN_CONFIDENCE {
                return false;
            }
            if text.chars().count() > TITLE_MAX_CHARS || !could_be_a_name(text) {
                return false;
            }
            let centre_y = line.bbox[1] + line.bbox[3] / 2.0;
            if centre_y < top || centre_y > bottom {
                return false;
            }
            let centre_x = line.bbox[0] + line.bbox[2] / 2.0;
            (centre_x - width / 2.0).abs() <= width * TITLE_CENTRE_SLACK
        })
        .collect();
    if candidates.is_empty() {
        return None;
    }
    candidates.sort_by(|a, b| b.bbox[3].partial_cmp(&a.bbox[3]).unwrap_or(Ordering::Equal));
    let best = candidates[0];
    if let Some(runner_up) = candidates.get(1) {
        if best.bbox[3] < runner_up.bbox[3] * TITLE_HEIGHT_MARGIN {
            return None;
        }
    }
    name_key(best.text.trim())
}

/// Whether a line could be the name of a person or a group rather than prose,
/// a headline or a row of buttons.
fn could_be_a_name(text: &str) -> bool {
    // A row of digits is a counter, not a name - the numbers under a home
    // screen's icons read as centred lines of exactly that shape, and they
    // change with the data, so they would make a key that never holds still.
    // Anything with a letter in it (CJK included) passes.
    if !text.chars().any(char::is_alphabetic) {
        return false;
    }
    // Sentence punctuation anywhere means this is a sentence, not a name. The
    // ellipsis a truncated long name ends with is not in this list on purpose:
    // it is stable, so it makes a perfectly good key.
    !text.contains([',', '.', '，', '。', '！', '!', '？', '?', '；', ';', '：', ':', '\n'])
}

/// The name as a key: no surrounding space, and no count, which changes while
/// the name it follows does not.
fn name_key(text: &str) -> Option<String> {
    let mut name = text.trim();
    // "群聊(5)" / "小李（3）": an unread or member count.
    for (open, close) in [('(', ')'), ('（', '）'), ('[', ']')] {
        if let Some(start) = name.rfind(open) {
            if name.ends_with(close) {
                let inner = &name[start + open.len_utf8()..name.len() - close.len_utf8()];
                if !inner.is_empty() && inner.chars().all(|c| c.is_ascii_digit()) {
                    name = name[..start].trim_end();
                }
            }
        }
    }
    if name.is_empty() {
        return None;
    }
    Some(name.to_string())
}

/// One bubble's lines into one message. `left` is the side every line in the
/// group agreed on (see the split rule above).
fn finish(group: &[OcrLine], left: bool, opts: &ScreenOptions) -> Msg {
    // Within a bubble the lines are joined with a space, the same as the
    // reference (`ChatCaptureService.kt:468`). It is redundant between Chinese
    // lines and necessary between English ones.
    let text = group
        .iter()
        .map(|l| l.text.trim())
        .collect::<Vec<_>>()
        .join(" ");
    let mine = left != opts.me_on_right;
    let from = if opts.all_from_other || !mine {
        Side::Other
    } else {
        Side::Me
    };
    Msg {
        from,
        text: clean_text(&text),
    }
}

/// Whether a group of lines sits on the left half of the screen.
///
/// The reference compares how close each edge of a bubble is to the avatar
/// column instead of using the bubble's centre, because a long message crosses
/// the middle of the screen and its centre says nothing
/// (`ChatAppAdapter.kt:230-236`). Same idea, on text boxes: whichever edge is
/// nearer its own margin wins, and a box that hugs one margin is decided by
/// that alone - a wide bubble can have both edges close to a margin, and then
/// the comparison would be a coin toss.
fn is_left(min_x: f32, max_x: f32, width: f32) -> bool {
    let left_gap = min_x;
    let right_gap = width - max_x;
    let edge = width * EDGE;
    if left_gap <= edge && right_gap > edge {
        return true;
    }
    if right_gap <= edge && left_gap > edge {
        return false;
    }
    left_gap <= right_gap
}

/// Strips the read receipt and the timestamp an app glues onto a bubble
/// (`ChatCaptureService.kt:478-489`), repeatedly, because both can be present
/// and in either order.
fn clean_text(raw: &str) -> String {
    let mut text = raw.trim().to_string();
    loop {
        let before = text.clone();
        for tail in ["已读", "未读"] {
            if let Some(stripped) = text.strip_suffix(tail) {
                text = stripped.trim_end().to_string();
            }
        }
        text = strip_trailing_time(&text);
        if text == before {
            return text;
        }
    }
}

/// Drops a trailing `H:MM` (the reference's `TAIL_TIME`, `\d{1,2}[:：]\d{2}$`),
/// without a regex.
fn strip_trailing_time(text: &str) -> String {
    let Some(colon) = text.rfind([':', '：']) else {
        return text.to_string();
    };
    // `:` is one byte and `：` is three, so the width has to come from the
    // character itself rather than from the byte offset.
    let colon_char = text[colon..].chars().next().expect("rfind found a char");
    let after = &text[colon + colon_char.len_utf8()..];
    if after.len() != 2 || !after.bytes().all(|b| b.is_ascii_digit()) {
        return text.to_string();
    }
    let before = &text[..colon];
    // The byte index of the first digit of the trailing run; digits are one
    // byte each, so the run's length is what is left of `before`.
    let Some(start) = before
        .char_indices()
        .rev()
        .take_while(|(_, c)| c.is_ascii_digit())
        .last()
        .map(|(i, _)| i)
    else {
        return text.to_string();
    };
    if before.len() - start > 2 {
        return text.to_string();
    }
    text[..start].trim_end().to_string()
}

/// A line that is only a date or a clock reading.
///
/// The reference matches `\d{1,2}[:：]\d{2}` against the whole line, which
/// misses the prefixes Chinese chat apps put in front of one. Over-filtering is
/// the safe direction here - a stray "昨天 21:05" would otherwise be read as a
/// message from the other person - so the prefixes are stripped first, and a
/// line that is nothing but a prefix ("昨天" alone, as a date separator) counts
/// as a timestamp too.
fn is_time_only(text: &str) -> bool {
    const PREFIXES: &[&str] = &[
        "上午", "下午", "凌晨", "中午", "晚上", "今天", "昨天", "前天",
        "周一", "周二", "周三", "周四", "周五", "周六", "周日", "周天",
        "星期一", "星期二", "星期三", "星期四", "星期五", "星期六", "星期日", "星期天",
    ];
    let mut rest = text.trim();
    let mut stripped = false;
    for prefix in PREFIXES {
        if let Some(after) = rest.strip_prefix(prefix) {
            rest = after.trim();
            stripped = true;
        }
    }
    if let Some(after) = strip_month_day(rest) {
        rest = after;
        stripped = true;
    }
    if rest.is_empty() {
        return stripped;
    }
    let Some((hour, minute)) = rest.split_once([':', '：']) else {
        return false;
    };
    let (hour, minute) = (hour.trim(), minute.trim());
    !hour.is_empty()
        && hour.len() <= 2
        && hour.bytes().all(|b| b.is_ascii_digit())
        && minute.len() == 2
        && minute.bytes().all(|b| b.is_ascii_digit())
}

/// Strips a leading `8月27日`.
fn strip_month_day(text: &str) -> Option<&str> {
    let (month, rest) = split_leading_digits(text)?;
    let rest = rest.strip_prefix('月')?;
    let (day, rest) = split_leading_digits(rest)?;
    let rest = rest.strip_prefix('日')?;
    if month.len() > 2 || day.len() > 2 {
        return None;
    }
    Some(rest.trim())
}

/// The leading run of ASCII digits and the rest, or `None` when there is none.
fn split_leading_digits(text: &str) -> Option<(&str, &str)> {
    let end = text.find(|c: char| !c.is_ascii_digit())?;
    if end == 0 {
        return None;
    }
    Some(text.split_at(end))
}

/// The change signature: the last few messages, so a screen that only moved
/// its clock or an animation does not look like a new message.
pub fn signature(messages: &[Msg]) -> String {
    let start = messages.len().saturating_sub(SIGNATURE_WINDOW);
    messages[start..]
        .iter()
        .map(|msg| format!("{}:{}", msg.from.key(), msg.text))
        .collect::<Vec<_>>()
        .join("|")
}

/// Who sent the newest message.
pub fn latest_from(messages: &[Msg]) -> Side {
    messages.last().map(|msg| msg.from).unwrap_or(Side::Other)
}

/// Folds what is on screen now into what has been seen before.
///
/// The screen always shows the newest messages, so its start should line up
/// with the history's end. Three cases:
///
/// * the screen overlaps the history's tail - append only what is past the
///   overlap (the normal case: one or two new messages);
/// * nothing overlaps, but the screen is made of messages already in the
///   history - the user scrolled back into old messages, so there is nothing
///   new, and appending would splice the conversation out of order;
/// * nothing overlaps and the screen is mostly messages we have never seen -
///   this is not the conversation the history is about. Reset to what is on
///   screen.
///
/// That last case used to append, on the theory that the messages had arrived
/// while nobody was reading. They might have - but the same picture is what a
/// different chat looks like, and appending then hands Jev two people's threads
/// spliced into one, which is how a copilot ends up drafting a reply to the
/// wrong person's message. The two are not distinguishable from the pixels, so
/// the answer has to be the one whose failure is survivable: resetting costs
/// history the analysis barely uses (the state sent to Jev is the last ten
/// messages, and those are on screen by definition), while splicing costs
/// correctness. The next read settles it either way - a continuing
/// conversation re-anchors on the screen it was reset to.
///
/// Returns whether the history changed.
pub fn merge(history: &mut Vec<Msg>, screen: &[Msg]) -> bool {
    if screen.is_empty() {
        return false;
    }
    let appended: &[Msg] = match tail_overlap(history, screen) {
        Some(k) => &screen[k..],
        None => {
            if mostly_known(history, screen) {
                return false;
            }
            history.clear();
            screen
        }
    };
    if appended.is_empty() {
        return false;
    }
    history.extend_from_slice(appended);
    if history.len() > MAX_HISTORY {
        let drop = history.len() - MAX_HISTORY;
        history.drain(..drop);
    }
    true
}

/// Whether `screen` is a continuation of `history`: it overlaps the tail, or it
/// is mostly made of messages the history already has.
///
/// This is the test `merge` acts on, and `conversations` uses it too - to
/// decide whether a screen that named no conversation belongs to the one it was
/// showing or to the unnamed bucket. Kept in one place so the two can never
/// disagree about what "the same conversation" means.
pub fn continues(history: &[Msg], screen: &[Msg]) -> bool {
    if screen.is_empty() || history.is_empty() {
        return false;
    }
    tail_overlap(history, screen).is_some() || mostly_known(history, screen)
}

/// The length of the overlap between the history's tail and the screen's start,
/// largest first, or `None` when nothing lines up.
fn tail_overlap(history: &[Msg], screen: &[Msg]) -> Option<usize> {
    (1..=history.len().min(screen.len()))
        .rev()
        .find(|&k| {
            history[history.len() - k..]
                .iter()
                .zip(&screen[..k])
                .all(|(a, b)| same(a, b))
        })
}

/// Whether most of the screen is already in the history.
///
/// Half is the line: a screen that is mostly known is a chunk of the same
/// conversation (a scroll, or messages older than the history's window), while
/// a stray message or two matching is a coincidence - "好的" is in every chat.
fn mostly_known(history: &[Msg], screen: &[Msg]) -> bool {
    let known = screen
        .iter()
        .filter(|msg| history.iter().any(|seen| same(seen, msg)))
        .count();
    known * 2 >= screen.len()
}

fn same(a: &Msg, b: &Msg) -> bool {
    a.from == b.from && a.text.trim() == b.text.trim()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A portrait screen, 1000x2000, with the chat area between 12% and 84%.
    fn screen() -> OcrImage {
        OcrImage {
            width: 1000,
            height: 2000,
            lines: Vec::new(),
        }
    }

    fn line(text: &str, x: f32, y: f32, width: f32) -> OcrLine {
        OcrLine {
            text: text.to_string(),
            confidence: 0.95,
            bbox: [x, y, width, 40.0],
        }
    }

    fn read(lines: Vec<OcrLine>, opts: &ScreenOptions) -> Vec<Msg> {
        let mut image = screen();
        image.lines = lines;
        read_screen(&image, opts)
    }

    fn opts() -> ScreenOptions {
        ScreenOptions::default()
    }

    fn text_of(messages: &[Msg]) -> Vec<&str> {
        messages.iter().map(|m| m.text.as_str()).collect()
    }

    #[test]
    fn a_wrapped_line_stays_with_the_bubble_it_belongs_to() {
        // Measured off a real WeChat screen (542 wide): my long message wrapped
        // to a second line that starts one pixel left of the first and ends
        // 36% of the width short of the right margin - nearer the left margin
        // than its own, which is how the second half of my own sentence was
        // read as the other person's. Here at 1000 wide: x=197 w=607 and
        // x=196 w=441, overlapping boxes 30px apart.
        let messages = read(
            vec![
                line("我现在过去看看，刚老杨打电话", 197.0, 800.0, 607.0),
                line("给我了，感觉他搞不定", 196.0, 830.0, 441.0),
            ],
            &opts(),
        );
        assert_eq!(text_of(&messages), ["我现在过去看看，刚老杨打电话 给我了，感觉他搞不定"]);
        assert_eq!(messages[0].from, Side::Me);
    }

    #[test]
    fn a_line_that_starts_elsewhere_is_still_its_own_message() {
        // The same two lines, but the second one starts at the other margin -
        // a different bubble, and the side rule still has its say.
        let messages = read(
            vec![
                line("我现在过去看看，刚老杨打电话", 197.0, 800.0, 607.0),
                line("给我了，感觉他搞不定", 100.0, 830.0, 341.0),
            ],
            &opts(),
        );
        assert_eq!(text_of(&messages), ["我现在过去看看，刚老杨打电话", "给我了，感觉他搞不定"]);
        assert_eq!(
            messages.iter().map(|m| m.from).collect::<Vec<_>>(),
            [Side::Me, Side::Other]
        );
    }

    #[test]
    fn splits_the_sides_by_which_margin_the_bubble_hugs() {
        // Left bubble starts at the left margin; right bubble ends at the right
        // one. Both are short, so both are decided by their own edge.
        let messages = read(
            vec![
                line("在吗", 100.0, 400.0, 160.0),
                line("在的", 640.0, 600.0, 260.0),
                line("帮我看下那个文档", 100.0, 800.0, 420.0),
            ],
            &opts(),
        );
        assert_eq!(text_of(&messages), ["在吗", "在的", "帮我看下那个文档"]);
        assert_eq!(
            messages.iter().map(|m| m.from).collect::<Vec<_>>(),
            [Side::Other, Side::Me, Side::Other]
        );
    }

    #[test]
    fn a_long_right_bubble_crossing_the_middle_is_still_mine() {
        // Starts left of centre and ends at the right margin: its centre is
        // around 0.5 and says nothing, its right edge says everything.
        let messages = read(vec![line("我把上周的方案改完发你了", 200.0, 400.0, 740.0)], &opts());
        assert_eq!(messages[0].from, Side::Me);
    }

    #[test]
    fn multiline_bubbles_join_and_a_gap_starts_a_new_one() {
        let messages = read(
            vec![
                line("第一行", 100.0, 400.0, 200.0),
                // 20px below the previous line's bottom (440) - same bubble.
                line("第二行", 100.0, 460.0, 200.0),
                // 100px below - a new bubble.
                line("另一条", 100.0, 600.0, 200.0),
            ],
            &opts(),
        );
        assert_eq!(text_of(&messages), ["第一行 第二行", "另一条"]);
    }

    #[test]
    fn a_side_change_splits_a_bubble_the_gap_would_have_merged() {
        // The case the gap rule alone gets wrong: "在吗" and "在的" are 20px
        // apart - well inside one bubble's own line spacing - but they are from
        // two different people, and only the side says so.
        let messages = read(
            vec![
                line("在吗", 100.0, 400.0, 160.0),
                line("在的", 640.0, 460.0, 260.0),
            ],
            &opts(),
        );
        assert_eq!(text_of(&messages), ["在吗", "在的"]);
        assert_eq!(
            messages.iter().map(|m| m.from).collect::<Vec<_>>(),
            [Side::Other, Side::Me]
        );
    }

    #[test]
    fn the_gap_factor_is_configurable() {
        // 60px apart with 40px-tall lines: inside the default 1.2x (48px would
        // be the threshold - 60 > 48, so the default splits) and inside a
        // looser 2.0x (80px), which merges.
        let lines = vec![
            line("第一行", 100.0, 400.0, 200.0),
            line("第二行", 100.0, 500.0, 200.0),
        ];
        assert_eq!(text_of(&read(lines.clone(), &opts())), ["第一行", "第二行"]);
        let loose = ScreenOptions {
            line_gap: 3.0,
            ..opts()
        };
        assert_eq!(text_of(&read(lines, &loose)), ["第一行 第二行"]);
    }

    /// A telephone-style keypad: `rows` rows of `per_row` keys, the way the
    /// phone's own keyboard looks with a chat behind it.
    fn keypad(image: &mut OcrImage, top: f32, rows: usize, per_row: usize) {
        let height = image.height as f32;
        let width = image.width as f32;
        let labels = ["1", "ABC", "DEF", "GHI", "JKL", "MNO", "PQRS", "TUV"];
        let pitch = height * 0.09;
        // A real key's label is about 40% of its row's pitch (measured on
        // captures: 69/160 on iOS, 66/153 on Android).
        let label_height = height * 0.04;
        for row in 0..rows {
            for key in 0..per_row {
                image.lines.push(tall_line(
                    labels[(row * per_row + key) % labels.len()],
                    width * (0.30 + 0.18 * key as f32),
                    top + pitch * row as f32,
                    width * 0.12,
                    label_height,
                ));
            }
        }
    }

    #[test]
    fn a_keyboard_is_not_read_as_a_conversation() {
        // The screen the copilot was reading when it answered "我 ABC / 对方
        // DEF": with the keyboard up the conversation is the top half, and the
        // keypad used to fall inside the crop and parse as messages - the left
        // keys as mine, the right ones as theirs.
        let mut image = screen();
        image.lines = vec![
            line("在吗", 100.0, 400.0, 160.0),
            line("在的", 640.0, 500.0, 260.0),
            line("昨晚七点出的手术室", 100.0, 700.0, 400.0),
        ];
        keypad(&mut image, 1300.0, 4, 3);

        let messages = read_screen(&image, &opts());
        assert_eq!(
            text_of(&messages),
            ["在吗", "在的", "昨晚七点出的手术室"],
            "the keypad must not be read as messages"
        );
    }

    #[test]
    fn a_keypad_that_prints_a_digit_above_each_key_is_a_keyboard() {
        // What the OCR returns for the telephone pad this was reported from:
        // every key is two lines, the digit in small print above its letters, so
        // the rows alternate in height and in spacing. The first version of the
        // rule wanted uniform rows and missed the keyboard entirely.
        let mut image = screen();
        image.lines = vec![
            line("在吗", 100.0, 400.0, 160.0),
            line("在的", 640.0, 500.0, 260.0),
        ];
        let digits = ["1", "2", "3"];
        let letters = ["ABC", "DEF", "GHI"];
        for row in 0..4 {
            let y = 1300.0 + row as f32 * 150.0;
            for (key, digit) in digits.iter().enumerate() {
                let x = 240.0 + key as f32 * 200.0;
                image.lines.push(tall_line(digit, x, y, 30.0, 38.0));
                image.lines.push(tall_line(letters[key], x, y + 36.0, 110.0, 66.0));
            }
        }
        let messages = read_screen(&image, &opts());
        assert_eq!(text_of(&messages), ["在吗", "在的"]);
    }

    #[test]
    fn a_home_screen_of_app_icons_is_not_a_keyboard() {
        // The shape this used to read as a keyboard, and cut in half: a grid of
        // app icons. Rows of four or five short labels, all the same size, flush
        // with the bottom of the screen - a keypad in every respect but one. An
        // icon's label is small print under a big glyph, so its row is twice as
        // tall as its text; a key's label fills its key.
        //
        // Measured on the two screenshots that failed: 54px labels on a 224px
        // pitch (0.24) against 69/160 on iOS and 66/153 on Android (0.43).
        let mut image = screen();
        image.lines = vec![line("明天要把银行的事情办了", 100.0, 700.0, 700.0)];
        for row in 0..4 {
            let y = 1250.0 + row as f32 * 224.0;
            for (key, label) in ["淘宝", "闲鱼", "支付宝", "微信", "地图"].iter().enumerate() {
                image.lines.push(tall_line(
                    label,
                    60.0 + key as f32 * 190.0,
                    y,
                    110.0,
                    54.0,
                ));
            }
        }
        let messages = read_screen(&image, &opts());
        assert!(
            text_of(&messages).iter().any(|text| *text == "明天要把银行的事情办了"),
            "nothing on this screen is a keyboard, so nothing may be cut"
        );
    }

    #[test]
    fn an_ios_pad_with_a_two_line_language_key_is_a_keyboard() {
        // The layout of the pad on an iPhone, as the OCR reads it: the left
        // column of punctuation is mostly unreadable, the backspace key comes
        // back as a stray "X", and the language key prints 中 over 英 in one box
        // - half again as tall as its neighbours. Wanting every key the same
        // height leaves three rows of keys, and three is below the bar.
        let mut image = screen();
        image.lines = vec![
            line("明天要把银行的事情办了", 120.0, 520.0, 700.0),
            line("我躺下啦", 120.0, 700.0, 300.0),
            // The capture's newest message, which sits just above the input bar.
            line("通话时长 01:22", 400.0, 940.0, 500.0),
        ];
        // Four rows, at the proportions the capture had (1179x2556 -> 1000x2000).
        let rows: [&[(&str, f32)]; 4] = [
            &[("@#", 118.0), ("ABC", 68.0), ("DEF", 69.0), ("X", 30.0)],
            &[("GHI", 69.0), ("JKL", 70.0), ("MNO", 68.0), ("换行", 70.0)],
            &[("TUV", 68.0), ("PQRS", 70.0), ("WXYZ", 66.0)],
            &[("中英", 119.0), ("符号", 70.0), ("123", 64.0)],
        ];
        let mut y = 1355.0;
        for row in rows {
            let mut x = 60.0;
            for (label, height) in row {
                image.lines.push(tall_line(label, x, y, 110.0, *height));
                x += 190.0;
            }
            y += 128.0;
        }
        let messages = read_screen(&image, &opts());
        // The trailing time is stripped from the last one, as it always is.
        assert_eq!(
            text_of(&messages),
            ["明天要把银行的事情办了", "我躺下啦", "通话时长"]
        );
    }

    #[test]
    fn a_twenty_six_key_keyboard_is_a_keyboard_too() {
        // The other layout on the same phone: three rows of nine or ten single
        // letters and a bottom row that may hold only two labels. Four rows it
        // does not have - which is why the rule counts keys per row as well as
        // rows per keyboard.
        let mut image = screen();
        image.lines = vec![
            line("在吗", 100.0, 400.0, 160.0),
            line("在的", 640.0, 500.0, 260.0),
            line("昨晚七点出的手术室", 100.0, 700.0, 400.0),
        ];
        // Flush with the bottom of the screen, as a real keyboard is: a run of
        // short labels that stops short of the edge is not one.
        for (row, letters) in ["qwertyuiop", "asdfghjkl", "zxcvbnm"].iter().enumerate() {
            let y = 1400.0 + row as f32 * 170.0;
            let step = 900.0 / (letters.chars().count() as f32 + 1.0);
            for (key, letter) in letters.chars().enumerate() {
                image.lines.push(tall_line(
                    &letter.to_string(),
                    50.0 + step * (key as f32 + 1.0),
                    y,
                    40.0,
                    70.0,
                ));
            }
        }
        for (key, label) in ["123", "换行"].iter().enumerate() {
            image.lines.push(tall_line(label, 300.0 + key as f32 * 300.0, 1910.0, 120.0, 70.0));
        }
        let messages = read_screen(&image, &opts());
        assert_eq!(text_of(&messages), ["在吗", "在的", "昨晚七点出的手术室"]);
    }

    #[test]
    fn the_newest_message_above_the_keyboard_survives() {
        // The bar is between the keyboard and the conversation, and the message
        // the copilot exists to answer is the one just above it: the cut must
        // clear the keys without eating that line.
        let mut image = screen();
        image.lines = vec![
            line("在吗", 100.0, 400.0, 160.0),
            // The last thing they said, a little above the input bar.
            line("恢复的怎么样?", 100.0, 1180.0, 300.0),
        ];
        keypad(&mut image, 1300.0, 4, 3);
        let messages = read_screen(&image, &opts());
        assert_eq!(text_of(&messages), ["在吗", "恢复的怎么样?"]);
    }

    #[test]
    fn a_told_keyboard_cuts_even_when_nothing_looks_like_keys() {
        // A handwriting pad, a keyboard in a script the OCR reads as blobs, a
        // floating keyboard: none of it has rows of short labels, so no shape
        // rule can find it. Android knows exactly where its input method is,
        // and that is what the hint carries.
        let mut image = screen();
        image.lines = vec![
            line("在吗", 100.0, 400.0, 160.0),
            line("在的", 640.0, 500.0, 260.0),
            // Below the keyboard's top edge: the pad's own hints, not messages.
            line("手写", 100.0, 1500.0, 200.0),
            line("清除", 600.0, 1500.0, 200.0),
        ];
        let messages = read_screen_with_keyboard(&image, &opts(), Some(1480.0));
        assert_eq!(text_of(&messages), ["在吗", "在的"]);

        // And without the hint those lines are all a shape reader has, so they
        // stay: the fallback is a fallback, not a promise.
        let without = read_screen_with_keyboard(&image, &opts(), None);
        assert!(text_of(&without).iter().any(|text| *text == "手写"));
    }

    #[test]
    fn a_draft_above_the_keyboard_is_not_read_as_a_message() {
        // The input bar sits between the conversation and the keyboard, and a
        // half-typed draft in it is not something that was said. It used to be
        // read as mine, which makes the judgement think the reply is already
        // out.
        let mut image = screen();
        image.lines = vec![
            line("在吗", 100.0, 400.0, 160.0),
            line("在的", 640.0, 500.0, 260.0),
            // In the field: about a third of a row pitch above the keypad.
            line("我正打算回他", 100.0, 1270.0, 300.0),
        ];
        keypad(&mut image, 1300.0, 4, 3);

        let messages = read_screen(&image, &opts());
        assert_eq!(text_of(&messages), ["在吗", "在的"]);
    }

    #[test]
    fn short_messages_on_their_own_lines_are_not_a_keyboard() {
        // A chat of short messages: each one is alone on its line, so no row
        // has two labels, and a conversation must survive the rule that finds
        // keyboards by counting labels per row.
        let mut image = screen();
        image.lines = vec![
            line("在吗", 100.0, 1200.0, 160.0),
            line("在的", 640.0, 1290.0, 160.0),
            line("嗯", 100.0, 1380.0, 60.0),
            line("好", 640.0, 1470.0, 60.0),
            line("行", 100.0, 1560.0, 60.0),
        ];
        let messages = read_screen(&image, &opts());
        assert_eq!(text_of(&messages), ["在吗", "在的", "嗯", "好", "行"]);
    }

    #[test]
    fn a_chat_card_with_a_caption_is_not_a_keyboard() {
        // Two labels on one line, three times over: the shape of a transfer
        // card, and the closest a conversation comes to a keyboard. Three rows
        // of it is not enough - the labels are not the same size and the rows
        // are not evenly spaced.
        let mut image = screen();
        image.lines = vec![
            tall_line("¥666.00", 100.0, 1200.0, 200.0, 40.0),
            tall_line("已退还", 620.0, 1204.0, 120.0, 30.0),
            tall_line("¥120.00", 100.0, 1400.0, 200.0, 40.0),
            tall_line("已收", 640.0, 1402.0, 90.0, 30.0),
            tall_line("¥38.50", 100.0, 1600.0, 180.0, 40.0),
            tall_line("待领取", 620.0, 1610.0, 150.0, 34.0),
        ];
        let messages = read_screen(&image, &opts());
        assert_eq!(messages.len(), 6, "the cards are messages, not keys");
    }

    #[test]
    fn the_status_bar_and_the_input_area_are_dropped() {
        let messages = read(
            vec![
                line("9:41", 450.0, 20.0, 100.0),      // status bar
                line("聊天内容", 100.0, 800.0, 300.0),  // kept
                line("说点什么", 100.0, 1800.0, 200.0), // input placeholder
            ],
            &opts(),
        );
        assert_eq!(text_of(&messages), ["聊天内容"]);
    }

    #[test]
    fn time_and_date_separators_are_not_messages() {
        for separator in ["12:34", "下午 3:05", "昨天 21:05", "8月27日", "星期二 09:30"] {
            let messages = read(
                vec![
                    line(separator, 450.0, 700.0, 100.0),
                    line("真的吗", 100.0, 900.0, 200.0),
                ],
                &opts(),
            );
            assert_eq!(text_of(&messages), ["真的吗"], "{separator}");
        }
    }

    #[test]
    fn a_message_that_merely_contains_a_time_survives() {
        let messages = read(vec![line("8:30 出发行吗", 100.0, 800.0, 300.0)], &opts());
        assert_eq!(text_of(&messages), ["8:30 出发行吗"]);
    }

    #[test]
    fn read_receipts_and_trailing_timestamps_are_stripped() {
        let messages = read(
            vec![
                line("好的已读", 640.0, 400.0, 160.0),
                line("明天见 21:05", 100.0, 500.0, 260.0),
            ],
            &opts(),
        );
        assert_eq!(text_of(&messages), ["好的", "明天见"]);
    }

    #[test]
    fn low_confidence_lines_are_dropped() {
        let mut image = screen();
        image.lines = vec![
            OcrLine {
                text: "模糊".into(),
                confidence: 0.2,
                bbox: [100.0, 400.0, 100.0, 40.0],
            },
            line("清楚", 100.0, 500.0, 100.0),
        ];
        let messages = read_screen(&image, &opts());
        assert_eq!(text_of(&messages), ["清楚"]);
    }

    #[test]
    fn the_switches_flip_the_sides() {
        let lines = vec![line("左边", 100.0, 400.0, 160.0), line("右边", 640.0, 500.0, 260.0)];

        let swapped = ScreenOptions {
            me_on_right: false,
            ..opts()
        };
        let messages = read(lines.clone(), &swapped);
        assert_eq!(
            messages.iter().map(|m| m.from).collect::<Vec<_>>(),
            [Side::Me, Side::Other]
        );

        let all_other = ScreenOptions {
            all_from_other: true,
            ..opts()
        };
        let messages = read(lines, &all_other);
        assert!(messages.iter().all(|m| m.from == Side::Other));
    }

    /// A line of a given type size, for the header band.
    fn tall_line(text: &str, x: f32, y: f32, width: f32, height: f32) -> OcrLine {
        OcrLine {
            text: text.to_string(),
            confidence: 0.95,
            bbox: [x, y, width, height],
        }
    }

    /// A 1000x2000 screen whose header reads like a chat app's: status bar at
    /// the top, the name centred under it, a smaller subtitle, a button at the
    /// right margin.
    fn chat_header(name: &str) -> OcrImage {
        let mut image = screen();
        image.lines = vec![
            tall_line("9:41", 40.0, 40.0, 120.0, 40.0),        // status bar, left
            tall_line("100%", 880.0, 40.0, 80.0, 40.0),        // status bar, right
            tall_line(name, 380.0, 160.0, 240.0, 64.0),        // the name
            tall_line("对方正在输入…", 390.0, 240.0, 220.0, 36.0), // subtitle
            tall_line("···", 920.0, 165.0, 40.0, 30.0),        // the menu button
            line("晚上一起吃饭？", 100.0, 700.0, 300.0),
        ];
        image
    }

    #[test]
    fn the_name_in_the_header_is_read() {
        assert_eq!(
            read_title(&chat_header("小李"), &opts()).as_deref(),
            Some("小李")
        );
    }

    #[test]
    fn a_count_after_the_name_is_not_part_of_the_key() {
        // Unread and member counts move; the name does not.
        for header in ["群聊(5)", "小李（3）", "Team[12]"] {
            let image = chat_header(header);
            let expected = header.split(['(', '（', '[']).next().unwrap();
            assert_eq!(
                read_title(&image, &opts()).as_deref(),
                Some(expected),
                "{header}"
            );
        }
    }

    #[test]
    fn the_status_bar_is_not_a_name() {
        // Clock and battery only: both sit at the edges, and neither is the
        // biggest thing in the band. Nothing comes back at all.
        let mut image = screen();
        image.lines = vec![
            tall_line("9:41", 40.0, 40.0, 120.0, 40.0),
            tall_line("100%", 880.0, 40.0, 80.0, 40.0),
        ];
        assert_eq!(read_title(&image, &opts()), None);
    }

    #[test]
    fn two_lines_of_the_same_size_are_not_a_name() {
        // Nothing stands out, so this is not a chat header: guessing between
        // them would key a conversation on a coin flip.
        let mut image = screen();
        image.lines = vec![
            tall_line("上海", 380.0, 95.0, 240.0, 64.0),
            tall_line("苏州", 380.0, 155.0, 240.0, 64.0),
        ];
        assert_eq!(read_title(&image, &opts()), None);
    }

    #[test]
    fn a_row_of_digits_is_not_a_name() {
        // The numbers under a home screen's icons are centred lines the size of
        // a title, and they change as the data does.
        let mut image = screen();
        image.lines = vec![tall_line("839", 400.0, 150.0, 200.0, 64.0)];
        assert_eq!(read_title(&image, &opts()), None);
    }

    #[test]
    fn a_page_title_is_too_much_like_a_sentence() {
        // The news page in the regression screenshots reads its headline as a
        // centred line; punctuation anywhere rules it out.
        let mut image = screen();
        image.lines = vec![tall_line(
            "习近平同美国总统特朗普举行会谈，双方同意保持沟通",
            40.0,
            170.0,
            920.0,
            64.0,
        )];
        assert_eq!(read_title(&image, &opts()), None);
    }

    #[test]
    fn an_empty_band_turns_the_reader_off() {
        let off = ScreenOptions {
            title_bottom: 0.0,
            ..opts()
        };
        assert_eq!(read_title(&chat_header("小李"), &off), None);
    }

    #[test]
    fn merging_appends_only_what_is_new() {
        let mut history = vec![
            msg(Side::Other, "在吗"),
            msg(Side::Me, "在的"),
        ];
        let screen = vec![
            msg(Side::Me, "在的"),
            msg(Side::Other, "帮我看下文档"),
        ];
        assert!(merge(&mut history, &screen));
        assert_eq!(text_of(&history), ["在吗", "在的", "帮我看下文档"]);

        // The same screen again changes nothing.
        assert!(!merge(&mut history, &screen));
        assert_eq!(history.len(), 3);
    }

    #[test]
    fn merging_takes_the_whole_screen_when_messages_arrived_unseen() {
        let mut history = vec![msg(Side::Other, "在吗")];
        let screen = vec![
            msg(Side::Other, "在吗"),
            msg(Side::Me, "在的"),
            msg(Side::Other, "那说定了"),
        ];
        // Overlap of one: only the two new ones are appended.
        assert!(merge(&mut history, &screen));
        assert_eq!(text_of(&history), ["在吗", "在的", "那说定了"]);
    }

    #[test]
    fn merging_starts_over_when_the_screen_is_xphoneuse_conversation() {
        let mut history = vec![
            msg(Side::Other, "在吗"),
            msg(Side::Me, "在的"),
            msg(Side::Other, "那说定了"),
        ];
        // Another chat - or another app - nothing on screen is anything we have
        // seen. Appending these would put two people in one thread.
        let screen = vec![
            msg(Side::Me, "文件我发你邮箱了"),
            msg(Side::Other, "收到，我看下"),
        ];
        assert!(merge(&mut history, &screen));
        assert_eq!(text_of(&history), ["文件我发你邮箱了", "收到，我看下"]);

        // And the new conversation continues normally from there.
        let next = vec![msg(Side::Other, "收到，我看下"), msg(Side::Me, "好")];
        assert!(merge(&mut history, &next));
        assert_eq!(text_of(&history), ["文件我发你邮箱了", "收到，我看下", "好"]);
    }

    #[test]
    fn a_shared_greeting_is_not_enough_to_call_it_the_same_chat() {
        // "好的" is in every chat: one match out of four is a coincidence, and
        // the last-message test this used to run on would have kept the old
        // conversation alive because of it.
        let mut history = vec![msg(Side::Me, "好的"), msg(Side::Other, "嗯")];
        let screen = vec![
            msg(Side::Other, "表我已经交了"),
            msg(Side::Me, "好的"),
            msg(Side::Other, "你那边呢"),
            msg(Side::Other, "别拖了"),
        ];
        assert!(merge(&mut history, &screen));
        assert_eq!(
            text_of(&history),
            ["表我已经交了", "好的", "你那边呢", "别拖了"]
        );
    }

    #[test]
    fn a_scroll_back_past_the_history_is_still_the_same_conversation() {
        // Scrolled up past the oldest message the history kept: most of the
        // screen is known, so this is a scroll, not a new chat - resetting here
        // would trade the recent tail for older messages for no reason.
        let mut history = vec![
            msg(Side::Other, "五"),
            msg(Side::Me, "六"),
            msg(Side::Other, "七"),
        ];
        let screen = vec![
            msg(Side::Other, "四"),
            msg(Side::Other, "五"),
            msg(Side::Me, "六"),
            msg(Side::Other, "七"),
        ];
        assert!(!merge(&mut history, &screen));
        assert_eq!(text_of(&history), ["五", "六", "七"]);
    }

    #[test]
    fn merging_ignores_a_scroll_back_into_old_messages() {
        let mut history = vec![
            msg(Side::Other, "一"),
            msg(Side::Me, "二"),
            msg(Side::Other, "三"),
        ];
        // The user scrolled up: no overlap with the tail, and the last message
        // on screen is one we already know.
        let screen = vec![msg(Side::Other, "一"), msg(Side::Me, "二")];
        assert!(!merge(&mut history, &screen));
        assert_eq!(text_of(&history), ["一", "二", "三"]);
    }

    #[test]
    fn history_is_capped() {
        let mut history: Vec<Msg> = (0..MAX_HISTORY).map(|i| msg(Side::Other, &i.to_string())).collect();
        // Anchored on the tail, so this goes down the append path - the one
        // that can push the history past the cap. (A screen with nothing in
        // common would be a new conversation and reset instead.)
        let mut screen = history[history.len() - 2..].to_vec();
        screen.push(msg(Side::Other, "新的"));
        assert!(merge(&mut history, &screen));
        assert_eq!(history.len(), MAX_HISTORY);
        assert_eq!(history.last().unwrap().text, "新的");
        assert_eq!(history[0].text, "1");
    }

    #[test]
    fn the_signature_covers_the_tail() {
        let history: Vec<Msg> = (0..10).map(|i| msg(Side::Other, &i.to_string())).collect();
        let before = signature(&history);
        assert!(before.ends_with("other:9"));
        assert!(!before.contains("other:0"));
        assert_eq!(before.split('|').count(), SIGNATURE_WINDOW);
        // A new message always changes it.
        let mut extended = history.clone();
        extended.push(msg(Side::Other, "10"));
        assert_ne!(signature(&extended), before);
    }

    #[test]
    fn latest_from_reads_the_newest_message() {
        assert_eq!(latest_from(&[]), Side::Other);
        assert_eq!(latest_from(&[msg(Side::Me, "hi")]), Side::Me);
    }

    fn msg(from: Side, text: &str) -> Msg {
        Msg {
            from,
            text: text.to_string(),
        }
    }
}
