//! Timezones: a unit that keeps its own summer time when nobody tells it.
//!
//! Pure logic. A zone is stored as a name for people — `Europe/Rome` — and a
//! POSIX rule for the clock — `<+01>-1<+02>,M3.5.0,M10.5.0/3`. **The unit
//! never turns one into the other.** The admin page does, in the browser,
//! from the timezone data every browser carries and keeps current; the unit
//! only stores what it is given, and reads the rule. So a country changing
//! its law needs the page opened and saved again, never a firmware update.
//!
//! ## The rule
//!
//! `std offset [dst [offset] ,start[/time],end[/time]]`, the `TZ` format ESP-IDF
//! and every Unix use, in the subset worth supporting:
//!
//! - names are three or more letters (`CET`) or anything in angle brackets
//!   (`<+01>`, what the browser-derived rules use);
//! - **offsets are written west-positive** — `-1` is UTC**+1** — the one trap
//!   in the format, and why everything inside this module is east-positive
//!   minutes, the same as `Wall::offset_minutes`;
//! - a daylight-saving zone must give its dates, and only as `Mm.w.d`: month,
//!   week 1–5 where 5 means the last, weekday with 0 for Sunday. The `Jn` and
//!   `n` day-of-year forms are refused, and no zone uses them.
//!
//! ## Who wins
//!
//! **Home Assistant's live time, when there is one.** It carries the offset in
//! force, from the tz database Home Assistant ships, so the schedule task
//! follows the stored rule only when no live time has arrived for a while. A
//! rule gone stale then only matters to a unit with nothing better.

use core::fmt::Write as _;

use heapless::String;

use crate::schedule::{Date, Wall, civil_from_days, days_from_civil};

/// Longest zone name kept. The longest IANA name in use is 32 characters
/// (`America/Argentina/ComodRivadavia`).
pub const NAME_LEN: usize = 48;

/// Longest rule kept. Real ones run to about 40.
pub const RULE_LEN: usize = 64;

const DAY_S: i64 = 86_400;

/// One end of summer time: the `Mm.w.d/time` of the rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Change {
    month: u8,
    /// 1–5, 5 meaning the last such weekday of the month.
    week: u8,
    /// 0 is Sunday.
    weekday: u8,
    /// Seconds after local midnight, on the clock in force *before* the
    /// change. May be negative or past a day, as the format allows.
    time_s: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Dst {
    /// East-positive minutes, like `Wall::offset_minutes`.
    offset: i16,
    start: Change,
    end: Change,
}

/// A parsed rule. East-positive minutes throughout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rule {
    std: i16,
    dst: Option<Dst>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuleError {
    /// Not the format at all, or a field out of range.
    Malformed,
    /// Summer time with no dates, or dates in the `Jn`/`n` forms.
    Unsupported,
}

impl Rule {
    pub fn parse(text: &str) -> Result<Self, RuleError> {
        let mut p = Parser {
            s: text.trim().as_bytes(),
            at: 0,
        };
        p.name()?;
        let std = -p.offset()?;
        if p.done() {
            return Ok(Self { std, dst: None });
        }

        p.name()?;
        let offset = if matches!(p.peek(), Some(b',') | None) {
            std + 60
        } else {
            -p.offset()?
        };
        if !p.take(b',') {
            return Err(RuleError::Unsupported);
        }
        let start = p.change()?;
        if !p.take(b',') {
            return Err(RuleError::Malformed);
        }
        let end = p.change()?;
        if !p.done() {
            return Err(RuleError::Malformed);
        }
        Ok(Self {
            std,
            dst: Some(Dst { offset, start, end }),
        })
    }

    /// The offset in force at a UTC instant, seconds since 1970.
    pub fn offset_at_utc(&self, utc_s: i64) -> i16 {
        let Some(dst) = self.dst else {
            return self.std;
        };
        let year = civil_from_days((utc_s + self.std as i64 * 60).div_euclid(DAY_S)).year;
        // The start is written in standard time, the end in summer time.
        let start = dst.start.local_seconds_in(year) - self.std as i64 * 60;
        let end = dst.end.local_seconds_in(year) - dst.offset as i64 * 60;
        let summer = if start < end {
            (start..end).contains(&utc_s)
        } else {
            // Southern hemisphere: summer runs across New Year.
            utc_s < end || utc_s >= start
        };
        if summer { dst.offset } else { self.std }
    }

    /// The offset a local reading with no offset attached should be taken to
    /// be in. For the hour that happens twice in autumn, standard time; for
    /// the hour that never happens in spring, whichever the rule lands on.
    pub fn offset_for_local(&self, local: Wall) -> i16 {
        let as_std = local.local_seconds() - self.std as i64 * 60;
        let found = self.offset_at_utc(as_std);
        if found == self.std {
            return self.std;
        }
        found
    }
}

impl Change {
    /// Local seconds since 1970 at which this change happens in `year`.
    fn local_seconds_in(self, year: u16) -> i64 {
        let first = Date {
            year,
            month: self.month,
            day: 1,
        };
        let first_days = days_from_civil(first);
        // 1970-01-01 was a Thursday, and Sunday is 0.
        let first_weekday = (first_days + 4).rem_euclid(7);
        let mut day =
            1 + (self.weekday as i64 - first_weekday).rem_euclid(7) + (self.week as i64 - 1) * 7;
        let dim = Date::days_in_month(year, self.month) as i64;
        while day > dim {
            day -= 7;
        }
        (first_days + day - 1) * DAY_S + self.time_s as i64
    }
}

/// The wall time a clock reading `wall` should read under `rule`, or `None`
/// if it already does.
///
/// A reading with no offset — the knob's, an RTC never told one — is taken as
/// local time and given the offset the rule puts there. A reading with one is
/// treated as an instant, and moved to the offset the rule says is in force:
/// that is the summer-time change, an hour forward or back, and the
/// scheduler's guards already handle a clock that jumps.
pub fn follow(rule: &Rule, wall: Wall) -> Option<Wall> {
    match wall.offset_minutes {
        None => Some(Wall {
            offset_minutes: Some(rule.offset_for_local(wall)),
            ..wall
        }),
        Some(offset) => {
            let utc = wall.local_seconds() - offset as i64 * 60;
            let expected = rule.offset_at_utc(utc);
            (expected != offset)
                .then(|| Wall::from_local_seconds(utc + expected as i64 * 60, Some(expected)))
        }
    }
}

struct Parser<'a> {
    s: &'a [u8],
    at: usize,
}

impl Parser<'_> {
    fn peek(&self) -> Option<u8> {
        self.s.get(self.at).copied()
    }

    fn take(&mut self, b: u8) -> bool {
        if self.peek() == Some(b) {
            self.at += 1;
            true
        } else {
            false
        }
    }

    fn done(&self) -> bool {
        self.at == self.s.len()
    }

    fn name(&mut self) -> Result<(), RuleError> {
        if self.take(b'<') {
            let start = self.at;
            while let Some(b) = self.peek() {
                if b == b'>' {
                    break;
                }
                if !(b.is_ascii_alphanumeric() || b == b'+' || b == b'-') {
                    return Err(RuleError::Malformed);
                }
                self.at += 1;
            }
            if self.at == start || !self.take(b'>') {
                return Err(RuleError::Malformed);
            }
            return Ok(());
        }
        let start = self.at;
        while self.peek().is_some_and(|b| b.is_ascii_alphabetic()) {
            self.at += 1;
        }
        if self.at - start < 3 {
            return Err(RuleError::Malformed);
        }
        Ok(())
    }

    fn number(&mut self, max: u32) -> Result<u32, RuleError> {
        let start = self.at;
        let mut n: u32 = 0;
        while let Some(b) = self.peek().filter(u8::is_ascii_digit) {
            n = n * 10 + (b - b'0') as u32;
            self.at += 1;
            if n > max {
                return Err(RuleError::Malformed);
            }
        }
        if self.at == start {
            return Err(RuleError::Malformed);
        }
        Ok(n)
    }

    /// `[+-]h[:mm[:ss]]` as signed seconds, up to `max_h` hours.
    fn hms(&mut self, max_h: u32) -> Result<i32, RuleError> {
        let sign = if self.take(b'-') {
            -1
        } else {
            self.take(b'+');
            1
        };
        let mut s = self.number(max_h)? * 3600;
        if self.take(b':') {
            s += self.number(59)? * 60;
            if self.take(b':') {
                s += self.number(59)?;
            }
        }
        Ok(sign * s as i32)
    }

    /// A zone offset as the rule writes it: west-positive minutes.
    fn offset(&mut self) -> Result<i16, RuleError> {
        let s = self.hms(24)?;
        if s % 60 != 0 {
            return Err(RuleError::Malformed);
        }
        Ok((s / 60) as i16)
    }

    fn change(&mut self) -> Result<Change, RuleError> {
        if !self.take(b'M') {
            return Err(RuleError::Unsupported);
        }
        let month = self.number(12)? as u8;
        if month == 0 || !self.take(b'.') {
            return Err(RuleError::Malformed);
        }
        let week = self.number(5)? as u8;
        if week == 0 || !self.take(b'.') {
            return Err(RuleError::Malformed);
        }
        let weekday = self.number(6)? as u8;
        let time_s = if self.take(b'/') {
            self.hms(167)?
        } else {
            2 * 3600
        };
        Ok(Change {
            month,
            week,
            weekday,
            time_s,
        })
    }
}

// ---------------------------------------------------------------------------
// The stored zone
// ---------------------------------------------------------------------------

/// A zone as stored: its name, for people, and its rule, for the clock.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Zone {
    pub name: String<NAME_LEN>,
    pub rule: String<RULE_LEN>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ZoneError {
    /// Empty, too long, or a character no IANA name uses.
    BadName,
    TooLong,
    BadRule(RuleError),
}

impl Zone {
    /// A zone, checked: the rule must parse, and the name must look like an
    /// IANA name — it is shown back on the page, so it is held to letters,
    /// digits and `/_+-`.
    pub fn new(name: &str, rule: &str) -> Result<Self, ZoneError> {
        let name = name.trim();
        let rule = rule.trim();
        if name.is_empty()
            || !name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"/_+-".contains(&b))
        {
            return Err(ZoneError::BadName);
        }
        Rule::parse(rule).map_err(ZoneError::BadRule)?;
        Ok(Self {
            name: String::try_from(name).map_err(|_| ZoneError::TooLong)?,
            rule: String::try_from(rule).map_err(|_| ZoneError::TooLong)?,
        })
    }

    pub fn parsed(&self) -> Rule {
        // Checked on the way in, by `new` and by `decode`.
        Rule::parse(&self.rule).unwrap_or(Rule { std: 0, dst: None })
    }
}

/// Bytes a zone takes in flash: magic, name, rule, CRC.
pub const ZONE_RECORD_LEN: usize = 4 + 1 + NAME_LEN + 1 + RULE_LEN + 4;

/// `FDZ` for feeder zone, and a layout version. Its own sector, like the
/// schedule's `FDS1`, so no other record's format moves for it.
const ZONE_MAGIC: [u8; 4] = *b"FDZ1";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ZoneRecordError {
    /// No zone was ever set here: erased flash, or bytes without this
    /// record's magic — the sector predates it on every unit built before, and
    /// whatever the partition held then is not a corrupt zone. The unit keeps
    /// plain local time.
    NotStored,
    /// Our magic, and then something wrong: an interrupted write.
    Corrupt,
}

impl Zone {
    pub fn encode(&self) -> [u8; ZONE_RECORD_LEN] {
        let mut out = [0u8; ZONE_RECORD_LEN];
        out[..4].copy_from_slice(&ZONE_MAGIC);
        out[4] = self.name.len() as u8;
        out[5..5 + self.name.len()].copy_from_slice(self.name.as_bytes());
        let at = 5 + NAME_LEN;
        out[at] = self.rule.len() as u8;
        out[at + 1..at + 1 + self.rule.len()].copy_from_slice(self.rule.as_bytes());
        let crc = crate::provisioning::crc32(&out[..ZONE_RECORD_LEN - 4]);
        out[ZONE_RECORD_LEN - 4..].copy_from_slice(&crc.to_le_bytes());
        out
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, ZoneRecordError> {
        let bytes = bytes
            .get(..ZONE_RECORD_LEN)
            .ok_or(ZoneRecordError::Corrupt)?;
        if bytes[..4] != ZONE_MAGIC {
            return Err(ZoneRecordError::NotStored);
        }
        let crc = u32::from_le_bytes(bytes[ZONE_RECORD_LEN - 4..].try_into().unwrap());
        if crc != crate::provisioning::crc32(&bytes[..ZONE_RECORD_LEN - 4]) {
            return Err(ZoneRecordError::Corrupt);
        }
        let text = |at: usize, max: usize| {
            let len = bytes[at] as usize;
            (len <= max)
                .then(|| core::str::from_utf8(&bytes[at + 1..at + 1 + len]).ok())
                .flatten()
        };
        let name = text(4, NAME_LEN).ok_or(ZoneRecordError::Corrupt)?;
        let rule = text(5 + NAME_LEN, RULE_LEN).ok_or(ZoneRecordError::Corrupt)?;
        Self::new(name, rule).map_err(|_| ZoneRecordError::Corrupt)
    }
}

/// `+02:00`, for the console and the page.
pub struct Offset(pub i16);

impl core::fmt::Display for Offset {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let sign = if self.0 < 0 { '-' } else { '+' };
        let a = self.0.unsigned_abs();
        let mut s: String<8> = String::new();
        let _ = write!(s, "{sign}{:02}:{:02}", a / 60, a % 60);
        f.write_str(&s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// UTC seconds for a UTC date and time.
    fn utc(y: u16, mo: u8, d: u8, h: u32, mi: u32) -> i64 {
        Wall {
            date: Date {
                year: y,
                month: mo,
                day: d,
            },
            second_of_day: h * 3600 + mi * 60,
            offset_minutes: Some(0),
        }
        .local_seconds()
    }

    fn rule(text: &str) -> Rule {
        Rule::parse(text).unwrap_or_else(|e| panic!("{text}: {e:?}"))
    }

    const ROME: &str = "<+01>-1<+02>,M3.5.0,M10.5.0/3";

    #[test]
    fn rome_changes_at_one_utc_on_the_last_sundays() {
        let r = rule(ROME);
        // 2026: 29 March and 25 October, both at 01:00 UTC.
        assert_eq!(r.offset_at_utc(utc(2026, 3, 29, 0, 59)), 60);
        assert_eq!(r.offset_at_utc(utc(2026, 3, 29, 1, 0)), 120);
        assert_eq!(r.offset_at_utc(utc(2026, 10, 25, 0, 59)), 120);
        assert_eq!(r.offset_at_utc(utc(2026, 10, 25, 1, 0)), 60);
        assert_eq!(r.offset_at_utc(utc(2026, 1, 1, 0, 0)), 60);
        assert_eq!(r.offset_at_utc(utc(2026, 7, 1, 0, 0)), 120);
    }

    /// The same zone in the letters people write, which must mean the same.
    #[test]
    fn named_and_numeric_spellings_agree() {
        assert_eq!(rule("CET-1CEST,M3.5.0,M10.5.0/3"), rule(ROME));
        assert_eq!(rule("CET-1CEST-2,M3.5.0/2,M10.5.0/3:00"), rule(ROME));
    }

    #[test]
    fn other_zones_change_when_they_should() {
        // US Eastern: second Sunday of March at 02:00 EST, first Sunday of
        // November at 02:00 EDT.
        let ny = rule("EST5EDT,M3.2.0,M11.1.0");
        assert_eq!(ny.offset_at_utc(utc(2026, 3, 8, 6, 59)), -300);
        assert_eq!(ny.offset_at_utc(utc(2026, 3, 8, 7, 0)), -240);
        assert_eq!(ny.offset_at_utc(utc(2026, 11, 1, 5, 59)), -240);
        assert_eq!(ny.offset_at_utc(utc(2026, 11, 1, 6, 0)), -300);

        // London: 01:00 local both ways.
        let uk = rule("GMT0BST,M3.5.0/1,M10.5.0");
        assert_eq!(uk.offset_at_utc(utc(2026, 3, 29, 0, 59)), 0);
        assert_eq!(uk.offset_at_utc(utc(2026, 3, 29, 1, 0)), 60);
        assert_eq!(uk.offset_at_utc(utc(2026, 10, 25, 1, 0)), 0);
    }

    /// Sydney's summer runs across New Year: first Sunday of October to the
    /// first Sunday of April.
    #[test]
    fn a_southern_summer_crosses_new_year() {
        let syd = rule("AEST-10AEDT,M10.1.0,M4.1.0/3");
        assert_eq!(syd.offset_at_utc(utc(2026, 1, 15, 0, 0)), 660);
        assert_eq!(syd.offset_at_utc(utc(2026, 7, 15, 0, 0)), 600);
        // 5 April 2026, 03:00 AEDT = 4 April 16:00 UTC.
        assert_eq!(syd.offset_at_utc(utc(2026, 4, 4, 15, 59)), 660);
        assert_eq!(syd.offset_at_utc(utc(2026, 4, 4, 16, 0)), 600);
        // 4 October 2026, 02:00 AEST = 3 October 16:00 UTC.
        assert_eq!(syd.offset_at_utc(utc(2026, 10, 3, 15, 59)), 600);
        assert_eq!(syd.offset_at_utc(utc(2026, 10, 3, 16, 0)), 660);
        assert_eq!(syd.offset_at_utc(utc(2026, 12, 31, 23, 0)), 660);
    }

    #[test]
    fn zones_without_summer_time_are_one_offset() {
        assert_eq!(rule("JST-9").offset_at_utc(utc(2026, 7, 1, 0, 0)), 540);
        assert_eq!(rule("<-03>3").offset_at_utc(utc(2026, 7, 1, 0, 0)), -180);
        assert_eq!(rule("<+0530>-5:30").offset_at_utc(0), 330);
        assert_eq!(rule("<+0545>-5:45").offset_at_utc(0), 345);
    }

    /// Rules a browser derives for the odd zones, as the admin page sends
    /// them: a half-hour summer, a quarter-hour zone changing at 02:45, and a
    /// change at midnight.
    #[test]
    fn browser_rules_for_the_odd_zones_follow() {
        let lord_howe = rule("<+1030>-10:30<+11>-11,M10.1.0,M4.1.0");
        assert_eq!(lord_howe.offset_at_utc(utc(2026, 1, 15, 0, 0)), 660);
        assert_eq!(lord_howe.offset_at_utc(utc(2026, 7, 15, 0, 0)), 630);

        let chatham = rule("<+1245>-12:45<+1345>,M9.5.0/2:45,M4.1.0/3:45");
        assert_eq!(chatham.offset_at_utc(utc(2026, 1, 15, 0, 0)), 825);
        assert_eq!(chatham.offset_at_utc(utc(2026, 7, 15, 0, 0)), 765);

        // Santiago: 6 September 2026, 00:00 at -04 is 04:00 UTC.
        let santiago = rule("<-04>4<-03>,M9.1.0/0,M4.1.0/0");
        assert_eq!(santiago.offset_at_utc(utc(2026, 9, 6, 3, 59)), -240);
        assert_eq!(santiago.offset_at_utc(utc(2026, 9, 6, 4, 0)), -180);
    }

    /// Week 5 is the last, whether the month has four such days or five.
    #[test]
    fn the_last_sunday_is_found_in_short_and_long_months() {
        let r = rule(ROME);
        // 2027: 28 March and 31 October.
        assert_eq!(r.offset_at_utc(utc(2027, 3, 28, 0, 59)), 60);
        assert_eq!(r.offset_at_utc(utc(2027, 3, 28, 1, 0)), 120);
        assert_eq!(r.offset_at_utc(utc(2027, 10, 31, 0, 59)), 120);
        assert_eq!(r.offset_at_utc(utc(2027, 10, 31, 1, 0)), 60);
    }

    #[test]
    fn rules_it_cannot_follow_are_refused() {
        for (text, why) in [
            ("", RuleError::Malformed),
            ("CE-1", RuleError::Malformed),
            ("CET", RuleError::Malformed),
            ("<>0", RuleError::Malformed),
            ("<+01-1", RuleError::Malformed),
            ("CET-25", RuleError::Malformed),
            ("CET-1CEST", RuleError::Unsupported),
            ("CET-1CEST,J60,J300", RuleError::Unsupported),
            ("CET-1CEST,M13.5.0,M10.5.0", RuleError::Malformed),
            ("CET-1CEST,M3.6.0,M10.5.0", RuleError::Malformed),
            ("CET-1CEST,M3.5.7,M10.5.0", RuleError::Malformed),
            ("CET-1CEST,M3.5.0", RuleError::Malformed),
            ("CET-1CEST,M3.5.0,M10.5.0 junk", RuleError::Malformed),
            ("CET-1:30:15", RuleError::Malformed),
        ] {
            assert_eq!(Rule::parse(text), Err(why), "{text:?}");
        }
    }

    fn wall(y: u16, mo: u8, d: u8, h: u32, mi: u32, off: Option<i16>) -> Wall {
        Wall {
            date: Date {
                year: y,
                month: mo,
                day: d,
            },
            second_of_day: h * 3600 + mi * 60,
            offset_minutes: off,
        }
    }

    #[test]
    fn the_clock_jumps_forward_in_spring() {
        let r = rule(ROME);
        // 02:00 CET is 01:00 UTC: summer time has begun, so it reads 03:00.
        assert_eq!(
            follow(&r, wall(2026, 3, 29, 2, 0, Some(60))),
            Some(wall(2026, 3, 29, 3, 0, Some(120)))
        );
        assert_eq!(follow(&r, wall(2026, 3, 29, 1, 59, Some(60))), None);
    }

    #[test]
    fn the_clock_goes_back_in_autumn() {
        let r = rule(ROME);
        assert_eq!(
            follow(&r, wall(2026, 10, 25, 3, 0, Some(120))),
            Some(wall(2026, 10, 25, 2, 0, Some(60)))
        );
        // Then stays: 02:00 CET is 01:00 UTC, standard time.
        assert_eq!(follow(&r, wall(2026, 10, 25, 2, 0, Some(60))), None);
    }

    /// A reading with no offset — the knob's — is local time, and only gains
    /// an offset; it does not move.
    #[test]
    fn a_reading_without_an_offset_is_given_one_not_moved() {
        let r = rule(ROME);
        assert_eq!(
            follow(&r, wall(2026, 9, 28, 8, 0, None)),
            Some(wall(2026, 9, 28, 8, 0, Some(120)))
        );
        assert_eq!(
            follow(&r, wall(2026, 12, 1, 8, 0, None)),
            Some(wall(2026, 12, 1, 8, 0, Some(60)))
        );
        // The autumn hour that happens twice is taken as standard time.
        assert_eq!(r.offset_for_local(wall(2026, 10, 25, 2, 30, None)), 60);
    }

    /// Home Assistant's `+02:00` in summer is what the rule says too, so a
    /// live time and the rule agree and nothing moves.
    #[test]
    fn a_time_already_in_the_right_offset_is_left_alone() {
        let r = rule(ROME);
        assert_eq!(follow(&r, wall(2026, 9, 28, 8, 0, Some(120))), None);
        assert_eq!(
            follow(&rule("JST-9"), wall(2026, 9, 28, 8, 0, Some(540))),
            None
        );
    }

    /// A unit switched off in summer and on in winter holds summer time with
    /// its summer offset; the rule moves it back an hour, not by guesswork.
    #[test]
    fn an_offset_carried_across_a_change_is_corrected() {
        let r = rule(ROME);
        assert_eq!(
            follow(&r, wall(2026, 11, 5, 9, 0, Some(120))),
            Some(wall(2026, 11, 5, 8, 0, Some(60)))
        );
    }

    #[test]
    fn a_zone_round_trips_through_flash() {
        let zone = Zone::new("Europe/Rome", ROME).unwrap();
        assert_eq!(Zone::decode(&zone.encode()), Ok(zone.clone()));
        assert_eq!(
            Zone::decode(&[0xFF; ZONE_RECORD_LEN]),
            Err(ZoneRecordError::NotStored)
        );
        let mut bad = zone.encode();
        bad[10] ^= 1;
        assert_eq!(Zone::decode(&bad), Err(ZoneRecordError::Corrupt));
        // Whatever the sector held before this record existed is no zone,
        // rather than a broken one.
        assert_eq!(
            Zone::decode(&[0x00; ZONE_RECORD_LEN]),
            Err(ZoneRecordError::NotStored)
        );
    }

    #[test]
    fn a_zone_name_is_held_to_what_iana_names_use() {
        assert!(Zone::new("America/Argentina/Buenos_Aires", "<-03>3").is_ok());
        assert!(Zone::new("Etc/GMT+5", "<-05>5").is_ok());
        for name in ["", "Europe/<Rome>", "Europe Rome", "Europe/Rome\""] {
            assert_eq!(Zone::new(name, ROME), Err(ZoneError::BadName), "{name}");
        }
        assert!(matches!(
            Zone::new("Europe/Rome", "nonsense"),
            Err(ZoneError::BadRule(_))
        ));
    }

    #[test]
    fn offsets_print_the_way_iso_does() {
        let mut s: String<8> = String::new();
        let _ = write!(s, "{}", Offset(120));
        assert_eq!(s, "+02:00");
        s.clear();
        let _ = write!(s, "{}", Offset(-210));
        assert_eq!(s, "-03:30");
    }
}
