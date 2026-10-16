//! What the admin page and the setup page share: the buffer they are rendered
//! into, HTML escaping, the tab icon, and the factory reset's form.
//!
//! Pure logic. A control's, in ADR-0028's layers.

use core::fmt::Write as _;

use crate::request::field;

/// How much rendered HTML a page may take.
///
/// Sized against the worst case rather than the typical one, and
/// `the_widest_possible_form_still_fits` holds it to that: six fields at their
/// echo-back limit, every character one that escapes to six bytes, and the
/// longest error above them. `heapless` writes silently stop at the cap, so a
/// page that outgrew this would reach the phone truncated mid-tag with a
/// `Content-Length` agreeing with the truncation — a blank form and nothing on
/// the console.
///
/// 12 KB rather than the setup form's 8: the admin page shares the buffer —
/// see `http::slots` — and carries the timezone script inline, and
/// `admin::tests::the_widest_possible_page_still_fits` holds it to 90% of
/// this. Three connections make that 36 KB of static RAM, reserved on every
/// boot whichever server runs.
pub const PAGE_LEN: usize = 12 * 1024;

/// The tab icon, inline: a cat, drawn by the browser's own emoji font, so it
/// costs a hundred-odd bytes and no request. Without a `rel=icon` a browser
/// asks for `/favicon.ico`, which spends one of the few connection slots on a
/// 404. Shared by the setup form and the admin page.
pub const FAVICON: &str = "<link rel=icon href=\"data:image/svg+xml,<svg xmlns='http://www.w3.org/2000/svg' \
viewBox='0 0 100 100'><text y='.9em' font-size='90'>🐱</text></svg>\">";

/// A string with the four characters that would break an HTML attribute
/// replaced by their entities.
///
/// Wrapping rather than allocating, because the page is written straight into
/// the response buffer and there is nowhere to put a second copy.
///
/// This is about **correctness before safety**. An SSID may legitimately
/// contain `&` or `"`, and an unescaped `"` ends the `value="..."` attribute
/// early: the field silently loses the rest of its contents, and the person
/// re-typing it has no idea why. That the same escaping also stops an SSID
/// closing a tag is a second reason, not the first one.
#[derive(Debug, Clone, Copy)]
pub struct Escaped<'a>(pub &'a str);

impl core::fmt::Display for Escaped<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        for ch in self.0.chars() {
            match ch {
                '&' => f.write_str("&amp;")?,
                '<' => f.write_str("&lt;")?,
                '>' => f.write_str("&gt;")?,
                '"' => f.write_str("&quot;")?,
                _ => f.write_char(ch)?,
            }
        }
        Ok(())
    }
}

/// The factory reset's form, on the setup page and the admin page alike: a box
/// to tick and a browser `confirm`, two deliberate acts before anything is
/// erased. [`erase_confirmed`] checks the box again on the unit.
pub const ERASE_FORM: &str = "<form method=post action=/erase \
onsubmit=\"return confirm('Erase everything on this feeder?')\">\
<label><input type=checkbox name=erase value=everything required> \
Erase calibration, meals, timezone, pause and network</label>\
<button type=submit>Factory reset</button></form>";

/// Whether a `POST /erase` carries the ticked box. The browser enforces it
/// with `required`; this holds a client without one, `curl`, to it too.
pub fn erase_confirmed(body: &str) -> bool {
    field::<16>(body, "erase").ok().flatten().as_deref() == Some("everything")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_quote_in_an_ssid_cannot_end_the_attribute_early() {
        // The bug this prevents is not exotic: the field silently loses
        // everything after the quote and the person re-typing it never learns
        // why.
        let rendered = std::format!("{}", Escaped("Bob\"s <net> & co"));
        assert_eq!(rendered, "Bob&quot;s &lt;net&gt; &amp; co");
        assert!(!rendered.contains('"'));
        assert!(!rendered.contains('<'));
    }

    #[test]
    fn escaping_leaves_an_ordinary_ssid_alone() {
        // Including the characters a Wi-Fi name really does contain. Escaping
        // an apostrophe or a space would show mojibake on the form.
        for plain in ["My Network", "Caffe\u{300}", "it's-5GHz_2", ""] {
            assert_eq!(std::format!("{}", Escaped(plain)), plain);
        }
    }

    #[test]
    fn a_factory_reset_needs_the_box_ticked() {
        assert!(erase_confirmed("erase=everything"));
        assert!(!erase_confirmed(""), "a bare POST erases nothing");
        assert!(!erase_confirmed("erase=on"), "a box with another value");
        assert!(!erase_confirmed("erase=everything2"));
    }
}
