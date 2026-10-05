//! What a day of attendance is, worked out from facts alone: the shift, the punches, the calendar
//! and any leave. No records are read here, so every rule is tested natively.
//!
//! The design comes from reading both Odoo 19 and Frappe HRMS (see `docs/design.md`):
//! * the shift's **window** is resolved for the work date (Frappe), overnight shifts included, and the
//!   work date is the day the shift *starts*;
//! * punches are **paired in order**, and anything that does not pair (an out with no in, a second in)
//!   is **flagged for review, never dropped** (Frappe drops an odd trailing punch silently);
//! * time between pairs is a recorded **break**, and a shift's usual break is deducted only when the
//!   person did not record one (neither product handles both);
//! * the verdict follows the worked minutes and the thresholds (Frappe), but a day that conflicts with
//!   leave is kept as leave and **flagged** rather than silently overwritten;
//! * the same facts always give the same result and the same `inputs_hash`, so recomputing is a diff.

use aether_sdk::dates::{Duration, NaiveDate, NaiveDateTime, NaiveTime};

/// Minutes between two instants.
fn minutes(from: NaiveDateTime, to: NaiveDateTime) -> i64 {
    (to - from).num_minutes()
}

/// A shift as the rules see it. Times are the clock on the wall at the place, `offset_min` east of
/// UTC (a fixed offset: places that change their clock twice a year need the next version).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shift {
    pub start: NaiveTime,
    pub end: NaiveTime,
    pub offset_min: i64,
    /// A punch this many minutes before the start still belongs to the shift.
    pub early_window: i64,
    /// A punch this many minutes after the end still belongs to the shift.
    pub late_window: i64,
    /// The usual unpaid break, in minutes.
    pub break_min: i64,
    pub grace_late: i64,
    pub grace_early: i64,
    /// Worked minutes below this are an absence (0: no threshold).
    pub absent_below: i64,
    /// Worked minutes below this are half a day (0: no threshold).
    pub half_day_below: i64,
}

/// Where a shift falls on a work date, in UTC.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Window {
    pub start: NaiveDateTime,
    pub end: NaiveDateTime,
    /// The earliest and latest punch that belongs to it.
    pub from: NaiveDateTime,
    pub to: NaiveDateTime,
}

impl Shift {
    pub fn crosses_midnight(&self) -> bool {
        self.end <= self.start
    }

    /// Scheduled minutes of work: the span minus the usual break.
    pub fn scheduled_minutes(&self) -> i64 {
        let span = if self.crosses_midnight() {
            24 * 60 - (self.start - self.end).num_minutes()
        } else {
            (self.end - self.start).num_minutes()
        };
        (span - self.break_min).max(0)
    }

    /// A shift whose span and windows add up to a day or more would overlap itself.
    pub fn is_sound(&self) -> bool {
        let span = if self.crosses_midnight() {
            24 * 60 - (self.start - self.end).num_minutes()
        } else {
            (self.end - self.start).num_minutes()
        };
        self.start != self.end && span + self.early_window + self.late_window < 24 * 60
    }

    /// The shift on `work_date`: it starts that day and, if it crosses midnight, ends the next.
    pub fn window(&self, work_date: NaiveDate) -> Window {
        let offset = Duration::minutes(self.offset_min);
        let start = work_date.and_time(self.start) - offset;
        let end_day = if self.crosses_midnight() { work_date + Duration::days(1) } else { work_date };
        let end = end_day.and_time(self.end) - offset;
        Window {
            start,
            end,
            from: start - Duration::minutes(self.early_window),
            to: end + Duration::minutes(self.late_window),
        }
    }
}

/// Which way a punch goes. `Auto` follows the one before it (the first of the day is an in).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    In,
    Out,
    Auto,
}

impl Direction {
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "in" => Some(Self::In),
            "out" => Some(Self::Out),
            "auto" | "" => Some(Self::Auto),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Punch {
    pub id: String,
    pub at: NaiveDateTime,
    pub direction: Direction,
}

/// Why a day needs a person to look at it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Review {
    /// An in with no out (still working, or forgot to punch out).
    OpenSpan,
    /// A punch that does not pair: two ins in a row, or an out with no in.
    OddPunches,
    /// A single span longer than the sanity limit.
    TooLong,
    /// Punches on a day of approved leave.
    LeaveConflict,
}

impl Review {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::OpenSpan => "open_span",
            Self::OddPunches => "odd_punches",
            Self::TooLong => "too_long",
            Self::LeaveConflict => "leave_conflict",
        }
    }
}

/// The longest single span believed without a look (hours).
pub const MAX_SPAN_HOURS: i64 = 16;
/// Two punches the same way this close together are one press of the button.
pub const DEBOUNCE_SECS: i64 = 60;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pairing {
    pub spans: Vec<(NaiveDateTime, NaiveDateTime)>,
    pub first_in: Option<NaiveDateTime>,
    pub last_out: Option<NaiveDateTime>,
    pub worked: i64,
    /// Minutes between spans, from the first in to the last out.
    pub gaps: i64,
    pub flags: Vec<Review>,
}

/// Pair punches in time order. Nothing is dropped: a punch that does not pair raises a flag.
pub fn pair(punches: &[Punch]) -> Pairing {
    let mut ordered: Vec<&Punch> = punches.iter().collect();
    ordered.sort_by_key(|p| (p.at, p.id.clone()));
    let mut spans = Vec::new();
    let mut flags = Vec::new();
    let mut open: Option<NaiveDateTime> = None;
    let mut last: Option<(NaiveDateTime, Direction)> = None;
    for punch in ordered {
        let direction = match punch.direction {
            Direction::Auto => {
                if open.is_some() {
                    Direction::Out
                } else {
                    Direction::In
                }
            }
            fixed => fixed,
        };
        // One press of the button recorded twice.
        if let Some((at, previous)) = last {
            if previous == direction && (punch.at - at).num_seconds().abs() < DEBOUNCE_SECS {
                continue;
            }
        }
        last = Some((punch.at, direction));
        match (direction, open) {
            (Direction::In, None) => open = Some(punch.at),
            (Direction::In, Some(_)) => flags.push(Review::OddPunches),
            (Direction::Out, Some(start)) => {
                spans.push((start, punch.at));
                open = None;
            }
            (Direction::Out, None) => flags.push(Review::OddPunches),
            (Direction::Auto, _) => {}
        }
    }
    if open.is_some() {
        flags.push(Review::OpenSpan);
    }
    let worked: i64 = spans.iter().map(|(a, b)| minutes(*a, *b)).sum();
    if spans.iter().any(|(a, b)| minutes(*a, *b) > MAX_SPAN_HOURS * 60) {
        flags.push(Review::TooLong);
    }
    let first_in = spans.first().map(|s| s.0).or(open);
    let last_out = spans.last().map(|s| s.1);
    let gaps = spans.windows(2).map(|pair| minutes(pair[0].1, pair[1].0)).sum();
    flags.sort();
    flags.dedup();
    Pairing { spans, first_in, last_out, worked, gaps, flags }
}

/// What the calendar says about the day.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DayKind {
    Working,
    WeeklyOff,
    Holiday,
}

/// Approved leave on the day.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeaveCover {
    None,
    Half,
    Full,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Present,
    Absent,
    HalfDay,
    Leave,
    Holiday,
    WeeklyOff,
    Unscheduled,
    Incomplete,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Present => "present",
            Self::Absent => "absent",
            Self::HalfDay => "half_day",
            Self::Leave => "leave",
            Self::Holiday => "holiday",
            Self::WeeklyOff => "weekly_off",
            Self::Unscheduled => "unscheduled",
            Self::Incomplete => "incomplete",
        }
    }
}

/// Everything a day is worked out from.
#[derive(Debug, Clone)]
pub struct Facts {
    pub work_date: NaiveDate,
    pub shift: Option<Shift>,
    pub kind: DayKind,
    pub leave: LeaveCover,
    pub punches: Vec<Punch>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Day {
    pub status: Status,
    pub first_in: Option<NaiveDateTime>,
    pub last_out: Option<NaiveDateTime>,
    pub worked: i64,
    pub breaks: i64,
    pub scheduled: i64,
    pub late: i64,
    pub early_exit: i64,
    pub overtime: i64,
    pub is_late: bool,
    pub is_early_exit: bool,
    pub review: Vec<Review>,
}

/// The day, or `None` when there is nothing to say yet (no punches on a working day: whether that is
/// an absence is decided later, once the day is over).
pub fn compute(facts: &Facts) -> Option<Day> {
    let window = facts.shift.as_ref().map(|shift| shift.window(facts.work_date));
    // Only punches in the shift's window belong to the day (without a shift: the calendar day).
    let belonging: Vec<Punch> = facts
        .punches
        .iter()
        .filter(|p| match &window {
            Some(w) => p.at >= w.from && p.at <= w.to,
            None => p.at.date() == facts.work_date,
        })
        .cloned()
        .collect();
    let scheduled = facts.shift.as_ref().map_or(0, Shift::scheduled_minutes);

    if belonging.is_empty() {
        let status = match (facts.leave, facts.kind) {
            (LeaveCover::Full, _) => Status::Leave,
            (_, DayKind::Holiday) => Status::Holiday,
            (_, DayKind::WeeklyOff) => Status::WeeklyOff,
            _ => return None,
        };
        return Some(Day {
            status,
            first_in: None,
            last_out: None,
            worked: 0,
            breaks: 0,
            scheduled: if status == Status::Leave { scheduled } else { 0 },
            late: 0,
            early_exit: 0,
            overtime: 0,
            is_late: false,
            is_early_exit: false,
            review: Vec::new(),
        });
    }

    let pairing = pair(&belonging);
    let mut review = pairing.flags.clone();
    // The breaks taken are the gaps between spans; the shift's usual break is deducted only when the
    // person recorded none over a long enough stay.
    let mut breaks = pairing.gaps;
    let mut worked = pairing.worked;
    if let Some(shift) = &facts.shift {
        let stay = match (pairing.first_in, pairing.last_out) {
            (Some(a), Some(b)) => minutes(a, b),
            _ => 0,
        };
        if shift.break_min > 0 && breaks == 0 && stay > 6 * 60 && !pairing.spans.is_empty() {
            let extra = shift.break_min.min(worked);
            worked -= extra;
            breaks += extra;
        }
    }

    let (mut late, mut early_exit, mut is_late, mut is_early_exit) = (0, 0, false, false);
    if let (Some(shift), Some(w)) = (&facts.shift, &window) {
        if let Some(first) = pairing.first_in {
            late = minutes(w.start, first).max(0);
            is_late = late > shift.grace_late;
        }
        if let Some(last) = pairing.last_out {
            early_exit = minutes(last, w.end).max(0);
            is_early_exit = early_exit > shift.grace_early;
        }
    }

    // Leave wins, but the conflict is shown rather than hidden.
    if facts.leave == LeaveCover::Full {
        review.push(Review::LeaveConflict);
        review.sort();
        review.dedup();
        return Some(Day {
            status: Status::Leave,
            first_in: pairing.first_in,
            last_out: pairing.last_out,
            worked,
            breaks,
            scheduled,
            late: 0,
            early_exit: 0,
            overtime: 0,
            is_late: false,
            is_early_exit: false,
            review,
        });
    }

    // A half day of leave halves what has to be worked.
    let divisor = if facts.leave == LeaveCover::Half { 2 } else { 1 };
    let (absent_below, half_day_below) = facts
        .shift
        .as_ref()
        .map_or((0, 0), |s| (s.absent_below / divisor, s.half_day_below / divisor));
    let open = review.contains(&Review::OpenSpan);
    let status = if open && pairing.spans.is_empty() {
        Status::Incomplete
    } else if facts.shift.is_none() {
        Status::Unscheduled
    } else if absent_below > 0 && worked < absent_below {
        Status::Absent
    } else if half_day_below > 0 && worked < half_day_below {
        Status::HalfDay
    } else {
        Status::Present
    };
    let overtime = match facts.kind {
        DayKind::Working => (worked - scheduled / divisor).max(0),
        DayKind::WeeklyOff | DayKind::Holiday => worked,
    };
    Some(Day {
        status,
        first_in: pairing.first_in,
        last_out: pairing.last_out,
        worked,
        breaks,
        scheduled: scheduled / divisor,
        late,
        early_exit,
        overtime: if status == Status::Incomplete { 0 } else { overtime },
        is_late,
        is_early_exit,
        review,
    })
}

/// A stable fingerprint of the facts a day was worked out from: recomputing writes nothing when it
/// has not changed.
pub fn inputs_hash(facts: &Facts) -> String {
    let mut text = format!("{}|{:?}|{:?}|", facts.work_date, facts.kind, facts.leave);
    if let Some(shift) = &facts.shift {
        text.push_str(&format!("{shift:?}"));
    }
    let mut punches: Vec<String> = facts.punches.iter().map(|p| format!("{}@{}:{:?}", p.id, p.at, p.direction)).collect();
    punches.sort();
    text.push('|');
    text.push_str(&punches.join(","));
    // FNV-1a, 64 bits.
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in text.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

/// Great-circle distance in metres between two coordinates in degrees.
pub fn distance_m(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
    let radius = 6_371_000.0_f64;
    let (p1, p2) = (lat1.to_radians(), lat2.to_radians());
    let (dp, dl) = ((lat2 - lat1).to_radians(), (lon2 - lon1).to_radians());
    let a = (dp / 2.0).sin().powi(2) + p1.cos() * p2.cos() * (dl / 2.0).sin().powi(2);
    2.0 * radius * a.sqrt().asin()
}

#[cfg(test)]
mod tests {
    use aether_sdk::dates::{parse_date, parse_datetime, parse_time};

    use super::*;

    fn d(text: &str) -> NaiveDate {
        parse_date(text).unwrap_or_default()
    }

    fn at(text: &str) -> NaiveDateTime {
        parse_datetime(text).unwrap_or_default()
    }

    fn t(text: &str) -> NaiveTime {
        parse_time(text).unwrap_or_default()
    }

    fn day_shift() -> Shift {
        Shift {
            start: t("08:00"),
            end: t("17:00"),
            offset_min: 0,
            early_window: 60,
            late_window: 60,
            break_min: 60,
            grace_late: 10,
            grace_early: 10,
            absent_below: 120,
            half_day_below: 240,
        }
    }

    fn punch(id: &str, when: &str, direction: Direction) -> Punch {
        Punch { id: id.into(), at: at(when), direction }
    }

    fn facts(shift: Option<Shift>, punches: Vec<Punch>) -> Facts {
        Facts { work_date: d("2026-10-05"), shift, kind: DayKind::Working, leave: LeaveCover::None, punches }
    }

    #[test]
    fn a_normal_day_is_present_with_the_usual_break_deducted() {
        let day = compute(&facts(
            Some(day_shift()),
            vec![punch("a", "2026-10-05T08:03:00Z", Direction::Auto), punch("b", "2026-10-05T17:05:00Z", Direction::Auto)],
        ));
        let day = day.unwrap_or_else(|| panic!("a day"));
        assert_eq!(day.status, Status::Present);
        assert_eq!(day.scheduled, 480, "nine hours less the hour's break");
        assert_eq!(day.worked, 9 * 60 + 2 - 60, "the stay is 9h02, less the usual break because none was recorded");
        assert_eq!(day.breaks, 60);
        assert!(!day.is_late && !day.is_early_exit, "three minutes late is inside the grace");
        assert_eq!(day.late, 3);
        assert!(day.review.is_empty());
    }

    #[test]
    fn a_recorded_break_replaces_the_usual_one() {
        let day = compute(&facts(
            Some(day_shift()),
            vec![
                punch("a", "2026-10-05T08:00:00Z", Direction::In),
                punch("b", "2026-10-05T12:00:00Z", Direction::Out),
                punch("c", "2026-10-05T12:45:00Z", Direction::In),
                punch("d", "2026-10-05T17:00:00Z", Direction::Out),
            ],
        ))
        .unwrap_or_else(|| panic!("a day"));
        assert_eq!(day.breaks, 45);
        assert_eq!(day.worked, 4 * 60 + 4 * 60 + 15);
        assert_eq!(day.status, Status::Present);
    }

    #[test]
    fn late_and_early_beyond_the_grace_are_flagged() {
        let day = compute(&facts(
            Some(day_shift()),
            vec![punch("a", "2026-10-05T08:40:00Z", Direction::Auto), punch("b", "2026-10-05T16:20:00Z", Direction::Auto)],
        ))
        .unwrap_or_else(|| panic!("a day"));
        assert!(day.is_late && day.is_early_exit);
        assert_eq!((day.late, day.early_exit), (40, 40));
    }

    #[test]
    fn thresholds_decide_absent_and_half_day() {
        let short = |out: &str| {
            compute(&facts(
                Some(Shift { break_min: 0, ..day_shift() }),
                vec![punch("a", "2026-10-05T08:00:00Z", Direction::Auto), punch("b", out, Direction::Auto)],
            ))
            .map(|d| d.status)
        };
        assert_eq!(short("2026-10-05T09:30:00Z"), Some(Status::Absent), "90 minutes is below the absent threshold");
        assert_eq!(short("2026-10-05T11:00:00Z"), Some(Status::HalfDay));
        assert_eq!(short("2026-10-05T13:00:00Z"), Some(Status::Present));
    }

    #[test]
    fn an_overnight_shift_belongs_to_the_day_it_starts_and_catches_the_morning_out() {
        let night = Shift { start: t("22:00"), end: t("06:00"), break_min: 0, early_window: 60, late_window: 60, ..day_shift() };
        assert!(night.is_sound() && night.crosses_midnight());
        assert_eq!(night.scheduled_minutes(), 8 * 60);
        let day = compute(&facts(
            Some(night),
            vec![punch("a", "2026-10-05T21:55:00Z", Direction::Auto), punch("b", "2026-10-06T06:10:00Z", Direction::Auto)],
        ))
        .unwrap_or_else(|| panic!("a day"));
        assert_eq!(day.status, Status::Present);
        assert_eq!(day.worked, 8 * 60 + 15);
        // A punch from the day before's shift is not this day's.
        let other = compute(&facts(Some(Shift { break_min: 0, ..day_shift() }), vec![punch("z", "2026-10-04T08:00:00Z", Direction::In)]));
        assert!(other.is_none());
    }

    #[test]
    fn the_clock_offset_moves_the_window() {
        // 08:00 local at +01:00 is 07:00 UTC.
        let lagos = Shift { offset_min: 60, break_min: 0, ..day_shift() };
        let w = lagos.window(d("2026-10-05"));
        assert_eq!(w.start, at("2026-10-05T07:00:00Z"));
        let day = compute(&facts(
            Some(lagos),
            vec![punch("a", "2026-10-05T07:00:00Z", Direction::Auto), punch("b", "2026-10-05T16:00:00Z", Direction::Auto)],
        ))
        .unwrap_or_else(|| panic!("a day"));
        assert_eq!(day.late, 0);
        assert_eq!(day.worked, 9 * 60);
    }

    #[test]
    fn punches_that_do_not_pair_are_flagged_never_dropped() {
        let pairing = pair(&[
            punch("a", "2026-10-05T08:00:00Z", Direction::In),
            punch("b", "2026-10-05T09:00:00Z", Direction::In),
            punch("c", "2026-10-05T12:00:00Z", Direction::Out),
            punch("d", "2026-10-05T13:00:00Z", Direction::Out),
            punch("e", "2026-10-05T14:00:00Z", Direction::In),
        ]);
        assert_eq!(pairing.spans.len(), 1);
        assert!(pairing.flags.contains(&Review::OddPunches));
        assert!(pairing.flags.contains(&Review::OpenSpan));
        assert_eq!(pairing.worked, 4 * 60, "the first in pairs with the first out");
    }

    #[test]
    fn a_double_press_is_one_punch_and_an_open_day_is_incomplete() {
        let pairing = pair(&[punch("a", "2026-10-05T08:00:00Z", Direction::Auto), punch("b", "2026-10-05T08:00:20Z", Direction::Auto)]);
        // The second press, auto, is an out 20 seconds after the in: a span of 0.
        assert_eq!(pairing.spans.len(), 1);
        let only_in = compute(&facts(Some(day_shift()), vec![punch("a", "2026-10-05T08:00:00Z", Direction::In), punch("b", "2026-10-05T08:00:30Z", Direction::In)]))
            .unwrap_or_else(|| panic!("a day"));
        assert_eq!(only_in.status, Status::Incomplete);
        assert!(only_in.review.contains(&Review::OpenSpan));
    }

    #[test]
    fn no_punches_means_nothing_to_say_unless_the_calendar_or_leave_does() {
        let mut f = facts(Some(day_shift()), vec![]);
        assert!(compute(&f).is_none(), "a working day with no punches is decided by the nightly job");
        f.kind = DayKind::Holiday;
        assert_eq!(compute(&f).map(|d| d.status), Some(Status::Holiday));
        f.kind = DayKind::WeeklyOff;
        assert_eq!(compute(&f).map(|d| d.status), Some(Status::WeeklyOff));
        f.kind = DayKind::Working;
        f.leave = LeaveCover::Full;
        assert_eq!(compute(&f).map(|d| d.status), Some(Status::Leave));
    }

    #[test]
    fn working_on_a_day_off_is_all_overtime_and_working_through_leave_is_flagged() {
        let mut f = facts(
            Some(Shift { break_min: 0, ..day_shift() }),
            vec![punch("a", "2026-10-05T08:00:00Z", Direction::Auto), punch("b", "2026-10-05T12:00:00Z", Direction::Auto)],
        );
        f.kind = DayKind::Holiday;
        let holiday = compute(&f).unwrap_or_else(|| panic!("a day"));
        assert_eq!((holiday.status, holiday.overtime), (Status::Present, 240));
        f.kind = DayKind::Working;
        f.leave = LeaveCover::Full;
        let on_leave = compute(&f).unwrap_or_else(|| panic!("a day"));
        assert_eq!(on_leave.status, Status::Leave, "leave wins");
        assert!(on_leave.review.contains(&Review::LeaveConflict), "but the conflict is shown");
        assert_eq!(on_leave.overtime, 0);
    }

    #[test]
    fn half_a_day_of_leave_halves_what_must_be_worked() {
        let mut f = facts(
            Some(Shift { break_min: 0, ..day_shift() }),
            vec![punch("a", "2026-10-05T08:00:00Z", Direction::Auto), punch("b", "2026-10-05T12:15:00Z", Direction::Auto)],
        );
        assert_eq!(compute(&f).map(|d| d.status), Some(Status::Present), "4h15 clears the half-day threshold of 4h");
        f.leave = LeaveCover::Half;
        let half = compute(&f).unwrap_or_else(|| panic!("a day"));
        assert_eq!(half.status, Status::Present);
        assert_eq!(half.scheduled, 270, "half of nine hours");
    }

    #[test]
    fn the_same_facts_give_the_same_hash_and_any_change_gives_another() {
        let a = facts(Some(day_shift()), vec![punch("a", "2026-10-05T08:00:00Z", Direction::Auto), punch("b", "2026-10-05T17:00:00Z", Direction::Auto)]);
        let mut shuffled = a.clone();
        shuffled.punches.reverse();
        assert_eq!(inputs_hash(&a), inputs_hash(&shuffled));
        let mut changed = a.clone();
        changed.punches[1].at = at("2026-10-05T17:30:00Z");
        assert_ne!(inputs_hash(&a), inputs_hash(&changed));
        let mut on_leave = a.clone();
        on_leave.leave = LeaveCover::Half;
        assert_ne!(inputs_hash(&a), inputs_hash(&on_leave));
    }

    #[test]
    fn an_unsound_shift_is_recognised_and_distances_are_in_metres() {
        assert!(day_shift().is_sound());
        assert!(!Shift { early_window: 600, late_window: 600, ..day_shift() }.is_sound());
        assert!(!Shift { end: t("08:00"), ..day_shift() }.is_sound(), "start == end");
        // About 111 km per degree of latitude.
        let metres = distance_m(13.0, -16.5, 14.0, -16.5);
        assert!((metres - 111_195.0).abs() < 500.0, "{metres}");
        assert!(distance_m(13.45, -16.58, 13.45, -16.58) < 1.0);
    }
}
