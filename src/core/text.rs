//! Decoding text the way CVSNT writes it.
//!
//! CVS control files (`Entries`, `Root`, `Repository`, `Tag`) are written in the console's
//! code page, not UTF-8. On a Chinese Windows that is GBK ("Chinese Simplified", code page
//! 936), and a real working copy proves it: its `CVS/Entries` holds names whose bytes are not
//! valid UTF-8. Read as UTF-8, every Chinese letter becomes a replacement mark, the name
//! matches no file on disk, and the file list shows the wrong state.
//!
//! The two-byte table in `gbk_table.bin` is generated from Python's own `gbk` codec by
//! `gen_gbk.py`, so the mapping is exact rather than approximated. It is compiled in, so no
//! file has to be shipped beside the program.
//!
//! What this does *not* do is guess. A byte pair with no character becomes U+FFFD and is
//! counted, because a wrong filename is worse than a visible gap: a wrong name silently
//! matches nothing, while a replacement mark can be seen.

/// The exact GBK table: one u16 per (lead, trail) pair, indexed by `index()`.
static GBK: &[u8] = include_bytes!("gbk_table.bin");

/// Trails run 0x40..=0xFE, skipping 0x7F, so there are 190 of them.
const TRAILS: usize = 0xFF - 0x40 - 1;

fn index(lead: u8, trail: u8) -> Option<usize> {
    if !(0x81..=0xFE).contains(&lead) {
        return None;
    }
    if !(0x40..=0xFE).contains(&trail) || trail == 0x7F {
        return None;
    }
    // Trails below 0x7F occupy positions 0..=62, those above start at 63.
    let t = if trail < 0x7F {
        (trail - 0x40) as usize
    } else {
        (trail - 0x40 - 1) as usize
    };
    let i = (lead as usize - 0x81) * TRAILS + t;
    if i * 2 + 1 < GBK.len() {
        Some(i)
    } else {
        None
    }
}

/// The code point for a two-byte GBK pair, or `None` when the pair has no character.
pub fn gbk_char(lead: u8, trail: u8) -> Option<char> {
    let i = index(lead, trail)?;
    let cp = u16::from_le_bytes([GBK[i * 2], GBK[i * 2 + 1]]) as u32;
    if cp == 0 {
        None
    } else {
        char::from_u32(cp)
    }
}

/// A run of bytes decoded to text, plus how many bytes had no mapping.
pub struct Decoded {
    pub text: String,
    pub replaced: usize,
    /// True when the bytes were valid UTF-8 and were decoded as such.
    pub was_utf8: bool,
}

impl Decoded {
    /// A one-line note for the status bar, or `None` when the text was clean UTF-8.
    pub fn note(&self) -> Option<String> {
        if self.was_utf8 {
            None
        } else if self.replaced == 0 {
            Some("read as GBK (code page 936)".to_string())
        } else {
            Some(format!(
                "read as GBK; {} byte(s) had no character",
                self.replaced
            ))
        }
    }
}

/// Decode bytes that may be UTF-8 (modern CVS) or GBK (CVSNT on a Chinese Windows).
pub fn decode(bytes: &[u8]) -> Decoded {
    match std::str::from_utf8(bytes) {
        Ok(s) => Decoded {
            text: s.to_string(),
            replaced: 0,
            was_utf8: true,
        },
        Err(_) => decode_gbk(bytes),
    }
}

/// Decode as GBK (code page 936). ASCII passes through unchanged; a lead/trail pair is looked
/// up in the exact table; anything with no meaning becomes U+FFFD and is counted.
pub fn decode_gbk(bytes: &[u8]) -> Decoded {
    let mut text = String::with_capacity(bytes.len());
    let mut replaced = 0usize;
    let mut i = 0usize;
    while i < bytes.len() {
        let b = bytes[i];
        if b < 0x80 {
            text.push(b as char);
            i += 1;
            continue;
        }
        if i + 1 < bytes.len() {
            if let Some(c) = gbk_char(b, bytes[i + 1]) {
                text.push(c);
                i += 2;
                continue;
            }
        }
        text.push('\u{FFFD}');
        replaced += 1;
        i += 1;
    }
    Decoded {
        text,
        replaced,
        was_utf8: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_ascii_passes_through() {
        let d = decode(b"D/test.txt/0/dummy timestamp//\n");
        assert_eq!(d.replaced, 0);
        assert_eq!(d.text, "D/test.txt/0/dummy timestamp//\n");
    }

    #[test]
    fn valid_utf8_is_read_as_utf8() {
        let d = decode("文档\n".as_bytes());
        assert!(d.was_utf8);
        assert_eq!(d.text, "文档\n");
    }

    #[test]
    fn gbk_bytes_are_not_mistaken_for_utf8() {
        // 0xD6 0xD0 is "中" in GBK, and those bytes are not valid UTF-8.
        let d = decode(&[0xD6, 0xD0]);
        assert!(!d.was_utf8, "must not claim to be utf-8");
        assert_eq!(d.text, "中");
        assert_eq!(d.replaced, 0);
    }

    #[test]
    fn the_same_text_in_gbk_and_utf8_comes_out_equal() {
        // "测试文档" written both ways must produce the same string, which is what makes a
        // name read from Entries match the file on disk.
        let utf8 = "测试文档";
        let gbk: Vec<u8> = vec![0xB2, 0xE2, 0xCA, 0xD4, 0xCE, 0xC4, 0xB5, 0xB5];
        assert_eq!(decode(&gbk).text, utf8);
    }

    #[test]
    fn a_stray_lead_byte_is_counted_not_swallowed() {
        let d = decode_gbk(&[0x41, 0x81]);
        assert_eq!(d.replaced, 1);
        assert!(d.text.starts_with('A'));
    }

    #[test]
    fn trail_byte_0x7f_has_no_character() {
        assert!(gbk_char(0x81, 0x7F).is_none());
        assert!(gbk_char(0x81, 0x40).is_some());
    }

    #[test]
    fn the_note_says_when_gbk_was_used() {
        let d = decode(&[0xD6, 0xD0]);
        assert!(d.note().unwrap().contains("GBK"));
        assert!(decode(b"abc").note().is_none());
    }
}
