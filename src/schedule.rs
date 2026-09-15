//! The clock and the feeding schedule.
//!
//! Pure logic: no broker, no timer, no executor. Wall-clock time arrives as a
//! parsed [`Wall`] and monotonic time as milliseconds, so every decision is a
//! function of its inputs and the whole thing is testable on the host.
//!
//! There is no RTC and no NTP. Home Assistant publishes the current time and
//! the schedule as retained messages; this unit holds them in RAM and advances
//! a [`LocalClock`] between them. Nothing is written to flash, so a power cycle
//! forgets everything and waits to be told again rather than guessing.
//!
//! ## Never feed twice
//!
//! The one rule worth stating plainly: **a missed meal is preferable to a
//! double one.** Three separate mechanisms enforce it, each covering a failure
//! the others cannot see.
//!
//! 1. **The consumed marker.** [`Scheduler`] remembers the day and the
//!    time-of-day of the slot it last resolved. A slot at or before that is
//!    never revisited, whether it was fed, skipped for being late, or skipped
//!    because the unit was paused.
//! 2. **The baseline pass.** The first look at the clock after boot never
//!    feeds; it only records where the day already is. Without it, rebooting a
//!    few seconds after 08:00 would dispense the 08:00 slot again, because the
//!    consumed marker lives in RAM and does not survive the reboot.
//! 3. **The lateness limit.** A slot resolved more than [`MAX_LATENESS_S`]
//!    after its time is marked consumed without feeding. This is what stops a
//!    clock correction — a `time` message that jumps the unit forward past a
//!    slot — from being mistaken for the slot falling due.
//!
//! The marker is keyed on the slot's **time of day, not its index**. Home
//! Assistant can republish a schedule with a slot inserted or removed at any
//! time, and an index would then silently point at a different meal.

use heapless::Vec;

/// A schedule longer than this is rejected rather than truncated. Eight meals a
/// day is already well past anything a cat feeder needs.
pub const MAX_SLOTS: usize = 8;

/// A slot resolved more than this long after its time is marked consumed
/// instead of being fed.
///
/// Home Assistant republishes the time every minute and the local clock runs
/// continuously between those messages, so a slot falling due normally is
/// noticed within the scheduler's tick — a second or so. Two minutes is
/// therefore a wide margin for a genuine crossing while still being far too
/// short to swallow a clock jump, which moves the unit by minutes or hours.
pub const MAX_LATENESS_S: u32 = 120;

/// A calendar date. Only ever used as an identity for "the same day".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Date {
    pub year: u16,
    pub month: u8,
    pub day: u8,
}

impl Date {
    fn is_leap(year: u16) -> bool {
        year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400))
    }

    fn days_in_month(year: u16, month: u8) -> u8 {
        match month {
            1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
            4 | 6 | 9 | 11 => 30,
            2 if Self::is_leap(year) => 29,
            2 => 28,
            _ => 0,
        }
    }

    fn is_valid(&self) -> bool {
        self.month >= 1
            && self.month <= 12
            && self.day >= 1
            && self.day <= Self::days_in_month(self.year, self.month)
    }

    /// The next calendar day. Used only when the local clock runs past midnight
    /// without hearing from the broker.
    #[must_use]
    pub fn next_day(self) -> Self {
        if self.day < Self::days_in_month(self.year, self.month) {
            Self {
                day: self.day + 1,
                ..self
            }
        } else if self.month < 12 {
            Self {
                year: self.year,
                month: self.month + 1,
                day: 1,
            }
        } else {
            Self {
                year: self.year.saturating_add(1),
                month: 1,
                day: 1,
            }
        }
    }
}

/// Local wall-clock time: a date plus a position within the day.
///
/// **The offset is recorded and never applied.** Home Assistant publishes its
/// own local time, and the feeders live in the same house, so the wall-clock
/// fields already arrive in the frame the schedule is written in — `08:00` in a
/// slot means 08:00 on the kitchen wall. There is nothing to convert to.
///
/// Not applying it is what makes daylight saving free: in October Home
/// Assistant simply starts sending `+01:00` and the wall-clock fields shift
/// with it, needing no timezone rules on the device.
///
/// It is kept rather than dropped because it is the one clue that the
/// assumption has been broken. An automation switched from `now()` to
/// `utcnow()` would still publish a valid-looking time, and every meal would
/// quietly move by the offset. Logging it at startup makes that visible on the
/// first line of the console instead.
///
/// `None` means the payload carried no recognisable offset, which is itself
/// worth seeing in the log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Wall {
    pub date: Date,
    pub second_of_day: u32,
    /// Minutes east of UTC, as published. Informational only.
    pub offset_minutes: Option<i16>,
}

/// Seconds in a day. No leap-second handling: a leap second is parsed as :59.
const DAY_S: u32 = 24 * 60 * 60;

impl core::fmt::Display for Wall {
    /// ISO 8601, with the offset back on when there was one.
    ///
    /// Used for both the console and the `last_fed` field, so a timestamp reads
    /// the same wherever it turns up.
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}",
            self.date.year,
            self.date.month,
            self.date.day,
            self.second_of_day / 3600,
            (self.second_of_day / 60) % 60,
            self.second_of_day % 60,
        )?;

        match self.offset_minutes {
            None => Ok(()),
            Some(offset) => {
                let (sign, minutes) = if offset < 0 {
                    ('-', -offset)
                } else {
                    ('+', offset)
                };
                write!(f, "{sign}{:02}:{:02}", minutes / 60, minutes % 60)
            }
        }
    }
}

impl Wall {
    pub fn minute_of_day(&self) -> u16 {
        (self.second_of_day / 60) as u16
    }

    /// Advances by whole seconds, rolling the date over as needed.
    ///
    /// The rollover loop is bounded. A monotonic counter far ahead of the last
    /// alignment means something is badly wrong, and spinning through a century
    /// of dates is not a useful response.
    fn plus_seconds(self, seconds: u64) -> Option<Self> {
        let total = self.second_of_day as u64 + seconds;
        let days = total / DAY_S as u64;
        if days > MAX_ROLLOVER_DAYS {
            return None;
        }

        let mut date = self.date;
        for _ in 0..days {
            date = date.next_day();
        }
        Some(Self {
            date,
            second_of_day: (total % DAY_S as u64) as u32,
            ..self
        })
    }
}

/// Roughly three years. Past this the local clock is not worth trusting.
const MAX_ROLLOVER_DAYS: u64 = 1_100;

/// Why a `feeder/time` payload could not be used.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimeError {
    /// Too short, or the fixed separators are not where they must be.
    Malformed,
    /// Syntactically fine but not a real instant, such as the 31st of February.
    OutOfRange,
}

/// Parses `2026-09-14T08:00:00+02:00` into local wall-clock time.
///
/// Handles the spellings that actually arrive: a numeric offset, a trailing
/// `Z`, and the fractional seconds Python's `datetime.isoformat()` emits. The
/// offset is recorded but never applied — see [`Wall`].
///
/// Surrounding whitespace and a pair of wrapping double quotes are tolerated,
/// because the contract is written with quotes and Home Assistant's
/// `{{ now().isoformat() }}` publishes without them.
pub fn parse_time(payload: &[u8]) -> Result<Wall, TimeError> {
    let text = core::str::from_utf8(payload)
        .map_err(|_| TimeError::Malformed)?
        .trim()
        .trim_matches('"')
        .trim();
    let b = text.as_bytes();

    // YYYY-MM-DDTHH:MM:SS is 19 bytes and every separator is fixed.
    if b.len() < 19 || b[4] != b'-' || b[7] != b'-' || b[13] != b':' || b[16] != b':' {
        return Err(TimeError::Malformed);
    }
    if b[10] != b'T' && b[10] != b't' && b[10] != b' ' {
        return Err(TimeError::Malformed);
    }

    let year = digits(&b[0..4]).ok_or(TimeError::Malformed)?;
    let month = digits(&b[5..7]).ok_or(TimeError::Malformed)? as u8;
    let day = digits(&b[8..10]).ok_or(TimeError::Malformed)? as u8;
    let hour = digits(&b[11..13]).ok_or(TimeError::Malformed)? as u32;
    let minute = digits(&b[14..16]).ok_or(TimeError::Malformed)? as u32;
    let second = digits(&b[17..19]).ok_or(TimeError::Malformed)? as u32;

    let date = Date { year, month, day };
    if !date.is_valid() || hour > 23 || minute > 59 || second > 60 {
        return Err(TimeError::OutOfRange);
    }

    Ok(Wall {
        date,
        // A leap second lands on :59 rather than being rejected.
        second_of_day: hour * 3600 + minute * 60 + second.min(59),
        offset_minutes: parse_offset(&b[19..]),
    })
}

/// Reads the trailing offset, if there is one this understands.
///
/// Never fails. The offset is informational and no decision reads it, so an
/// unfamiliar suffix must cost the unit its time — the one thing it cannot
/// recover on its own while the broker is away.
fn parse_offset(mut rest: &[u8]) -> Option<i16> {
    // Fractional seconds first: `...:00.123456+02:00`.
    if rest.first() == Some(&b'.') {
        let mut at = 1;
        while matches!(rest.get(at), Some(b'0'..=b'9')) {
            at += 1;
        }
        rest = &rest[at..];
    }

    let (sign, body) = match rest.first()? {
        b'Z' | b'z' if rest.len() == 1 => return Some(0),
        b'+' => (1i16, &rest[1..]),
        b'-' => (-1i16, &rest[1..]),
        _ => return None,
    };

    // +HH:MM, +HHMM and +HH are all in the wild.
    let (hours, minutes) = match body.len() {
        5 if body[2] == b':' => (digits(&body[0..2])?, digits(&body[3..5])?),
        4 => (digits(&body[0..2])?, digits(&body[2..4])?),
        2 => (digits(&body[0..2])?, 0),
        _ => return None,
    };
    if hours > 23 || minutes > 59 {
        return None;
    }

    Some(sign * (hours as i16 * 60 + minutes as i16))
}

fn digits(bytes: &[u8]) -> Option<u16> {
    let mut value: u16 = 0;
    for &b in bytes {
        let d = b.checked_sub(b'0')?;
        if d > 9 {
            return None;
        }
        value = value.checked_mul(10)?.checked_add(d as u16)?;
    }
    Some(value)
}

/// Wall-clock time held in RAM and advanced by the monotonic counter.
///
/// Re-aligned on every `feeder/time` message. Between messages it free-runs, so
/// losing the broker does not stop the schedule — which is the point, since the
/// broker is also the only thing that could ever tell it the time again.
#[derive(Debug, Clone, Copy, Default)]
pub struct LocalClock {
    anchor: Option<(u64, Wall)>,
}

/// What an alignment did to the clock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Alignment {
    /// The clock had never been set. The schedule can start running.
    Started,
    /// Adjusted by this many seconds, positive if the clock moved forward.
    Adjusted { drift_s: i64 },
}

impl LocalClock {
    pub const fn new() -> Self {
        Self { anchor: None }
    }

    /// Pins wall-clock time to a reading of the monotonic counter.
    pub fn align(&mut self, monotonic_ms: u64, wall: Wall) -> Alignment {
        let before = self.now(monotonic_ms);
        self.anchor = Some((monotonic_ms, wall));

        match before {
            None => Alignment::Started,
            Some(old) => Alignment::Adjusted {
                drift_s: seconds_between(old, wall),
            },
        }
    }

    /// Local time now, or `None` if the broker has never said what time it is.
    ///
    /// `None` is the correct answer to "what time is it" on a unit that has
    /// been power-cycled with no broker. It waits; it does not guess.
    pub fn now(&self, monotonic_ms: u64) -> Option<Wall> {
        let (anchored_at, wall) = self.anchor?;
        wall.plus_seconds(monotonic_ms.saturating_sub(anchored_at) / 1_000)
    }

    pub fn is_set(&self) -> bool {
        self.anchor.is_some()
    }
}

/// Signed difference in seconds, `to - from`, for reporting drift only.
///
/// Saturates well before overflowing, and does not attempt real calendar
/// arithmetic across a date change: a correction that also moves the date is
/// reported as a whole number of days plus the difference within the day, which
/// is accurate enough for a log line.
fn seconds_between(from: Wall, to: Wall) -> i64 {
    let within_day = to.second_of_day as i64 - from.second_of_day as i64;
    if to.date == from.date {
        within_day
    } else if (to.date.year, to.date.month, to.date.day)
        > (from.date.year, from.date.month, from.date.day)
    {
        within_day + DAY_S as i64
    } else {
        within_day - DAY_S as i64
    }
}

/// One feeding time. `minute_of_day` is local wall-clock, matching [`Wall`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Slot {
    pub minute_of_day: u16,
    pub portions: u8,
}

/// Why a `feeder/schedule` payload could not be used.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScheduleError {
    /// Not the JSON this contract describes.
    Malformed,
    /// More than [`MAX_SLOTS`] entries. Rejected whole rather than truncated:
    /// a silently shortened schedule skips meals with nothing to show for it.
    TooManySlots,
    /// A `time` that is not `HH:MM`, or a `portions` above 255.
    BadSlot,
}

/// The feeding schedule, as published retained by Home Assistant.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Schedule {
    slots: Vec<Slot, MAX_SLOTS>,
}

impl Schedule {
    pub const fn new() -> Self {
        Self { slots: Vec::new() }
    }

    pub fn slots(&self) -> &[Slot] {
        &self.slots
    }

    pub fn len(&self) -> usize {
        self.slots.len()
    }

    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    /// Parses `[{"time":"08:00","portions":2},{"time":"19:00","portions":2}]`.
    ///
    /// Hand-rolled rather than pulling in a JSON crate, because the shape is
    /// fixed and machine-generated. It is written to reject rather than assume:
    /// every malformed input is an error, never a partial schedule, since a
    /// half-read schedule feeds the wrong meals rather than none.
    ///
    /// Unknown keys are skipped, so adding a field to the contract later does
    /// not brick a unit running older firmware.
    pub fn parse(payload: &[u8]) -> Result<Self, ScheduleError> {
        let mut json = Json::new(payload);
        let mut slots = Vec::new();

        json.expect(b'[')?;
        if json.take(b']') {
            return json.end().map(|()| Self { slots });
        }

        loop {
            let slot = json.slot()?;
            slots.push(slot).map_err(|_| ScheduleError::TooManySlots)?;

            if json.take(b',') {
                continue;
            }
            json.expect(b']')?;
            break;
        }

        json.end()?;
        Ok(Self { slots })
    }
}

/// A cursor over the schedule payload.
struct Json<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Json<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, at: 0 }
    }

    fn skip_space(&mut self) {
        while matches!(self.bytes.get(self.at), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.at += 1;
        }
    }

    fn peek(&mut self) -> Option<u8> {
        self.skip_space();
        self.bytes.get(self.at).copied()
    }

    fn take(&mut self, wanted: u8) -> bool {
        if self.peek() == Some(wanted) {
            self.at += 1;
            true
        } else {
            false
        }
    }

    fn expect(&mut self, wanted: u8) -> Result<(), ScheduleError> {
        if self.take(wanted) {
            Ok(())
        } else {
            Err(ScheduleError::Malformed)
        }
    }

    /// Trailing content is an error: a payload this firmware only half
    /// understands is not one it should act on.
    fn end(&mut self) -> Result<(), ScheduleError> {
        if self.peek().is_none() {
            Ok(())
        } else {
            Err(ScheduleError::Malformed)
        }
    }

    fn string(&mut self) -> Result<&'a str, ScheduleError> {
        self.expect(b'"')?;
        let start = self.at;
        while let Some(&byte) = self.bytes.get(self.at) {
            match byte {
                b'"' => {
                    let text = core::str::from_utf8(&self.bytes[start..self.at])
                        .map_err(|_| ScheduleError::Malformed)?;
                    self.at += 1;
                    return Ok(text);
                }
                // No escapes in a time or a key. Refusing them keeps the parser
                // honest rather than silently mis-reading one.
                b'\\' => return Err(ScheduleError::Malformed),
                _ => self.at += 1,
            }
        }
        Err(ScheduleError::Malformed)
    }

    fn number(&mut self) -> Result<u32, ScheduleError> {
        self.skip_space();
        let start = self.at;
        while matches!(self.bytes.get(self.at), Some(b'0'..=b'9')) {
            self.at += 1;
        }
        if start == self.at {
            return Err(ScheduleError::Malformed);
        }

        let mut value: u32 = 0;
        for &digit in &self.bytes[start..self.at] {
            value = value
                .checked_mul(10)
                .and_then(|v| v.checked_add((digit - b'0') as u32))
                .ok_or(ScheduleError::BadSlot)?;
        }
        Ok(value)
    }

    /// Skips one value of any shape, so an unknown key costs nothing.
    fn skip_value(&mut self) -> Result<(), ScheduleError> {
        match self.peek().ok_or(ScheduleError::Malformed)? {
            b'"' => self.string().map(|_| ()),
            open @ (b'{' | b'[') => {
                let close = if open == b'{' { b'}' } else { b']' };
                let mut depth = 0usize;
                while let Some(&byte) = self.bytes.get(self.at) {
                    match byte {
                        // Strings first: braces inside them are not structure.
                        b'"' => {
                            self.string()?;
                            continue;
                        }
                        b'{' | b'[' => depth += 1,
                        b'}' | b']' => {
                            depth -= 1;
                            if depth == 0 {
                                if byte != close {
                                    return Err(ScheduleError::Malformed);
                                }
                                self.at += 1;
                                return Ok(());
                            }
                        }
                        _ => {}
                    }
                    self.at += 1;
                }
                Err(ScheduleError::Malformed)
            }
            // A number, or one of true/false/null.
            _ => {
                while matches!(self.bytes.get(self.at), Some(byte)
                    if !matches!(byte, b',' | b'}' | b']' | b' ' | b'\t' | b'\n' | b'\r'))
                {
                    self.at += 1;
                }
                Ok(())
            }
        }
    }

    fn slot(&mut self) -> Result<Slot, ScheduleError> {
        self.expect(b'{')?;

        let mut minute_of_day = None;
        let mut portions = None;

        loop {
            let key = self.string()?;
            self.expect(b':')?;

            match key {
                "time" => minute_of_day = Some(parse_hhmm(self.string()?)?),
                "portions" => {
                    portions =
                        Some(u8::try_from(self.number()?).map_err(|_| ScheduleError::BadSlot)?)
                }
                _ => self.skip_value()?,
            }

            if self.take(b',') {
                continue;
            }
            self.expect(b'}')?;
            break;
        }

        Ok(Slot {
            minute_of_day: minute_of_day.ok_or(ScheduleError::BadSlot)?,
            portions: portions.ok_or(ScheduleError::BadSlot)?,
        })
    }
}

/// `"08:00"` into minutes since midnight.
fn parse_hhmm(text: &str) -> Result<u16, ScheduleError> {
    let b = text.as_bytes();
    if b.len() != 5 || b[2] != b':' {
        return Err(ScheduleError::BadSlot);
    }

    let hour = digits(&b[0..2]).ok_or(ScheduleError::BadSlot)?;
    let minute = digits(&b[3..5]).ok_or(ScheduleError::BadSlot)?;
    if hour > 23 || minute > 59 {
        return Err(ScheduleError::BadSlot);
    }

    Ok(hour * 60 + minute)
}

/// What the scheduler decided this tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Due {
    /// No slot is outstanding.
    Nothing,
    /// Feed this many portions. The slot is already marked consumed, so this is
    /// returned exactly once however often the caller asks again.
    Feed { minute_of_day: u16, portions: u8 },
    /// A slot was resolved without feeding. It will not come back.
    Consumed { minute_of_day: u16, why: Skipped },
}

/// Why a slot was marked consumed rather than fed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Skipped {
    /// The very first look at the clock, which only takes a baseline.
    Baseline,
    /// The unit is paused. Resuming must not replay it.
    Paused,
    /// Resolved more than [`MAX_LATENESS_S`] after its time, so the unit either
    /// was not running or its clock just jumped. Not a meal to catch up.
    TooLate { by_s: u32 },
}

/// Decides when to feed, and refuses to feed twice.
#[derive(Debug, Clone, Default)]
pub struct Scheduler {
    schedule: Schedule,
    /// The day and time-of-day of the last slot resolved, fed or not.
    consumed_through: Option<(Date, u16)>,
    /// False until the first look at the clock. See the module docs.
    baselined: bool,
}

impl Scheduler {
    pub const fn new() -> Self {
        Self {
            schedule: Schedule::new(),
            consumed_through: None,
            baselined: false,
        }
    }

    /// Replaces the schedule.
    ///
    /// Deliberately does **not** clear the consumed marker. The marker is a
    /// time of day, so it stays meaningful across a schedule that gains or
    /// loses slots — which is exactly why it is not an index.
    pub fn set_schedule(&mut self, schedule: Schedule) {
        self.schedule = schedule;
    }

    pub fn schedule(&self) -> &Schedule {
        &self.schedule
    }

    /// Resolves at most one slot per call.
    ///
    /// Call it whenever convenient — every second is plenty. Calling it more
    /// often changes nothing, because a resolved slot is marked consumed before
    /// this returns.
    pub fn next_due(&mut self, now: Wall, paused: bool) -> Due {
        let after = match self.consumed_through {
            Some((date, minute)) if date == now.date => Some(minute),
            // A new day re-arms every slot. Yesterday's marker says nothing
            // about today.
            _ => None,
        };

        let now_minute = now.minute_of_day();
        let due = self
            .schedule
            .slots()
            .iter()
            .filter(|slot| slot.minute_of_day <= now_minute)
            .filter(|slot| after.is_none_or(|limit| slot.minute_of_day > limit))
            // The latest outstanding slot. Anything earlier is a missed meal,
            // and missed meals are not caught up.
            .max_by_key(|slot| slot.minute_of_day)
            .copied();

        let Some(slot) = due else {
            // Nothing outstanding, but the clock has now been seen.
            self.baselined = true;
            return Due::Nothing;
        };

        self.consumed_through = Some((now.date, slot.minute_of_day));
        let minute_of_day = slot.minute_of_day;

        if !self.baselined {
            self.baselined = true;
            return Due::Consumed {
                minute_of_day,
                why: Skipped::Baseline,
            };
        }

        let lateness_s = now.second_of_day - slot.minute_of_day as u32 * 60;
        if lateness_s > MAX_LATENESS_S {
            return Due::Consumed {
                minute_of_day,
                why: Skipped::TooLate { by_s: lateness_s },
            };
        }

        if paused {
            return Due::Consumed {
                minute_of_day,
                why: Skipped::Paused,
            };
        }

        Due::Feed {
            minute_of_day,
            portions: slot.portions,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn date(year: u16, month: u8, day: u8) -> Date {
        Date { year, month, day }
    }

    fn wall(day: u8, hour: u32, minute: u32) -> Wall {
        wall_s(day, hour, minute, 0)
    }

    fn wall_s(day: u8, hour: u32, minute: u32, second: u32) -> Wall {
        Wall {
            date: date(2026, 9, day),
            second_of_day: hour * 3600 + minute * 60 + second,
            offset_minutes: None,
        }
    }

    /// The schedule Home Assistant publishes in the contract: 08:00 and 19:00.
    fn two_meals() -> Scheduler {
        let mut scheduler = Scheduler::new();
        scheduler.set_schedule(
            Schedule::parse(br#"[{"time":"08:00","portions":2},{"time":"19:00","portions":3}]"#)
                .unwrap(),
        );
        scheduler
    }

    /// Takes the baseline the way the real task does, before any slot is due.
    fn armed_at(scheduler: &mut Scheduler, now: Wall) {
        assert_eq!(scheduler.next_due(now, false), Due::Nothing);
    }

    // ---- dates ----

    #[test]
    fn next_day_walks_through_a_month() {
        assert_eq!(date(2026, 9, 14).next_day(), date(2026, 9, 15));
        assert_eq!(date(2026, 9, 30).next_day(), date(2026, 10, 1));
        assert_eq!(date(2026, 12, 31).next_day(), date(2027, 1, 1));
    }

    #[test]
    fn february_knows_about_leap_years() {
        assert_eq!(date(2026, 2, 28).next_day(), date(2026, 3, 1));
        assert_eq!(date(2028, 2, 28).next_day(), date(2028, 2, 29));
        assert_eq!(date(2028, 2, 29).next_day(), date(2028, 3, 1));
        // 2100 is divisible by 4 but not a leap year.
        assert_eq!(date(2100, 2, 28).next_day(), date(2100, 3, 1));
        assert_eq!(date(2000, 2, 28).next_day(), date(2000, 2, 29));
    }

    // ---- parsing time ----

    #[test]
    fn parses_the_time_home_assistant_publishes() {
        let parsed = parse_time(b"2026-09-14T08:00:00+02:00").unwrap();
        assert_eq!(parsed.date, date(2026, 9, 14));
        assert_eq!(parsed.second_of_day, 8 * 3600);
    }

    #[test]
    fn tolerates_the_spellings_that_actually_arrive() {
        // Every one of these means eight in the morning, whatever it says about
        // the zone, so the wall-clock fields must come out identical.
        for payload in [
            br#""2026-09-14T08:00:00+02:00""#.as_slice(), // quoted, as the contract writes it
            b"2026-09-14T08:00:00.123456+02:00",          // microseconds, as isoformat emits
            b"2026-09-14T08:00:00Z",
            b"2026-09-14 08:00:00+02:00", // space separator
            b"  2026-09-14T08:00:00Z\n",  // surrounding whitespace
            b"2026-09-14T08:00:00+0200",  // no colon in the offset
            b"2026-09-14T08:00:00",       // no offset at all
        ] {
            let parsed = parse_time(payload).unwrap_or_else(|e| panic!("{payload:?}: {e:?}"));
            assert_eq!(parsed.date, date(2026, 9, 14), "{payload:?}");
            assert_eq!(parsed.second_of_day, 8 * 3600, "{payload:?}");
        }
    }

    #[test]
    fn reads_the_offsets_that_exist_in_the_world() {
        let offset = |text: &[u8]| parse_time(text).unwrap().offset_minutes;

        assert_eq!(offset(b"2026-09-14T08:00:00Z"), Some(0));
        assert_eq!(offset(b"2026-09-14T08:00:00+00:00"), Some(0));
        // India, Newfoundland and the Chatham Islands: not whole hours.
        assert_eq!(offset(b"2026-09-14T08:00:00+05:30"), Some(330));
        assert_eq!(offset(b"2026-09-14T08:00:00-03:30"), Some(-210));
        assert_eq!(offset(b"2026-09-14T08:00:00+12:45"), Some(765));
        // Hour-only spelling.
        assert_eq!(offset(b"2026-09-14T08:00:00-05"), Some(-300));
    }

    #[test]
    fn an_unreadable_offset_never_costs_the_time() {
        // The offset feeds no decision. Losing the time would stop the schedule,
        // and the unit cannot recover it alone while the broker is away.
        let parsed = parse_time(b"2026-09-14T08:00:00 CEST").unwrap();
        assert_eq!(parsed.second_of_day, 8 * 3600);
        assert_eq!(parsed.offset_minutes, None);
    }

    #[test]
    fn a_time_prints_back_the_way_it_arrived() {
        for text in [
            "2026-09-14T08:00:00+02:00",
            "2026-09-14T08:00:00+00:00",
            "2026-12-31T23:59:59-03:30",
            "2026-09-14T08:00:00", // no offset in, none out
        ] {
            let parsed = parse_time(text.as_bytes()).unwrap();
            assert_eq!(std::format!("{parsed}"), text);
        }

        // `Z` is the one spelling that comes back differently, as +00:00.
        let utc = parse_time(b"2026-09-14T08:00:00Z").unwrap();
        assert_eq!(std::format!("{utc}"), "2026-09-14T08:00:00+00:00");
    }

    #[test]
    fn the_offset_is_carried_across_a_free_run() {
        // It ends up in `last_fed`, so it has to survive the local clock
        // advancing and rolling over midnight.
        let mut clock = LocalClock::new();
        clock.align(0, parse_time(b"2026-09-14T23:59:30+02:00").unwrap());

        let later = clock.now(60_000).unwrap();
        assert_eq!(later.date, date(2026, 9, 15));
        assert_eq!(later.offset_minutes, Some(120));
    }

    #[test]
    fn the_offset_is_recorded_but_never_applied() {
        // Schedules are local wall-clock times, so 08:00 is 08:00 whatever the
        // offset says. Applying it would move every meal by an hour for half the
        // year. It is kept only so the console can show it.
        let cet = parse_time(b"2026-09-14T08:00:00+01:00").unwrap();
        let cest = parse_time(b"2026-09-14T08:00:00+02:00").unwrap();

        assert_eq!(cet.second_of_day, cest.second_of_day);
        assert_eq!(cet.offset_minutes, Some(60));
        assert_eq!(cest.offset_minutes, Some(120));
    }

    #[test]
    fn rejects_rubbish_rather_than_guessing() {
        for payload in [
            b"".as_slice(),
            b"not a time",
            b"2026-09-14",
            b"2026/09/14T08:00:00",
            b"20260914T080000",
            b"202X-09-14T08:00:00Z",
        ] {
            assert_eq!(
                parse_time(payload),
                Err(TimeError::Malformed),
                "{payload:?}"
            );
        }
    }

    #[test]
    fn rejects_dates_that_do_not_exist() {
        assert_eq!(
            parse_time(b"2026-02-30T08:00:00Z"),
            Err(TimeError::OutOfRange)
        );
        assert_eq!(
            parse_time(b"2026-13-01T08:00:00Z"),
            Err(TimeError::OutOfRange)
        );
        assert_eq!(
            parse_time(b"2026-09-14T25:00:00Z"),
            Err(TimeError::OutOfRange)
        );
        // ...but 29 February in a leap year is fine.
        assert!(parse_time(b"2028-02-29T08:00:00Z").is_ok());
    }

    // ---- the local clock ----

    #[test]
    fn the_clock_says_nothing_until_it_is_told() {
        let clock = LocalClock::new();
        assert!(!clock.is_set());
        assert_eq!(clock.now(123_456), None);
    }

    #[test]
    fn the_clock_free_runs_between_messages() {
        let mut clock = LocalClock::new();
        assert_eq!(clock.align(10_000, wall(14, 8, 0)), Alignment::Started);

        // Ten minutes of monotonic time with no word from the broker.
        let later = clock.now(10_000 + 600_000).unwrap();
        assert_eq!(later, wall(14, 8, 10));
    }

    #[test]
    fn the_clock_rolls_over_midnight_on_its_own() {
        let mut clock = LocalClock::new();
        clock.align(0, wall_s(30, 23, 59, 30));

        let later = clock.now(60_000).unwrap();
        assert_eq!(later.date, date(2026, 10, 1));
        assert_eq!(later.second_of_day, 30);
    }

    #[test]
    fn realignment_reports_the_drift_it_corrected() {
        let mut clock = LocalClock::new();
        clock.align(0, wall(14, 8, 0));

        // A minute of monotonic time passes, but the broker says five went by.
        assert_eq!(
            clock.align(60_000, wall(14, 8, 5)),
            Alignment::Adjusted { drift_s: 240 }
        );
    }

    // ---- parsing the schedule ----

    #[test]
    fn parses_the_schedule_home_assistant_publishes() {
        let schedule =
            Schedule::parse(br#"[{"time":"08:00","portions":2},{"time":"19:00","portions":2}]"#)
                .unwrap();

        assert_eq!(
            schedule.slots(),
            [
                Slot {
                    minute_of_day: 480,
                    portions: 2
                },
                Slot {
                    minute_of_day: 1140,
                    portions: 2
                },
            ]
        );
    }

    #[test]
    fn an_empty_schedule_is_valid_and_means_never() {
        let schedule = Schedule::parse(b"[]").unwrap();
        assert!(schedule.is_empty());

        let mut scheduler = Scheduler::new();
        scheduler.set_schedule(schedule);
        armed_at(&mut scheduler, wall(14, 8, 0));
        assert_eq!(scheduler.next_due(wall(14, 12, 0), false), Due::Nothing);
    }

    #[test]
    fn whitespace_and_key_order_do_not_matter() {
        let schedule =
            Schedule::parse(b"[\n  { \"portions\" : 4 , \"time\" : \"07:30\" }\n]").unwrap();

        assert_eq!(
            schedule.slots(),
            [Slot {
                minute_of_day: 450,
                portions: 4
            }]
        );
    }

    #[test]
    fn an_unknown_key_is_skipped_rather_than_fatal() {
        // So adding a field to the contract does not brick older firmware.
        let schedule = Schedule::parse(
            br#"[{"time":"08:00","label":"breakfast","meta":{"x":[1,2]},"portions":2}]"#,
        )
        .unwrap();

        assert_eq!(schedule.len(), 1);
        assert_eq!(schedule.slots()[0].portions, 2);
    }

    #[test]
    fn a_malformed_schedule_is_rejected_whole() {
        // Never a partial schedule: half a schedule feeds the wrong meals.
        for payload in [
            b"".as_slice(),
            b"[",
            b"[{}]",
            br#"[{"time":"08:00"}]"#,
            br#"[{"portions":2}]"#,
            br#"[{"time":"08:00","portions":2}"#,
            br#"[{"time":"08:00","portions":2}] trailing"#,
            br#"{"time":"08:00","portions":2}"#,
            br#"[{"time":"0800","portions":2}]"#,
            br#"[{"time":"25:00","portions":2}]"#,
            br#"[{"time":"08:60","portions":2}]"#,
            br#"[{"time":"08:00","portions":300}]"#,
        ] {
            assert!(Schedule::parse(payload).is_err(), "{payload:?}");
        }
    }

    #[test]
    fn too_many_slots_is_refused_not_truncated() {
        let mut payload = alloc_string("[");
        for i in 0..=MAX_SLOTS {
            if i > 0 {
                payload.push(',');
            }
            payload.push_str(&format!(r#"{{"time":"0{}:00","portions":1}}"#, i % 10));
        }
        payload.push(']');

        assert_eq!(
            Schedule::parse(payload.as_bytes()),
            Err(ScheduleError::TooManySlots)
        );
    }

    fn alloc_string(s: &str) -> std::string::String {
        std::string::String::from(s)
    }

    // ---- the double-feed guard ----

    #[test]
    fn the_first_look_at_the_clock_never_feeds() {
        // The guard lives in RAM, so a reboot just after 08:00 would otherwise
        // dispense the 08:00 slot for a second time.
        let mut scheduler = two_meals();

        assert_eq!(
            scheduler.next_due(wall_s(14, 8, 0, 30), false),
            Due::Consumed {
                minute_of_day: 480,
                why: Skipped::Baseline
            }
        );
        // And it stays consumed.
        assert_eq!(scheduler.next_due(wall_s(14, 8, 1, 0), false), Due::Nothing);
    }

    #[test]
    fn a_slot_fires_once_when_the_clock_reaches_it() {
        let mut scheduler = two_meals();
        armed_at(&mut scheduler, wall(14, 7, 59));

        assert_eq!(
            scheduler.next_due(wall(14, 8, 0), false),
            Due::Feed {
                minute_of_day: 480,
                portions: 2
            }
        );

        // Asked again a second later, and a minute later, and nothing happens.
        assert_eq!(scheduler.next_due(wall_s(14, 8, 0, 1), false), Due::Nothing);
        assert_eq!(scheduler.next_due(wall(14, 8, 1), false), Due::Nothing);
    }

    #[test]
    fn the_second_meal_of_the_day_still_fires() {
        let mut scheduler = two_meals();
        armed_at(&mut scheduler, wall(14, 7, 59));
        scheduler.next_due(wall(14, 8, 0), false);

        assert_eq!(
            scheduler.next_due(wall(14, 19, 0), false),
            Due::Feed {
                minute_of_day: 1140,
                portions: 3
            }
        );
    }

    #[test]
    fn a_new_day_rearms_every_slot() {
        let mut scheduler = two_meals();
        armed_at(&mut scheduler, wall(14, 7, 59));
        scheduler.next_due(wall(14, 8, 0), false);
        scheduler.next_due(wall(14, 19, 0), false);

        assert_eq!(
            scheduler.next_due(wall(15, 8, 0), false),
            Due::Feed {
                minute_of_day: 480,
                portions: 2
            }
        );
    }

    #[test]
    fn a_clock_jump_past_a_slot_does_not_catch_it_up() {
        // The unit's clock was wrong and Home Assistant corrected it. That is
        // not the same as the slot falling due.
        let mut scheduler = two_meals();
        armed_at(&mut scheduler, wall(14, 7, 0));

        assert_eq!(
            scheduler.next_due(wall(14, 12, 0), false),
            Due::Consumed {
                minute_of_day: 480,
                why: Skipped::TooLate { by_s: 4 * 60 * 60 }
            }
        );
        // And it is not waiting to fire later either.
        assert_eq!(scheduler.next_due(wall(14, 12, 1), false), Due::Nothing);
    }

    #[test]
    fn a_clock_jump_past_a_slot_already_fed_changes_nothing() {
        let mut scheduler = two_meals();
        armed_at(&mut scheduler, wall(14, 7, 59));
        scheduler.next_due(wall(14, 8, 0), false);

        assert_eq!(scheduler.next_due(wall(14, 8, 5), false), Due::Nothing);
    }

    #[test]
    fn a_slot_a_little_late_still_feeds() {
        // The broker going quiet for a minute must not cost a meal.
        let mut scheduler = two_meals();
        armed_at(&mut scheduler, wall(14, 7, 59));

        assert_eq!(
            scheduler.next_due(wall_s(14, 8, 1, 0), false),
            Due::Feed {
                minute_of_day: 480,
                portions: 2
            }
        );
    }

    #[test]
    fn the_lateness_limit_is_where_it_says_it_is() {
        let mut scheduler = two_meals();
        armed_at(&mut scheduler, wall(14, 7, 59));
        assert!(matches!(
            scheduler.next_due(wall_s(14, 8, 0, MAX_LATENESS_S), false),
            Due::Feed { .. }
        ));

        let mut scheduler = two_meals();
        armed_at(&mut scheduler, wall(14, 7, 59));
        assert!(matches!(
            scheduler.next_due(wall_s(14, 8, 0, MAX_LATENESS_S + 1), false),
            Due::Consumed {
                why: Skipped::TooLate { .. },
                ..
            }
        ));
    }

    // ---- pause ----

    #[test]
    fn a_paused_slot_is_consumed_rather_than_fed() {
        let mut scheduler = two_meals();
        armed_at(&mut scheduler, wall(14, 7, 59));

        assert_eq!(
            scheduler.next_due(wall(14, 8, 0), true),
            Due::Consumed {
                minute_of_day: 480,
                why: Skipped::Paused
            }
        );
    }

    #[test]
    fn resuming_never_replays_the_slot_that_was_paused_over() {
        // Otherwise every unpause dispenses the meal that was deliberately
        // skipped, which is the whole reason slots are consumed rather than
        // left outstanding.
        let mut scheduler = two_meals();
        armed_at(&mut scheduler, wall(14, 7, 59));
        scheduler.next_due(wall(14, 8, 0), true);

        assert_eq!(scheduler.next_due(wall(14, 8, 30), false), Due::Nothing);
    }

    #[test]
    fn resuming_leaves_later_slots_alone() {
        let mut scheduler = two_meals();
        armed_at(&mut scheduler, wall(14, 7, 59));
        scheduler.next_due(wall(14, 8, 0), true);

        assert_eq!(
            scheduler.next_due(wall(14, 19, 0), false),
            Due::Feed {
                minute_of_day: 1140,
                portions: 3
            }
        );
    }

    #[test]
    fn a_unit_paused_across_a_whole_day_feeds_nothing() {
        let mut scheduler = two_meals();
        armed_at(&mut scheduler, wall(14, 7, 0));

        for hour in 8..24 {
            match scheduler.next_due(wall(14, hour, 0), true) {
                Due::Feed { .. } => panic!("fed while paused at {hour}:00"),
                _ => continue,
            }
        }
    }

    // ---- changing the schedule ----

    #[test]
    fn removing_an_earlier_slot_does_not_cost_the_later_one() {
        // The consumed marker is a time of day, not an index. With an index,
        // dropping the 07:00 slot would shift 19:00 down onto the index already
        // marked consumed, and the evening meal would silently vanish.
        let mut scheduler = Scheduler::new();
        scheduler.set_schedule(
            Schedule::parse(
                br#"[{"time":"07:00","portions":1},{"time":"08:00","portions":2},{"time":"19:00","portions":3}]"#,
            )
            .unwrap(),
        );
        armed_at(&mut scheduler, wall(14, 6, 0));
        scheduler.next_due(wall(14, 8, 0), false);

        scheduler.set_schedule(
            Schedule::parse(br#"[{"time":"08:00","portions":2},{"time":"19:00","portions":3}]"#)
                .unwrap(),
        );

        assert_eq!(
            scheduler.next_due(wall(14, 19, 0), false),
            Due::Feed {
                minute_of_day: 1140,
                portions: 3
            }
        );
    }

    #[test]
    fn a_slot_inserted_earlier_in_the_day_is_not_fed_retroactively() {
        let mut scheduler = two_meals();
        armed_at(&mut scheduler, wall(14, 7, 59));
        scheduler.next_due(wall(14, 8, 0), false);

        // Home Assistant adds a midday meal while the day is already past it.
        scheduler.set_schedule(
            Schedule::parse(
                br#"[{"time":"08:00","portions":2},{"time":"12:00","portions":1},{"time":"19:00","portions":3}]"#,
            )
            .unwrap(),
        );

        assert_eq!(
            scheduler.next_due(wall(14, 13, 0), false),
            Due::Consumed {
                minute_of_day: 720,
                why: Skipped::TooLate { by_s: 3600 }
            }
        );
    }

    /// Runs a whole day the way the task does, one call a second, and returns
    /// every feed it produced as `(minute_of_day, portions)`.
    fn run_a_day(scheduler: &mut Scheduler, day: u8, paused: bool) -> std::vec::Vec<(u16, u8)> {
        let mut fed = std::vec::Vec::new();
        for second in 0..DAY_S {
            let now = Wall {
                date: date(2026, 9, day),
                second_of_day: second,
                offset_minutes: None,
            };
            if let Due::Feed {
                minute_of_day,
                portions,
            } = scheduler.next_due(now, paused)
            {
                fed.push((minute_of_day, portions));
            }
        }
        fed
    }

    #[test]
    fn five_meals_a_day_all_fire_exactly_once() {
        // The schedule is allowed up to MAX_SLOTS entries and the usual case is
        // two, but three, four or five are perfectly ordinary. Resolving only
        // the latest outstanding slot per call must not cost any of them.
        let mut scheduler = Scheduler::new();
        scheduler.set_schedule(
            Schedule::parse(
                br#"[{"time":"07:00","portions":1},
                     {"time":"10:00","portions":2},
                     {"time":"13:00","portions":1},
                     {"time":"16:00","portions":3},
                     {"time":"19:00","portions":2}]"#,
            )
            .unwrap(),
        );

        assert_eq!(
            run_a_day(&mut scheduler, 14, false),
            [(420, 1), (600, 2), (780, 1), (960, 3), (1140, 2)]
        );

        // And the next day does the same, rather than the marker from day one
        // suppressing anything.
        assert_eq!(run_a_day(&mut scheduler, 15, false).len(), 5);
    }

    #[test]
    fn a_full_schedule_is_fed_in_full() {
        // MAX_SLOTS is the documented limit, so walk right up to it.
        let mut payload = std::string::String::from("[");
        for slot in 0..MAX_SLOTS {
            if slot > 0 {
                payload.push(',');
            }
            payload.push_str(&std::format!(
                r#"{{"time":"{:02}:30","portions":1}}"#,
                slot + 8
            ));
        }
        payload.push(']');

        let mut scheduler = Scheduler::new();
        scheduler.set_schedule(Schedule::parse(payload.as_bytes()).unwrap());

        assert_eq!(run_a_day(&mut scheduler, 14, false).len(), MAX_SLOTS);
    }

    #[test]
    fn slots_a_minute_apart_both_fire() {
        // Nothing in the guard assumes meals are hours apart.
        let mut scheduler = Scheduler::new();
        scheduler.set_schedule(
            Schedule::parse(br#"[{"time":"08:00","portions":1},{"time":"08:01","portions":1}]"#)
                .unwrap(),
        );

        assert_eq!(run_a_day(&mut scheduler, 14, false), [(480, 1), (481, 1)]);
    }

    #[test]
    fn a_unit_that_boots_midday_still_feeds_the_rest_of_the_day() {
        // The baseline consumes everything already past, and must consume
        // nothing else. Three meals gone, two still to come.
        let mut scheduler = Scheduler::new();
        scheduler.set_schedule(
            Schedule::parse(
                br#"[{"time":"07:00","portions":1},
                     {"time":"10:00","portions":2},
                     {"time":"13:00","portions":1},
                     {"time":"16:00","portions":3},
                     {"time":"19:00","portions":2}]"#,
            )
            .unwrap(),
        );

        assert_eq!(
            scheduler.next_due(wall(14, 13, 30), false),
            Due::Consumed {
                minute_of_day: 780,
                why: Skipped::Baseline
            }
        );

        let mut fed = std::vec::Vec::new();
        for second in (13 * 3600 + 1801)..DAY_S {
            let now = Wall {
                date: date(2026, 9, 14),
                second_of_day: second,
                offset_minutes: None,
            };
            if let Due::Feed { minute_of_day, .. } = scheduler.next_due(now, false) {
                fed.push(minute_of_day);
            }
        }
        assert_eq!(fed, [960, 1140]);
    }

    #[test]
    fn only_the_latest_outstanding_slot_is_resolved_per_call() {
        // Two slots crossed at once means one missed meal and one resolution,
        // never two feeds queued back to back.
        let mut scheduler = two_meals();
        armed_at(&mut scheduler, wall(14, 7, 0));

        assert_eq!(
            scheduler.next_due(wall(14, 19, 0), false),
            Due::Feed {
                minute_of_day: 1140,
                portions: 3
            }
        );
        assert_eq!(scheduler.next_due(wall(14, 19, 1), false), Due::Nothing);
    }
}
