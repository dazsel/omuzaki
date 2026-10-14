//! Proportions annotation: head-unit rulers over a turnaround and the readout of the measured ratios.

use omuzaki_anim_api::HumanoidBone;
use omuzaki_char_api::{BodyFit, JointRef};
use omuzaki_core::colour::Colour;

use super::{Align, LayoutError, Rect, SheetLayout, TextStyle, Weight};

/// The most head units a figure may have: past this the rulers would be closer than a pixel.
const MAX_HEADS: f32 = 64.0;
/// How opaque a ruler is, so it reads on the paper without competing with the art.
const RULER_ALPHA: u8 = 120;

/// A figure's measured proportions.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Proportions {
    /// Height in heads: body height divided by head height.
    pub heads: f32,
    /// Shoulder width divided by hip width.
    pub shoulder_to_hip: f32,
    /// Leg length (hip joint to the ground) divided by torso length (hip joint to the neck).
    pub leg_to_torso: f32,
}

/// Where a figure stands on the page: the rows of the top of its head and of its soles, shared by every view of a
/// turnaround because the views are rendered with one camera.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct RulerSpan {
    /// Row of the top of the head.
    pub top: f32,
    /// Row of the soles.
    pub bottom: f32,
}

/// One ruler: the line `index` head units below the top of the head.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Ruler {
    /// 0 is the top of the head, 1 the chin line of a one-head-tall head, and so on.
    pub index: u32,
    /// The row it was drawn on (`top + index * head unit`); the line's pixel row is this rounded.
    pub y: f32,
}

impl Proportions {
    /// Measures a body from its joints ([`BodyFit::joint`], bind space, metres, standing on `y = 0` as VRM does).
    ///
    /// Shoulder and hip widths are the distances between the left and right upper-arm and upper-leg joints; the leg is
    /// the hips joint's height and the torso is the rise from there to the neck. `height` is the body's total height
    /// and `head_height` the head's, which the joints cannot give (no joint marks the crown).
    ///
    /// # Errors
    /// [`LayoutError::MissingJoint`] when the fit lacks one of the joints, [`LayoutError::Proportions`] when a length
    /// is not a positive finite number.
    pub fn from_body(fit: &dyn BodyFit, height: f32, head_height: f32) -> Result<Self, LayoutError> {
        let joint = |bone: HumanoidBone| {
            fit.joint(&JointRef::Humanoid(bone)).ok_or(LayoutError::MissingJoint { joint: bone.name() })
        };
        let hips = joint(HumanoidBone::Hips)?;
        let neck = joint(HumanoidBone::Neck)?;
        let shoulders = (joint(HumanoidBone::LeftUpperArm)?.x - joint(HumanoidBone::RightUpperArm)?.x).abs();
        let hip_width = (joint(HumanoidBone::LeftUpperLeg)?.x - joint(HumanoidBone::RightUpperLeg)?.x).abs();
        let (leg, torso) = (hips.y, neck.y - hips.y);
        for (what, v) in [
            ("height", height),
            ("head height", head_height),
            ("shoulder width", shoulders),
            ("hip width", hip_width),
            ("leg length", leg),
            ("torso length", torso),
        ] {
            if !(v.is_finite() && v > 0.0) {
                return Err(LayoutError::Proportions {
                    reason: format!("{what} is {v}; it must be a positive number"),
                });
            }
        }
        Ok(Self { heads: height / head_height, shoulder_to_hip: shoulders / hip_width, leg_to_torso: leg / torso })
    }

    /// The rulers for a figure spanning `span`: one at the top of the head and one per whole head unit below it.
    ///
    /// # Errors
    /// [`LayoutError::Proportions`] when `heads` is outside `(0, 64]` or the span is not top above bottom.
    pub fn rulers(&self, span: RulerSpan) -> Result<Vec<Ruler>, LayoutError> {
        let ok = self.heads.is_finite() && self.heads > 0.0 && self.heads <= MAX_HEADS;
        let span_ok = span.top.is_finite() && span.bottom.is_finite() && span.bottom > span.top;
        if !(ok && span_ok) {
            return Err(LayoutError::Proportions {
                reason: format!("{} heads over rows {}..{} cannot be ruled", self.heads, span.top, span.bottom),
            });
        }
        let unit = (span.bottom - span.top) / self.heads;
        // Whole units only; a tiny allowance so 7.0 heads computed as 6.9999998 still gets its seventh line.
        let whole = (self.heads + 1e-4).floor() as u32;
        Ok((0..=whole).map(|index| Ruler { index, y: span.top + index as f32 * unit }).collect())
    }
}

impl SheetLayout {
    /// Draws the head-unit rulers across `x_range` for a figure spanning `span`, numbering them `1..N` in the gutter on
    /// the left, and marks the soles with the measured height. Returns the rulers drawn.
    ///
    /// Rulers are faint dashed lines in the accent colour; draw them **before** the figures so the art covers them.
    ///
    /// # Errors
    /// As [`Proportions::rulers`].
    pub fn rulers(
        &mut self,
        proportions: &Proportions,
        span: RulerSpan,
        x_range: (i32, i32),
    ) -> Result<Vec<Ruler>, LayoutError> {
        let rulers = proportions.rulers(span)?;
        let styles = self.styles();
        let accent = self.theme.accent;
        let faint = Colour { a: RULER_ALPHA, ..accent };
        let numeral = TextStyle { colour: accent, weight: Weight::SemiBold, ..styles.small };
        let cap = self.text_metrics(&numeral).cap_height.round() as i32;
        let (x0, x1) = x_range;
        let thickness = self.metrics.hairline;
        let tick = self.metrics.gap / 2;
        let dash = self.metrics.gap;
        let numeral_x = (x0 - tick - self.metrics.gap / 3) as f32;

        let line_at = |this: &mut Self, y: f32, label: Option<String>, solid: bool| {
            let row = y.round() as i32 - thickness / 2;
            if solid {
                this.canvas.fill_rect(Rect::new(x0 - tick, row, x1 - x0 + tick, thickness), faint);
            } else {
                this.canvas.fill_rect(Rect::new(x0 - tick, row, tick, thickness), accent);
                let mut x = x0;
                // A dash that would be cut to a speck at the end is left out.
                while x1 - x >= dash / 2 {
                    this.canvas.fill_rect(Rect::new(x, row, dash.min(x1 - x), thickness), faint);
                    x += dash * 2;
                }
            }
            if let Some(label) = label {
                this.text_on_baseline(&label, &numeral, (numeral_x, y.round() as i32 + cap / 2), Align::Right, None);
            }
        };

        for ruler in &rulers {
            let label = (ruler.index > 0).then(|| ruler.index.to_string());
            line_at(self, ruler.y, label, ruler.index == 0);
        }
        // The soles close the figure. When the height is a whole number of heads that is the last numbered ruler.
        let last = rulers.last().map_or(span.top, |r| r.y);
        if (span.bottom - last).abs() >= 1.0 {
            line_at(self, span.bottom, Some(format!("{:.1}", proportions.heads)), true);
        }
        Ok(rulers)
    }

    /// The width [`proportions_readout`](Self::proportions_readout) needs, so a heading can leave room for it.
    pub fn proportions_readout_width(&self, proportions: &Proportions) -> i32 {
        let styles = self.styles();
        let items = readout_items(proportions);
        let gap = self.metrics.gap as f32 * 1.6;
        let width: f32 = items.iter().map(|(label, value)| self.item_width(label, value, &styles)).sum::<f32>()
            + gap * (items.len() - 1) as f32;
        width.ceil() as i32
    }

    /// Draws the measured ratios right-aligned on `baseline` inside `row`: `HEIGHT 7.5 heads`, `SHOULDER : HIP 1.18`,
    /// `LEG : TORSO 1.42`.
    pub fn proportions_readout(&mut self, row: Rect, baseline: i32, proportions: &Proportions) {
        let styles = self.styles();
        let value_style = TextStyle { weight: Weight::SemiBold, colour: self.theme.accent, ..styles.value };
        let gap = self.metrics.gap as f32 * 1.6;
        let inner_gap = self.em_px(&styles.kicker) * 0.7;
        let mut x = row.right() as f32 - self.proportions_readout_width(proportions) as f32;
        for (label, value) in readout_items(proportions) {
            let label_w = self.text_on_baseline(label, &styles.kicker, (x, baseline), Align::Left, None);
            x += label_w + inner_gap;
            x += self.text_on_baseline(&value, &value_style, (x, baseline), Align::Left, None) + gap;
        }
    }

    fn item_width(&self, label: &str, value: &str, styles: &super::Styles) -> f32 {
        let value_style = TextStyle { weight: Weight::SemiBold, ..styles.value };
        self.text_width(label, &styles.kicker) + self.em_px(&styles.kicker) * 0.7 + self.text_width(value, &value_style)
    }
}

fn readout_items(p: &Proportions) -> [(&'static str, String); 3] {
    [
        ("Height", format!("{:.1} heads", p.heads)),
        ("Shoulder : hip", format!("{:.2}", p.shoulder_to_hip)),
        ("Leg : torso", format!("{:.2}", p.leg_to_torso)),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use omuzaki_char_api::JointFit;
    use omuzaki_core::glam::Vec3;

    fn body() -> JointFit {
        let at = |bone, x, y| (JointRef::Humanoid(bone), Vec3::new(x, y, 0.0));
        JointFit::new([
            at(HumanoidBone::Hips, 0.0, 0.9),
            at(HumanoidBone::Neck, 0.0, 1.35),
            at(HumanoidBone::LeftUpperArm, 0.18, 1.3),
            at(HumanoidBone::RightUpperArm, -0.18, 1.3),
            at(HumanoidBone::LeftUpperLeg, 0.09, 0.88),
            at(HumanoidBone::RightUpperLeg, -0.09, 0.88),
        ])
    }

    #[test]
    fn ratios_come_from_the_joints() {
        let p = Proportions::from_body(&body(), 1.6, 0.2).unwrap();
        assert!((p.heads - 8.0).abs() < 1e-5);
        assert!((p.shoulder_to_hip - 2.0).abs() < 1e-5, "0.36 / 0.18");
        assert!((p.leg_to_torso - 2.0).abs() < 1e-5, "0.9 / 0.45");
    }

    #[test]
    fn a_missing_joint_is_named() {
        let err = Proportions::from_body(&JointFit::default(), 1.6, 0.2).unwrap_err();
        assert_eq!(err, LayoutError::MissingJoint { joint: "hips" });
        assert!(Proportions::from_body(&body(), 1.6, 0.0).is_err(), "no head height");
    }

    #[test]
    fn rulers_divide_the_figure_into_head_units() {
        let p = Proportions { heads: 7.5, shoulder_to_hip: 1.0, leg_to_torso: 1.0 };
        let rulers = p.rulers(RulerSpan { top: 100.0, bottom: 850.0 }).unwrap();
        assert_eq!(rulers.len(), 8, "the crown and seven whole heads");
        assert!((rulers[1].y - 200.0).abs() < 1e-4 && (rulers[7].y - 800.0).abs() < 1e-4);
        let whole = Proportions { heads: 8.0, ..p };
        assert_eq!(whole.rulers(RulerSpan { top: 0.0, bottom: 800.0 }).unwrap().len(), 9, "the soles are ruler 8");
    }

    #[test]
    fn nonsense_spans_are_errors() {
        let p = Proportions { heads: 7.5, shoulder_to_hip: 1.0, leg_to_torso: 1.0 };
        assert!(p.rulers(RulerSpan { top: 10.0, bottom: 10.0 }).is_err());
        assert!(Proportions { heads: 0.0, ..p }.rulers(RulerSpan { top: 0.0, bottom: 10.0 }).is_err());
        assert!(Proportions { heads: f32::NAN, ..p }.rulers(RulerSpan { top: 0.0, bottom: 10.0 }).is_err());
        assert!(Proportions { heads: 1000.0, ..p }.rulers(RulerSpan { top: 0.0, bottom: 10.0 }).is_err());
    }
}
