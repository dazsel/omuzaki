//! Integer pixel geometry for the sheet grid: rectangles, weighted splits and grids.
//!
//! Everything on the page snaps to whole pixels so edges are crisp and cells line up; remainders from a division are
//! spread by rounding the *cumulative* edge, so no cell is ever more than a pixel off its ideal size and the cells
//! always tile the area exactly.

/// A rectangle in page pixels. `x`, `y` is the top-left corner; the sizes are never negative.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct Rect {
    /// Left edge.
    pub x: i32,
    /// Top edge.
    pub y: i32,
    /// Width.
    pub w: i32,
    /// Height.
    pub h: i32,
}

/// Which way a split runs.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum Axis {
    Columns,
    Rows,
}

/// Where a smaller box sits inside a larger one along the vertical axis.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum Anchor {
    /// Centred.
    #[default]
    Centre,
    /// Resting on the bottom edge: figures in a turnaround stand on one ground line.
    Bottom,
}

impl Rect {
    /// A rectangle; negative sizes become empty.
    pub const fn new(x: i32, y: i32, w: i32, h: i32) -> Self {
        Self { x, y, w: if w < 0 { 0 } else { w }, h: if h < 0 { 0 } else { h } }
    }

    /// The first column right of the rectangle.
    pub const fn right(self) -> i32 {
        self.x + self.w
    }

    /// The first row below the rectangle.
    pub const fn bottom(self) -> i32 {
        self.y + self.h
    }

    /// Whether the rectangle covers no pixel.
    pub const fn is_empty(self) -> bool {
        self.w == 0 || self.h == 0
    }

    /// The horizontal centre.
    pub fn centre_x(self) -> f32 {
        self.x as f32 + self.w as f32 / 2.0
    }

    /// The row at `t` of the way down, `t = 0` the top edge and `t = 1` the bottom edge: where a figure's top or soles
    /// sit when a render places them at a known fraction of its height.
    pub fn y_at(self, t: f32) -> f32 {
        self.y as f32 + self.h as f32 * t
    }

    /// The rectangle shrunk by `dx` on the left and right and `dy` on the top and bottom.
    #[must_use]
    pub const fn inset(self, dx: i32, dy: i32) -> Self {
        Self::new(self.x + dx, self.y + dy, self.w - 2 * dx, self.h - 2 * dy)
    }

    /// Splits off a strip `h` tall from the top: `(strip, the rest)`.
    pub const fn take_top(self, h: i32) -> (Self, Self) {
        let h = if h > self.h { self.h } else { h };
        (Self::new(self.x, self.y, self.w, h), Self::new(self.x, self.y + h, self.w, self.h - h))
    }

    /// Splits off a strip `h` tall from the bottom: `(the rest, strip)`.
    pub const fn take_bottom(self, h: i32) -> (Self, Self) {
        let h = if h > self.h { self.h } else { h };
        (Self::new(self.x, self.y, self.w, self.h - h), Self::new(self.x, self.bottom() - h, self.w, h))
    }

    /// Splits off a strip `w` wide from the left: `(strip, the rest)`.
    pub const fn take_left(self, w: i32) -> (Self, Self) {
        let w = if w > self.w { self.w } else { w };
        (Self::new(self.x, self.y, w, self.h), Self::new(self.x + w, self.y, self.w - w, self.h))
    }

    /// Columns in the ratio `weights`, `gap` pixels apart, tiling the rectangle exactly.
    pub fn columns(self, weights: &[f32], gap: i32) -> Vec<Self> {
        self.split(Axis::Columns, weights, gap)
    }

    /// Rows in the ratio `weights`, `gap` pixels apart, tiling the rectangle exactly.
    pub fn rows(self, weights: &[f32], gap: i32) -> Vec<Self> {
        self.split(Axis::Rows, weights, gap)
    }

    /// `cols x rows` equal cells in reading order (left to right, then top to bottom).
    pub fn grid(self, cols: usize, rows: usize, gap_x: i32, gap_y: i32) -> Vec<Self> {
        let columns = self.columns(&vec![1.0; cols], gap_x);
        self.rows(&vec![1.0; rows], gap_y)
            .into_iter()
            .flat_map(|row| columns.iter().map(move |c| Self::new(c.x, row.y, c.w, row.h)))
            .collect()
    }

    /// The largest box with the aspect of `w x h` that fits inside, centred across and placed per `anchor` down.
    /// It scales up as well as down, so a small image fills its cell.
    #[must_use]
    pub fn contain(self, w: u32, h: u32, anchor: Anchor) -> Self {
        if w == 0 || h == 0 || self.is_empty() {
            return Self::new(self.x, self.y, 0, 0);
        }
        // f64 so the ratio comparison is exact for every size a sheet can hold.
        let scale = (f64::from(self.w) / f64::from(w)).min(f64::from(self.h) / f64::from(h));
        let fit_w = ((f64::from(w) * scale).round() as i32).clamp(1, self.w);
        let fit_h = ((f64::from(h) * scale).round() as i32).clamp(1, self.h);
        let y = match anchor {
            Anchor::Centre => self.y + (self.h - fit_h) / 2,
            Anchor::Bottom => self.bottom() - fit_h,
        };
        Self::new(self.x + (self.w - fit_w) / 2, y, fit_w, fit_h)
    }

    fn split(self, axis: Axis, weights: &[f32], gap: i32) -> Vec<Self> {
        let total: f64 = weights.iter().map(|&w| f64::from(w.max(0.0))).sum();
        if weights.is_empty() || total <= 0.0 {
            return Vec::new();
        }
        let (start, len) = match axis {
            Axis::Columns => (self.x, self.w),
            Axis::Rows => (self.y, self.h),
        };
        let gaps = gap * (weights.len() as i32 - 1);
        let avail = f64::from((len - gaps).max(0));
        // Cumulative edges: cell `i` spans [edge(i), edge(i + 1) - gap).
        let mut cum = 0.0;
        let mut edges = vec![0_i32];
        for &w in weights {
            cum += f64::from(w.max(0.0));
            edges.push((avail * cum / total).round() as i32);
        }
        (0..weights.len())
            .map(|i| {
                let from = start + edges[i] + gap * i as i32;
                let size = edges[i + 1] - edges[i];
                match axis {
                    Axis::Columns => Self::new(from, self.y, size, self.h),
                    Axis::Rows => Self::new(self.x, from, self.w, size),
                }
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn columns_tile_the_area_exactly_whatever_the_remainder() {
        let area = Rect::new(10, 20, 1001, 50);
        let cols = area.columns(&[1.0, 1.0, 1.0], 7);
        assert_eq!(cols.len(), 3);
        assert_eq!(cols[0].x, 10);
        assert_eq!(cols[2].right(), area.right());
        for pair in cols.windows(2) {
            assert_eq!(pair[1].x - pair[0].right(), 7, "the gap between neighbours is exact");
        }
        let widths: Vec<_> = cols.iter().map(|c| c.w).collect();
        assert!(widths.iter().max().unwrap() - widths.iter().min().unwrap() <= 1, "{widths:?}");
    }

    #[test]
    fn weighted_rows_follow_their_ratio() {
        let rows = Rect::new(0, 0, 100, 1000).rows(&[3.0, 1.0], 0);
        assert_eq!((rows[0].h, rows[1].h), (750, 250));
        assert_eq!(rows[1].y, 750);
    }

    #[test]
    fn grid_is_row_major() {
        let cells = Rect::new(0, 0, 90, 40).grid(3, 2, 0, 0);
        assert_eq!(cells.len(), 6);
        assert_eq!((cells[1].x, cells[1].y), (30, 0));
        assert_eq!((cells[3].x, cells[3].y), (0, 20));
    }

    #[test]
    fn contain_keeps_the_aspect_and_honours_the_anchor() {
        let cell = Rect::new(0, 0, 200, 300);
        assert_eq!(cell.contain(100, 100, Anchor::Centre), Rect::new(0, 50, 200, 200));
        assert_eq!(cell.contain(100, 100, Anchor::Bottom), Rect::new(0, 100, 200, 200));
        assert_eq!(cell.contain(0, 5, Anchor::Centre).w, 0, "a degenerate image fits nothing");
    }

    #[test]
    fn strips_never_overshoot() {
        let (top, rest) = Rect::new(0, 0, 10, 10).take_top(25);
        assert_eq!((top.h, rest.h), (10, 0));
    }
}
