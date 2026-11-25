use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, ModifierKeyCode};

pub(crate) const HID: u32 = 0x0007_0000;

pub(crate) fn usage(key: KeyCode) -> Option<(u32, bool)> {
    let (usage, shift) = match key {
        KeyCode::Char(c @ 'a'..='z') => (4 + (c as u32 - 'a' as u32), false),
        KeyCode::Char(c @ 'A'..='Z') => (4 + (c as u32 - 'A' as u32), true),
        KeyCode::Char(c @ '1'..='9') => (0x1e + (c as u32 - '1' as u32), false),
        KeyCode::Char('0') => (0x27, false),
        KeyCode::Enter => (0x28, false),
        KeyCode::Esc => (0x29, false),
        KeyCode::Backspace => (0x2a, false),
        KeyCode::Tab | KeyCode::BackTab => (0x2b, key == KeyCode::BackTab),
        KeyCode::Char(' ') => (0x2c, false),
        KeyCode::Char(c) => return punctuation(c),
        KeyCode::CapsLock => (0x39, false),
        KeyCode::F(n @ 1..=12) => (0x39 + u32::from(n), false),
        KeyCode::F(n @ 13..=24) => (0x68 + u32::from(n) - 13, false),
        KeyCode::Insert => (0x49, false),
        KeyCode::Home => (0x4a, false),
        KeyCode::PageUp => (0x4b, false),
        KeyCode::Delete => (0x4c, false),
        KeyCode::End => (0x4d, false),
        KeyCode::PageDown => (0x4e, false),
        KeyCode::Right => (0x4f, false),
        KeyCode::Left => (0x50, false),
        KeyCode::Down => (0x51, false),
        KeyCode::Up => (0x52, false),
        KeyCode::Modifier(modifier) => (
            match modifier {
                ModifierKeyCode::LeftControl => 0xe0,
                ModifierKeyCode::LeftShift => 0xe1,
                ModifierKeyCode::LeftAlt => 0xe2,
                ModifierKeyCode::LeftSuper => 0xe3,
                ModifierKeyCode::RightControl => 0xe4,
                ModifierKeyCode::RightShift => 0xe5,
                ModifierKeyCode::RightAlt => 0xe6,
                ModifierKeyCode::RightSuper => 0xe7,
                _ => return None,
            },
            false,
        ),
        _ => return None,
    };
    Some((HID + usage, shift))
}

fn punctuation(key: char) -> Option<(u32, bool)> {
    const SYMBOLS: [(char, char, u32); 21] = [
        ('-', '_', 0x2d),
        ('=', '+', 0x2e),
        ('[', '{', 0x2f),
        (']', '}', 0x30),
        ('\\', '|', 0x31),
        (';', ':', 0x33),
        ('\'', '"', 0x34),
        ('`', '~', 0x35),
        (',', '<', 0x36),
        ('.', '>', 0x37),
        ('/', '?', 0x38),
        ('1', '!', 0x1e),
        ('2', '@', 0x1f),
        ('3', '#', 0x20),
        ('4', '$', 0x21),
        ('5', '%', 0x22),
        ('6', '^', 0x23),
        ('7', '&', 0x24),
        ('8', '*', 0x25),
        ('9', '(', 0x26),
        ('0', ')', 0x27),
    ];
    SYMBOLS.iter().find_map(|&(plain, shifted, usage)| {
        (plain == key || shifted == key).then_some((HID + usage, shifted == key))
    })
}

pub(crate) fn modifiers(flags: KeyModifiers, shifted: bool) -> impl Iterator<Item = u32> {
    [
        (0xe0, flags.contains(KeyModifiers::CONTROL)),
        (0xe1, shifted || flags.contains(KeyModifiers::SHIFT)),
        (0xe2, flags.contains(KeyModifiers::ALT)),
        (
            0xe3,
            flags.intersects(KeyModifiers::SUPER | KeyModifiers::META),
        ),
    ]
    .into_iter()
    .filter_map(|(usage, active)| active.then_some(HID + usage))
}

pub(crate) fn shortcut(key: KeyEvent, letter: char) -> bool {
    key.modifiers.contains(KeyModifiers::CONTROL)
        && matches!(key.code, KeyCode::Char(c) if c.eq_ignore_ascii_case(&letter))
}
