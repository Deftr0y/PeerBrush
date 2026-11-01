//! Editing an already admitted native draft never grows its String allocation.
//! The editor retains the real draft reservation. TextEdit's separate temporary
//! and undo-state owners must be admitted before the widget is invoked.
use crate::large_document::{Reservation, ResidentBudget};
use eframe::egui::TextBuffer;
use std::ops::Range;

pub(super) struct Fixed<'a> {
    text: &'a mut String,
    limit: usize,
    refused: &'a mut bool,
    _owner: &'a Reservation,
}
impl<'a> Fixed<'a> {
    pub(super) fn new(
        text: &'a mut String,
        limit: usize,
        pool: &ResidentBudget,
        owner: &'a Reservation,
        refused: &'a mut bool,
    ) -> Result<Self, String> {
        if !owner.uses_budget(pool)
            || text.len() > limit
            || limit > text.capacity()
            || text.capacity() > owner.bytes()
        {
            return Err("Text draft buffer differs from its resident admission".into());
        }
        Ok(Self {
            text,
            limit,
            refused,
            _owner: owner,
        })
    }
    fn byte(&self, character: usize) -> usize {
        self.text
            .char_indices()
            .nth(character)
            .map_or(self.text.len(), |(byte, _)| byte)
    }
}
impl TextBuffer for Fixed<'_> {
    fn is_mutable(&self) -> bool {
        true
    }
    fn as_str(&self) -> &str {
        self.text
    }
    fn insert_text(&mut self, text: &str, character: usize) -> usize {
        if self
            .text
            .len()
            .checked_add(text.len())
            .is_none_or(|n| n > self.limit)
        {
            *self.refused = true;
            return 0;
        }
        let byte = self.byte(character);
        // limit <= original capacity: no implicit reserve/growth is reachable.
        self.text.insert_str(byte, text);
        text.chars().count()
    }
    fn delete_char_range(&mut self, characters: Range<usize>) {
        let start = self.byte(characters.start);
        let end = self.byte(characters.end);
        if start <= end {
            self.text.replace_range(start..end, "");
        }
    }
    fn replace_with(&mut self, text: &str) {
        // The trait default clears first. A refused paste/undo must preserve
        // the old human text atomically, so validate before changing anything.
        if text.len() > self.limit {
            *self.refused = true;
            return;
        }
        self.text.clear();
        self.text.push_str(text);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fixed_utf8_edits_never_grow_and_refused_replace_keeps_original_text() {
        let pool = ResidentBudget::new(4096).unwrap();
        let owner = pool.reserve_bytes(128, 128).unwrap();
        let mut text = String::new();
        text.try_reserve_exact(64).unwrap();
        text.push_str("aé🙂z");
        let capacity = text.capacity();
        let mut refused = false;
        {
            let mut buffer = Fixed::new(&mut text, 16, &pool, &owner, &mut refused).unwrap();
            assert_eq!(buffer.insert_text("界", 2), 1);
            assert_eq!(buffer.as_str(), "aé界🙂z");
            buffer.delete_char_range(1..3);
            assert_eq!(buffer.as_str(), "a🙂z");
            assert_eq!(buffer.insert_text("🙂🙂🙂", 1), 0);
            assert_eq!(buffer.as_str(), "a🙂z");
            buffer.replace_with("🙂🙂🙂🙂🙂");
            assert_eq!(buffer.as_str(), "a🙂z");
            buffer.replace_with("é界");
            assert_eq!(buffer.as_str(), "é界");
        }
        assert!(refused);
        assert_eq!(text.capacity(), capacity);
        assert_eq!(pool.used(), 128);
        drop(text);
        drop(owner);
        assert_eq!(pool.used(), 0);
    }
    #[test]
    fn a_same_capacity_foreign_pool_is_not_an_editor_admission() {
        let pool = ResidentBudget::new(4096).unwrap();
        let other = ResidentBudget::new(4096).unwrap();
        let owner = other.reserve_bytes(128, 128).unwrap();
        let mut text = String::with_capacity(64);
        let mut refused = false;
        assert!(Fixed::new(&mut text, 16, &pool, &owner, &mut refused).is_err());
    }
}
