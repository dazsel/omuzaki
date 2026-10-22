//! The time nodes (design §6, D4, D5): timers on the engine clock (`timer.every`, `timer.once`, `timer.stopwatch`,
//! category Time) and the wall clock in a time zone (`schedule.at`, and the pure conditions `cond.time_between` and
//! `cond.date`, category Conditions). Time zones are IANA names, DST included, through `jiff`; with none named, the
//! system's zone.

use jiff::civil::{Date, DateTime, Time, Weekday};
use jiff::tz::{AmbiguousOffset, TimeZone};
use jiff::{Timestamp, ToSpan};
use omuzaki_core::time::Seconds;
use omuzaki_l10n_api::Text;
use omuzaki_live_api::event::UtcMillis;
use omuzaki_live_api::node::{
    Category, EvalCx, Input, Node, NodeClass, NodeCx, NodeError, NodeKind, NodeMeta, ParamError, ParamErrors, Ports,
    WakeId,
};
use omuzaki_live_api::register_node;
use omuzaki_live_api::value::{PortType, Val};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::l10n;

/// The shortest `timer.every` period, in seconds.
const MIN_PERIOD: f64 = 0.05;

/// The longest `schedule.at` sleeps between looks at the wall clock, in seconds: a wall-clock jump is noticed within
/// it.
const LOOK_EVERY: f64 = 60.0;

/// A pure node's handle: it never gets an event (design D4).
fn pure_handle(input: &Input<'_>) -> Result<(), NodeError> {
    match input {
        Input::Event { port, .. } => Err(NodeError::unknown_port(port)),
        Input::Start | Input::Stop | Input::Trigger { .. } | Input::Wake { .. } | Input::Reply { .. } => Ok(()),
        other => Err(NodeError::unhandled(other)),
    }
}

fn seconds_error(key: &str, seconds: f64, min: f64) -> Option<ParamError> {
    if !seconds.is_finite() || seconds < 0.0 {
        Some(ParamError::new(key, l10n::bad_seconds()))
    } else if seconds < min {
        Some(ParamError::new(key, l10n::seconds_too_short(min)))
    } else {
        None
    }
}

fn errors(errors: Vec<ParamError>) -> Result<(), ParamErrors> {
    if errors.is_empty() { Ok(()) } else { Err(ParamErrors(errors)) }
}

// ── timer.every ────────────────────────────────────────────────────────────────────────────────────────────────

/// Parameters of `timer.every`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EveryParams {
    /// The period, in seconds: at least 0.05.
    pub seconds: f64,
}

impl Default for EveryParams {
    fn default() -> Self {
        Self { seconds: 60.0 }
    }
}

/// Ticks every `seconds` from when it starts, without drift: the n-th tick is at start + n × seconds.
#[derive(Debug)]
pub struct Every {
    seconds: f64,
    started: Option<Seconds>,
    count: f64,
    wake: Option<WakeId>,
}

impl NodeKind for Every {
    type Params = EveryParams;

    fn meta() -> NodeMeta {
        NodeMeta::new("timer.every", "Tick again and again, every few seconds.", NodeClass::Active, Category::Time)
    }

    fn ports(_: &EveryParams) -> Ports {
        Ports::new()
            .event_in("start", "Starts again from now (it also starts when the show does).")
            .event_in("stop", "Stops ticking.")
            .event_out("tick", "Each tick; the number of the tick is on `count`.")
            .value_out("count", PortType::Number, "Ticks since the start: 1, 2, 3...")
    }

    fn check(params: &EveryParams) -> Result<(), ParamErrors> {
        errors(seconds_error("seconds", params.seconds, MIN_PERIOD).into_iter().collect())
    }

    fn new(params: EveryParams) -> Self {
        Self { seconds: params.seconds.max(MIN_PERIOD), started: None, count: 0.0, wake: None }
    }
}

impl Every {
    fn start(&mut self, cx: &mut NodeCx<'_>) -> Result<(), NodeError> {
        self.stop(cx);
        let now = cx.now();
        self.started = Some(now);
        self.count = 0.0;
        self.wake = Some(cx.wake_at(Seconds(now.0 + self.seconds))?);
        Ok(())
    }

    fn stop(&mut self, cx: &mut NodeCx<'_>) {
        if let Some(id) = self.wake.take() {
            cx.cancel_wake(id);
        }
        self.started = None;
    }
}

impl Node for Every {
    fn handle(&mut self, input: Input<'_>, cx: &mut NodeCx<'_>) -> Result<(), NodeError> {
        match input {
            Input::Start | Input::Event { port: "start", .. } => self.start(cx),
            Input::Event { port: "stop", .. } => {
                self.stop(cx);
                Ok(())
            }
            Input::Event { port, .. } => Err(NodeError::unknown_port(port)),
            Input::Wake { id, .. } if Some(id) == self.wake => {
                let Some(started) = self.started else { return Ok(()) };
                self.count += 1.0;
                // From the start, not from this tick, so rounding never accumulates.
                self.wake = Some(cx.wake_at(Seconds(started.0 + (self.count + 1.0) * self.seconds))?);
                cx.set("count", Val::Number(self.count))?;
                cx.fire("tick")
            }
            Input::Stop | Input::Trigger { .. } | Input::Wake { .. } | Input::Reply { .. } => Ok(()),
            other => Err(NodeError::unhandled(&other)),
        }
    }
}
register_node!(Every);

// ── timer.once ─────────────────────────────────────────────────────────────────────────────────────────────────

/// Parameters of `timer.once`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OnceParams {
    /// How long after the start, in seconds.
    pub seconds: f64,
}

impl Default for OnceParams {
    fn default() -> Self {
        Self { seconds: 10.0 }
    }
}

/// Fires once, `seconds` after the show starts; `in` starts the wait again.
#[derive(Debug)]
pub struct Once {
    seconds: f64,
    wake: Option<WakeId>,
}

impl NodeKind for Once {
    type Params = OnceParams;

    fn meta() -> NodeMeta {
        NodeMeta::new("timer.once", "Fire once, a while after the show starts.", NodeClass::Active, Category::Time)
    }

    fn ports(_: &OnceParams) -> Ports {
        Ports::new().event_in("in", "Starts the wait again from now.").event_out("out", "The wait is over.")
    }

    fn check(params: &OnceParams) -> Result<(), ParamErrors> {
        errors(seconds_error("seconds", params.seconds, 0.0).into_iter().collect())
    }

    fn new(params: OnceParams) -> Self {
        Self { seconds: params.seconds.max(0.0), wake: None }
    }
}

impl Node for Once {
    fn handle(&mut self, input: Input<'_>, cx: &mut NodeCx<'_>) -> Result<(), NodeError> {
        match input {
            Input::Start | Input::Event { port: "in", .. } => {
                if let Some(id) = self.wake.take() {
                    cx.cancel_wake(id);
                }
                self.wake = Some(cx.wake_after(Seconds(self.seconds))?);
                Ok(())
            }
            Input::Event { port, .. } => Err(NodeError::unknown_port(port)),
            Input::Wake { id, .. } if Some(id) == self.wake => {
                self.wake = None;
                cx.fire("out")
            }
            Input::Stop | Input::Trigger { .. } | Input::Wake { .. } | Input::Reply { .. } => Ok(()),
            other => Err(NodeError::unhandled(&other)),
        }
    }
}
register_node!(Once);

// ── timer.stopwatch ────────────────────────────────────────────────────────────────────────────────────────────

/// Parameters of `timer.stopwatch`: none.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StopwatchParams {}

/// Measures time between `start` and `stop`; `reset` sets it back to 0.
#[derive(Debug, Default)]
pub struct Stopwatch {
    /// Seconds from earlier runs since the last reset.
    banked: f64,
    /// When the current run started, while running.
    since: Option<Seconds>,
}

impl Stopwatch {
    fn elapsed(&self, now: Seconds) -> f64 {
        self.banked + self.since.map_or(0.0, |since| (now.0 - since.0).max(0.0))
    }
}

impl NodeKind for Stopwatch {
    type Params = StopwatchParams;

    fn meta() -> NodeMeta {
        NodeMeta::new("timer.stopwatch", "Measure how long something takes.", NodeClass::Active, Category::Time)
    }

    fn ports(_: &StopwatchParams) -> Ports {
        Ports::new()
            .event_in("start", "Starts (or carries on) measuring.")
            .event_in("stop", "Stops measuring.")
            .event_in("reset", "Sets the time back to 0; a running stopwatch keeps running.")
            .event_out("stopped", "It stopped; the time is on `elapsed`.")
            .live_out("elapsed", PortType::Number, "Seconds measured so far.")
    }

    fn new(_: StopwatchParams) -> Self {
        Self::default()
    }
}

impl Node for Stopwatch {
    fn handle(&mut self, input: Input<'_>, cx: &mut NodeCx<'_>) -> Result<(), NodeError> {
        match input {
            Input::Event { port: "start", .. } => {
                self.since.get_or_insert(cx.now());
                Ok(())
            }
            Input::Event { port: "stop", .. } => {
                let now = cx.now();
                if self.since.is_none() {
                    return Ok(());
                }
                self.banked = self.elapsed(now);
                self.since = None;
                cx.fire("stopped")
            }
            Input::Event { port: "reset", .. } => {
                self.banked = 0.0;
                if self.since.is_some() {
                    self.since = Some(cx.now());
                }
                Ok(())
            }
            Input::Event { port, .. } => Err(NodeError::unknown_port(port)),
            Input::Start | Input::Stop | Input::Trigger { .. } | Input::Wake { .. } | Input::Reply { .. } => Ok(()),
            other => Err(NodeError::unhandled(&other)),
        }
    }

    fn eval(&self, port: &str, cx: &mut EvalCx<'_>) -> Result<Val, NodeError> {
        match port {
            "elapsed" => Ok(Val::Number(self.elapsed(cx.now()))),
            other => Err(NodeError::not_live(other)),
        }
    }
}
register_node!(Stopwatch);

// ── the wall clock ─────────────────────────────────────────────────────────────────────────────────────────────

/// A day of the week.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Day {
    /// Monday.
    Mon,
    /// Tuesday.
    Tue,
    /// Wednesday.
    Wed,
    /// Thursday.
    Thu,
    /// Friday.
    Fri,
    /// Saturday.
    Sat,
    /// Sunday.
    Sun,
}

impl Day {
    /// Every day, Monday first.
    pub const ALL: [Day; 7] = [Day::Mon, Day::Tue, Day::Wed, Day::Thu, Day::Fri, Day::Sat, Day::Sun];

    const fn weekday(self) -> Weekday {
        match self {
            Day::Mon => Weekday::Monday,
            Day::Tue => Weekday::Tuesday,
            Day::Wed => Weekday::Wednesday,
            Day::Thu => Weekday::Thursday,
            Day::Fri => Weekday::Friday,
            Day::Sat => Weekday::Saturday,
            Day::Sun => Weekday::Sunday,
        }
    }
}

fn every_day() -> Vec<Day> {
    Day::ALL.to_vec()
}

/// The days a node keeps, as a set.
#[derive(Clone, Copy, Debug)]
struct Days([bool; 7]);

impl Days {
    fn of(days: &[Day]) -> Self {
        let mut set = [false; 7];
        for day in days {
            set[usize::from(day.weekday().to_monday_zero_offset().unsigned_abs())] = true;
        }
        Self(set)
    }

    fn has(self, date: Date) -> bool {
        self.0[usize::from(date.weekday().to_monday_zero_offset().unsigned_abs())]
    }
}

/// A `HH:MM` time of day, or why it isn't one.
fn parse_time(text: &str) -> Result<Time, Text> {
    match text.parse::<Time>() {
        Ok(time) if text.len() == 5 => Ok(time),
        _ => Err(l10n::bad_time(text)),
    }
}

/// The zone named `name`, the system's when none is named, or why there is none.
fn zone(name: Option<&str>) -> Result<TimeZone, Text> {
    match name {
        Some(name) => TimeZone::get(name).map_err(|_| l10n::unknown_time_zone(name)),
        None => Ok(TimeZone::system()),
    }
}

/// The zone for a node whose `check` passed; UTC if it has gone since.
fn zone_or_utc(name: Option<&str>) -> TimeZone {
    zone(name).unwrap_or(TimeZone::UTC)
}

fn timestamp(utc: UtcMillis) -> Timestamp {
    Timestamp::from_millisecond(utc.0).unwrap_or(Timestamp::UNIX_EPOCH)
}

fn days_error(days: &[Day]) -> Option<ParamError> {
    days.is_empty().then(|| ParamError::new("days", l10n::no_days()))
}

fn zone_error(tz: Option<&str>) -> Option<ParamError> {
    zone(tz).err().map(|text| ParamError::new("tz", text))
}

// ── schedule.at ────────────────────────────────────────────────────────────────────────────────────────────────

/// Parameters of `schedule.at`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ScheduleParams {
    /// The local time of day, `HH:MM` (24-hour).
    pub time: String,
    /// The days it fires on.
    #[serde(default = "every_day")]
    pub days: Vec<Day>,
    /// The IANA time zone, such as `Europe/London`; the system's when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tz: Option<String>,
}

impl Default for ScheduleParams {
    fn default() -> Self {
        Self { time: "12:00".to_owned(), days: every_day(), tz: None }
    }
}

/// Fires at a local time of day on chosen days.
///
/// It looks at the wall clock at that time and at least once a minute, and fires when the day's time has passed
/// since it last looked, at most once per local day: a jump forward past the time fires it once, a jump back doesn't
/// fire it again. On the autumn day a time happens twice it fires at the first; on the spring day a time is skipped it
/// fires at the first instant after the gap (02:30 in New York fires at 03:00 summer time).
#[derive(Debug)]
pub struct Schedule {
    time: Time,
    days: Days,
    tz: TimeZone,
    looked: Option<Timestamp>,
    fired_on: Option<Date>,
    wake: Option<WakeId>,
}

impl NodeKind for Schedule {
    type Params = ScheduleParams;

    fn meta() -> NodeMeta {
        NodeMeta::new("schedule.at", "At a time of day, on chosen days.", NodeClass::Active, Category::Time)
    }

    fn ports(_: &ScheduleParams) -> Ports {
        Ports::new().event_out("fired", "It is the time.")
    }

    fn check(params: &ScheduleParams) -> Result<(), ParamErrors> {
        let time = parse_time(&params.time).err().map(|text| ParamError::new("time", text));
        errors(time.into_iter().chain(days_error(&params.days)).chain(zone_error(params.tz.as_deref())).collect())
    }

    fn new(params: ScheduleParams) -> Self {
        Self {
            time: parse_time(&params.time).unwrap_or(Time::midnight()),
            days: Days::of(&params.days),
            tz: zone_or_utc(params.tz.as_deref()),
            looked: None,
            fired_on: None,
            wake: None,
        }
    }
}

impl Schedule {
    /// When the time happens on `date`, by the DST rules above.
    fn on(&self, date: Date) -> Option<Timestamp> {
        let at = date.to_datetime(self.time);
        let ambiguous = self.tz.to_ambiguous_timestamp(at);
        if !matches!(ambiguous.offset(), AmbiguousOffset::Gap { .. }) {
            return ambiguous.earlier().ok();
        }
        // The first local minute after the gap; transitions fall on whole minutes.
        (1..=24 * 60).find_map(|minutes: i64| {
            let later: DateTime = at.checked_add(minutes.minutes()).ok()?;
            let ambiguous = self.tz.to_ambiguous_timestamp(later);
            (!matches!(ambiguous.offset(), AmbiguousOffset::Gap { .. })).then(|| ambiguous.earlier().ok()).flatten()
        })
    }

    /// The days around `today`, from `back` days before it, for `count` days.
    fn around(today: Date, back: i64, count: usize) -> impl Iterator<Item = Date> {
        let first = today.checked_sub(back.days()).unwrap_or(today);
        std::iter::successors(Some(first), |d| d.tomorrow().ok()).take(count)
    }

    fn look(&mut self, cx: &mut NodeCx<'_>) -> Result<(), NodeError> {
        let now = timestamp(cx.utc());
        let today = now.to_zoned(self.tz.clone()).date();
        let latest = Self::around(today, 7, 8)
            .filter(|d| self.days.has(*d))
            .filter_map(|d| self.on(d).map(|at| (d, at)))
            .filter(|(_, at)| *at <= now)
            .last();
        let mut fire = false;
        if let (Some((date, at)), Some(looked)) = (latest, self.looked)
            && at > looked
            && self.fired_on != Some(date)
        {
            self.fired_on = Some(date);
            fire = true;
        }
        self.looked = Some(now);
        let next =
            Self::around(today, 1, 10).filter(|d| self.days.has(*d)).filter_map(|d| self.on(d)).find(|at| *at > now);
        let until = next.map_or(LOOK_EVERY, |at| millis_between(now, at) / 1_000.0);
        self.wake = Some(cx.wake_after(Seconds(until.min(LOOK_EVERY)))?);
        if fire { cx.fire("fired") } else { Ok(()) }
    }
}

/// Milliseconds from `from` to `to`; schedules are days apart at most, far inside f64's exact range.
#[allow(clippy::cast_precision_loss)]
fn millis_between(from: Timestamp, to: Timestamp) -> f64 {
    (to.as_millisecond() - from.as_millisecond()) as f64
}

impl Node for Schedule {
    fn handle(&mut self, input: Input<'_>, cx: &mut NodeCx<'_>) -> Result<(), NodeError> {
        match input {
            Input::Start => {
                self.looked = None;
                self.look(cx)
            }
            Input::Wake { id, .. } if Some(id) == self.wake => self.look(cx),
            Input::Event { port, .. } => Err(NodeError::unknown_port(port)),
            Input::Stop | Input::Trigger { .. } | Input::Wake { .. } | Input::Reply { .. } => Ok(()),
            other => Err(NodeError::unhandled(&other)),
        }
    }
}
register_node!(Schedule);

// ── cond.time_between ──────────────────────────────────────────────────────────────────────────────────────────

/// Parameters of `cond.time_between`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BetweenParams {
    /// From this local time, `HH:MM`, included.
    pub from: String,
    /// To this local time, `HH:MM`, not included; earlier than `from` runs over midnight.
    pub to: String,
    /// The days it holds on; after midnight belongs to the day the range started.
    #[serde(default = "every_day")]
    pub days: Vec<Day>,
    /// The IANA time zone, such as `Europe/London`; the system's when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tz: Option<String>,
}

impl Default for BetweenParams {
    fn default() -> Self {
        Self { from: "09:00".to_owned(), to: "17:00".to_owned(), days: every_day(), tz: None }
    }
}

/// Whether the local time is in a daily range.
#[derive(Debug)]
pub struct Between {
    from: Time,
    to: Time,
    days: Days,
    tz: TimeZone,
}

impl NodeKind for Between {
    type Params = BetweenParams;

    fn meta() -> NodeMeta {
        NodeMeta::new(
            "cond.time_between",
            "Whether it is between two times of day.",
            NodeClass::Pure,
            Category::Conditions,
        )
    }

    fn ports(_: &BetweenParams) -> Ports {
        Ports::new().live_out("value", PortType::Bool, "Whether the local time is in the range.")
    }

    fn check(params: &BetweenParams) -> Result<(), ParamErrors> {
        let from = parse_time(&params.from).err().map(|text| ParamError::new("from", text));
        let to = parse_time(&params.to).err().map(|text| ParamError::new("to", text));
        errors(
            from.into_iter()
                .chain(to)
                .chain(days_error(&params.days))
                .chain(zone_error(params.tz.as_deref()))
                .collect(),
        )
    }

    fn new(params: BetweenParams) -> Self {
        Self {
            from: parse_time(&params.from).unwrap_or(Time::midnight()),
            to: parse_time(&params.to).unwrap_or(Time::midnight()),
            days: Days::of(&params.days),
            tz: zone_or_utc(params.tz.as_deref()),
        }
    }
}

impl Between {
    fn holds(&self, at: Timestamp) -> bool {
        let local = at.to_zoned(self.tz.clone());
        let (date, time) = (local.date(), local.time());
        if self.from <= self.to {
            // `from == to` is the whole day.
            self.days.has(date) && (self.from == self.to || (self.from <= time && time < self.to))
        } else {
            (time >= self.from && self.days.has(date))
                || (time < self.to && date.yesterday().is_ok_and(|d| self.days.has(d)))
        }
    }
}

impl Node for Between {
    fn handle(&mut self, input: Input<'_>, _: &mut NodeCx<'_>) -> Result<(), NodeError> {
        pure_handle(&input)
    }

    fn eval(&self, port: &str, cx: &mut EvalCx<'_>) -> Result<Val, NodeError> {
        match port {
            "value" => Ok(Val::Bool(self.holds(timestamp(cx.utc())))),
            other => Err(NodeError::not_live(other)),
        }
    }
}
register_node!(Between);

// ── cond.date ──────────────────────────────────────────────────────────────────────────────────────────────────

/// A date bound: `MM-DD` every year, or `YYYY-MM-DD` once.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Bound {
    Yearly(i8, i8),
    Once(Date),
}

/// A `MM-DD` or `YYYY-MM-DD` date, or why it isn't one.
fn parse_bound(text: &str) -> Result<Bound, Text> {
    let bad = || l10n::bad_date(text);
    match text.len() {
        // A leap year, so 02-29 is a date.
        5 => format!("2000-{text}").parse::<Date>().map(|d| Bound::Yearly(d.month(), d.day())).map_err(|_| bad()),
        10 => text.parse::<Date>().map(Bound::Once).map_err(|_| bad()),
        _ => Err(bad()),
    }
}

/// Parameters of `cond.date`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DateParams {
    /// The first day, included: `MM-DD` repeats every year, `YYYY-MM-DD` doesn't.
    pub from: String,
    /// The last day, included, in the same form as `from`; with `MM-DD`, earlier than `from` runs over the new year.
    pub to: String,
    /// The IANA time zone, such as `Europe/London`; the system's when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tz: Option<String>,
}

impl Default for DateParams {
    fn default() -> Self {
        Self { from: "12-24".to_owned(), to: "12-26".to_owned(), tz: None }
    }
}

/// Whether today is in a range of dates.
#[derive(Debug)]
pub struct InDates {
    from: Bound,
    to: Bound,
    tz: TimeZone,
}

impl NodeKind for InDates {
    type Params = DateParams;

    fn meta() -> NodeMeta {
        NodeMeta::new("cond.date", "Whether today is in a range of dates.", NodeClass::Pure, Category::Conditions)
    }

    fn ports(_: &DateParams) -> Ports {
        Ports::new().live_out("value", PortType::Bool, "Whether today's local date is in the range.")
    }

    fn check(params: &DateParams) -> Result<(), ParamErrors> {
        let mut found = Vec::new();
        let from = parse_bound(&params.from).map_err(|text| found.push(ParamError::new("from", text)));
        let to = parse_bound(&params.to).map_err(|text| found.push(ParamError::new("to", text)));
        match (from, to) {
            (Ok(Bound::Once(from)), Ok(Bound::Once(to))) if from > to => {
                found.push(ParamError::new("to", l10n::dates_backwards()));
            }
            (Ok(Bound::Once(_)), Ok(Bound::Yearly(..))) | (Ok(Bound::Yearly(..)), Ok(Bound::Once(_))) => {
                found.push(ParamError::new("to", l10n::date_forms_differ()));
            }
            _ => {}
        }
        found.extend(zone_error(params.tz.as_deref()));
        errors(found)
    }

    fn new(params: DateParams) -> Self {
        let fallback = Bound::Yearly(1, 1);
        Self {
            from: parse_bound(&params.from).unwrap_or(fallback),
            to: parse_bound(&params.to).unwrap_or(fallback),
            tz: zone_or_utc(params.tz.as_deref()),
        }
    }
}

impl InDates {
    fn holds(&self, at: Timestamp) -> bool {
        let today = at.to_zoned(self.tz.clone()).date();
        match (self.from, self.to) {
            (Bound::Once(from), Bound::Once(to)) => from <= today && today <= to,
            (Bound::Yearly(fm, fd), Bound::Yearly(tm, td)) => {
                let (day, from, to) = ((today.month(), today.day()), (fm, fd), (tm, td));
                if from <= to { from <= day && day <= to } else { day >= from || day <= to }
            }
            _ => false,
        }
    }
}

impl Node for InDates {
    fn handle(&mut self, input: Input<'_>, _: &mut NodeCx<'_>) -> Result<(), NodeError> {
        pure_handle(&input)
    }

    fn eval(&self, port: &str, cx: &mut EvalCx<'_>) -> Result<Val, NodeError> {
        match port {
            "value" => Ok(Val::Bool(self.holds(timestamp(cx.utc())))),
            other => Err(NodeError::not_live(other)),
        }
    }
}
register_node!(InDates);
