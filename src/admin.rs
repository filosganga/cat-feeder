//! The admin page a configured unit serves on the house network — v2's
//! point 5. What it shows, what it accepts, and who may use it.
//!
//! Pure logic. `web.rs` moves the bytes; everything here is host-tested,
//! because this is the one part of the firmware reachable by anything on the
//! LAN.
//!
//! ## The rules
//!
//! - **Authenticated, always, with the derived password.** The same
//!   `base32(sha256("<ap_secret>:<id>"))` the setup network uses, the sticker
//!   carries and `dev/ap-password.sh` prints — so a unit is never open, and
//!   there is no reset flow: a reset gesture forgets the network in the
//!   record, which is the root of trust. HTTP Basic, any username.
//! - **Never render a stored secret.** The network form shows the SSID, the
//!   broker and its username; both password boxes are always empty, and
//!   **empty means keep**. A password can be replaced from here, never read.
//!   An open Wi-Fi network — an *empty* password — is therefore set through
//!   setup mode, not here.
//! - **Mutations are POST, and a POST from another site is refused.** A
//!   browser that has logged in sends the credentials again on a form another
//!   page submits to this address, so a POST carrying an `Origin` that is not
//!   this unit is turned away. No `Origin` at all is a non-browser client —
//!   `curl` — which has to present the password itself.
//! - **Feeding, the clock and the calibration act at once, like the knob.**
//!   A feed is one more producer on the feed queue, a clock set here goes in
//!   as a hand-set time exactly as the knob's does, and a calibration saved
//!   here is written to the record and applied at the feeder's next idle
//!   moment — the same path as the knob's `Detent` and `Portion`.
//! - **Saving the schedule does not restart; saving the network does.** The
//!   network stack is built once at boot. A restart would also cost the clock
//!   nothing now — the RTC keeps it — but it does cost ~6 s off the broker.
//!
//! Portions on the page are capped at `MAX_CLICKS`, like the `Meal n`
//! entities. That is a click count, so on a unit with a portion scale above
//! 100% a meal near the cap is clamped to `MAX_CLICKS` clicks when it runs —
//! the same as a large meal from any other source.
//!
//! It is plaintext HTTP on the LAN, like every comparable device. The password
//! crosses the network base64-encoded, which is to say in the clear to anyone
//! who can see the traffic.

use core::fmt::Write as _;

use heapless::String;

use crate::calibrate::{DETENTS, Failure, Progress};
use crate::menu::{Calibration, ClockEdit, Field};
use crate::portions::MAX_CLICKS;
use crate::provisioning::{
    Escaped, FAVICON, FormError, PAGE_LEN, PASSWORD_LEN, Record, field, record_from_form,
};
use crate::schedule::{
    Date, EditError, MAX_SLOTS, Schedule, Slot, SlotChange, SlotEdit, Wall, parse_slot_time,
};
use crate::tz::{Offset, Zone, ZoneError};

// ---------------------------------------------------------------------------
// Who may
// ---------------------------------------------------------------------------

/// Whether an `Authorization` header carries this unit's password.
///
/// `Basic <base64(user:password)>`; the username is ignored. Compared in time
/// independent of where the first difference is, which on a LAN device is
/// cheap insurance rather than a known attack.
pub fn authorized(authorization: &str, password: &str) -> bool {
    let Some(encoded) = authorization
        .strip_prefix("Basic ")
        .or_else(|| authorization.strip_prefix("basic "))
    else {
        return false;
    };
    let mut decoded = [0u8; 96];
    let Some(len) = base64_decode(encoded.trim(), &mut decoded) else {
        return false;
    };
    let Some(colon) = decoded[..len].iter().position(|&b| b == b':') else {
        return false;
    };
    constant_time_eq(&decoded[colon + 1..len], password.as_bytes())
}

/// The challenge that makes a browser ask for the password.
pub const CHALLENGE: &str = "WWW-Authenticate: Basic realm=\"cat-feeder\", charset=\"UTF-8\"\r\n";

/// Whether a POST may be acted on: no `Origin` (not a browser), or an
/// `Origin` naming the host the request was sent to.
pub fn same_origin(origin: &str, host: &str) -> bool {
    if origin.is_empty() {
        return true;
    }
    !host.is_empty() && origin.strip_prefix("http://") == Some(host)
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Standard base64 with padding, into `out`. `None` for anything malformed
/// or too long.
fn base64_decode(input: &str, out: &mut [u8]) -> Option<usize> {
    fn value(c: u8) -> Option<u32> {
        Some(match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        } as u32)
    }

    let bytes = input.as_bytes();
    if bytes.is_empty() || !bytes.len().is_multiple_of(4) {
        return None;
    }
    let mut len = 0;
    for (i, chunk) in bytes.chunks(4).enumerate() {
        let last = i == bytes.len() / 4 - 1;
        let pad = chunk.iter().rev().take_while(|&&c| c == b'=').count();
        if pad > 2 || (pad > 0 && !last) {
            return None;
        }
        let mut word = 0u32;
        for &c in &chunk[..4 - pad] {
            word = word << 6 | value(c)?;
        }
        word <<= 6 * pad as u32;
        let produced = 3 - pad;
        if len + produced > out.len() {
            return None;
        }
        for k in 0..produced {
            out[len + k] = (word >> (16 - 8 * k)) as u8;
        }
        len += produced;
    }
    Some(len)
}

// ---------------------------------------------------------------------------
// What they may send
// ---------------------------------------------------------------------------

/// Why the schedule form was not taken. The schedule is left as it was.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScheduleFormError {
    /// A meal with no time, below one that has one. Meals are numbered by
    /// position, so a gap cannot simply be closed up.
    MissingTime(u8),
    BadTime(u8),
    BadPortions(u8),
}

impl core::fmt::Display for ScheduleFormError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::MissingTime(n) => write!(
                f,
                "Meal {n} needs a time. To stop a meal without renumbering the \
                 ones after it, set its portions to 0."
            ),
            Self::BadTime(n) => write!(f, "Meal {n}'s time is not a time of day."),
            Self::BadPortions(n) => {
                write!(
                    f,
                    "Meal {n}'s portions must be a whole number from 0 to {MAX_CLICKS}."
                )
            }
        }
    }
}

/// The schedule the form describes: `t1`…`t8` as `HH:MM`, `p1`…`p8` as a
/// count, empty portions meaning 0.
///
/// Rows below the last one with a time are dropped, which is how a meal is
/// removed from the end. A row with no time *above* one that has one is an
/// error rather than a gap closed up, because closing it would renumber every
/// meal after it — `Meal 3` in Home Assistant would silently become the old
/// `Meal 4`.
pub fn schedule_from_form(body: &str) -> Result<Schedule, ScheduleFormError> {
    let mut rows: [(Option<String<8>>, Option<String<8>>); MAX_SLOTS] =
        core::array::from_fn(|_| (None, None));
    for (i, row) in rows.iter_mut().enumerate() {
        let n = i as u8 + 1;
        let mut t: String<3> = String::new();
        let mut p: String<3> = String::new();
        let _ = write!(t, "t{n}");
        let _ = write!(p, "p{n}");
        row.0 = named_field(body, &t)
            .map_err(|_| ScheduleFormError::BadTime(n))?
            .filter(|v| !v.trim().is_empty());
        row.1 = named_field(body, &p)
            .map_err(|_| ScheduleFormError::BadPortions(n))?
            .filter(|v| !v.trim().is_empty());
    }

    let used = rows
        .iter()
        .rposition(|(t, _)| t.is_some())
        .map_or(0, |i| i + 1);
    let mut schedule = Schedule::new();
    for (i, (time, portions)) in rows[..used].iter().enumerate() {
        let n = i as u8 + 1;
        let time = time.as_ref().ok_or(ScheduleFormError::MissingTime(n))?;
        let minute = parse_slot_time(time.trim()).map_err(|_| ScheduleFormError::BadTime(n))?;
        let portions = match portions {
            None => 0,
            Some(p) => p
                .trim()
                .parse::<u8>()
                .ok()
                .filter(|p| *p <= MAX_CLICKS)
                .ok_or(ScheduleFormError::BadPortions(n))?,
        };
        let index = i as u8;
        schedule = schedule
            .edited(SlotEdit {
                index,
                change: SlotChange::Time(minute),
            })
            .and_then(|s| {
                s.edited(SlotEdit {
                    index,
                    change: SlotChange::Portions(portions),
                })
            })
            .map_err(|e: EditError| match e {
                EditError::BadValue => ScheduleFormError::BadTime(n),
                _ => ScheduleFormError::MissingTime(n),
            })?;
    }
    Ok(schedule)
}

/// `field` wants a `&'static str` name, for its error; the schedule rows are
/// built at runtime, so they are looked up by hand with the same decoding.
fn named_field(body: &str, name: &str) -> Result<Option<String<8>>, FormError> {
    for pair in body.split('&') {
        if let Some((key, value)) = pair.split_once('=')
            && key == name
        {
            // Reuse the real decoder by handing it a one-field body.
            let mut one: String<64> = String::new();
            one.push_str("v=").map_err(|_| FormError::TooLong("v"))?;
            one.push_str(value).map_err(|_| FormError::TooLong("v"))?;
            return field::<8>(&one, "v");
        }
    }
    Ok(None)
}

/// The record the network form describes, on top of `current`.
///
/// `current` is read from flash at the moment of saving, so the calibration
/// the knob last saved is carried across rather than whatever the unit booted
/// with. A password box left empty keeps the stored password — see the module
/// doc for why an empty password cannot be *set* here.
pub fn network_from_form(body: &str, current: &Record) -> Result<Record, FormError> {
    let mut record = record_from_form(body, Some(current))?;
    if field::<PASSWORD_LEN>(body, "wifi_password")?.is_none_or(|p| p.is_empty()) {
        record.wifi_password = current.wifi_password.clone();
    }
    if field::<PASSWORD_LEN>(body, "mqtt_password")?.is_none_or(|p| p.is_empty()) {
        record.mqtt_password = current.mqtt_password.clone();
    }
    Ok(record)
}

/// How many portions `POST /feed` asks for: 1 to `MAX_CLICKS`. Zero is not a
/// feed, and the cap is the same one every other manual feed meets.
pub fn feed_from_form(body: &str) -> Result<u8, &'static str> {
    let text = field::<8>(body, "portions")
        .ok()
        .flatten()
        .ok_or("How many portions?")?;
    text.trim()
        .parse::<u8>()
        .ok()
        .filter(|n| (1..=MAX_CLICKS).contains(n))
        .ok_or("Portions must be a whole number from 1 to 16.")
}

/// The time `POST /clock` sets: `YYYY-MM-DDTHH:MM`, or with `:SS`, which is
/// what a `datetime-local` input sends. Local wall-clock, no offset — the same
/// as the knob's clock, and for the same reason: the schedule is written in
/// kitchen time. Years are the knob's, which the DS3231 can hold.
pub fn clock_from_form(body: &str) -> Result<Wall, &'static str> {
    const BAD: &str = "That is not a date and time.";
    let text = field::<32>(body, "at").ok().flatten().ok_or(BAD)?;
    let t = text.trim().as_bytes();
    if !(t.len() == 16 || t.len() == 19) || t[4] != b'-' || t[7] != b'-' || t[10] != b'T' {
        return Err(BAD);
    }
    let num = |range: core::ops::Range<usize>| -> Result<u16, &'static str> {
        let part = &t[range];
        if !part.iter().all(u8::is_ascii_digit) {
            return Err(BAD);
        }
        Ok(part.iter().fold(0u16, |n, d| n * 10 + (d - b'0') as u16))
    };
    let date = Date {
        year: num(0..4)?,
        month: num(5..7)? as u8,
        day: num(8..10)? as u8,
    };
    if !date.is_valid() {
        return Err(BAD);
    }
    let (first, last) = ClockEdit::YEARS;
    if !(first..=last).contains(&date.year) {
        return Err("The year must be between 2025 and 2099.");
    }
    let time = core::str::from_utf8(&t[11..]).map_err(|_| BAD)?;
    let minute_of_day = parse_slot_time(time).map_err(|_| BAD)?;
    let second = if t.len() == 19 { num(17..19)? } else { 0 };
    Ok(Wall {
        date,
        second_of_day: minute_of_day as u32 * 60 + second as u32,
        offset_minutes: None,
    })
}

/// What `POST /clock` asks for: a time, a timezone, or both. Either may be
/// absent — the time box empty, or a browser without the script that fills
/// the timezone list — and an absent one is left as it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClockForm {
    pub at: Option<Wall>,
    pub zone: Option<Zone>,
}

pub fn clock_form(body: &str) -> Result<ClockForm, &'static str> {
    let present = |name| {
        field::<64>(body, name)
            .ok()
            .flatten()
            .filter(|v| !v.trim().is_empty())
    };
    let at = match present("at") {
        Some(_) => Some(clock_from_form(body)?),
        None => None,
    };
    let zone = match present("zone") {
        None => None,
        Some(name) => {
            let rule = present("rule").ok_or("That timezone came with no rule.")?;
            Some(Zone::new(&name, &rule).map_err(|e| match e {
                ZoneError::BadName | ZoneError::TooLong => "That is not a timezone name.",
                ZoneError::BadRule(_) => "The timezone rule is not one this feeder can follow.",
            })?)
        }
    };
    Ok(ClockForm { at, zone })
}

/// The calibration `POST /calibration` asks for, on top of `current`: either
/// figure may be absent, which keeps it. Held to the knob's own ranges and
/// steps, so the page cannot store anything the knob could not.
pub fn calibration_from_form(
    body: &str,
    current: Calibration,
) -> Result<Calibration, &'static str> {
    let read = |name: &'static str, field: Field, message: &'static str| {
        match crate::provisioning::field::<8>(body, name).ok().flatten() {
            None => Ok(current.get(field)),
            Some(text) => {
                let (min, max, step) = field.range();
                text.trim()
                    .parse::<u16>()
                    .ok()
                    .filter(|v| (min..=max).contains(v) && (v - min) % step == 0)
                    .ok_or(message)
            }
        }
    };
    Ok(Calibration {
        detent_ms: read(
            "detent_ms",
            Field::Detent,
            "The detent interval must be 200 to 5000 ms, in steps of 10.",
        )?,
        portion_scale_pct: read(
            "portion_scale",
            Field::PortionScale,
            "The portion scale must be 25 to 300 %, in steps of 5.",
        )?,
    })
}

// ---------------------------------------------------------------------------
// What they see
// ---------------------------------------------------------------------------

/// The unit as the page describes it. No secrets: there is no field for one.
#[derive(Debug, Clone, Copy)]
pub struct Status<'a> {
    pub id: &'a str,
    pub version: &'a str,
    /// The time now, only while the clock is trusted.
    pub now: Option<Wall>,
    pub paused: bool,
    pub jammed: bool,
    pub next: Option<Slot>,
    pub last_fed: Option<(Wall, u8)>,
    /// The calibration in force.
    pub calibration: Calibration,
    /// The latest calibration run, whoever started it.
    pub progress: Progress,
    /// The timezone followed when nobody publishes the time.
    pub zone: Option<&'a Zone>,
}

/// The network as it is configured, less its two passwords.
#[derive(Debug, Clone, Copy)]
pub struct Network<'a> {
    pub wifi_ssid: &'a str,
    pub mqtt_host: &'a str,
    pub mqtt_port: u16,
    pub mqtt_user: &'a str,
}

/// A line above the forms after a POST.
#[derive(Debug, Clone, Copy)]
pub enum Notice<'a> {
    Done(&'a str),
    Problem(&'a str),
}

const STYLE: &str = "body{font:16px/1.5 system-ui,sans-serif;margin:0 auto;padding:1.5rem;\
max-width:30rem;background:#faf9f7;color:#222}\
h1{font-size:1.25rem;margin:0 0 .25rem}h2{font-size:1.05rem;margin:1.75rem 0 .5rem}\
dl{display:grid;grid-template-columns:auto 1fr;gap:.1rem 1rem;margin:0}dt{color:#666}dd{margin:0}\
table{border-collapse:collapse}td{padding:.2rem .4rem .2rem 0}\
label{display:block;margin:.6rem 0 .15rem;font-weight:600;font-size:.9rem}\
input{box-sizing:border-box;padding:.45rem;font-size:1rem;border:1px solid #bbb;\
border-radius:.3rem;background:#fff}.wide{width:100%}input[type=number]{width:5rem}\
button{margin-top:1rem;padding:.6rem 1.2rem;font-size:1rem;border:0;border-radius:.3rem;\
background:#2f6f4f;color:#fff}\
.ok{background:#e8f3ec;border:1px solid #2f6f4f}.err{background:#fdecea;border:1px solid #d9534f}\
.ok,.err{border-radius:.3rem;padding:.6rem;margin:1rem 0}.hint{color:#666;font-size:.85rem;margin:.3rem 0}";

/// The whole page: status, the schedule form and the network form.
///
/// `schedule` is what the unit holds, `None` if it was never given one.
/// `submitted` is a rejected network form's body, so the non-secret fields
/// come back as they were typed; `""` otherwise.
pub fn render_page(
    page: &mut String<PAGE_LEN>,
    status: &Status,
    schedule: Option<&Schedule>,
    network: &Network,
    submitted: &str,
    notice: Option<Notice>,
) {
    page.clear();
    let _ = write!(
        page,
        "<!doctype html><html lang=en><head><meta charset=utf-8>\
         <meta name=viewport content=\"width=device-width,initial-scale=1\">\
         <title>cat-feeder {id}</title>{FAVICON}<style>{STYLE}</style>",
        id = Escaped(status.id),
    );
    // A running calibration redraws itself, so its clicks can be watched —
    // from `/`, never from the address it was drawn at. After *Run
    // calibration* that address is `/calibrate`, which only answers a POST,
    // so a plain refresh landed on "Not here" while the motor was turning.
    if matches!(status.progress, Progress::Running { .. }) {
        let _ = page.push_str("<meta http-equiv=refresh content=\"2;url=/\">");
    }
    let _ = write!(
        page,
        "</head><body><h1>cat-feeder {id}</h1><p class=hint>firmware {version}</p>",
        id = Escaped(status.id),
        version = Escaped(status.version),
    );

    match notice {
        Some(Notice::Done(text)) => {
            let _ = write!(page, "<p class=ok>{}</p>", Escaped(text));
        }
        Some(Notice::Problem(text)) => {
            let _ = write!(page, "<p class=err>{}</p>", Escaped(text));
        }
        None => {}
    }

    render_status(page, status, schedule);
    render_feed(page);
    render_schedule(page, schedule);
    render_clock(page, status.zone);
    render_calibration(page, status.calibration, status.progress);
    render_network(page, network, submitted);
    let _ = page.push_str("</body></html>");
}

fn render_status(page: &mut String<PAGE_LEN>, status: &Status, schedule: Option<&Schedule>) {
    let _ = page.push_str("<dl><dt>Clock</dt><dd>");
    match status.now {
        Some(now) => {
            let _ = write!(page, "{}", Clock(now));
            if let Some(offset) = now.offset_minutes {
                let _ = write!(page, " {}", Offset(offset));
            }
        }
        None => {
            let _ = page.push_str("not set — the schedule is holding");
        }
    }
    let meals = schedule.map_or(0, Schedule::meals);
    let _ = page.push_str("</dd><dt>Timezone</dt><dd>");
    match status.zone {
        Some(zone) => {
            let _ = write!(page, "{}", Escaped(&zone.name));
        }
        None => {
            let _ = page.push_str("not set — no summer time of its own");
        }
    }
    let _ = write!(page, "</dd><dt>Meals</dt><dd>{meals} a day");
    if status.paused {
        let _ = page.push_str(", <b>paused</b>");
    }
    let _ = page.push_str("</dd><dt>Next</dt><dd>");
    match status
        .next
        .filter(|_| !status.paused && status.now.is_some())
    {
        Some(slot) => {
            let _ = write!(
                page,
                "{:02}:{:02}, {} portions",
                slot.minute_of_day / 60,
                slot.minute_of_day % 60,
                slot.portions
            );
        }
        None => {
            let _ = page.push_str("—");
        }
    }
    let _ = page.push_str("</dd><dt>Last fed</dt><dd>");
    match status.last_fed {
        Some((at, portions)) => {
            let _ = write!(page, "{}, {portions} portions", Clock(at));
        }
        None => {
            let _ = page.push_str("not since power-on");
        }
    }
    let _ = page.push_str("</dd>");
    if status.jammed {
        let _ = page.push_str("<dt>Mechanism</dt><dd><b>jammed</b></dd>");
    }
    let _ = page.push_str("</dl>");
}

fn render_schedule(page: &mut String<PAGE_LEN>, schedule: Option<&Schedule>) {
    let _ = write!(
        page,
        "<h2>Meals</h2><form method=post action=/schedule><table>\
         <tr><td></td><td>time</td><td>portions</td></tr>"
    );
    let slots = schedule.map_or(&[][..], Schedule::slots);
    for i in 0..MAX_SLOTS {
        let n = i + 1;
        let _ = write!(page, "<tr><td>Meal {n}</td><td><input type=time name=t{n}");
        if let Some(slot) = slots.get(i) {
            let _ = write!(
                page,
                " value={:02}:{:02}></td><td><input type=number name=p{n} min=0 max={MAX_CLICKS} value={}>",
                slot.minute_of_day / 60,
                slot.minute_of_day % 60,
                slot.portions
            );
        } else {
            let _ = write!(
                page,
                "></td><td><input type=number name=p{n} min=0 max={MAX_CLICKS}>"
            );
        }
        let _ = page.push_str("</td></tr>");
    }
    let _ = page.push_str(
        "</table><p class=hint>0 portions keeps a meal in its place but stops it \
         feeding. Clear the times at the bottom to remove meals.</p>\
         <button type=submit>Save meals</button></form>",
    );
}

fn render_feed(page: &mut String<PAGE_LEN>) {
    let _ = write!(
        page,
        "<h2>Feed</h2><form method=post action=/feed>\
         <input type=number name=portions min=1 max={MAX_CLICKS} value=1> \
         <button type=submit>Feed now</button></form>"
    );
}

/// `onclick` fills the box with the browser's own clock, seconds included,
/// and sends it — the quick way, since the phone in your hand has the time.
const THIS_DEVICE: &str = "var d=new Date(),p=function(n){return(n<10?'0':'')+n};\
this.form.at.value=d.getFullYear()+'-'+p(d.getMonth()+1)+'-'+p(d.getDate())+'T'+\
p(d.getHours())+':'+p(d.getMinutes())+':'+p(d.getSeconds());this.form.submit()";

/// The timezone list and its rule, worked out in the browser from the tz
/// data it carries — see `tz.rs` for why the unit never does this itself.
///
/// For each zone: scan this year a day at a time for a change of offset,
/// narrow each change to the minute, and write the two changes as a POSIX
/// rule, `<+01>-1<+02>,M3.5.0,M10.5.0/3`. A zone with no change is one offset;
/// one with more than two a year — Ramadan in Morocco — is written as the
/// offset now, and Home Assistant's live time corrects it when there is one.
/// Checked in node against every zone a browser lists: all 418 parse in
/// `tz::Rule::parse` and agree with the browser at 24 instants of the year.
const RULE_JS: &str = r#"function rule(z){var F=new Intl.DateTimeFormat('en-US',{timeZone:z,hourCycle:'h23',year:'numeric',month:'numeric',day:'numeric',hour:'numeric',minute:'numeric'});function off(t){var g={};F.formatToParts(t).forEach(function(p){g[p.type]=+p.value});return Math.round((Date.UTC(g.year,g.month-1,g.day,g.hour%24,g.minute)-t)/6e4)}function two(n){return(n<10?'0':'')+n}function hm(m){var a=Math.abs(m);return(m<0?'-':'')+Math.floor(a/60)+(a%60?':'+two(a%60):'')}function nm(m){var a=Math.abs(m);return'<'+(m<0?'-':'+')+two(Math.floor(a/60))+(a%60?two(a%60):'')+'>'}function at(t,o){var d=new Date(t+o*6e4),y=d.getUTCFullYear(),mo=d.getUTCMonth(),dd=d.getUTCDate(),n=new Date(Date.UTC(y,mo+1,0)).getUTCDate(),w=dd+7>n?5:Math.ceil(dd/7),m=d.getUTCHours()*60+d.getUTCMinutes();return',M'+(mo+1)+'.'+w+'.'+d.getUTCDay()+(m==120?'':'/'+hm(m))}var D=864e5,y=new Date().getFullYear(),t=Date.UTC(y,0,1),e=Date.UTC(y+1,0,1),o=off(t),c=[];for(;t<e;t+=D){var n=off(t+D);if(n!=o){var a=t,b=t+D;while(b-a>6e4){var m=a+Math.floor((b-a)/12e4)*6e4;if(off(m)==o)a=m;else b=m}c.push([b,o,n]);o=n}}if(c.length!=2)return nm(o)+hm(-o);var lo=Math.min(c[0][1],c[0][2]),hi=Math.max(c[0][1],c[0][2]),s=c[0][2]==hi?c[0]:c[1],f=c[0][2]==hi?c[1]:c[0];return nm(lo)+hm(-lo)+nm(hi)+(hi-lo==60?'':hm(-hi))+at(s[0],s[1])+at(f[0],f[1])}"#;

/// Fills the list, preselects the stored zone or else the browser's own, and
/// keeps the rule box following the choice. When the browser's rule for the
/// stored zone differs from the stored one — the law changed — it says so,
/// and *Set* saves the browser's.
const ZONE_JS: &str = r#"(function(){var f=document.getElementById('clk'),s=f.zone,r=f.rule,h=document.getElementById('tzh'),cur=s.dataset.cur,me=Intl.DateTimeFormat().resolvedOptions().timeZone,w=cur||me,zs=Intl.supportedValuesOf?Intl.supportedValuesOf('timeZone'):[me];
if(zs.indexOf(w)<0)zs=zs.concat(w);
zs.forEach(function(z){s.add(new Option(z,z,false,z==w))});
function fill(){var v=rule(s.value);h.textContent=!cur?"No timezone yet: Set saves "+s.value+".":s.value==cur&&r.value!=v?"This browser has a newer rule for this zone: Set saves it.":"";r.value=v}
s.onchange=fill;fill()})();"#;

fn render_clock(page: &mut String<PAGE_LEN>, zone: Option<&Zone>) {
    let (name, rule) = zone.map_or(("", ""), |z| (z.name.as_str(), z.rule.as_str()));
    let _ = write!(
        page,
        "<h2>Clock</h2><form id=clk method=post action=/clock>\
         <input type=datetime-local step=1 name=at> \
         <button type=button onclick=\"{THIS_DEVICE}\">Use this device's time</button>\
         <label for=zone>Timezone</label><select class=wide id=zone name=zone data-cur=\"{}\"></select>\
         <p class=hint id=tzh></p>\
         <details><summary>Advanced</summary><label for=rule>Rule, POSIX <code>TZ</code> format</label>\
         <input class=wide id=rule name=rule autocapitalize=off autocorrect=off spellcheck=false value=\"{}\"></details>\
         <button type=submit>Set</button></form>\
         <p class=hint>Leave the time empty to change only the timezone. Local time, as \
         on the kitchen wall. The timezone moves the clock for summer time when \
         nobody tells it the time; Home Assistant's time, when it arrives, still \
         has the last word.</p><script>{RULE_JS}{ZONE_JS}</script>",
        Escaped(name),
        Escaped(rule)
    );
}

fn render_calibration(page: &mut String<PAGE_LEN>, calibration: Calibration, progress: Progress) {
    let (dmin, dmax, dstep) = Field::Detent.range();
    let (smin, smax, sstep) = Field::PortionScale.range();
    let _ = write!(
        page,
        "<h2>Calibration</h2><form method=post action=/calibration>\
         <label for=detent_ms>Detent interval, ms</label>\
         <input id=detent_ms type=number name=detent_ms min={dmin} max={dmax} step={dstep} value={}>\
         <label for=portion_scale>Portion scale, %</label>\
         <input id=portion_scale type=number name=portion_scale min={smin} max={smax} step={sstep} value={}>\
         <br><button type=submit>Save calibration</button></form>",
        calibration.detent_ms, calibration.portion_scale_pct
    );

    match progress {
        Progress::None => {}
        Progress::Running { clicks } => {
            let _ = write!(
                page,
                "<p class=ok>Running: click {clicks} of {DETENTS}.</p>"
            );
        }
        Progress::Finished(Ok(m)) => {
            let _ = write!(
                page,
                "<p>Last run measured <b>{} ms</b> (gaps {}–{} ms).</p>",
                m.detent_ms, m.fastest_ms, m.slowest_ms
            );
            if m.detent_ms != calibration.detent_ms {
                let _ = write!(
                    page,
                    "<form method=post action=/calibration>\
                     <input type=hidden name=detent_ms value={}>\
                     <button type=submit>Save {} ms</button></form>",
                    m.detent_ms, m.detent_ms
                );
            }
        }
        Progress::Finished(Err(failure)) => {
            let _ = write!(page, "<p class=err>Last run: {}</p>", FailureText(failure));
        }
    }

    let _ = write!(
        page,
        "<form method=post action=/calibrate onsubmit=\"return confirm('This turns \
         {DETENTS} portions into the bowl. Measure with a full hopper.')\">\
         <button type=submit>Run calibration</button></form>\
         <p class=hint>Turns {DETENTS} detents and times them, to measure this \
         mechanism. It dispenses food: put a bowl under it, with the hopper full, \
         which is the slowest the feeder runs.</p>"
    );
}

/// A failed run as the page says it.
struct FailureText(Failure);

impl core::fmt::Display for FailureText {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self.0 {
            Failure::Jammed => f.write_str("no click for too long, so it stopped. Is it jammed?"),
            Failure::Inconsistent {
                fastest_ms,
                slowest_ms,
            } => write!(
                f,
                "the gaps disagreed ({fastest_ms}–{slowest_ms} ms), so a click was \
                 missed or doubled. Nothing to save; run it again."
            ),
            Failure::TooFast { slowest_ms } => write!(
                f,
                "{slowest_ms} ms between clicks is switch bounce, not a detent."
            ),
            Failure::TooSlow { slowest_ms } => write!(
                f,
                "{slowest_ms} ms between clicks is slower than can be stored; \
                 the mechanism is nearly stalled."
            ),
        }
    }
}

fn render_network(page: &mut String<PAGE_LEN>, network: &Network, submitted: &str) {
    let _ = page.push_str(
        "<h2>Network</h2><p class=hint>Saving restarts the feeder. A wrong \
         network or broker takes it offline until it is set up again: hold the \
         button through power-on for setup mode.</p>\
         <form method=post action=/network>",
    );
    let mut port: String<5> = String::new();
    let _ = write!(port, "{}", network.mqtt_port);

    input(
        page,
        "wifi_ssid",
        "Wi-Fi network",
        "text",
        network.wifi_ssid,
        submitted,
    );
    input(page, "wifi_password", "Wi-Fi password", "password", "", "");
    input(
        page,
        "mqtt_host",
        "Broker address",
        "text",
        network.mqtt_host,
        submitted,
    );
    input(page, "mqtt_port", "Broker port", "text", &port, submitted);
    input(
        page,
        "mqtt_user",
        "Broker username",
        "text",
        network.mqtt_user,
        submitted,
    );
    input(page, "mqtt_password", "Broker password", "password", "", "");
    let _ = page.push_str(
        "<p class=hint>Leave a password empty to keep the one stored. Stored \
         passwords are never shown.</p><button type=submit>Save and restart</button></form>",
    );
}

/// One labelled input: what was just typed if the form came back, otherwise
/// `current`. A password is always passed `""` for both.
fn input(
    page: &mut String<PAGE_LEN>,
    name: &'static str,
    label: &str,
    kind: &str,
    current: &str,
    submitted: &str,
) {
    let typed = field::<PASSWORD_LEN>(submitted, name).ok().flatten();
    let value = typed.as_deref().unwrap_or(current);
    let _ = write!(
        page,
        "<label for={name}>{label}</label><input class=wide id={name} name={name} \
         type={kind} autocapitalize=off autocorrect=off spellcheck=false \
         autocomplete={} value=\"{}\">",
        if kind == "password" {
            "new-password"
        } else {
            "off"
        },
        Escaped(value)
    );
}

/// `2026-09-28 08:00`, the wall clock without the offset or seconds.
struct Clock(Wall);

impl core::fmt::Display for Clock {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let d = self.0.date;
        let s = self.0.second_of_day;
        write!(
            f,
            "{:04}-{:02}-{:02} {:02}:{:02}",
            d.year,
            d.month,
            d.day,
            s / 3600,
            s / 60 % 60
        )
    }
}

/// The page shown once, on the way into a restart.
pub fn render_restarting(page: &mut String<PAGE_LEN>, record: &Record) {
    page.clear();
    let _ = write!(
        page,
        "<!doctype html><html lang=en><head><meta charset=utf-8>\
         <meta name=viewport content=\"width=device-width,initial-scale=1\">\
         <title>cat-feeder</title>{FAVICON}<style>{STYLE}</style></head><body><h1>Saved</h1>\
         <p>This feeder is restarting and will join <b>{}</b>, then connect to \
         the broker at <b>{}:{}</b>.</p><p>Its address may change. If it does \
         not come back, hold the button through a power cycle to set it up \
         again.</p></body></html>",
        Escaped(&record.wifi_ssid),
        Escaped(&record.mqtt_host),
        record.mqtt_port,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schedule::Date;

    fn basic(user_pass: &str) -> std::string::String {
        // A tiny encoder for the tests, independent of the decoder above.
        const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let b = user_pass.as_bytes();
        let mut out = std::string::String::from("Basic ");
        for c in b.chunks(3) {
            let w = (c[0] as u32) << 16
                | (*c.get(1).unwrap_or(&0) as u32) << 8
                | *c.get(2).unwrap_or(&0) as u32;
            for k in 0..4 {
                if k <= c.len() {
                    out.push(T[(w >> (18 - 6 * k) & 63) as usize] as char);
                } else {
                    out.push('=');
                }
            }
        }
        out
    }

    const PW: &str = "H75T-C7VT-6FAV";

    #[test]
    fn the_derived_password_opens_it_with_any_username() {
        assert!(authorized(&basic("admin:H75T-C7VT-6FAV"), PW));
        assert!(authorized(&basic(":H75T-C7VT-6FAV"), PW));
        assert!(authorized(&basic("someone:H75T-C7VT-6FAV"), PW));
        // RFC 7617: the scheme name is case-insensitive in practice.
        assert!(authorized(
            &basic("a:H75T-C7VT-6FAV").replacen("Basic", "basic", 1),
            PW
        ));
    }

    #[test]
    fn anything_else_is_refused() {
        for header in [
            std::string::String::new(),
            basic("admin:H75T-C7VT-6FA"),
            basic("admin:H75T-C7VT-6FAVX"),
            basic("admin:h75t-c7vt-6fav"),
            basic("H75T-C7VT-6FAV"),
            "Basic !!!!".into(),
            "Basic YWRtaW4".into(),
            "Bearer H75T-C7VT-6FAV".into(),
            basic("admin:H75T-C7VT-6FAV").replacen("Basic ", "", 1),
        ] {
            assert!(!authorized(&header, PW), "{header}");
        }
    }

    /// A password containing a colon still works: only the first colon
    /// separates the username.
    #[test]
    fn a_colon_in_the_password_is_part_of_it() {
        assert!(authorized(&basic("u:a:b"), "a:b"));
    }

    #[test]
    fn base64_decodes_every_padding() {
        let mut out = [0u8; 16];
        for (text, plain) in [
            ("YQ==", "a"),
            ("YWI=", "ab"),
            ("YWJj", "abc"),
            ("YWJjZA==", "abcd"),
        ] {
            let n = base64_decode(text, &mut out).unwrap();
            assert_eq!(&out[..n], plain.as_bytes(), "{text}");
        }
        for bad in ["", "Y", "YQ=", "Y===", "YQ==YQ==", "YW=j"] {
            assert_eq!(base64_decode(bad, &mut out), None, "{bad}");
        }
        assert_eq!(base64_decode("YWJjZA==", &mut [0u8; 3]), None);
    }

    #[test]
    fn a_post_from_another_site_is_refused() {
        assert!(same_origin("http://192.168.68.60", "192.168.68.60"));
        assert!(same_origin("", "192.168.68.60"));
        assert!(same_origin("", ""));
        assert!(!same_origin("http://evil.example", "192.168.68.60"));
        assert!(!same_origin("https://192.168.68.60", "192.168.68.60"));
        assert!(!same_origin("null", "192.168.68.60"));
        assert!(!same_origin("http://192.168.68.60", ""));
        assert!(!same_origin("http://192.168.68.600", "192.168.68.60"));
    }

    // ---- the schedule form ----

    fn json(s: &Schedule) -> std::string::String {
        s.to_json().as_str().into()
    }

    #[test]
    fn the_form_becomes_the_schedule_in_its_rows_order() {
        let s = schedule_from_form("t1=08%3A00&p1=2&t2=19%3A00&p2=3&t3=&p3=&t4=&p4=").unwrap();
        assert_eq!(
            json(&s),
            r#"[{"time":"08:00","portions":2},{"time":"19:00","portions":3}]"#
        );
    }

    #[test]
    fn empty_portions_are_a_meal_switched_off() {
        let s = schedule_from_form("t1=08%3A00&p1=&t2=12%3A00&p2=1").unwrap();
        assert_eq!(s.meals(), 1);
        assert_eq!(s.slots()[0].portions, 0);
    }

    #[test]
    fn an_empty_form_is_no_meals() {
        assert_eq!(schedule_from_form(""), Ok(Schedule::new()));
        assert_eq!(schedule_from_form("t1=&p1=3"), Ok(Schedule::new()));
    }

    #[test]
    fn a_gap_is_refused_rather_than_renumbering() {
        assert_eq!(
            schedule_from_form("t1=08%3A00&p1=2&t2=&p2=&t3=19%3A00&p3=2"),
            Err(ScheduleFormError::MissingTime(2))
        );
    }

    #[test]
    fn bad_rows_are_named() {
        assert_eq!(
            schedule_from_form("t1=8am&p1=2"),
            Err(ScheduleFormError::BadTime(1))
        );
        assert_eq!(
            schedule_from_form("t1=08%3A00&p1=2&t2=09%3A00&p2=17"),
            Err(ScheduleFormError::BadPortions(2))
        );
        assert_eq!(
            schedule_from_form("t1=08%3A00&p1=-1"),
            Err(ScheduleFormError::BadPortions(1))
        );
    }

    #[test]
    fn seconds_from_a_browser_are_dropped() {
        let s = schedule_from_form("t1=08%3A00%3A00&p1=1").unwrap();
        assert_eq!(s.slots()[0].minute_of_day, 480);
    }

    // ---- the network form ----

    fn current() -> Record {
        Record {
            wifi_ssid: "home".try_into().unwrap(),
            wifi_password: "wifi-secret".try_into().unwrap(),
            mqtt_host: "192.168.68.105".try_into().unwrap(),
            mqtt_port: 1883,
            mqtt_user: "feeder".try_into().unwrap(),
            mqtt_password: "mqtt-secret".try_into().unwrap(),
            detent_ms: 2120,
            portion_scale_pct: 133,
        }
    }

    const FORM: &str = "wifi_ssid=home2&wifi_password=&mqtt_host=192.168.68.126\
                        &mqtt_port=1883&mqtt_user=cat-feeder&mqtt_password=";

    #[test]
    fn empty_password_boxes_keep_what_is_stored() {
        let r = network_from_form(FORM, &current()).unwrap();
        assert_eq!(r.wifi_ssid, "home2");
        assert_eq!(r.mqtt_host, "192.168.68.126");
        assert_eq!(r.mqtt_user, "cat-feeder");
        assert_eq!(r.wifi_password, "wifi-secret");
        assert_eq!(r.mqtt_password, "mqtt-secret");
    }

    #[test]
    fn a_typed_password_replaces_it() {
        let body = FORM
            .replace("wifi_password=", "wifi_password=new+one")
            .replace("mqtt_password=", "mqtt_password=p%26q");
        let r = network_from_form(&body, &current()).unwrap();
        assert_eq!(r.wifi_password, "new one");
        assert_eq!(r.mqtt_password, "p&q");
    }

    #[test]
    fn the_calibration_is_carried_across() {
        let r = network_from_form(FORM, &current()).unwrap();
        assert_eq!((r.detent_ms, r.portion_scale_pct), (2120, 133));
    }

    #[test]
    fn the_setup_forms_rules_still_apply() {
        let body = FORM.replace("192.168.68.126", "broker.lan");
        assert_eq!(
            network_from_form(&body, &current()),
            Err(FormError::NotAnIp("mqtt_host"))
        );
    }

    // ---- feed, clock, calibration ----

    #[test]
    fn a_feed_is_one_to_the_cap() {
        assert_eq!(feed_from_form("portions=1"), Ok(1));
        assert_eq!(feed_from_form("portions=16"), Ok(MAX_CLICKS));
        for bad in ["portions=0", "portions=17", "portions=", "portions=two", ""] {
            assert!(feed_from_form(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn the_clock_reads_what_a_browser_sends() {
        let wall = clock_from_form("at=2026-09-28T08%3A05%3A42").unwrap();
        assert_eq!(
            (wall.date.year, wall.date.month, wall.date.day),
            (2026, 9, 28)
        );
        assert_eq!(wall.second_of_day, 8 * 3600 + 5 * 60 + 42);
        assert_eq!(wall.offset_minutes, None);

        let no_seconds = clock_from_form("at=2026-09-28T08%3A05").unwrap();
        assert_eq!(no_seconds.second_of_day, 8 * 3600 + 5 * 60);
    }

    #[test]
    fn a_clock_that_cannot_be_is_refused() {
        for bad in [
            "",
            "at=",
            "at=2026-02-30T08%3A00",
            "at=2026-13-01T08%3A00",
            "at=2026-09-28T24%3A00",
            "at=2026-09-28T08%3A00%3A60",
            "at=2026-09-28 08%3A00",
            "at=2024-09-28T08%3A00",
            "at=2100-01-01T00%3A00",
            "at=26-09-28T08%3A00",
        ] {
            assert!(clock_from_form(bad).is_err(), "{bad}");
        }
    }

    fn calibration() -> Calibration {
        Calibration {
            portion_scale_pct: 100,
            detent_ms: 1_900,
        }
    }

    #[test]
    fn calibration_keeps_what_the_form_leaves_out() {
        assert_eq!(
            calibration_from_form("detent_ms=2140", calibration()),
            Ok(Calibration {
                portion_scale_pct: 100,
                detent_ms: 2_140
            })
        );
        assert_eq!(
            calibration_from_form("portion_scale=135", calibration()),
            Ok(Calibration {
                portion_scale_pct: 135,
                detent_ms: 1_900
            })
        );
    }

    /// Only what the knob could have stored.
    #[test]
    fn calibration_is_held_to_the_knobs_ranges_and_steps() {
        for bad in [
            "detent_ms=190",
            "detent_ms=5010",
            "detent_ms=2145",
            "detent_ms=",
            "portion_scale=20",
            "portion_scale=305",
            "portion_scale=101",
        ] {
            assert!(calibration_from_form(bad, calibration()).is_err(), "{bad}");
        }
        assert!(calibration_from_form("detent_ms=200&portion_scale=300", calibration()).is_ok());
    }

    /// The messages spell the limits out in words; these hold them to the
    /// constants, so a changed limit fails here rather than on the page.
    #[test]
    fn the_messages_state_the_real_limits() {
        let feed = feed_from_form("portions=0").unwrap_err();
        assert!(feed.contains(&std::format!("1 to {MAX_CLICKS}")));

        let (min, max, step) = Field::Detent.range();
        let detent = calibration_from_form("detent_ms=1", calibration()).unwrap_err();
        assert!(detent.contains(&std::format!("{min} to {max} ms, in steps of {step}")));

        let (min, max, step) = Field::PortionScale.range();
        let scale = calibration_from_form("portion_scale=1", calibration()).unwrap_err();
        assert!(scale.contains(&std::format!("{min} to {max} %, in steps of {step}")));

        let (first, last) = ClockEdit::YEARS;
        let year = clock_from_form("at=2000-01-01T00%3A00").unwrap_err();
        assert!(year.contains(&std::format!("between {first} and {last}")));
    }

    #[test]
    fn the_page_offers_the_last_run_only_when_it_differs() {
        use crate::calibrate::Measurement;
        let run = |detent_ms| {
            Progress::Finished(Ok(Measurement {
                detent_ms,
                fastest_ms: 2_050,
                slowest_ms: 2_117,
            }))
        };
        let mut page = String::new();
        let differs = Status {
            progress: run(2_120),
            ..status()
        };
        render_page(&mut page, &differs, None, &network(), "", None);
        assert!(page.contains("Last run measured <b>2120 ms</b> (gaps 2050–2117 ms)"));
        assert!(page.contains("name=detent_ms value=2120><button type=submit>Save 2120 ms"));

        let same = Status {
            progress: run(2_140),
            ..status()
        };
        render_page(&mut page, &same, None, &network(), "", None);
        assert!(!page.contains("Save 2140 ms"));
    }

    #[test]
    fn a_running_calibration_refreshes_itself() {
        let mut page = String::new();
        let running = Status {
            progress: Progress::Running { clicks: 2 },
            ..status()
        };
        render_page(&mut page, &running, None, &network(), "", None);
        assert!(page.contains("<meta http-equiv=refresh content=\"2;url=/\">"));
        assert!(page.contains("click 2 of 5"));

        render_page(&mut page, &status(), None, &network(), "", None);
        assert!(!page.contains("http-equiv=refresh"));
    }

    #[test]
    fn a_failed_run_says_why_and_offers_nothing_to_save() {
        let mut page = String::new();
        let failed = Status {
            progress: Progress::Finished(Err(Failure::Inconsistent {
                fastest_ms: 2_000,
                slowest_ms: 4_000,
            })),
            ..status()
        };
        render_page(&mut page, &failed, None, &network(), "", None);
        assert!(page.contains("the gaps disagreed (2000–4000 ms)"));
        assert!(!page.contains("type=hidden"));
    }

    /// Centred, with its own icon, so the browser never asks for
    /// `/favicon.ico` and spends a connection slot on a 404.
    #[test]
    fn the_page_is_centred_and_brings_its_own_icon() {
        let page = render(None, "", None);
        assert!(page.contains("margin:0 auto"));
        assert!(page.contains("<link rel=icon href=\"data:image/svg+xml,"));
        let mut form = String::new();
        crate::provisioning::render_form(&mut form, "", None, None);
        assert!(form.contains("margin:0 auto") && form.contains(FAVICON));
    }

    /// Empty, so saving only a timezone never winds the clock back to the
    /// moment the page was drawn.
    #[test]
    fn the_clock_box_starts_empty() {
        let page = render(None, "", None);
        assert!(page.contains("<input type=datetime-local step=1 name=at>"));
    }

    const ROME: &str = "<+01>-1<+02>,M3.5.0,M10.5.0/3";

    #[test]
    fn the_clock_form_takes_a_time_a_zone_or_both() {
        let both = clock_form(&std::format!(
            "at=2026-09-28T08%3A05&zone=Europe%2FRome&rule={}",
            ROME.replace('<', "%3C")
                .replace('>', "%3E")
                .replace('+', "%2B")
                .replace(',', "%2C")
                .replace('/', "%2F")
        ))
        .unwrap();
        assert_eq!(both.at.map(|w| w.second_of_day), Some(8 * 3600 + 5 * 60));
        assert_eq!(
            both.zone.as_ref().map(|z| z.name.as_str()),
            Some("Europe/Rome")
        );
        assert_eq!(both.zone.as_ref().map(|z| z.rule.as_str()), Some(ROME));

        let zone_only = clock_form("at=&zone=Asia%2FTokyo&rule=%3C%2B09%3E-9").unwrap();
        assert_eq!(zone_only.at, None);
        assert!(zone_only.zone.is_some());

        // No script, so no list: the zone is left alone.
        let time_only = clock_form("at=2026-09-28T08%3A05").unwrap();
        assert_eq!(time_only.zone, None);
    }

    #[test]
    fn a_zone_it_cannot_follow_is_refused() {
        assert!(clock_form("zone=Europe%2FRome").is_err());
        assert!(clock_form("zone=Europe%2FRome&rule=nonsense").is_err());
        assert!(clock_form("zone=%3Cscript%3E&rule=%3C%2B09%3E-9").is_err());
    }

    #[test]
    fn the_page_shows_the_zone_and_preselects_it() {
        let zone = Zone::new("Europe/Rome", ROME).unwrap();
        let mut page = String::new();
        let with_zone = Status {
            zone: Some(&zone),
            now: Some(Wall {
                offset_minutes: Some(120),
                ..at(7, 5)
            }),
            ..status()
        };
        render_page(&mut page, &with_zone, None, &network(), "", None);
        assert!(page.contains("<dt>Timezone</dt><dd>Europe/Rome</dd>"));
        assert!(page.contains("2026-09-28 07:05 +02:00"));
        assert!(page.contains("data-cur=\"Europe/Rome\""));
        assert!(page.contains("value=\"&lt;+01&gt;-1&lt;+02&gt;,M3.5.0,M10.5.0/3\""));

        let page = render(None, "", None);
        assert!(page.contains("not set — no summer time of its own"));
        assert!(page.contains("data-cur=\"\""));
    }

    /// The script is inline, so nothing in it may end the element early.
    #[test]
    fn the_script_cannot_close_itself() {
        assert!(!RULE_JS.contains("</"));
        assert!(!ZONE_JS.contains("</"));
    }

    // ---- the page ----

    fn at(h: u32, m: u32) -> Wall {
        Wall {
            date: Date {
                year: 2026,
                month: 9,
                day: 28,
            },
            second_of_day: h * 3600 + m * 60,
            offset_minutes: Some(120),
        }
    }

    fn status() -> Status<'static> {
        Status {
            id: "99177c",
            version: "0.1.0",
            now: Some(at(7, 5)),
            paused: false,
            jammed: false,
            next: Some(Slot {
                minute_of_day: 480,
                portions: 2,
            }),
            last_fed: Some((at(19, 0), 2)),
            calibration: Calibration {
                portion_scale_pct: 100,
                detent_ms: 2_140,
            },
            progress: Progress::None,
            zone: None,
        }
    }

    fn network() -> Network<'static> {
        Network {
            wifi_ssid: "home",
            mqtt_host: "192.168.68.105",
            mqtt_port: 1883,
            mqtt_user: "feeder",
        }
    }

    fn render(
        schedule: Option<&Schedule>,
        submitted: &str,
        notice: Option<Notice>,
    ) -> String<PAGE_LEN> {
        let mut page = String::new();
        render_page(
            &mut page,
            &status(),
            schedule,
            &network(),
            submitted,
            notice,
        );
        assert!(
            page.ends_with("</html>"),
            "truncated at {} bytes",
            page.len()
        );
        page
    }

    #[test]
    fn the_page_shows_the_unit_and_its_meals() {
        let schedule =
            Schedule::parse(br#"[{"time":"08:00","portions":2},{"time":"19:00","portions":0}]"#)
                .unwrap();
        let page = render(Some(&schedule), "", None);
        assert!(page.contains("cat-feeder 99177c"));
        assert!(page.contains("2026-09-28 07:05"));
        assert!(page.contains("1 a day"));
        assert!(page.contains("08:00, 2 portions"));
        assert!(page.contains("name=t1 value=08:00>"));
        assert!(page.contains("name=p1 min=0 max=16 value=2>"));
        assert!(page.contains("name=t2 value=19:00>"));
        assert!(page.contains("name=p2 min=0 max=16 value=0>"));
        assert!(page.contains("name=t3></td>"));
        assert!(page.contains("value=\"home\""));
    }

    /// The whole reason `Network` has no password field: nothing stored can
    /// reach the page, and a rejected form does not echo a typed one either.
    #[test]
    fn no_password_ever_reaches_the_page() {
        let submitted = "wifi_ssid=x&wifi_password=typed-wifi&mqtt_password=typed-mqtt";
        let page = render(None, submitted, Some(Notice::Problem("no")));
        assert!(!page.contains("typed-wifi"));
        assert!(!page.contains("typed-mqtt"));
        assert!(page.contains("id=wifi_password name=wifi_password type=password"));
        assert_eq!(page.matches("type=password").count(), 2);
        assert_eq!(page.matches("type=password autocapitalize=off autocorrect=off spellcheck=false autocomplete=new-password value=\"\"").count(), 2);
    }

    #[test]
    fn a_rejected_network_form_keeps_what_was_typed() {
        let page = render(None, "wifi_ssid=typo%26net&mqtt_host=broker.lan", None);
        assert!(page.contains("value=\"typo&amp;net\""));
        assert!(page.contains("value=\"broker.lan\""));
    }

    #[test]
    fn a_paused_or_clockless_unit_promises_no_next_meal() {
        let mut page = String::new();
        let paused = Status {
            paused: true,
            ..status()
        };
        render_page(&mut page, &paused, None, &network(), "", None);
        assert!(page.contains("<b>paused</b>"));
        assert!(!page.contains("08:00, 2 portions"));

        let clockless = Status {
            now: None,
            ..status()
        };
        render_page(&mut page, &clockless, None, &network(), "", None);
        assert!(page.contains("not set"));
        assert!(!page.contains("08:00, 2 portions"));
    }

    /// Worst case: every field and the notice at their longest, in the
    /// character that costs most to escape.
    #[test]
    fn the_widest_possible_page_still_fits() {
        let mut full = Schedule::new();
        for i in 0..MAX_SLOTS as u8 {
            full = full
                .edited(SlotEdit {
                    index: i,
                    change: SlotChange::Time(23 * 60 + 59),
                })
                .unwrap()
                .edited(SlotEdit {
                    index: i,
                    change: SlotChange::Portions(MAX_CLICKS),
                })
                .unwrap();
        }
        let amp = "&".repeat(64);
        // The longest IANA name, and a rule of the longest shape a browser
        // derives, whose brackets escape to four bytes each.
        let zone = Zone::new(
            "America/Argentina/ComodRivadavia",
            "<+1245>-12:45<+1345>,M9.5.0/2:45,M4.1.0/3:45",
        )
        .unwrap();
        let wide = Network {
            wifi_ssid: &amp[..32],
            mqtt_host: &amp,
            mqtt_port: 65535,
            mqtt_user: &amp[..32],
        };
        let jammed = Status {
            id: "ffffff",
            version: "10.20.30-rc.4",
            paused: true,
            jammed: true,
            progress: Progress::Finished(Ok(crate::calibrate::Measurement {
                detent_ms: u16::MAX,
                fastest_ms: u64::MAX,
                slowest_ms: u64::MAX,
            })),
            zone: Some(&zone),
            ..status()
        };
        let mut page = String::new();
        render_page(
            &mut page,
            &jammed,
            Some(&full),
            &wide,
            "",
            Some(Notice::Problem(&amp)),
        );
        assert!(
            page.ends_with("</html>"),
            "truncated: {} of {PAGE_LEN}",
            page.len()
        );
        // Headroom, so the next section added fails here and not on a phone.
        assert!(
            page.len() < PAGE_LEN * 9 / 10,
            "{} of {PAGE_LEN}",
            page.len()
        );
    }
}
