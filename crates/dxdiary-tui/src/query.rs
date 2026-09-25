//! Direct terminal capability queries.
//!
//! Measured against a real WezTerm (spike 05), which settled how this had to
//! work:
//!
//! | query | WezTerm 20240203 |
//! |---|---|
//! | `CSI ? u` — kitty keyboard | **no reply** |
//! | `CSI > q` — XTVERSION | `WezTerm 20240203-110809-5046fc22` |
//! | `DCS +q 524742 ST` — XTGETTCAP `RGB` | `1+r524742=382F382F38` → `8/8/8` |
//!
//! So XTGETTCAP is the reliable signal and the kitty keyboard query is not —
//! the opposite of what was assumed before measuring. `RGB=8/8/8` means eight
//! bits per channel: truecolor, stated by the terminal rather than inferred
//! from an environment variable that does not survive SSH.
//!
//! DA1 (`CSI c`) is sent last and used as a sync marker. Every terminal
//! answers it, and it cannot be reordered ahead of the earlier replies, so its
//! arrival means the batch is complete. That makes the read deterministic
//! instead of dependent on a timeout expiring.
//!
//! Unix only, and deliberately so — dxdiary targets Linux. Reading raw stdin
//! with a timeout needs `poll(2)`; supporting Windows would mean a parallel
//! console-API implementation for no current benefit, since the Windows use
//! case is WSL, which is Linux.

use std::io::Write;
use std::time::{Duration, Instant};

/// Hex-encoded terminfo capability names, as XTGETTCAP wants them.
const CAP_RGB: &str = "524742"; // "RGB"  — 24-bit colour
const CAP_TC: &str = "5463"; // "Tc"   — the older tmux spelling

/// Cap on bytes read, so a terminal that streams garbage cannot hang startup.
const MAX_REPLY: usize = 4096;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Probe {
    /// The terminal affirmed 24-bit colour via `RGB` or `Tc`.
    pub truecolor: bool,
    /// Terminal identification from XTVERSION, if it answered.
    pub version: Option<String>,
    /// The terminal answered at all. False means every query timed out, so
    /// nothing here should be trusted over the environment.
    pub responded: bool,
}

/// Query the terminal. Requires raw mode to already be enabled.
///
/// Runs before the event loop starts, synchronously on the main thread, so
/// nothing races it for stdin. A keystroke typed during the few milliseconds
/// it takes would be consumed here, which is why it must not be called again
/// once the app is running.
pub fn probe_terminal(timeout: Duration) -> Probe {
    let queries = format!("\x1bP+q{CAP_RGB}\x1b\\\x1bP+q{CAP_TC}\x1b\\\x1b[>q\x1b[c");

    let mut out = std::io::stdout();
    if out.write_all(queries.as_bytes()).is_err() || out.flush().is_err() {
        return Probe::default();
    }

    let raw = read_until_da1(timeout);
    parse(&raw)
}

/// Read stdin until DA1 replies or the deadline passes.
fn read_until_da1(timeout: Duration) -> Vec<u8> {
    use std::os::unix::io::AsRawFd;

    let fd = std::io::stdin().as_raw_fd();
    let deadline = Instant::now() + timeout;
    let mut buf = Vec::with_capacity(256);

    while buf.len() < MAX_REPLY {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }

        let mut pfd = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: one initialised pollfd, count matches, timeout is in range.
        let ready = unsafe {
            libc::poll(
                &mut pfd,
                1,
                remaining.as_millis().min(i32::MAX as u128) as i32,
            )
        };
        if ready <= 0 {
            break; // timeout, or an error we cannot act on
        }

        let mut chunk = [0u8; 256];
        // SAFETY: reading into a local buffer with its true length.
        let n = unsafe { libc::read(fd, chunk.as_mut_ptr().cast(), chunk.len()) };
        if n <= 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n as usize]);

        if has_da1_reply(&buf) {
            break;
        }
    }
    buf
}

/// DA1 answers as `CSI ? <params> c`. It was sent last, so seeing it means
/// every earlier reply has already arrived.
fn has_da1_reply(buf: &[u8]) -> bool {
    let Some(start) = buf.windows(3).position(|w| w == b"\x1b[?") else {
        return false;
    };
    buf[start..].contains(&b'c')
}

fn parse(raw: &[u8]) -> Probe {
    if raw.is_empty() {
        return Probe::default();
    }

    // Success is `DCS 1 + r <name>=<value> ST`; failure is `DCS 0 + r <name>`.
    let ok_rgb = find_cap_value(raw, CAP_RGB);
    let ok_tc = find_cap_value(raw, CAP_TC);

    // WezTerm answers RGB with "8/8/8" (bits per channel). Some terminals send
    // the capability with no value at all, which still means present.
    let truecolor = match (&ok_rgb, &ok_tc) {
        (Some(v), _) => v.is_empty() || v.contains("8/8/8") || v.contains('8'),
        (_, Some(_)) => true,
        _ => false,
    };

    Probe {
        truecolor,
        version: find_xtversion(raw),
        responded: true,
    }
}

/// Extract and hex-decode the value for one capability, if the terminal
/// reported success for it.
fn find_cap_value(raw: &[u8], cap_hex: &str) -> Option<String> {
    let needle = format!("1+r{cap_hex}");
    let text = String::from_utf8_lossy(raw);
    let rest = text.split(&needle).nth(1)?;

    // Either `=<hexvalue>` then ST, or ST immediately.
    let value_hex = match rest.strip_prefix('=') {
        Some(v) => v.split('\x1b').next().unwrap_or(""),
        None => "",
    };
    Some(decode_hex(value_hex))
}

fn find_xtversion(raw: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(raw);
    let rest = text.split(">|").nth(1)?;
    let name = rest.split('\x1b').next()?.trim();
    (!name.is_empty()).then(|| name.to_string())
}

fn decode_hex(hex: &str) -> String {
    let bytes: Vec<u8> = hex
        .as_bytes()
        .chunks(2)
        .filter_map(|pair| {
            let s = std::str::from_utf8(pair).ok()?;
            u8::from_str_radix(s, 16).ok()
        })
        .collect();
    String::from_utf8_lossy(&bytes).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Exactly what a real WezTerm 20240203 sent back (spike 05).
    const WEZTERM_REPLY: &[u8] =
        b"\x1bP>|WezTerm 20240203-110809-5046fc22\x1b\\\x1bP1+r524742=382F382F38\x1b\\\x1b[?65;4;6;18;22c";

    #[test]
    fn decodes_the_recorded_wezterm_reply() {
        let p = parse(WEZTERM_REPLY);
        assert!(p.responded);
        assert!(p.truecolor, "RGB=8/8/8 means 24-bit");
        assert_eq!(
            p.version.as_deref(),
            Some("WezTerm 20240203-110809-5046fc22")
        );
    }

    #[test]
    fn hex_decoding_matches_the_wire_format() {
        assert_eq!(decode_hex("382F382F38"), "8/8/8");
        assert_eq!(decode_hex("524742"), "RGB");
        assert_eq!(decode_hex(""), "");
    }

    #[test]
    fn a_capability_the_terminal_denies_is_not_truecolor() {
        // `0+r` is the failure form.
        let reply = b"\x1bP0+r524742\x1b\\\x1b[?62c";
        let p = parse(reply);
        assert!(p.responded, "the terminal did answer");
        assert!(!p.truecolor, "but it denied RGB");
    }

    #[test]
    fn the_older_tc_capability_also_counts() {
        let reply = b"\x1bP1+r5463\x1b\\\x1b[?62c";
        assert!(parse(reply).truecolor);
    }

    #[test]
    fn silence_is_not_a_yes() {
        let p = parse(b"");
        assert!(!p.responded);
        assert!(!p.truecolor);
    }

    #[test]
    fn da1_is_only_detected_once_it_is_complete() {
        assert!(!has_da1_reply(b"\x1bP1+r524742=38\x1b\\"), "no DA1 yet");
        assert!(!has_da1_reply(b"\x1b[?65;4;6"), "still mid-reply");
        assert!(has_da1_reply(b"\x1b[?65;4;6;18;22c"));
    }

    #[test]
    fn xtversion_is_optional() {
        let reply = b"\x1bP1+r524742=382F382F38\x1b\\\x1b[?62c";
        let p = parse(reply);
        assert!(p.truecolor);
        assert_eq!(p.version, None);
    }
}
