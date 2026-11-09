//! Which conversation is on screen, and what was said in it.
//!
//! One history per conversation, named by the string the app prints in its
//! header ([`transcript::read_title`](crate::transcript::read_title)): that is
//! the only identity a mirror has. Android's accessibility tree would name the
//! chat outright, and that is what the reference implementation reads; a
//! picture of the screen does not.
//!
//! The name is therefore treated as a bonus, not a requirement. When the header
//! gives up nothing - the reader is deliberately hard to satisfy - the screen
//! continues in whichever conversation is current, and
//! [`merge`](crate::transcript::merge) keeps that honest by replacing a history
//! the screen does not continue instead of splicing onto it. Naming a
//! conversation can only make the boundaries sharper; it can never make a wrong
//! one right, because the merge rule still runs inside every buffer.
//!
//! In memory only, like everything else in this crate: nothing is written to
//! disk, and closing the app forgets every conversation.

use crate::transcript::{self, Msg};

/// How many conversations are remembered at once.
///
/// Each is capped at [`transcript::MAX_HISTORY`] messages, so this is the whole
/// budget for history. A chat the user was in half an hour ago is worth keeping
/// - coming back to it should not start from nothing - but not at any price,
/// and the least recently used one is the first to go.
pub const MAX_CONVERSATIONS: usize = 8;

/// The conversations the copilot has read, most recently used first.
#[derive(Debug, Default)]
pub struct Conversations {
    entries: Vec<Entry>,
}

#[derive(Debug)]
struct Entry {
    /// `None` for a conversation whose name could not be read: everything read
    /// while no name was available lands here, and the merge rule separates it
    /// from a real switch as well as pixels can.
    title: Option<String>,
    messages: Vec<Msg>,
}

impl Conversations {
    pub fn new() -> Self {
        Self::default()
    }

    /// The conversation on screen now, with `screen` folded into it.
    ///
    /// `title` is `Some` only when the header was read with confidence; `None`
    /// means "no name was available", not "a conversation without a name".
    pub fn absorb(&mut self, title: Option<String>, screen: &[Msg]) -> &[Msg] {
        let index = self.entry_for(title, screen);
        let entry = self.entries.remove(index);
        self.entries.insert(0, entry);
        transcript::merge(&mut self.entries[0].messages, screen);
        self.entries.truncate(MAX_CONVERSATIONS);
        &self.entries[0].messages
    }

    /// The index of the entry a screen with this name belongs to, creating or
    /// renaming one when it has to.
    fn entry_for(&mut self, title: Option<String>, screen: &[Msg]) -> usize {
        let Some(name) = title else {
            // No name on screen. Two screens land here and they want opposite
            // treatment: the current chat with its name hidden for a moment (a
            // typing indicator over the header, a scroll that pushed it out),
            // and something that is not that chat at all (a dialog, the home
            // screen, an app with no header). The screen itself decides -
            // whether it continues the conversation that is open - and anything
            // that does not goes to the unnamed entry instead of being merged
            // into a chat it has nothing to do with, which would cost that chat
            // its history for nothing.
            let stays = match self.entries.first() {
                None => true,
                Some(entry) => {
                    entry.title.is_none() || transcript::continues(&entry.messages, screen)
                }
            };
            if stays {
                if self.entries.is_empty() {
                    self.entries.push(Entry {
                        title: None,
                        messages: Vec::new(),
                    });
                }
                return 0;
            }
            return match self.entries.iter().position(|entry| entry.title.is_none()) {
                Some(index) => index,
                None => {
                    self.entries.push(Entry {
                        title: None,
                        messages: Vec::new(),
                    });
                    self.entries.len() - 1
                }
            };
        };
        match self
            .entries
            .iter()
            .position(|entry| entry.title.as_deref() == Some(name.as_str()))
        {
            Some(index) => index,
            None => {
                // A name not seen before. If the current conversation never had
                // a name, this is almost certainly that same conversation - the
                // header only just became readable - so the name is adopted and
                // the history kept. If it is in fact another chat, nothing is
                // lost by trying: the merge throws that history out, because
                // nothing on this screen matches it.
                if matches!(self.entries.first(), Some(Entry { title: None, .. })) {
                    self.entries[0].title = Some(name);
                    return 0;
                }
                self.entries.insert(
                    0,
                    Entry {
                        title: Some(name),
                        messages: Vec::new(),
                    },
                );
                0
            }
        }
    }

    /// The name of the conversation on screen, when it has one.
    pub fn title(&self) -> Option<&str> {
        self.entries.first().and_then(|entry| entry.title.as_deref())
    }

    /// What has been read of the current conversation.
    pub fn messages(&self) -> &[Msg] {
        self.entries
            .first()
            .map(|entry| entry.messages.as_slice())
            .unwrap_or(&[])
    }

    /// Forget every conversation.
    pub fn clear(&mut self) {
        self.entries.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transcript::Side;

    fn msg(from: Side, text: &str) -> Msg {
        Msg {
            from,
            text: text.to_string(),
        }
    }

    fn text_of(messages: &[Msg]) -> Vec<String> {
        messages.iter().map(|m| m.text.clone()).collect()
    }

    #[test]
    fn two_chats_do_not_share_a_buffer() {
        let mut store = Conversations::new();
        store.absorb(
            Some("小李".into()),
            &[msg(Side::Other, "在吗"), msg(Side::Me, "在的")],
        );
        store.absorb(Some("工作群".into()), &[msg(Side::Other, "明天九点开会")]);

        assert_eq!(store.title(), Some("工作群"));
        assert_eq!(text_of(store.messages()), ["明天九点开会"]);

        // Back to the first chat: what was read of it is still there.
        store.absorb(Some("小李".into()), &[msg(Side::Me, "在的"), msg(Side::Other, "帮我看看")]);
        assert_eq!(store.title(), Some("小李"));
        assert_eq!(text_of(store.messages()), ["在吗", "在的", "帮我看看"]);
    }

    #[test]
    fn a_nameless_screen_that_continues_the_chat_stays_in_it() {
        // The header hidden for a moment - a typing indicator over it, a scroll
        // that pushed it out - while the conversation itself carries on.
        let mut store = Conversations::new();
        store.absorb(
            Some("小李".into()),
            &[msg(Side::Other, "在吗"), msg(Side::Me, "在的")],
        );
        store.absorb(None, &[msg(Side::Me, "在的"), msg(Side::Other, "帮我看看")]);

        assert_eq!(store.title(), Some("小李"));
        assert_eq!(text_of(store.messages()), ["在吗", "在的", "帮我看看"]);
    }

    #[test]
    fn a_nameless_screen_that_is_something_else_costs_no_history() {
        // A dialog, the home screen, an app with no header: none of it is the
        // chat that is open, and none of it may empty it.
        let mut store = Conversations::new();
        store.absorb(
            Some("小李".into()),
            &[msg(Side::Other, "在吗"), msg(Side::Me, "在的")],
        );
        store.absorb(None, &[msg(Side::Other, "确定"), msg(Side::Other, "取消")]);

        // The screen on display is that other thing, honestly reported.
        assert_eq!(store.title(), None);
        assert_eq!(text_of(store.messages()), ["确定", "取消"]);

        // And the chat is still there when it comes back.
        store.absorb(Some("小李".into()), &[msg(Side::Me, "在的")]);
        assert_eq!(store.title(), Some("小李"));
        assert_eq!(text_of(store.messages()), ["在吗", "在的"]);
    }

    #[test]
    fn a_name_that_arrives_late_takes_over_the_unnamed_chat() {
        // Read while the header was unreadable, then read again once it was
        // not: the same conversation, and it keeps what was already seen.
        let mut store = Conversations::new();
        store.absorb(None, &[msg(Side::Other, "在吗")]);
        store.absorb(Some("小李".into()), &[msg(Side::Other, "在吗"), msg(Side::Me, "在的")]);

        assert_eq!(store.title(), Some("小李"));
        assert_eq!(text_of(store.messages()), ["在吗", "在的"]);
    }

    #[test]
    fn an_unnamed_chat_is_not_handed_a_name_it_does_not_belong_to() {
        // Same shape, except the screen is a different conversation: adopting
        // the name must not drag the old history along.
        let mut store = Conversations::new();
        store.absorb(None, &[msg(Side::Other, "在吗"), msg(Side::Me, "在的")]);
        store.absorb(Some("小李".into()), &[msg(Side::Other, "合同我发你邮箱了")]);

        assert_eq!(store.title(), Some("小李"));
        assert_eq!(text_of(store.messages()), ["合同我发你邮箱了"]);
    }

    #[test]
    fn the_oldest_conversation_is_the_first_to_be_forgotten() {
        let mut store = Conversations::new();
        for i in 0..MAX_CONVERSATIONS {
            store.absorb(Some(format!("聊天{i}")), &[msg(Side::Other, "在吗")]);
        }
        // One more, then ask for the first one again: it is gone, and it comes
        // back as a new conversation rather than with its old history.
        store.absorb(Some("新聊天".into()), &[msg(Side::Other, "在吗")]);
        store.absorb(Some("聊天0".into()), &[msg(Side::Other, "在吗")]);
        assert_eq!(text_of(store.messages()), ["在吗"]);
    }

    #[test]
    fn clearing_forgets_everything() {
        let mut store = Conversations::new();
        store.absorb(Some("小李".into()), &[msg(Side::Other, "在吗")]);
        store.clear();
        assert_eq!(store.title(), None);
        assert!(store.messages().is_empty());
    }
}
