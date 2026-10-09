//! Editable draft state. Cursor positions are characters internally and UTF-16 on TSP.

use tern_sdk::wire::Edit;

#[derive(Default)]
pub struct Composer {
    pub text: String,
    pub images: Vec<super::attach::ImageAttachment>,
    pub cursor: usize,
    undo: Vec<(String, usize)>,
    history: Vec<String>,
    history_index: Option<usize>,
    saved_draft: String,
}

impl Composer {
    fn byte(&self, chars: usize) -> usize {
        self.text
            .char_indices()
            .nth(chars)
            .map_or(self.text.len(), |(i, _)| i)
    }

    pub fn utf16_cursor(&self) -> usize {
        self.text
            .chars()
            .take(self.cursor)
            .map(char::len_utf16)
            .sum()
    }

    fn char_at_utf16(&self, position: usize) -> Option<usize> {
        let mut utf16 = 0;
        for (index, c) in self.text.chars().enumerate() {
            if position == utf16 {
                return Some(index);
            }
            utf16 += c.len_utf16();
        }
        (utf16 == position).then_some(self.text.chars().count())
    }

    fn save(&mut self) {
        self.undo.push((self.text.clone(), self.cursor));
        self.history_index = None;
    }

    pub fn insert(&mut self, value: &str) {
        if value.is_empty() {
            return;
        }
        self.save();
        let byte = self.byte(self.cursor);
        self.text.insert_str(byte, value);
        self.cursor += value.chars().count();
    }
    /// Replace the draft as a single undoable edit (for editing a sent prompt).
    pub fn replace(&mut self, text: &str) {
        self.save();
        self.text.clear();
        self.text.push_str(text);
        self.cursor = text.chars().count();
    }

    pub fn backspace(&mut self) {
        if self.cursor > 0 {
            self.save();
            let end = self.byte(self.cursor);
            let start = self.byte(self.cursor - 1);
            self.text.replace_range(start..end, "");
            self.cursor -= 1;
        }
    }

    pub fn delete(&mut self) {
        if self.cursor < self.text.chars().count() {
            self.save();
            let start = self.byte(self.cursor);
            let end = self.byte(self.cursor + 1);
            self.text.replace_range(start..end, "");
        }
    }

    /// Reject events against a stale editor version or splitting a surrogate pair.
    pub fn edit(&mut self, edit: &Edit) -> bool {
        let length: usize = self.text.encode_utf16().count();
        if edit.id != crate::ui::view::EDITOR_ID || edit.len != length || edit.from > edit.to {
            return false;
        }
        let (Some(from), Some(to)) = (self.char_at_utf16(edit.from), self.char_at_utf16(edit.to))
        else {
            return false;
        };
        let next_length = length - (edit.to - edit.from) + edit.text.encode_utf16().count();
        if edit.cursor > next_length {
            return false;
        }
        self.save();
        let start = self.byte(from);
        let end = self.byte(to);
        self.text.replace_range(start..end, &edit.text);
        self.cursor = self
            .char_at_utf16(edit.cursor)
            .unwrap_or(from + edit.text.chars().count());
        true
    }

    pub fn undo(&mut self) -> bool {
        if let Some((text, cursor)) = self.undo.pop() {
            self.text = text;
            self.cursor = cursor;
            self.history_index = None;
            true
        } else {
            false
        }
    }

    pub fn take(&mut self) -> Option<String> {
        if self.text.trim().is_empty() && self.images.is_empty() {
            return None;
        }
        let text = std::mem::take(&mut self.text);
        self.cursor = 0;
        self.undo.clear();
        self.history_index = None;
        if !text.is_empty() {
            self.history.push(text.clone());
        }
        Some(text)
    }

    pub fn up(&mut self) -> bool {
        if self.history.is_empty() || (self.cursor != 0 && !self.text.is_empty()) {
            return false;
        }
        let index = match self.history_index {
            Some(0) => return false,
            Some(i) => i - 1,
            None => {
                self.saved_draft = self.text.clone();
                self.history.len() - 1
            }
        };
        self.history_index = Some(index);
        self.text.clone_from(&self.history[index]);
        self.cursor = self.text.chars().count();
        true
    }

    pub fn down(&mut self) -> bool {
        if self.cursor != self.text.chars().count() && !self.text.is_empty() {
            return false;
        }
        let Some(index) = self.history_index else {
            return false;
        };
        if index + 1 == self.history.len() {
            self.history_index = None;
            self.text.clone_from(&self.saved_draft);
        } else {
            self.history_index = Some(index + 1);
            self.text.clone_from(&self.history[index + 1]);
        }
        self.cursor = self.text.chars().count();
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tern_sdk::wire::Event;

    fn edit(body: serde_json::Value) -> Edit {
        let Event::Edit(edit) = Event::decode(body.as_object().unwrap().clone()) else {
            panic!("edit")
        };
        edit
    }

    #[test]
    fn utf16_edit_rejects_stale_and_surrogate_splits() {
        let mut c = Composer::default();
        c.insert("a😀z");
        let e = edit(
            serde_json::json!({"ev":"edit","id":crate::ui::view::EDITOR_ID,"from":1,"to":3,"text":"é","cursor":2,"len":4}),
        );
        assert!(c.edit(&e));
        assert_eq!((c.text.as_str(), c.utf16_cursor()), ("aéz", 2));
        assert!(!c.edit(&e));
        c.undo();
        let split = edit(
            serde_json::json!({"ev":"edit","id":crate::ui::view::EDITOR_ID,"from":2,"to":3,"text":"x","cursor":3,"len":4}),
        );
        assert!(!c.edit(&split));
    }

    #[test]
    fn undo_and_history_restore_draft() {
        let mut c = Composer::default();
        c.insert("one");
        c.backspace();
        assert!(c.undo());
        assert_eq!(c.take().as_deref(), Some("one"));
        c.insert("draft");
        c.cursor = 0;
        assert!(c.up());
        assert_eq!(c.text, "one");
        assert!(c.down());
        assert_eq!(c.text, "draft");
    }
    #[test]
    fn replacing_a_draft_can_be_undone() {
        let mut c = Composer::default();
        c.insert("unsent");
        c.replace("older 😀");
        assert_eq!((c.text.as_str(), c.utf16_cursor()), ("older 😀", 8));
        assert!(c.undo());
        assert_eq!(c.text, "unsent");
    }
}
