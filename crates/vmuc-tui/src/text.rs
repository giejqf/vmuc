// SPDX-License-Identifier: Apache-2.0
//! Text for the screen: sizes, durations, sparkline windows and console
//! lines made safe to draw.

/// The last `width` samples: a sparkline draws from the left and cuts
/// off what does not fit.
pub fn newest(mut v: Vec<u64>, width: u16) -> Vec<u64> {
    v.drain(..v.len().saturating_sub(usize::from(width)));
    v
}

/// `812 MiB`, `1.5 GiB`.
pub fn mib(bytes: u64) -> String {
    let m = bytes >> 20;
    if m >= 10 * 1024 {
        format!("{}.{} GiB", m / 1024, (m % 1024) * 10 / 1024)
    } else {
        format!("{m} MiB")
    }
}

pub fn uptime(secs: u64) -> String {
    match secs {
        0..60 => format!("{secs}s"),
        60..3600 => format!("{}m{:02}s", secs / 60, secs % 60),
        _ => format!("{}h{:02}m", secs / 3600, secs % 3600 / 60),
    }
}

/// A console line as a terminal would leave it: escape sequences
/// dropped, text before a carriage return overwritten, tabs expanded and
/// other control characters removed.
pub fn clean(raw: &str) -> String {
    let raw = raw.rsplit('\r').find(|s| !s.is_empty()).unwrap_or("");
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\x1b' => match chars.next() {
                // CSI: parameters, then one final byte in @..~.
                Some('[') => {
                    for c in chars.by_ref() {
                        if ('@'..='~').contains(&c) {
                            break;
                        }
                    }
                }
                // OSC: up to BEL or ST.
                Some(']') => {
                    while let Some(c) = chars.next() {
                        if c == '\x07' || (c == '\x1b' && chars.next_if_eq(&'\\').is_some()) {
                            break;
                        }
                    }
                }
                _ => {}
            },
            '\t' => out.push_str("    "),
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn console_lines_lose_escapes_and_overdrawn_text() {
        assert_eq!(clean("\x1b[0;1;31m*\x1b[0m] Job"), "*] Job");
        assert_eq!(
            clean("\x1bM\x1b[K[  *] Job running (5s / 1min)"),
            "[  *] Job running (5s / 1min)"
        );
        assert_eq!(clean("10%\r20%\r"), "20%");
        assert_eq!(clean("\x1b]0;title\x07after\tx"), "after    x");
        assert_eq!(clean("bell\x07"), "bell");
    }

    #[test]
    fn sparklines_get_the_newest_samples() {
        assert_eq!(newest(vec![1, 2, 3, 4], 2), vec![3, 4]);
        assert_eq!(newest(vec![1], 5), vec![1]);
    }

    #[test]
    fn sizes_and_durations_read_naturally() {
        assert_eq!(mib(812 << 20), "812 MiB");
        assert_eq!(mib(15 << 30), "15.0 GiB");
        assert_eq!(uptime(59), "59s");
        assert_eq!(uptime(3599), "59m59s");
        assert_eq!(uptime(3 * 3600 + 120), "3h02m");
    }
}
