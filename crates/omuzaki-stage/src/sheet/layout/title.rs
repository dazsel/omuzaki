//! The title block: the character's name large on the left, where the sheet came from on the right.

use std::fmt;

use omuzaki_core::time::{Clock, Seconds};

use super::{Align, Rect, SheetLayout};

const SECONDS_PER_DAY: f64 = 86_400.0;

/// A calendar date (proleptic Gregorian, UTC) for the title block.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct SheetDate {
    /// The year.
    pub year: i32,
    /// The month, 1 to 12.
    pub month: u8,
    /// The day of the month, 1 to 31.
    pub day: u8,
}

impl SheetDate {
    /// The date `seconds` after 1970-01-01T00:00:00Z.
    ///
    /// Uses Howard Hinnant's days-to-civil conversion, which is exact in integer maths. The title block never asks the
    /// wall clock (decision 0009): the date comes from whatever shot clock the caller holds, see
    /// [`from_clock`](Self::from_clock).
    pub fn from_unix_seconds(seconds: Seconds) -> Self {
        // A date this far out is not a date anyone means; saturate rather than overflow.
        let days = (seconds.0 / SECONDS_PER_DAY).floor().clamp(-1.0e9, 1.0e9) as i64;
        let z = days + 719_468;
        let era = z.div_euclid(146_097);
        let day_of_era = z.rem_euclid(146_097);
        let year_of_era = (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
        let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
        let mp = (5 * day_of_year + 2) / 153;
        let day = day_of_year - (153 * mp + 2) / 5 + 1;
        let month = if mp < 10 { mp + 3 } else { mp - 9 };
        let year = year_of_era + era * 400 + i64::from(month <= 2);
        // Month and day are in range by the algorithm; the year fits an i32 for the clamped day count.
        Self { year: year as i32, month: month as u8, day: day as u8 }
    }

    /// The date on a shot clock **that counts Unix seconds** (seconds since 1970-01-01T00:00:00Z).
    ///
    /// Most clocks do not: `omuzaki_core::time::Clock` counts from its own zero, so a `SystemClock` or a fresh
    /// `FakeClock` would print `1970-01-01`. Nothing seeds the clock with a date, so a caller that has no date for
    /// the shot passes `None` to [`TitleBlock::date`] and the title block leaves the field out rather than print a
    /// made-up one.
    pub fn from_clock(clock: &dyn Clock) -> Self {
        Self::from_unix_seconds(clock.now())
    }
}

impl fmt::Display for SheetDate {
    /// ISO 8601, `2026-10-12`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:04}-{:02}-{:02}", self.year, self.month, self.day)
    }
}

/// What the title block says.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TitleBlock {
    /// The small line above the name (`Character sheet`).
    pub kicker: String,
    /// The character's name.
    pub name: String,
    /// The body template it was built from.
    pub template: String,
    /// The variant shown.
    pub variant: String,
    /// The build's short hash.
    pub build: String,
    /// When it was built, from the shot clock ([`SheetDate::from_clock`]). `None` leaves the date out of the block: a
    /// missing date is better than a fake one.
    pub date: Option<SheetDate>,
}

impl TitleBlock {
    /// The labelled values on the right of the block, in order. The date is the last one and only present when known.
    pub fn meta(&self) -> Vec<(&'static str, String)> {
        let mut meta =
            vec![("Template", self.template.clone()), ("Variant", self.variant.clone()), ("Build", self.build.clone())];
        meta.extend(self.date.map(|date| ("Date", date.to_string())));
        meta
    }
}

impl SheetLayout {
    /// Draws the title block across the top of the content area, with a rule under it, and returns the area left
    /// below (also stored as [`content`](Self::content)).
    pub fn title_block(&mut self, title: &TitleBlock) -> Rect {
        let styles = self.styles();
        let area = self.content;
        let (kicker_cap, title_cap) = (
            self.text_metrics(&styles.kicker).cap_height.round() as i32,
            self.text_metrics(&styles.title).cap_height.round() as i32,
        );
        let kicker_baseline = area.y + kicker_cap;
        let title_baseline = kicker_baseline + (self.em_px(&styles.kicker) * 1.5).round() as i32 + title_cap;
        // The metadata sits on the name's baseline, each label just above its value.
        let value_cap = self.text_metrics(&styles.value).cap_height.round() as i32;
        let label_baseline = title_baseline - value_cap - (self.em_px(&styles.kicker) * 1.3).round() as i32;

        // Metadata, right-aligned as one block: each column is as wide as its wider line, so values never collide.
        let gap = self.metrics.gap as f32 * 1.6;
        let max_value = area.w as f32 * 0.2;
        let columns: Vec<_> = title
            .meta()
            .into_iter()
            .map(|(label, value)| {
                let value = self.fonts.fit(&value, &styles.value, self.em_px(&styles.value), max_value);
                let width = self.text_width(label, &styles.kicker).max(self.text_width(&value, &styles.value));
                (label, value, width)
            })
            .collect();
        let block_w: f32 = columns.iter().map(|c| c.2).sum::<f32>() + gap * (columns.len() - 1) as f32;
        let mut x = area.right() as f32 - block_w;
        for (label, value, width) in &columns {
            self.text_on_baseline(label, &styles.kicker, (x, label_baseline), Align::Left, None);
            self.text_on_baseline(value, &styles.value, (x, title_baseline), Align::Left, None);
            x += width + gap;
        }

        let name_room = area.w as f32 - block_w - gap;
        self.text_on_baseline(
            &title.kicker,
            &styles.kicker,
            (area.x as f32, kicker_baseline),
            Align::Left,
            Some(name_room),
        );
        self.text_on_baseline(
            &title.name,
            &styles.title,
            (area.x as f32, title_baseline),
            Align::Left,
            Some(name_room),
        );

        let rule_y = title_baseline + (self.em_px(&styles.title) * 0.55).round() as i32;
        let thickness = self.metrics.hairline * 2;
        let ink = self.theme.ink;
        self.canvas.fill_rect(Rect::new(area.x, rule_y, area.w, thickness), ink);
        let below = rule_y + thickness + self.metrics.gap;
        self.content = Rect::new(area.x, below, area.w, area.bottom() - below);
        self.content
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use omuzaki_core::time::FakeClock;

    #[test]
    fn dates_come_from_the_epoch_exactly() {
        let date = |s: f64| SheetDate::from_unix_seconds(Seconds(s)).to_string();
        assert_eq!(date(0.0), "1970-01-01");
        assert_eq!(date(86_399.9), "1970-01-01");
        assert_eq!(date(86_400.0), "1970-01-02");
        assert_eq!(date(951_782_400.0), "2000-02-29", "a leap day");
        assert_eq!(date(1_791_763_200.0), "2026-10-12");
        assert_eq!(date(-86_400.0), "1969-12-31");
    }

    fn block(date: Option<SheetDate>) -> TitleBlock {
        TitleBlock {
            kicker: "Character sheet".into(),
            name: "Mira".into(),
            template: "soft-sd".into(),
            variant: "Game face".into(),
            build: "9111d06".into(),
            date,
        }
    }

    #[test]
    fn a_known_date_is_the_last_field_and_an_unknown_one_is_left_out() {
        let with = block(Some(SheetDate { year: 2026, month: 10, day: 12 })).meta();
        assert_eq!(with.last(), Some(&("Date", "2026-10-12".to_owned())));
        assert_eq!(with.len(), 4);
        let without = block(None).meta();
        assert_eq!(without.len(), 3, "no date field, not a fake one");
        assert!(without.iter().all(|(label, _)| *label != "Date"));
        assert!(without.iter().all(|(_, value)| !value.contains("1970")));
    }

    #[test]
    fn a_title_block_without_a_date_draws_less_ink_and_still_fills_the_rule() {
        let page = crate::sheet::layout::Page::custom(1000, 300, 150.0).unwrap();
        let render = |date| {
            let mut sheet = SheetLayout::new(page, crate::sheet::layout::Theme::light()).unwrap();
            let below = sheet.title_block(&block(date));
            (sheet.finish(), below)
        };
        let ink = |img: &image::RgbaImage| img.pixels().filter(|p| p.0[0] < 200).count();
        let (with, below_with) = render(Some(SheetDate { year: 2026, month: 10, day: 12 }));
        let (without, below_without) = render(None);
        assert!(ink(&without) < ink(&with), "the date column is not drawn");
        assert_eq!(below_with, below_without, "the block is the same height either way");
    }

    #[test]
    fn the_date_follows_the_clock_it_is_given() {
        let clock = FakeClock::new(Seconds(1_791_763_200.0));
        assert_eq!(SheetDate::from_clock(&clock).to_string(), "2026-10-12");
        clock.advance(Seconds(86_400.0 * 20.0));
        assert_eq!(SheetDate::from_clock(&clock).to_string(), "2026-11-01");
    }
}
