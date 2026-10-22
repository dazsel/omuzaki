//! The Channels panel of the Perform workspace (`docs/design/ux/shell.md` section 12, element 5): every animation
//! channel's live value as a meter, with the chip of who drives it (tracker, audio, score, hotkey, life layer: PF-23),
//! grouped Face, Head and Body, with a search.
//!
//! It reads the perform thread's newest frame through a [`ChannelFeed`] (a [`FrameReader`] clone the app puts in the
//! shell's views), never through an op: the values change 60 times a second and are not the document. It samples the
//! feed at most 30 times a second and, once warm, allocates nothing per frame: the names are made once per locale, the
//! list on show only when the group or the search changes, and the meters are drawn from a fixed array.

use egui::{RichText, ScrollArea, Ui};
use omuzaki_anim_api::{CHANNEL_CAPACITY, CHANNELS, ChannelMask, Source};

use super::text::{Word, channel_name, say, text};
use super::wiring::FrameReader;
use crate::ds::icons;
use crate::ds::theme::ds;
use crate::ds::widgets::{Channel, EmptyState, MeterGrid, Owner, Segment, Segmented, TextField};

/// The panel samples the feed at most this often.
pub const SAMPLE_HZ: f64 = 30.0;

/// The perform thread's frames, for the Channels panel: a view model the app inserts into the shell.
#[derive(Clone, Debug)]
pub struct ChannelFeed(FrameReader);

impl ChannelFeed {
    /// A feed over `reader`, a clone of the app's own reader (each clone keeps its own place).
    pub const fn new(reader: FrameReader) -> Self {
        Self(reader)
    }
}

/// Which channels the panel shows.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Group {
    /// Brows, eyes, gaze, mouth, the anime extras and the custom slots.
    #[default]
    Face,
    /// Head angles and shift.
    Head,
    /// Lean, body turn and breath.
    Body,
}

impl Group {
    /// The channels in the group.
    pub fn mask(self) -> ChannelMask {
        match self {
            Self::Face => ChannelMask::ALL.without(ChannelMask::HEAD).without(ChannelMask::BODY),
            Self::Head => ChannelMask::HEAD,
            Self::Body => ChannelMask::BODY,
        }
    }

    const fn word(self) -> Word {
        match self {
            Self::Face => Word::ChannelsFace,
            Self::Head => Word::ChannelsHead,
            Self::Body => Word::ChannelsBody,
        }
    }
}

/// The meter's owner chip for a bus source.
pub const fn owner_of(source: Source) -> Owner {
    match source {
        Source::Rest => Owner::Rest,
        Source::Score => Owner::Score,
        Source::Tracker => Owner::Tracker,
        Source::Audio => Owner::Audio,
        Source::Trigger => Owner::Hotkey,
        Source::Life => Owner::LifeLayer,
    }
}

/// What the panel last sampled from the feed, by the channel's place in [`CHANNELS`].
#[derive(Clone, Copy, Debug)]
pub struct Sample {
    /// Each channel's value, normalised to 0..=1 over its legal range.
    pub values: [f32; CHANNEL_CAPACITY],
    /// Who drove each channel.
    pub owners: [Owner; CHANNEL_CAPACITY],
}

/// The Channels panel's state.
#[derive(Debug)]
pub struct ChannelsPanel {
    group: Group,
    query: String,
    /// Each channel's shown name, by its place in [`CHANNELS`], in the locale `names_locale`.
    names: Vec<String>,
    names_locale: String,
    /// The places in [`CHANNELS`] of the channels on show, for `listed_for`.
    listed: Vec<usize>,
    listed_for: Option<(Group, String)>,
    sample: Option<Sample>,
    /// When the feed was last sampled, on egui's clock.
    sampled_at: Option<f64>,
    samples: u64,
}

impl Default for ChannelsPanel {
    fn default() -> Self {
        Self::new()
    }
}

impl ChannelsPanel {
    /// The panel on the Face group, with nothing sampled yet.
    pub fn new() -> Self {
        Self {
            group: Group::Face,
            query: String::new(),
            names: Vec::with_capacity(CHANNELS.len()),
            names_locale: String::new(),
            listed: Vec::with_capacity(CHANNELS.len()),
            listed_for: None,
            sample: None,
            sampled_at: None,
            samples: 0,
        }
    }

    /// How many times the panel has read the feed: at most [`SAMPLE_HZ`] a second of egui's clock.
    pub const fn samples(&self) -> u64 {
        self.samples
    }

    /// The newest sample, if any.
    pub const fn sample(&self) -> Option<&Sample> {
        self.sample.as_ref()
    }

    /// The group on show.
    pub const fn group(&self) -> Group {
        self.group
    }

    /// Shows `group`, as its control does.
    pub fn set_group(&mut self, group: Group) {
        self.group = group;
    }

    /// How many channels are on show, for the group and the search.
    pub fn listed(&self) -> usize {
        self.listed.len()
    }

    /// Brings the panel up to date at `now` (seconds on egui's clock): the names when the locale changed, the list when
    /// the group or the search did, and a sample of `feed` when one is due. Once warm, it allocates nothing.
    pub fn update(&mut self, ctx: &egui::Context, now: f64, feed: Option<&ChannelFeed>) {
        self.refresh_names(ctx);
        self.refresh_list();
        if let Some(feed) = feed {
            self.sample_feed(now, feed);
        }
    }

    /// Draws the panel, reading `feed` when one is given.
    pub fn ui(&mut self, ui: &mut Ui, feed: Option<&ChannelFeed>) {
        let ctx = ui.ctx().clone();
        let face = text(&ctx, Word::ChannelsFace);
        let head = text(&ctx, Word::ChannelsHead);
        let body = text(&ctx, Word::ChannelsBody);
        let segments =
            [Segment::new(Group::Face, &face), Segment::new(Group::Head, &head), Segment::new(Group::Body, &body)];
        let group = text(&ctx, Word::ChannelsGroup);
        ui.add(Segmented::new(&group, &mut self.group, &segments).id_salt("live.channels.group").stretch());
        let search = text(&ctx, Word::ChannelsSearch);
        ui.add(TextField::new(&search, &mut self.query).search().hint(&search).id_salt("live.channels.search"));
        ui.add_space(4.0);

        let now = ui.input(|i| i.time);
        self.update(&ctx, now, feed);
        if feed.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_secs_f64(1.0 / SAMPLE_HZ));
        }
        let Some(sample) = self.sample else {
            EmptyState::new(icons::GAUGE, &text(&ctx, Word::ChannelsIdle), &text(&ctx, Word::ChannelsIdleLine))
                .show(ui);
            return;
        };
        if self.listed.is_empty() {
            EmptyState::new(
                icons::MAGNIFYING_GLASS,
                &text(&ctx, Word::ChannelsNone),
                &text(&ctx, Word::ChannelsNoneLine),
            )
            .show(ui);
            return;
        }
        let mut cells = [Channel::new("", 0.0); CHANNEL_CAPACITY];
        for (cell, &place) in cells.iter_mut().zip(&self.listed) {
            let name = self.names.get(place).map_or("", String::as_str);
            *cell = Channel { owner: sample.owners[place], ..Channel::new(name, sample.values[place]) };
        }
        let cells = &cells[..self.listed.len().min(CHANNEL_CAPACITY)];
        let grid = say(&ctx, Word::ChannelsGrid, &[("group", &text(&ctx, self.group.word()))]);
        let legend = text(&ctx, Word::ChannelsLegend);
        let small = ds(&ctx).text.small.clone();
        ScrollArea::vertical().id_salt("live.channels.scroll").auto_shrink([false, false]).animated(false).show(
            ui,
            |ui| {
                MeterGrid::new(&grid, cells).columns(1).values(true).show(ui);
                ui.add_space(4.0);
                ui.label(RichText::new(&*legend).font(small));
            },
        );
    }

    /// Makes the names again when the locale changed since they were made.
    fn refresh_names(&mut self, ctx: &egui::Context) {
        let ds = ds(ctx);
        let locale = ds.strings.locale();
        if self.names.len() == CHANNELS.len() && self.names_locale == locale {
            return;
        }
        self.names_locale.clear();
        self.names_locale.push_str(locale);
        self.names = CHANNELS.iter().map(|info| channel_name(ctx, info.name)).collect();
        // The search matches the names, which have changed.
        self.listed_for = None;
    }

    /// Lists the channels of the group whose names hold the search, when either changed.
    fn refresh_list(&mut self) {
        if self.listed_for.as_ref().is_some_and(|(group, query)| *group == self.group && *query == self.query) {
            return;
        }
        let mask = self.group.mask();
        let needle = self.query.trim().to_lowercase();
        self.listed.clear();
        self.listed.extend(CHANNELS.iter().enumerate().filter_map(|(place, info)| {
            let named = needle.is_empty() || self.names.get(place).is_some_and(|n| n.to_lowercase().contains(&needle));
            (mask.contains(info.channel) && named).then_some(place)
        }));
        self.listed_for = Some((self.group, self.query.clone()));
    }

    /// Reads the feed's newest frame, at most [`SAMPLE_HZ`] a second.
    fn sample_feed(&mut self, now: f64, feed: &ChannelFeed) {
        let period = 1.0 / SAMPLE_HZ;
        // A little slack, so a 60 Hz frame clock that lands a hair early still samples on every other frame.
        if self.sampled_at.is_some_and(|at| now - at < period - 1e-3) {
            return;
        }
        let Some(frame) = feed.0.latest() else {
            return;
        };
        self.sampled_at = Some(now);
        self.samples += 1;
        let sample = self
            .sample
            .get_or_insert(Sample { values: [0.0; CHANNEL_CAPACITY], owners: [Owner::Rest; CHANNEL_CAPACITY] });
        for (place, info) in CHANNELS.iter().enumerate().take(CHANNEL_CAPACITY) {
            let value = frame.channels.get(info.channel);
            let span = info.max - info.min;
            sample.values[place] = if span > 0.0 { ((value - info.min) / span).clamp(0.0, 1.0) } else { 0.0 };
            sample.owners[place] = owner_of(frame.owners.of(info.channel));
        }
    }
}
