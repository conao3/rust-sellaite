/// The lowest xkb keycode a generated keymap assigns. xkb keycodes are evdev
/// codes plus 8, and evdev code 0 is reserved.
const FIRST_KEYCODE: u32 = 9;

/// The highest xkb keycode the compositor accepts.
const LAST_KEYCODE: u32 = 255;

/// The most distinct characters one keymap can hold.
pub const CAPACITY: usize = (LAST_KEYCODE - FIRST_KEYCODE + 1) as usize;

/// Splits the text into consecutive pieces that each use at most
/// [`CAPACITY`] distinct characters.
pub fn chunks(text: &[char]) -> Vec<&[char]> {
    let mut pieces = Vec::new();
    let mut start = 0;
    let mut seen = Vec::new();
    for (i, c) in text.iter().enumerate() {
        if !seen.contains(c) {
            if seen.len() == CAPACITY {
                pieces.push(&text[start..i]);
                start = i;
                seen.clear();
            }
            seen.push(*c);
        }
    }
    if start < text.len() {
        pieces.push(&text[start..]);
    }
    pieces
}

/// The distinct characters of the text in order of first appearance. The
/// character at index `i` is bound to xkb keycode `FIRST_KEYCODE + i`.
pub fn slots(text: &[char]) -> Vec<char> {
    let mut slots = Vec::new();
    for c in text {
        if !slots.contains(c) {
            slots.push(*c);
        }
    }
    slots
}

/// The evdev key code (xkb keycode minus 8) that types `c` with the keymap
/// built from `slots`.
pub fn evdev_key(slots: &[char], c: char) -> Option<u32> {
    slots
        .iter()
        .position(|s| *s == c)
        .map(|i| FIRST_KEYCODE + i as u32 - 8)
}

fn keysym(c: char) -> String {
    match c {
        '\n' => "Return".to_string(),
        '\t' => "Tab".to_string(),
        '\u{8}' => "BackSpace".to_string(),
        '\u{1b}' => "Escape".to_string(),
        c => format!("U{:04X}", c as u32),
    }
}

/// An xkb keymap in the text format that binds each slot to its keycode.
pub fn xkb_source(slots: &[char]) -> String {
    let mut codes = String::new();
    let mut symbols = String::new();
    for (i, c) in slots.iter().enumerate() {
        let code = FIRST_KEYCODE + i as u32;
        codes.push_str(&format!("    <K{code}> = {code};\n"));
        symbols.push_str(&format!("    key <K{code}> {{ [ {} ] }};\n", keysym(*c)));
    }
    format!(
        "xkb_keymap {{\n\
         xkb_keycodes \"sellaite\" {{\n    minimum = 8;\n    maximum = {LAST_KEYCODE};\n{codes}}};\n\
         xkb_types \"sellaite\" {{ include \"complete\" }};\n\
         xkb_compatibility \"sellaite\" {{ include \"complete\" }};\n\
         xkb_symbols \"sellaite\" {{\n{symbols}}};\n\
         }};\n"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_character_has_a_distinct_key() {
        let text: Vec<char> = "hello, world\n".chars().collect();
        let slots = slots(&text);
        let mut keys: Vec<u32> = text
            .iter()
            .map(|c| evdev_key(&slots, *c).unwrap())
            .collect();
        assert_eq!(evdev_key(&slots, 'h'), Some(1));
        keys.sort();
        keys.dedup();
        assert_eq!(keys.len(), slots.len());
    }

    #[test]
    fn chunks_cover_the_text_within_capacity() {
        let text: Vec<char> = (0..600u32)
            .map(|i| char::from_u32(0x4e00 + i % 400).unwrap())
            .collect();
        let pieces = chunks(&text);
        assert_eq!(pieces.concat(), text);
        assert!(pieces.iter().all(|p| slots(p).len() <= CAPACITY));
        assert!(pieces.len() > 1);
    }

    #[test]
    fn keymap_names_unicode_and_control_keys() {
        let source = xkb_source(&['a', '\n', 'あ']);
        assert!(source.contains("<K9> = 9;"));
        assert!(source.contains("key <K9> { [ U0061 ] };"));
        assert!(source.contains("key <K10> { [ Return ] };"));
        assert!(source.contains("key <K11> { [ U3042 ] };"));
    }
}
