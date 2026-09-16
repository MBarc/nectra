use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

#[derive(Default)]
pub(super) struct Draft {
    pub text: String,
    cursor: usize,
}
impl Draft {
    pub fn insert(&mut self, text: &str) {
        self.text.insert_str(self.cursor, text);
        let end = self.cursor + text.len();
        self.cursor = self
            .text
            .grapheme_indices(true)
            .map(|(i, _)| i)
            .find(|i| *i >= end)
            .unwrap_or(self.text.len());
    }
    pub fn key(&mut self, key: KeyEvent) {
        let previous = self
            .text
            .grapheme_indices(true)
            .map(|(i, _)| i)
            .take_while(|i| *i < self.cursor)
            .last()
            .unwrap_or(0);
        let next = self
            .text
            .grapheme_indices(true)
            .map(|(i, _)| i)
            .find(|i| *i > self.cursor)
            .unwrap_or(self.text.len());
        match key.code {
            KeyCode::Left => self.cursor = previous,
            KeyCode::Right => self.cursor = next,
            KeyCode::Home => self.cursor = 0,
            KeyCode::End => self.cursor = self.text.len(),
            KeyCode::Backspace => {
                self.text.drain(previous..self.cursor);
                self.cursor = previous;
            }
            KeyCode::Delete => {
                self.text.drain(self.cursor..next);
            }
            KeyCode::Char(c)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                self.insert(&c.to_string())
            }
            _ => {}
        }
    }
    pub fn take(&mut self) -> String {
        self.cursor = 0;
        std::mem::take(&mut self.text)
    }
    pub fn view(&self, width: usize) -> (String, usize) {
        if width == 0 {
            return (String::new(), 0);
        }
        let display = |g: &str| {
            g.chars()
                .map(|c| if c.is_control() { ' ' } else { c })
                .collect::<String>()
        };
        let mut start = self.cursor;
        let mut column = 0;
        for (offset, cluster) in self.text[..self.cursor].grapheme_indices(true).rev() {
            let size = display(cluster).width();
            if column + size >= width {
                break;
            }
            start = offset;
            column += size;
        }
        let mut visible = String::new();
        let mut used = 0;
        for cluster in self.text[start..].graphemes(true) {
            let cluster = display(cluster);
            let size = cluster.width();
            if used + size > width {
                break;
            }
            visible.push_str(&cluster);
            used += size;
        }
        (visible, column)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyModifiers;
    fn key(draft: &mut Draft, code: KeyCode) {
        draft.key(KeyEvent::new(code, KeyModifiers::NONE));
    }
    #[test]
    fn edits_in_the_middle_and_at_both_ends() {
        let mut d = Draft::default();
        d.insert("ac");
        key(&mut d, KeyCode::Left);
        d.insert("b");
        assert_eq!(d.text, "abc");
        key(&mut d, KeyCode::Home);
        key(&mut d, KeyCode::Delete);
        assert_eq!(d.text, "bc");
        key(&mut d, KeyCode::End);
        key(&mut d, KeyCode::Backspace);
        assert_eq!(d.text, "b");
    }
    #[test]
    fn deletes_complete_emoji_and_combining_clusters() {
        let mut d = Draft::default();
        d.insert("e\u{301}👩‍💻");
        key(&mut d, KeyCode::Backspace);
        assert_eq!(d.text, "e\u{301}");
        key(&mut d, KeyCode::Home);
        key(&mut d, KeyCode::Delete);
        assert!(d.text.is_empty());
    }
    #[test]
    fn viewport_tracks_the_cursor_and_uses_terminal_columns() {
        let mut d = Draft::default();
        d.insert("0123456789界Z");
        let (text, col) = d.view(6);
        assert!(text.ends_with('Z'));
        assert!(col < 6);
        assert!(unicode_width::UnicodeWidthStr::width(text.as_str()) <= 6);
        key(&mut d, KeyCode::Home);
        assert!(d.view(6).0.starts_with('0'));
    }
}
