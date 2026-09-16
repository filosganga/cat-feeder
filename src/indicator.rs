//! What the RGB LED is saying, and when.
//!
//! Pure logic: a snapshot of what the unit knows about itself goes in, a colour
//! comes out. No peripheral, no clock of its own, no executor — so the priority
//! ladder *and* the blink timing are host-tested rather than eyeballed on a
//! bench. `led.rs` does nothing but push the colour to the WS2812.
//!
//! ## Why there is an LED at all
//!
//! Not for decoration. `CLAUDE.md` names two states where the cats do not eat
//! and nothing raises an alarm — a feeder left paused, and one sitting at
//! `schedule holding` — and the second of those is worse than it sounds,
//! because **a unit that cannot reach the broker cannot report that it cannot
//! reach the broker**. No Wi-Fi, a wrong broker address, Home Assistant down
//! while Mosquitto stays up: in every one of those the unit is alive, correct,
//! and completely silent. Once three units are screwed into feeders with no
//! console attached, this is the only channel that still works when the network
//! is the thing that is broken.
//!
//! That is what fixes the priority order below: the states nothing else can
//! report outrank the ones Home Assistant already shows.
//!
//! ## Dark is the healthy state
//!
//! [`Status::Healthy`] flashes twice and then goes **dark, indefinitely**. Two
//! reasons, and the second is the real one:
//!
//! - Three feeders glowing in a kitchen at night is a thing you would regret.
//! - If lit were normal, lit would carry no information and nobody would look
//!   at it. The LED is worth having precisely because it is usually off.
//!
//! The cost is that dark no longer separates *healthy* from *dead*. That is
//! smaller than it looks: the realistic failure here is a brown-out on motor
//! start, which reboots, and a reboot replays the two-flash confirmation — so a
//! boot loop reads as a repeating double-flash, which is more legible than a
//! stuttering heartbeat would have been. A true hard hang stays invisible, but
//! it also stops feeding, which Home Assistant does see.
//!
//! ## Counting flashes, not comparing colours
//!
//! The three network faults get **one, two and three red flashes** rather than
//! three different hues. They have three different fixes — the router, the
//! broker address, Home Assistant's publish automation — so the LED has to tell
//! them apart, and counting flashes across a dark room is reliable where
//! telling amber from orange through a diffuser is not, and fails completely
//! for a colour-blind reader.
//!
//! Solid means a mechanical fault, blinking means a network one. That keeps
//! [`Status::Jammed`] unambiguous without needing a fourth count nobody could
//! count, and a jam is the one state where a permanently lit LED is warranted:
//! food is not being dispensed and somebody has to come and fix it.

/// A colour, in the order humans write it. [`crate::led`] reorders for the
/// WS2812, which wants green first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Rgb {
    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }
}

/// The palette, deliberately dim.
///
/// A WS2812 at full brightness is painful indoors and draws ~60 mA, and the
/// moment it would be brightest — [`Status::Feeding`] — is the moment the 5 V
/// rail is already sagging enough to need the 220 µF cap next to the DRV8833.
/// Staying under ~10% solves the glare and the current draw together.
///
/// These are not perceptually matched: at equal numbers green reads much
/// brighter than blue. The values below are nudged by eye and are meant to be
/// tuned once all three units are in place.
pub const OFF: Rgb = Rgb::new(0, 0, 0);
pub const RED: Rgb = Rgb::new(24, 0, 0);
pub const GREEN: Rgb = Rgb::new(0, 18, 0);
pub const BLUE: Rgb = Rgb::new(0, 0, 30);
pub const AMBER: Rgb = Rgb::new(22, 7, 0);
pub const WHITE: Rgb = Rgb::new(16, 16, 16);
/// Deliberately not [`BLUE`]: setup mode already owns blue, and these two must
/// not be confused — one means "type your Wi-Fi password in", the other means
/// "a tap will dispense food".
pub const CYAN: Rgb = Rgb::new(0, 16, 22);

/// How long one flash is lit, and how long the gap after it is.
///
/// One shape for every pattern, so a count is always read the same way. Tuned
/// so three flashes plus their gaps stay well inside the shortest period below.
const FLASH_ON_MS: u64 = 120;
const FLASH_GAP_MS: u64 = 200;
const FLASH_MS: u64 = FLASH_ON_MS + FLASH_GAP_MS;

/// Everything the LED is allowed to know, sampled together.
///
/// A plain snapshot rather than references to the live atomics, so
/// [`Indicator::poll`] cannot observe a value changing underneath it and cannot
/// be tested only on hardware.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Health {
    /// The outside button is armed, so a tap will feed. See `button.rs`.
    pub button_armed: bool,
    /// Setup mode: this unit has raised its own access point. Roadmap step 9.
    pub setup: bool,
    /// Associated with the configured Wi-Fi network.
    pub link: bool,
    /// Connected to the MQTT broker.
    pub broker: bool,
    /// The clock has been given a *live* time, so the schedule is armed. A
    /// retained time does not count — see `schedule.rs`.
    pub armed: bool,
    pub paused: bool,
    pub feeding: bool,
    pub jammed: bool,
}

/// The one thing the LED is showing. Exactly one, ever.
///
/// Encoding several facts at once in a single LED produces something nobody
/// can read, so [`Status::of`] picks a winner and the rest wait.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// The mechanism is stuck. Solid, because somebody has to come and look.
    Jammed,
    /// The motor is turning. Also the fastest way to tell "the command never
    /// arrived" from "the motor is dead" while wiring a unit up.
    Feeding,
    /// The outside button is armed: a tap now dispenses a portion.
    ///
    /// Transient and user-initiated, which is why it outranks everything below.
    /// Whatever it hides is still there ten seconds later, and during those ten
    /// seconds the only thing worth knowing is whether the button is listening.
    Armed,
    /// Unconfigured, serving the setup form on its own access point.
    Setup,
    /// Not associated with any Wi-Fi network. Fix: the router, or the credentials.
    NoLink,
    /// On the network but not talking to the broker. Fix: the broker address,
    /// or Mosquitto.
    NoBroker,
    /// Talking to the broker but never handed a live time, so the schedule is
    /// holding and **this unit will not feed**. Fix: Home Assistant's
    /// publish-the-time automation.
    NoTime,
    /// Running, reachable, and deliberately not feeding on schedule.
    Paused,
    /// Nothing to say.
    Healthy,
}

impl Status {
    /// The priority ladder. First match wins.
    ///
    /// Ordered by *what nothing else can tell you*, with two exceptions at the
    /// top: a jam because you are about to put your hands in the mechanism, and
    /// feeding because it is the one thing you actively want to watch happen.
    pub fn of(health: Health) -> Self {
        if health.jammed {
            Self::Jammed
        } else if health.feeding {
            Self::Feeding
        } else if health.button_armed {
            Self::Armed
        } else if health.setup {
            Self::Setup
        } else if !health.link {
            Self::NoLink
        } else if !health.broker {
            Self::NoBroker
        } else if !health.armed {
            Self::NoTime
        } else if health.paused {
            Self::Paused
        } else {
            Self::Healthy
        }
    }

    /// How to show it.
    pub fn pattern(self) -> Pattern {
        match self {
            // Solid: a mechanical fault, and the only state that stays lit.
            Self::Jammed => Pattern::solid(RED),
            Self::Feeding => Pattern::solid(WHITE),

            // Twice a second: unmistakably faster than anything else here, and
            // the urgency is honest — it lapses in ten seconds.
            Self::Armed => Pattern::repeating(CYAN, 1, 500),

            Self::Setup => Pattern::repeating(BLUE, 1, 2_000),

            // One, two, three: the router, the broker, Home Assistant.
            Self::NoLink => Pattern::repeating(RED, 1, 3_000),
            Self::NoBroker => Pattern::repeating(RED, 2, 3_000),
            Self::NoTime => Pattern::repeating(RED, 3, 3_000),

            // Slower and warmer than a fault: deliberate, not broken, but still
            // the state where cats quietly do not get fed.
            Self::Paused => Pattern::repeating(AMBER, 1, 5_000),

            // Twice, then dark forever.
            Self::Healthy => Pattern::once(GREEN, 2),
        }
    }
}

/// A colour and a rhythm.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pattern {
    pub colour: Rgb,
    /// Flashes per cycle. Zero holds [`Pattern::colour`] steady.
    pub flashes: u8,
    /// Cycle length. Zero plays the flashes once and then stays dark.
    pub period_ms: u64,
}

impl Pattern {
    pub const fn solid(colour: Rgb) -> Self {
        Self {
            colour,
            flashes: 0,
            period_ms: 0,
        }
    }

    pub const fn repeating(colour: Rgb, flashes: u8, period_ms: u64) -> Self {
        Self {
            colour,
            flashes,
            period_ms,
        }
    }

    pub const fn once(colour: Rgb, flashes: u8) -> Self {
        Self {
            colour,
            flashes,
            period_ms: 0,
        }
    }

    /// How long one full train of flashes takes.
    const fn train_ms(&self) -> u64 {
        self.flashes as u64 * FLASH_MS
    }

    /// The colour to show `elapsed_ms` after this pattern started.
    ///
    /// All arithmetic is in `u64` on purpose. Milliseconds since boot in a
    /// `u32` wrap after 49 days, and [`Status::Healthy`] is a one-shot that can
    /// legitimately stay current for months — a wrap there would replay the
    /// confirmation flashes out of nowhere, roughly every seven weeks, which is
    /// exactly the kind of bug nobody would ever reproduce on a bench.
    pub fn level_at(&self, elapsed_ms: u64) -> Rgb {
        if self.flashes == 0 {
            return self.colour;
        }

        let train = self.train_ms();

        let t = if self.period_ms == 0 {
            // One-shot. Clamping rather than wrapping is what makes it final.
            elapsed_ms.min(train)
        } else {
            // A period shorter than its own train would silently truncate the
            // flashes, and a truncated count is a wrong diagnosis rather than
            // an ugly one.
            elapsed_ms % self.period_ms.max(train)
        };

        if t >= train {
            return OFF;
        }

        if t % FLASH_MS < FLASH_ON_MS {
            self.colour
        } else {
            OFF
        }
    }
}

/// Tracks which pattern is running and how far into it we are.
///
/// The only state worth keeping: when the current status started. Everything
/// else is recomputed, so there is nothing to get out of step.
#[derive(Debug, Default)]
pub struct Indicator {
    status: Option<Status>,
    since_ms: u64,
}

impl Indicator {
    pub const fn new() -> Self {
        Self {
            status: None,
            since_ms: 0,
        }
    }

    /// The colour the LED should be showing right now.
    ///
    /// Restarts the pattern whenever the status changes, which is what gives
    /// [`Status::Healthy`] its confirmation flashes: they play on *entering*
    /// the state, so they mark boot completing, a feed finishing cleanly, and a
    /// dropped connection coming back — each of them a transition worth seeing,
    /// and none of them worth a permanent light afterwards.
    pub fn poll(&mut self, now_ms: u64, health: Health) -> Rgb {
        let next = Status::of(health);

        if self.status != Some(next) {
            self.status = Some(next);
            self.since_ms = now_ms;
        }

        next.pattern()
            .level_at(now_ms.saturating_sub(self.since_ms))
    }

    /// What is currently being shown, for the log line that says so.
    pub fn status(&self) -> Option<Status> {
        self.status
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Everything good. The base the ladder tests deviate from one field at a
    /// time, so each test names exactly the fact it is about.
    fn well() -> Health {
        Health {
            button_armed: false,
            setup: false,
            link: true,
            broker: true,
            armed: true,
            paused: false,
            feeding: false,
            jammed: false,
        }
    }

    #[test]
    fn all_well_is_healthy() {
        assert_eq!(Status::of(well()), Status::Healthy);
    }

    #[test]
    fn each_fault_is_named() {
        assert_eq!(
            Status::of(Health {
                link: false,
                ..well()
            }),
            Status::NoLink
        );
        assert_eq!(
            Status::of(Health {
                broker: false,
                ..well()
            }),
            Status::NoBroker
        );
        assert_eq!(
            Status::of(Health {
                armed: false,
                ..well()
            }),
            Status::NoTime
        );
    }

    #[test]
    fn the_worst_fault_wins() {
        // Nothing works at all: the LED says the *first* thing to fix, not the
        // last. Reporting "no time" to someone whose router is off would send
        // them to the wrong end of the system.
        let nothing = Health {
            link: false,
            broker: false,
            armed: false,
            ..well()
        };
        assert_eq!(Status::of(nothing), Status::NoLink);

        let no_broker = Health {
            broker: false,
            armed: false,
            ..well()
        };
        assert_eq!(Status::of(no_broker), Status::NoBroker);
    }

    #[test]
    fn a_jam_outranks_everything() {
        let bad = Health {
            jammed: true,
            feeding: true,
            button_armed: true,
            setup: true,
            link: false,
            broker: false,
            armed: false,
            paused: true,
        };
        assert_eq!(Status::of(bad), Status::Jammed);
    }

    #[test]
    fn feeding_outranks_being_offline() {
        // A manual feed works with the broker down, and watching the motor turn
        // is more useful in that moment than being told again that it is down.
        let offline = Health {
            feeding: true,
            link: false,
            broker: false,
            armed: false,
            ..well()
        };
        assert_eq!(Status::of(offline), Status::Feeding);
    }

    #[test]
    fn paused_is_the_last_thing_reported() {
        // Paused is visible in Home Assistant; a broken network is not. So it
        // loses to every fault and only shows once everything else is fine.
        assert_eq!(
            Status::of(Health {
                paused: true,
                ..well()
            }),
            Status::Paused
        );
        assert_eq!(
            Status::of(Health {
                paused: true,
                broker: false,
                ..well()
            }),
            Status::NoBroker
        );
    }

    #[test]
    fn an_armed_button_outranks_every_fault() {
        // The case that decides this: the broker is down, and manual feeding is
        // exactly what the button is for. Being re-told the network is out is
        // less useful than knowing the tap will land — and the fault is still
        // there ten seconds later when the arm lapses.
        let offline = Health {
            button_armed: true,
            link: false,
            broker: false,
            armed: false,
            ..well()
        };
        assert_eq!(Status::of(offline), Status::Armed);
    }

    #[test]
    fn feeding_outranks_an_armed_button() {
        // The tap's own result. Otherwise pressing it would show nothing new.
        let feeding = Health {
            button_armed: true,
            feeding: true,
            ..well()
        };
        assert_eq!(Status::of(feeding), Status::Feeding);
    }

    #[test]
    fn armed_is_the_fastest_thing_on_the_led() {
        // It is the only status with a deadline, so it should read as urgent
        // next to everything else.
        let armed = Status::Armed.pattern().period_ms;
        for status in [
            Status::Setup,
            Status::NoLink,
            Status::NoBroker,
            Status::NoTime,
            Status::Paused,
        ] {
            assert!(
                status.pattern().period_ms > armed,
                "{status:?} repeats at least as fast as Armed"
            );
        }
    }

    #[test]
    fn setup_mode_beats_having_no_link() {
        // In setup mode there is deliberately no station connection, so the
        // link fault is expected and reporting it would be noise.
        let setup = Health {
            setup: true,
            link: false,
            broker: false,
            armed: false,
            ..well()
        };
        assert_eq!(Status::of(setup), Status::Setup);
    }

    #[test]
    fn the_fault_codes_are_one_two_three() {
        // The whole diagnostic value is in these being countable and distinct.
        assert_eq!(Status::NoLink.pattern().flashes, 1);
        assert_eq!(Status::NoBroker.pattern().flashes, 2);
        assert_eq!(Status::NoTime.pattern().flashes, 3);

        for status in [Status::NoLink, Status::NoBroker, Status::NoTime] {
            assert_eq!(status.pattern().colour, RED);
        }
    }

    #[test]
    fn only_a_jam_stays_lit_indefinitely() {
        // Solid red is reserved for the mechanical fault. If anything else ever
        // becomes solid, "solid means go and look at the hub" stops holding.
        let long_after = 10 * 60 * 60 * 1000;

        assert_eq!(Status::Jammed.pattern().level_at(long_after), RED);

        for status in [
            Status::Armed,
            Status::Setup,
            Status::NoLink,
            Status::NoBroker,
            Status::NoTime,
            Status::Paused,
            Status::Healthy,
        ] {
            let pattern = status.pattern();
            assert_ne!(
                pattern.flashes, 0,
                "{status:?} is solid, which now means a jam"
            );
        }
    }

    #[test]
    fn a_solid_pattern_never_changes() {
        let solid = Pattern::solid(WHITE);
        for t in [0, 1, 500, 100_000, u64::MAX] {
            assert_eq!(solid.level_at(t), WHITE);
        }
    }

    #[test]
    fn a_flash_is_lit_then_dark() {
        let one = Pattern::repeating(RED, 1, 3_000);

        assert_eq!(one.level_at(0), RED);
        assert_eq!(one.level_at(FLASH_ON_MS - 1), RED);
        assert_eq!(one.level_at(FLASH_ON_MS), OFF);
        assert_eq!(one.level_at(FLASH_MS), OFF, "only one flash, then the gap");
        assert_eq!(one.level_at(2_999), OFF);
    }

    #[test]
    fn a_repeating_pattern_comes_back_round() {
        let two = Pattern::repeating(RED, 2, 3_000);

        assert_eq!(two.level_at(0), RED);
        assert_eq!(two.level_at(FLASH_MS), RED, "the second flash");
        assert_eq!(two.level_at(2 * FLASH_MS), OFF, "and then nothing");

        // Same phase, one period later, and a hundred periods later.
        assert_eq!(two.level_at(3_000), RED);
        assert_eq!(two.level_at(3_000 + FLASH_MS), RED);
        assert_eq!(two.level_at(300_000), RED);
    }

    #[test]
    fn counting_the_flashes_gives_the_code_back() {
        // What someone standing in the kitchen actually does: watch one period
        // and count. If this ever disagrees with `flashes`, the LED is lying.
        for status in [Status::NoLink, Status::NoBroker, Status::NoTime] {
            let pattern = status.pattern();

            let mut seen = 0;
            let mut lit = false;
            for t in 0..pattern.period_ms {
                let now_lit = pattern.level_at(t) != OFF;
                if now_lit && !lit {
                    seen += 1;
                }
                lit = now_lit;
            }

            assert_eq!(
                seen, pattern.flashes,
                "{status:?} shows {seen} flashes but claims {}",
                pattern.flashes
            );
        }
    }

    #[test]
    fn a_one_shot_goes_dark_and_stays_dark() {
        let confirm = Status::Healthy.pattern();

        assert_eq!(confirm.level_at(0), GREEN);
        assert_eq!(confirm.level_at(FLASH_MS), GREEN, "the second flash");
        assert_eq!(confirm.level_at(2 * FLASH_MS), OFF);

        // The point of the whole design: months later, still dark. A u32 of
        // milliseconds would have wrapped twice by the last of these and
        // replayed the flashes.
        for t in [10_000, 3_600_000, 86_400_000, 4_294_967_296, u64::MAX] {
            assert_eq!(confirm.level_at(t), OFF, "still lit at {t} ms");
        }
    }

    #[test]
    fn a_period_shorter_than_its_flashes_does_not_truncate_them() {
        // Guards the construction rather than any current pattern: a truncated
        // count would be a wrong diagnosis, not just an ugly rhythm.
        let cramped = Pattern::repeating(RED, 3, 100);

        let mut seen = 0;
        let mut lit = false;
        for t in 0..cramped.train_ms() {
            let now_lit = cramped.level_at(t) != OFF;
            if now_lit && !lit {
                seen += 1;
            }
            lit = now_lit;
        }

        assert_eq!(seen, 3);
    }

    #[test]
    fn every_pattern_fits_inside_its_period() {
        for status in [
            Status::Armed,
            Status::Setup,
            Status::NoLink,
            Status::NoBroker,
            Status::NoTime,
            Status::Paused,
        ] {
            let pattern = status.pattern();
            assert!(
                pattern.train_ms() < pattern.period_ms,
                "{status:?} has no gap between repeats, so the count cannot be read"
            );
        }
    }

    #[test]
    fn the_pattern_restarts_when_the_status_changes() {
        let mut indicator = Indicator::new();

        // Well into a fault's dark stretch.
        assert_eq!(indicator.poll(10_000, Health::default()), RED);
        assert_eq!(indicator.poll(10_000 + FLASH_ON_MS, Health::default()), OFF);

        // Recovering starts the confirmation from its first flash, rather than
        // joining the new pattern part-way through at whatever phase the old
        // one happened to be at.
        assert_eq!(indicator.poll(10_500, well()), GREEN);
        assert_eq!(indicator.status(), Some(Status::Healthy));
    }

    #[test]
    fn a_steady_status_keeps_its_phase() {
        let mut indicator = Indicator::new();
        let health = Health {
            broker: false,
            ..well()
        };

        assert_eq!(indicator.poll(1_000, health), RED);
        // Not restarted: the second flash of the two-flash code lands where the
        // pattern says it should, not where the last poll was.
        assert_eq!(indicator.poll(1_000 + FLASH_ON_MS, health), OFF);
        assert_eq!(indicator.poll(1_000 + FLASH_MS, health), RED);
    }

    #[test]
    fn healthy_confirms_again_after_a_feed() {
        let mut indicator = Indicator::new();

        assert_eq!(indicator.poll(0, well()), GREEN);
        assert_eq!(indicator.poll(5_000, well()), OFF, "settled, dark");

        let feeding = Health {
            feeding: true,
            ..well()
        };
        assert_eq!(indicator.poll(6_000, feeding), WHITE);

        // Returning to healthy re-flashes, so a clean feed ends with a visible
        // acknowledgement rather than the LED simply going out.
        assert_eq!(indicator.poll(8_000, well()), GREEN);
    }
}
