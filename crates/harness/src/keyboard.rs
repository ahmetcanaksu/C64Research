//! Turning characters into C64 keypresses.
//!
//! The C64 has no notion of a "character" arriving — it has a matrix of keys
//! that the KERNAL scans 60 times a second. So typing, whether from a host
//! keyboard or from a script, means pressing and releasing real keys with
//! realistic timing.
//!
//! The mapping itself is taken straight from the **KERNAL's own decode tables**
//! rather than written by hand, so it is the C64's mapping by construction.
//! That also makes it host-layout independent: whatever key you pressed, we work
//! from the *character* it produced.

use std::collections::{HashMap, VecDeque};

use c64::C64;


/// SHIFT's position in the C64 key matrix.
pub const SHIFT_KEY: u8 = 15;
/// RUN/STOP's position — the C64's break key.
pub const RUN_STOP_KEY: u8 = 63;

/// A character mapped to the key that types it: matrix code, and whether SHIFT
/// has to be held.
pub type KeyMap = HashMap<char, (u8, bool)>;

/// PETSCII byte -> ASCII-range char (the two coincide over `$20..=$5F`).
fn petscii_char(b: u8) -> Option<char> {
    match b {
        0x20..=0x5F => Some(b as char),
        _ => None,
    }
}

/// Is this code point a printable character rather than a control code?
///
/// Control codes are handled as *keys*, never as characters, because host
/// backends disagree about whether they even deliver them: minifb's macOS
/// backend drops every code point below 32 (and `$7F..$A0`) before the callback,
/// while its Windows one passes them straight through. Filtering uniformly is
/// what keeps RETURN working on both.
pub fn is_printable(code_point: u32) -> bool {
    code_point >= 32 && !(0x7F..0xA0).contains(&code_point)
}

/// Build a character -> key map from the KERNAL's keyboard decode tables.
pub fn char_map() -> KeyMap {
    use kernal::irq::{SHIFTED_KEYS, UNSHIFTED_KEYS};
    let mut m = HashMap::new();
    for idx in 0..64usize {
        if let Some(ch) = petscii_char(UNSHIFTED_KEYS[idx]) {
            m.entry(ch).or_insert((idx as u8, false));
            if ch.is_ascii_uppercase() {
                m.entry(ch.to_ascii_lowercase()).or_insert((idx as u8, false));
            }
        }
        if let Some(ch) = petscii_char(SHIFTED_KEYS[idx]) {
            m.entry(ch).or_insert((idx as u8, true));
        }
    }
    // Control codes. Typed ones never reach here (see `is_printable`), but
    // *pasted* and *scripted* text goes into the queue directly, so a newline
    // still has to become RETURN.
    m.insert('\r', (1, false));
    m.insert('\n', (1, false));
    m.insert('\u{8}', (0, false)); // backspace -> INST/DEL
    m.insert('\u{7f}', (0, false));
    m
}

/// Types queued characters into the C64's keyboard matrix, one at a time.
///
/// A real key is pressed and released. The KERNAL only registers a keystroke if
/// its 60 Hz scan (`SCNKEY`, `$EA87`) catches the key down, and only registers a
/// *second* one if a scan catches the key back up in between — it remembers the
/// last key index in `$00CB` to debounce. So each character is held for
/// [`Typist::HOLD_FRAMES`] frames, then followed by [`Typist::GAP_FRAMES`] frames
/// of released keyboard.
///
/// Those two numbers are why this is a type and not three lines inline: too
/// small and characters vanish, and the only symptom is a subtly mistyped
/// command a long way downstream.
#[derive(Default)]
pub struct Typist {
    /// The key being held: matrix code, whether SHIFT is down, frames left.
    hold: Option<(u8, bool, u8)>,
    /// Frames of released keyboard still owed before the next character.
    gap: u8,
    /// Characters seen with no C64 key to type them.
    dropped: Vec<char>,
}

impl Typist {
    /// Frames to hold a key down. A frame is 20 ms and the KERNAL scans every
    /// ~16.7 ms, so two frames guarantees a scan sees it.
    pub const HOLD_FRAMES: u8 = 2;
    /// Frames with no key pressed afterwards, so a scan can observe the release.
    pub const GAP_FRAMES: u8 = 2;

    /// True while a character is still being delivered.
    pub fn busy(&self) -> bool {
        self.hold.is_some() || self.gap > 0
    }

    /// Characters that had no C64 key and were skipped.
    ///
    /// Worth checking in a test: a silently dropped character is the difference
    /// between `LOAD"*",8,1` and something that loads from tape.
    pub fn dropped(&self) -> &[char] {
        &self.dropped
    }

    /// Advance one frame, pressing keys into `c64`'s matrix.
    ///
    /// The caller clears the matrix first, so this is additive with any keys a
    /// host is physically holding.
    pub fn frame(&mut self, c64: &mut C64, queue: &mut VecDeque<char>, map: &KeyMap) {
        if let Some((code, shift, left)) = self.hold {
            press(c64, code, shift);
            self.hold = if left > 1 {
                Some((code, shift, left - 1))
            } else {
                self.gap = Self::GAP_FRAMES;
                None
            };
        } else if self.gap > 0 {
            self.gap -= 1;
        } else if let Some(c) = queue.pop_front() {
            match map.get(&c) {
                Some(&(code, shift)) => {
                    press(c64, code, shift);
                    self.hold = Some((code, shift, Self::HOLD_FRAMES - 1));
                }
                None => self.dropped.push(c),
            }
        }
    }
}

fn press(c64: &mut C64, code: u8, shift: bool) {
    c64.board.set_key(code, true);
    if shift {
        c64.board.set_key(SHIFT_KEY, true);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_the_characters_a_load_command_needs() {
        let m = char_map();
        for c in "load\"*\",8,1\r".chars() {
            assert!(m.contains_key(&c), "no C64 key for {c:?}");
        }
    }

    #[test]
    fn shifted_characters_are_marked_as_shifted() {
        let m = char_map();
        let (_, shift) = m[&'"'];
        assert!(shift, "a quote is SHIFT+2 on a C64");
        let (_, shift) = m[&'8'];
        assert!(!shift, "a digit is unshifted");
    }

    #[test]
    fn control_codes_are_not_characters() {
        for cp in [0x00, 0x08, 0x0A, 0x0D, 0x1B, 0x7F, 0x9F] {
            assert!(!is_printable(cp), "${cp:02X} is a control code");
        }
        for cp in [0x20, b'A' as u32, 0x7E, 0xA0] {
            assert!(is_printable(cp), "${cp:02X} is printable");
        }
    }

    /// A machine with blank ROMs. Enough to exercise the key matrix, since
    /// nothing here ever steps the CPU.
    fn matrix_only_machine() -> Box<C64> {
        Box::new(C64::new(&[0; 0x2000], &[0; 0x2000], &[0; 0x1000]))
    }

    #[test]
    fn unmapped_characters_are_recorded_not_swallowed() {
        let map = char_map();
        let mut c64 = matrix_only_machine();
        let mut q: VecDeque<char> = "a\u{1F600}b".chars().collect();
        let mut t = Typist::default();

        // Run long enough to drain all three.
        for _ in 0..24 {
            c64.board.key_matrix = [0; 8];
            t.frame(&mut c64, &mut q, &map);
        }
        assert!(q.is_empty(), "every character should have been consumed");
        assert_eq!(t.dropped(), &['\u{1F600}'], "the emoji has no C64 key");
    }

    /// Each character is held, then released, so the KERNAL's scan sees both.
    #[test]
    fn a_character_is_pressed_then_released() {
        let map = char_map();
        let (code, _) = map[&'a'];
        let (line, bit) = ((code / 8) as usize, code % 8);

        let mut c64 = matrix_only_machine();
        let mut q: VecDeque<char> = "a".chars().collect();
        let mut t = Typist::default();

        let mut pressed = 0;
        let mut released_after = false;
        for _ in 0..(Typist::HOLD_FRAMES + Typist::GAP_FRAMES + 1) {
            c64.board.key_matrix = [0; 8];
            t.frame(&mut c64, &mut q, &map);
            if c64.board.key_matrix[line] & (1 << bit) != 0 {
                pressed += 1;
            } else if pressed > 0 {
                released_after = true;
            }
        }
        assert_eq!(pressed, Typist::HOLD_FRAMES, "held for HOLD_FRAMES frames");
        assert!(released_after, "and released afterwards, or the next key is debounced away");
    }
}
