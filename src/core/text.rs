//! Decoding text the way CVSNT writes it.
//!
//! CVS control files (`Entries`, `Root`, `Repository`, `Tag`) are written in the console's
//! code page, not UTF-8. On a Chinese Windows that is GBK ("Chinese Simplified", code page
//! 936), and a real working copy proves it: its `CVS/Entries` holds names whose bytes are not
//! valid UTF-8. Read as UTF-8, every Chinese letter becomes a replacement mark, the name
//! matches no file on disk, and the file list shows the wrong state.
//!
//! Decoding uses the same `encoding_rs` GBK decoder as the client. That decoder is the
//! Encoding Standard's GBK decoder, which also accepts GB18030's four-byte sequences. A byte
//! sequence it cannot use becomes U+FFFD and is counted, because a wrong filename is worse
//! than a visible gap: a wrong name silently matches nothing, while a replacement mark can be
//! seen. For the 101 two-byte slots where Windows CP936 returns a private-use character and
//! GB18030 assigns a standard one, the standard character wins (for example `A2 E3` is the
//! euro sign here, but U+E76C on Windows), so a legacy file named with that private-use
//! character can fail to match its `Entries` name.

/// A run of bytes decoded to text, plus how many replacement marks it needed.
pub struct Decoded {
    pub text: String,
    /// How many replacement marks the decoder produced.
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
                "read as GBK; {} character(s) could not be decoded",
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

/// Decode as GBK (code page 936). ASCII passes through unchanged; a sequence the decoder
/// cannot use becomes U+FFFD and is counted.
pub fn decode_gbk(bytes: &[u8]) -> Decoded {
    let (text, had_errors) = encoding_rs::GBK.decode_without_bom_handling(bytes);
    let replaced = if had_errors {
        text.matches('\u{FFFD}').count()
    } else {
        0
    };
    Decoded {
        text: text.into_owned(),
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
    fn an_invalid_trail_byte_is_counted_once() {
        // 0x7F is not a valid trail byte, so the lead becomes U+FFFD and the 0x7F stays as DEL.
        let d = decode_gbk(&[0x81, 0x7F]);
        assert_eq!(d.text, "\u{FFFD}\u{7F}");
        assert_eq!(d.replaced, 1);
    }

    #[test]
    fn the_note_says_when_gbk_was_used() {
        let d = decode(&[0xD6, 0xD0]);
        assert!(d.note().unwrap().contains("GBK"));
        assert!(decode(b"abc").note().is_none());

        let d = decode_gbk(&[0x81, 0x7F]);
        assert!(d.note().unwrap().contains("could not be decoded"));
    }

    #[test]
    fn windows_user_defined_area_is_decoded() {
        // 0xAA 0xA1 is the first slot of CP936's user-defined area, which Windows maps to
        // U+E000. Python's `gbk` codec rejects it, but a Windows-written name may use it.
        let d = decode_gbk(&[0xAA, 0xA1]);
        assert_eq!(d.text, "\u{E000}");
        assert_eq!(d.replaced, 0);
    }

    #[test]
    fn gbk_extension_range_is_decoded() {
        // 0x81 0x40 is the first pair of the GBK extension area, "丂"; the old table covered
        // this range too, and the test locks it in after the table's removal.
        let d = decode_gbk(&[0x81, 0x40]);
        assert_eq!(d.text, "\u{4E02}");
        assert_eq!(d.replaced, 0);
    }

    #[test]
    fn four_byte_gb18030_sequence_is_decoded() {
        // The decoder also accepts GB18030's four-byte form: "ђ".
        let d = decode_gbk(&[0x81, 0x30, 0xD3, 0x30]);
        assert_eq!(d.text, "\u{0452}");
        assert_eq!(d.replaced, 0);
    }

    #[test]
    fn gb18030_extension_slot_is_decoded() {
        // GB18030 assigns the euro sign to 0xA2 0xE3; Windows CP936 maps the slot to the
        // private-use U+E76C instead. The standard assignment wins here.
        assert_eq!(decode_gbk(&[0xA2, 0xE3]).text, "€");
    }
}
