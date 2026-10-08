//! Putting text on the system clipboard, in band.
//!
//! OSC 52 asks the *terminal* to set the clipboard, which is the only method
//! that works over SSH: there is no X display, no Wayland socket and no
//! pbcopy on the far side, but the escape sequence travels back up the same
//! pipe the drawing goes down. WezTerm, kitty, iTerm2 and xterm all implement
//! it; some terminals have it off by default, which is why a copy reports
//! what it sent rather than claiming success it cannot verify.

use std::io::Write;

/// Terminals refuse very large OSC 52 payloads, and a silent truncation would
/// be worse than a refusal. 64 KiB of source is far more than anyone selects
/// by hand.
pub const MAX_COPY_BYTES: usize = 64 * 1024;

/// Standard base64, which is all OSC 52 accepts.
///
/// Hand-rolled rather than taking a dependency for forty lines: this is the
/// only place the project needs it.
pub fn base64(input: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        let digits = [
            (n >> 18) & 0x3f,
            (n >> 12) & 0x3f,
            (n >> 6) & 0x3f,
            n & 0x3f,
        ];
        for (i, d) in digits.iter().enumerate() {
            // One padding character per byte the final chunk was short.
            if i > chunk.len() {
                out.push('=');
            } else {
                out.push(ALPHABET[*d as usize] as char);
            }
        }
    }
    out
}

/// Ask the terminal to put `text` on the clipboard.
///
/// Returns how many bytes were sent, or `None` when the text is too large to
/// send at all -- better to say so than to copy a prefix the user did not ask
/// for and will not notice.
pub fn copy(text: &str) -> std::io::Result<Option<usize>> {
    if text.len() > MAX_COPY_BYTES {
        return Ok(None);
    }
    let mut out = std::io::stdout();
    // `c` is the clipboard selection; BEL terminates, which every
    // implementation accepts.
    write!(out, "\x1b]52;c;{}\x07", base64(text.as_bytes()))?;
    out.flush()?;
    Ok(Some(text.len()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_the_standard_vectors() {
        // RFC 4648 section 10, which exercises every padding case.
        for (input, want) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(base64(input.as_bytes()), want, "{input:?}");
        }
    }

    #[test]
    fn base64_handles_bytes_that_are_not_text() {
        assert_eq!(base64(&[0x00, 0xff, 0x80]), "AP+A");
    }

    #[test]
    fn base64_encodes_multibyte_characters_by_their_bytes() {
        // Two bytes each, so this is a padding case as well.
        assert_eq!(base64("\u{3b1}\u{3b2}".as_bytes()), "zrHOsg==");
    }

    #[test]
    fn an_oversized_copy_is_refused_rather_than_truncated() {
        let huge = "x".repeat(MAX_COPY_BYTES + 1);
        assert_eq!(copy(&huge).unwrap(), None);
    }
}
