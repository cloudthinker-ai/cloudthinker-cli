use std::path::{Path, PathBuf};

use super::wire::MAX_UPLOAD;

const ESC: u8 = 0x1b;
const BEL: u8 = 0x07;
const CTRL_V: u8 = 0x16;
const CTRL_C: u8 = 0x03;
const CTRL_Z: u8 = 0x1a;
const PASTE_START: &[u8] = b"\x1b[200~";
const PASTE_END: &[u8] = b"\x1b[201~";
const KEY_V: u32 = 118;
const KEY_C: u32 = 99;
const KEY_Z: u32 = 122;
const MODIFY_OTHER_KEYS: u32 = 27;
const CTRL: u32 = 4;
const LOCKS: u32 = 64 | 128;
const MAX_HELD: usize = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyEvent {
    Forward(Vec<u8>),
    PasteKey(Vec<u8>),
    PasteKeyRepeat(Vec<u8>),
    PasteKeyRelease(Vec<u8>),
    Paste { text: Vec<u8>, raw: Vec<u8> },
    Detach,
}

#[derive(Debug, Default)]
pub struct Scanner {
    held: Vec<u8>,
    paste: Option<Vec<u8>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Token {
    Plain,
    PasteKey,
    PasteKeyRepeat,
    PasteKeyRelease,
    PasteStart,
    Detach,
}

#[derive(Default)]
struct Events(Vec<KeyEvent>);

impl Events {
    fn forward(&mut self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        match self.0.last_mut() {
            Some(KeyEvent::Forward(last)) => last.extend_from_slice(bytes),
            _ => self.0.push(KeyEvent::Forward(bytes.to_vec())),
        }
    }

    fn push(&mut self, event: KeyEvent) {
        self.0.push(event);
    }
}

impl Scanner {
    pub fn has_pending(&self) -> bool {
        !self.held.is_empty() || self.paste.is_some()
    }

    pub fn feed(&mut self, bytes: &[u8]) -> Vec<KeyEvent> {
        let mut events = Events::default();
        let mut input = std::mem::take(&mut self.held);
        input.extend_from_slice(bytes);
        let mut rest = input.as_slice();
        while !rest.is_empty() {
            if self.paste.is_some() {
                let used = self.take_paste(rest, &mut events);
                rest = rest.get(used..).unwrap_or_default();
                continue;
            }
            let Some((token, len)) = next_token(rest) else {
                self.hold(rest, &mut events);
                break;
            };
            let (bytes, tail) = rest.split_at_checked(len).unwrap_or((rest, &[]));
            match token {
                Token::Plain => events.forward(bytes),
                Token::PasteKey => events.push(KeyEvent::PasteKey(bytes.to_vec())),
                Token::PasteKeyRepeat => events.push(KeyEvent::PasteKeyRepeat(bytes.to_vec())),
                Token::PasteKeyRelease => events.push(KeyEvent::PasteKeyRelease(bytes.to_vec())),
                Token::PasteStart => self.paste = Some(bytes.to_vec()),
                Token::Detach => events.push(KeyEvent::Detach),
            }
            rest = tail;
        }
        events.0
    }

    pub fn flush(&mut self) -> Vec<KeyEvent> {
        let mut events = Events::default();
        if let Some(paste) = self.paste.take() {
            events.forward(&paste);
        }
        events.forward(&std::mem::take(&mut self.held));
        events.0
    }

    fn hold(&mut self, rest: &[u8], events: &mut Events) {
        if rest.len() > MAX_HELD {
            events.forward(rest);
        } else {
            self.held = rest.to_vec();
        }
    }

    fn take_paste(&mut self, bytes: &[u8], events: &mut Events) -> usize {
        let Some(paste) = self.paste.as_mut() else {
            return 0;
        };
        let before = paste.len();
        let from = before
            .saturating_sub(PASTE_END.len() - 1)
            .max(PASTE_START.len());
        paste.extend_from_slice(bytes);
        if let Some(found) = find(paste.get(from..).unwrap_or_default(), PASTE_END) {
            let end = from + found + PASTE_END.len();
            paste.truncate(end);
            if let Some(raw) = self.paste.take() {
                events.push(paste_event(raw));
            }
            return end.saturating_sub(before);
        }
        if paste.len() > MAX_UPLOAD
            && let Some(raw) = self.paste.take()
        {
            events.forward(&raw);
        }
        bytes.len()
    }
}

fn paste_event(raw: Vec<u8>) -> KeyEvent {
    let text = raw
        .get(PASTE_START.len()..raw.len().saturating_sub(PASTE_END.len()))
        .unwrap_or_default()
        .to_vec();
    KeyEvent::Paste { text, raw }
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn next_token(bytes: &[u8]) -> Option<(Token, usize)> {
    match bytes.first() {
        Some(&CTRL_V) => Some((Token::PasteKey, 1)),
        Some(&CTRL_Z) => Some((Token::Detach, 1)),
        Some(&ESC) => escape(bytes),
        _ => {
            let len = bytes
                .iter()
                .position(|&byte| matches!(byte, ESC | CTRL_V | CTRL_Z))
                .unwrap_or(bytes.len());
            Some((Token::Plain, len))
        }
    }
}

fn escape(bytes: &[u8]) -> Option<(Token, usize)> {
    match *bytes.get(1)? {
        b'[' => {
            let len = csi_len(bytes)?;
            Some((csi_token(bytes.get(..len).unwrap_or_default()), len))
        }
        b'O' => bytes.get(2).map(|_| (Token::Plain, 3)),
        b']' | b'P' | b'_' => string_len(bytes).map(|len| (Token::Plain, len)),
        ESC => Some((Token::Plain, 1)),
        _ => Some((Token::Plain, 2)),
    }
}

fn csi_len(bytes: &[u8]) -> Option<usize> {
    for (index, &byte) in bytes.iter().enumerate().skip(2) {
        match byte {
            0x20..=0x3f => {}
            0x40..=0x7e => return Some(index + 1),
            _ => return Some(index),
        }
    }
    None
}

fn string_len(bytes: &[u8]) -> Option<usize> {
    for (index, &byte) in bytes.iter().enumerate().skip(2) {
        match byte {
            BEL => return Some(index + 1),
            ESC => {
                return match bytes.get(index + 1) {
                    Some(b'\\') => Some(index + 2),
                    Some(_) => Some(index),
                    None => None,
                };
            }
            0x00..=0x1f => return Some(index),
            _ => {}
        }
    }
    None
}

fn csi_token(sequence: &[u8]) -> Token {
    if sequence == PASTE_START {
        return Token::PasteStart;
    }
    match ctrl_key(sequence) {
        Some((KEY_V, 1)) => Token::PasteKey,
        Some((KEY_V, 2)) => Token::PasteKeyRepeat,
        Some((KEY_V, 3)) => Token::PasteKeyRelease,
        Some((KEY_Z, 1)) => Token::Detach,
        _ => Token::Plain,
    }
}

fn ctrl_key(sequence: &[u8]) -> Option<(u32, u32)> {
    let (&last, body) = sequence.get(2..)?.split_last()?;
    let fields = CsiFields::parse(std::str::from_utf8(body).ok()?);
    match last {
        b'u' if is_ctrl_only(fields.get(1, 0)) => {
            Some((fields.get(0, 0)?, fields.get(1, 1).unwrap_or(1)))
        }
        b'~' if fields.get(0, 0) == Some(MODIFY_OTHER_KEYS) && is_ctrl_only(fields.get(1, 0)) => {
            Some((fields.get(2, 0)?, 1))
        }
        _ => None,
    }
}

pub fn interrupts(bytes: &[u8]) -> bool {
    let mut rest = bytes;
    while let Some(&first) = rest.first() {
        if first == CTRL_C {
            return true;
        }
        let len = if rest.get(1) == Some(&b'[') && first == ESC {
            let Some(len) = csi_len(rest) else {
                return false;
            };
            if ctrl_key(rest.get(..len).unwrap_or_default()) == Some((KEY_C, 1)) {
                return true;
            }
            len
        } else {
            1
        };
        rest = rest.get(len.max(1)..).unwrap_or_default();
    }
    false
}

struct CsiFields(Vec<Vec<Option<u32>>>);

impl CsiFields {
    fn parse(body: &str) -> Self {
        Self(
            body.split(';')
                .map(|field| field.split(':').map(|sub| sub.parse().ok()).collect())
                .collect(),
        )
    }

    fn get(&self, field: usize, sub: usize) -> Option<u32> {
        self.0.get(field)?.get(sub).copied().flatten()
    }
}

fn is_ctrl_only(modifiers: Option<u32>) -> bool {
    modifiers
        .unwrap_or(1)
        .checked_sub(1)
        .is_some_and(|bits| bits & !LOCKS == CTRL)
}

pub fn parse_dropped_paths(text: &str) -> Option<Vec<String>> {
    let tokens = split_words(text)?;
    if tokens.is_empty() {
        return None;
    }
    tokens
        .into_iter()
        .map(|token| {
            let path = match token.strip_prefix("file://") {
                Some(url) => file_url_path(url)?,
                None => token,
            };
            (path.starts_with('/') || path.starts_with("~/")).then_some(path)
        })
        .collect()
}

pub fn existing_files(tokens: &[String], home: Option<&Path>) -> Option<Vec<PathBuf>> {
    tokens
        .iter()
        .map(|token| {
            let path = match token.strip_prefix("~/") {
                Some(rest) => home?.join(rest),
                None => PathBuf::from(token),
            };
            let metadata = std::fs::metadata(&path).ok()?;
            let small = usize::try_from(metadata.len()).is_ok_and(|len| len <= MAX_UPLOAD);
            (metadata.is_file() && small).then_some(path)
        })
        .collect()
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Quote {
    None,
    Single,
    Double,
}

fn split_words(text: &str) -> Option<Vec<String>> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut started = false;
    let mut quote = Quote::None;
    let mut chars = text.chars();
    while let Some(ch) = chars.next() {
        match (quote, ch) {
            (Quote::Single, '\'') | (Quote::Double, '"') => quote = Quote::None,
            (Quote::Single, _) => word.push(ch),
            (Quote::None | Quote::Double, '\\') => word.push(chars.next()?),
            (Quote::Double, _) => word.push(ch),
            (Quote::None, '\'') => {
                quote = Quote::Single;
                started = true;
            }
            (Quote::None, '"') => {
                quote = Quote::Double;
                started = true;
            }
            (Quote::None, _) if ch.is_whitespace() => {
                if started || !word.is_empty() {
                    words.push(std::mem::take(&mut word));
                }
                started = false;
                continue;
            }
            (Quote::None, _) => word.push(ch),
        }
        started = started || !word.is_empty();
    }
    if quote != Quote::None {
        return None;
    }
    if started || !word.is_empty() {
        words.push(word);
    }
    Some(words)
}

fn file_url_path(url: &str) -> Option<String> {
    let path = url.strip_prefix("localhost").unwrap_or(url);
    if !path.starts_with('/') {
        return None;
    }
    percent_decode(path)
}

fn percent_decode(text: &str) -> Option<String> {
    let mut bytes = Vec::with_capacity(text.len());
    let mut input = text.bytes();
    while let Some(byte) = input.next() {
        if byte == b'%' {
            let high = hex_value(input.next()?)?;
            let low = hex_value(input.next()?)?;
            bytes.push((high << 4) | low);
        } else {
            bytes.push(byte);
        }
    }
    String::from_utf8(bytes).ok()
}

fn hex_value(byte: u8) -> Option<u8> {
    char::from(byte)
        .to_digit(16)
        .and_then(|digit| u8::try_from(digit).ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn forward(bytes: &[u8]) -> KeyEvent {
        KeyEvent::Forward(bytes.to_vec())
    }

    fn check(name: &str, chunks: &[&[u8]], expected: &[KeyEvent]) {
        let mut scanner = Scanner::default();
        let events: Vec<KeyEvent> = chunks
            .iter()
            .flat_map(|chunk| scanner.feed(chunk))
            .collect();
        assert_eq!(events, expected, "{name}");
        assert!(!scanner.has_pending(), "{name}");
    }

    #[test]
    fn ctrl_v_forms_are_paste_keys_and_everything_else_is_forwarded_in_order() {
        check(
            "legacy",
            &[b"a\x16b"],
            &[
                forward(b"a"),
                KeyEvent::PasteKey(b"\x16".to_vec()),
                forward(b"b"),
            ],
        );
        check(
            "kitty",
            &[b"\x1b[118;5u"],
            &[KeyEvent::PasteKey(b"\x1b[118;5u".to_vec())],
        );
        check(
            "kitty press",
            &[b"\x1b[118;5:1u"],
            &[KeyEvent::PasteKey(b"\x1b[118;5:1u".to_vec())],
        );
        check(
            "kitty repeat",
            &[b"\x1b[118;5:2u"],
            &[KeyEvent::PasteKeyRepeat(b"\x1b[118;5:2u".to_vec())],
        );
        check(
            "kitty release",
            &[b"\x1b[118;5:3u"],
            &[KeyEvent::PasteKeyRelease(b"\x1b[118;5:3u".to_vec())],
        );
        check(
            "kitty with caps lock",
            &[b"\x1b[118;69u"],
            &[KeyEvent::PasteKey(b"\x1b[118;69u".to_vec())],
        );
        check(
            "modifyOtherKeys",
            &[b"\x1b[27;5;118~"],
            &[KeyEvent::PasteKey(b"\x1b[27;5;118~".to_vec())],
        );
        check(
            "ctrl+shift+v is a key",
            &[b"\x1b[118;6u"],
            &[forward(b"\x1b[118;6u")],
        );
        check(
            "plain v is a key",
            &[b"\x1b[118u"],
            &[forward(b"\x1b[118u")],
        );
        check("kitty ctrl+c", &[b"\x1b[99;5u"], &[forward(b"\x1b[99;5u")]);
        check(
            "sgr mouse",
            &[b"\x1b[<0;10;5M\x1b[<0;10;5m"],
            &[forward(b"\x1b[<0;10;5M\x1b[<0;10;5m")],
        );
        check(
            "keys and text",
            &[b"hi\x1b[A\x1bOB\x1bx"],
            &[forward(b"hi\x1b[A\x1bOB\x1bx")],
        );
        check(
            "split kitty press",
            &[b"x\x1b[11", b"8;5", b"u"],
            &[forward(b"x"), KeyEvent::PasteKey(b"\x1b[118;5u".to_vec())],
        );
        check("split arrow", &[b"\x1b", b"[", b"A"], &[forward(b"\x1b[A")]);
        check(
            "split osc reply",
            &[b"\x1b]11;rgb:00/00", b"/00\x1b\\"],
            &[forward(b"\x1b]11;rgb:00/00/00\x1b\\")],
        );
        check("double escape", &[b"\x1b\x1b[A"], &[forward(b"\x1b\x1b[A")]);
    }

    #[test]
    fn ctrl_z_forms_detach_and_a_pasted_one_stays_text() {
        check(
            "legacy",
            &[b"a\x1ab"],
            &[forward(b"a"), KeyEvent::Detach, forward(b"b")],
        );
        check("kitty", &[b"\x1b[122;5u"], &[KeyEvent::Detach]);
        check("kitty press", &[b"\x1b[122;5:1u"], &[KeyEvent::Detach]);
        check(
            "kitty repeat",
            &[b"\x1b[122;5:2u"],
            &[forward(b"\x1b[122;5:2u")],
        );
        check(
            "kitty release",
            &[b"\x1b[122;5:3u"],
            &[forward(b"\x1b[122;5:3u")],
        );
        check("modifyOtherKeys", &[b"\x1b[27;5;122~"], &[KeyEvent::Detach]);
        check("plain z", &[b"\x1b[122u"], &[forward(b"\x1b[122u")]);
        check("old detach key", &[b"\x1c"], &[forward(b"\x1c")]);
        check(
            "inside a paste",
            &[b"\x1b[200~a\x1ab\x1b[201~"],
            &[KeyEvent::Paste {
                text: b"a\x1ab".to_vec(),
                raw: b"\x1b[200~a\x1ab\x1b[201~".to_vec(),
            }],
        );
    }

    #[test]
    fn ctrl_c_is_found_in_every_form_and_nothing_else_interrupts() {
        for yes in [
            &b"\x03"[..],
            b"ab\x03",
            b"\x1b[99;5u",
            b"\x1b[<0;3;4M\x1b[99;5:1u",
            b"\x1b[27;5;99~",
        ] {
            assert!(interrupts(yes), "{yes:?}");
        }
        for no in [
            &b"c"[..],
            b"\x1b[99;5:3u",
            b"\x1b[99u",
            b"\x1b[A\x1b[<0;3;4M",
            b"\x1b[99;5",
            b"",
        ] {
            assert!(!interrupts(no), "{no:?}");
        }
    }

    #[test]
    fn an_incomplete_sequence_is_held_until_flush() {
        let mut scanner = Scanner::default();
        assert_eq!(scanner.feed(b"ab\x1b["), vec![forward(b"ab")]);
        assert!(scanner.has_pending());
        assert_eq!(scanner.flush(), vec![forward(b"\x1b[")]);
        assert!(!scanner.has_pending());
        assert!(scanner.feed(b"\x1b").is_empty());
        assert_eq!(scanner.flush(), vec![forward(b"\x1b")]);
    }

    #[test]
    fn a_bracketed_paste_is_reassembled_across_reads() {
        check(
            "split paste",
            &[b"a\x1b[20", b"0~/tmp/x y", b".png\x1b[2", b"01~b"],
            &[
                forward(b"a"),
                KeyEvent::Paste {
                    text: b"/tmp/x y.png".to_vec(),
                    raw: b"\x1b[200~/tmp/x y.png\x1b[201~".to_vec(),
                },
                forward(b"b"),
            ],
        );
    }

    #[test]
    fn a_ctrl_v_byte_inside_a_paste_stays_paste_text() {
        check(
            "ctrl+v byte in a paste",
            &[b"\x1b[200~a\x16b\x1b[201~"],
            &[KeyEvent::Paste {
                text: b"a\x16b".to_vec(),
                raw: b"\x1b[200~a\x16b\x1b[201~".to_vec(),
            }],
        );
    }

    #[test]
    fn an_unfinished_paste_is_forwarded_raw_on_flush() {
        let mut scanner = Scanner::default();
        assert!(scanner.feed(b"\x1b[200~abc").is_empty());
        assert_eq!(scanner.flush(), vec![forward(b"\x1b[200~abc")]);
    }

    #[test]
    fn an_oversized_paste_is_forwarded_raw() {
        let mut scanner = Scanner::default();
        let big = vec![b'x'; MAX_UPLOAD];
        assert!(scanner.feed(b"\x1b[200~").is_empty());
        let events = scanner.feed(&big);
        assert_eq!(events.len(), 1);
        assert!(
            matches!(&events[0], KeyEvent::Forward(bytes) if bytes.len() == MAX_UPLOAD + PASTE_START.len())
        );
        assert!(!scanner.has_pending());
        assert_eq!(scanner.feed(b"\x1b[201~"), vec![forward(b"\x1b[201~")]);
    }

    #[test]
    fn dropped_path_forms_are_parsed() {
        let cases: Vec<(&str, Option<Vec<&str>>)> = vec![
            ("/Users/me/a.png", Some(vec!["/Users/me/a.png"])),
            (
                "/Users/me/my\\ shot.png ",
                Some(vec!["/Users/me/my shot.png"]),
            ),
            (
                "'/tmp/a b.png' \"/tmp/c d.log\"",
                Some(vec!["/tmp/a b.png", "/tmp/c d.log"]),
            ),
            (
                "file:///Users/me/a%20b.png",
                Some(vec!["/Users/me/a b.png"]),
            ),
            ("file://localhost/tmp/x.txt", Some(vec!["/tmp/x.txt"])),
            ("~/notes.md\n/tmp/b", Some(vec!["~/notes.md", "/tmp/b"])),
            ("", None),
            ("   ", None),
            ("hello world", None),
            ("/tmp/a relative.txt", None),
            ("'/tmp/unterminated", None),
            ("file://host/tmp/x", None),
            ("file:///bad%zz", None),
            ("''", None),
        ];
        for (text, expected) in cases {
            let expected =
                expected.map(|paths| paths.into_iter().map(String::from).collect::<Vec<_>>());
            assert_eq!(parse_dropped_paths(text), expected, "{text:?}");
        }
    }

    #[test]
    fn only_existing_regular_files_qualify() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.png");
        std::fs::write(&file, b"png").unwrap();
        std::fs::write(dir.path().join("b.log"), b"log").unwrap();
        let file_token = file.to_string_lossy().into_owned();
        assert_eq!(
            existing_files(&[file_token.clone(), "~/b.log".into()], Some(dir.path())),
            Some(vec![file.clone(), dir.path().join("b.log")])
        );
        assert_eq!(
            existing_files(
                &[file_token.clone(), "/no/such/file".into()],
                Some(dir.path())
            ),
            None
        );
        assert_eq!(
            existing_files(&[dir.path().to_string_lossy().into_owned()], None),
            None
        );
        assert_eq!(existing_files(&["~/b.log".into()], None), None);
    }
}
