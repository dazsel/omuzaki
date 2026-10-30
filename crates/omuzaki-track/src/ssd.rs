//! MediaPipe's single-shot detector decoding, for any of its SSD models: the anchors (`SsdAnchorsCalculator` with
//! fixed-size anchors), the box and keypoint decoding (`TensorsToDetectionsCalculator`, `reverse_output_order`) and the
//! weighted non-max suppression (`NonMaxSuppressionCalculator`, `WEIGHTED`). The face detector keeps its own copy for
//! its one model ([`crate::detector`]); the palm and pose detectors of [`crate::body`] share this one.

use crate::roi::BBox;

/// An SSD model's layout: its square input side, anchor strides, regression width and keypoint count.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct SsdLayout {
    /// Input side in pixels.
    pub input: usize,
    /// Anchor strides of the SSD layers; consecutive layers with one stride share a feature map.
    pub strides: &'static [usize],
    /// Values per anchor in the regression output: 4 box values, then two per keypoint, then possibly more.
    pub coords: usize,
}

impl SsdLayout {
    /// Anchor centres in normalised input coordinates. Every anchor is unit-sized (`fixed_anchor_size`), so only the
    /// centres matter; each feature-map cell holds two anchors per layer (aspect ratio 1 and the interpolated scale).
    pub fn anchor_centers(&self) -> Vec<(f32, f32)> {
        let mut anchors = Vec::new();
        let mut layer = 0;
        while layer < self.strides.len() {
            let stride = self.strides[layer];
            let mut same = 0;
            while layer + same < self.strides.len() && self.strides[layer + same] == stride {
                same += 1;
            }
            let cells = self.input.div_ceil(stride);
            for y in 0..cells {
                for x in 0..cells {
                    for _ in 0..same * 2 {
                        #[allow(clippy::cast_precision_loss)] // cell indices are tiny
                        anchors.push(((x as f32 + 0.5) / cells as f32, (y as f32 + 0.5) / cells as f32));
                    }
                }
            }
            layer += same;
        }
        anchors
    }
}

/// Raw scores are clamped before the sigmoid so it cannot overflow.
const SCORE_CLIP: f32 = 100.0;

/// One decoded detection with `K` keypoints, in normalised coordinates of the (letterboxed) model input.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Detection<const K: usize> {
    /// Index of the anchor it came from: the tie-break that makes an unstable sort order equal scores like a stable one.
    pub anchor: usize,
    /// Score after the sigmoid.
    pub score: f32,
    /// The box.
    pub bbox: BBox,
    /// The keypoints, `(x, y)`.
    pub keypoints: [(f32, f32); K],
}

pub(crate) fn sigmoid(x: f32) -> f32 {
    1.0 / (1.0 + (-x).exp())
}

/// Decodes raw outputs into `out` (cleared first): the detections scoring at least `min_score`.
#[allow(clippy::cast_precision_loss)] // input sides are small constants
pub fn decode<const K: usize>(
    layout: &SsdLayout,
    anchors: &[(f32, f32)],
    regressors: &[f32],
    scores: &[f32],
    min_score: f32,
    out: &mut Vec<Detection<K>>,
) {
    let scale = layout.input as f32;
    out.clear();
    for (i, &(ax, ay)) in anchors.iter().enumerate() {
        let Some(&raw_score) = scores.get(i) else { break };
        let score = sigmoid(raw_score.clamp(-SCORE_CLIP, SCORE_CLIP));
        if score < min_score {
            continue;
        }
        let Some(raw) = regressors.get(i * layout.coords..(i + 1) * layout.coords) else { break };
        let (xc, yc) = (raw[0] / scale + ax, raw[1] / scale + ay);
        let (w, h) = (raw[2] / scale, raw[3] / scale);
        let mut keypoints = [(0.0, 0.0); K];
        for (k, kp) in keypoints.iter_mut().enumerate() {
            *kp = (raw[4 + 2 * k] / scale + ax, raw[5 + 2 * k] / scale + ay);
        }
        out.push(Detection {
            anchor: i,
            score,
            bbox: BBox { xmin: xc - w / 2.0, ymin: yc - h / 2.0, width: w, height: h },
            keypoints,
        });
    }
}

/// Weighted non-max suppression: take the best box, average it (and its keypoints) with every box overlapping it by
/// more than `iou`, weighted by score, and repeat on what is left. `kept` (cleared first) receives the merged
/// detections, best first; `detections` is used up. Neither vector allocates once it has room for every anchor.
pub fn weighted_nms<const K: usize>(detections: &mut Vec<Detection<K>>, iou: f32, kept: &mut Vec<Detection<K>>) {
    detections.sort_unstable_by(|a, b| b.score.total_cmp(&a.score).then(a.anchor.cmp(&b.anchor)));
    kept.clear();
    while let Some(&best) = detections.first() {
        let mut total = 0.0f32;
        let (mut x0, mut y0, mut x1, mut y1) = (0.0f32, 0.0f32, 0.0f32, 0.0f32);
        let mut keypoints = [(0.0f32, 0.0f32); K];
        let mut remaining = 0;
        for read in 0..detections.len() {
            let d = detections[read];
            if read > 0 && d.bbox.iou(&best.bbox) <= iou {
                detections[remaining] = d;
                remaining += 1;
                continue;
            }
            total += d.score;
            x0 += d.bbox.xmin * d.score;
            y0 += d.bbox.ymin * d.score;
            x1 += (d.bbox.xmin + d.bbox.width) * d.score;
            y1 += (d.bbox.ymin + d.bbox.height) * d.score;
            for (kp, k) in keypoints.iter_mut().zip(&d.keypoints) {
                *kp = (kp.0 + k.0 * d.score, kp.1 + k.1 * d.score);
            }
        }
        detections.truncate(remaining);
        let (x0, y0, x1, y1) = (x0 / total, y0 / total, x1 / total, y1 / total);
        kept.push(Detection {
            anchor: best.anchor,
            score: best.score,
            bbox: BBox { xmin: x0, ymin: y0, width: x1 - x0, height: y1 - y0 },
            keypoints: keypoints.map(|(x, y)| (x / total, y / total)),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PALM: SsdLayout = SsdLayout { input: 192, strides: &[8, 16, 16, 16], coords: 18 };
    const POSE: SsdLayout = SsdLayout { input: 224, strides: &[8, 16, 32, 32, 32], coords: 12 };

    #[test]
    fn anchor_counts_match_the_models() {
        assert_eq!(PALM.anchor_centers().len(), 2016);
        let pose = POSE.anchor_centers();
        assert_eq!(pose.len(), 2254);
        // The pose detector's first layer: 28x28 cells, two anchors each.
        assert_eq!(pose[0], (0.5 / 28.0, 0.5 / 28.0));
        assert_eq!(pose[2], (1.5 / 28.0, 0.5 / 28.0));
        // Its last group: 7x7 cells, six anchors each, after 1568 + 392.
        assert_eq!(pose[1960], (0.5 / 7.0, 0.5 / 7.0));
        assert_eq!(pose[1965], pose[1960]);
    }

    #[test]
    fn decode_reads_boxes_and_keypoints_past_the_box() {
        let layout = SsdLayout { input: 100, strides: &[100], coords: 8 };
        let anchors = [(0.5, 0.5)];
        let reg = [10.0, -10.0, 20.0, 40.0, 5.0, 0.0, 0.0, 5.0];
        let mut out = Vec::new();
        decode::<2>(&layout, &anchors, &reg, &[5.0], 0.5, &mut out);
        let d = out[0];
        assert!((d.bbox.xmin - 0.5).abs() < 1e-6 && (d.bbox.ymin - 0.2).abs() < 1e-6);
        assert!((d.keypoints[0].0 - 0.55).abs() < 1e-6 && (d.keypoints[1].1 - 0.55).abs() < 1e-6);
        decode::<2>(&layout, &anchors, &reg, &[-5.0], 0.5, &mut out);
        assert!(out.is_empty(), "under the threshold");
        decode::<2>(&layout, &anchors, &reg[..4], &[5.0], 0.5, &mut out);
        assert!(out.is_empty(), "a short regressor row is no detection, never a panic");
    }

    #[test]
    fn nms_merges_overlaps_and_keeps_others_best_first() {
        let det = |score: f32, x: f32| Detection::<1> {
            anchor: 0,
            score,
            bbox: BBox { xmin: x, ymin: 0.0, width: 0.4, height: 0.4 },
            keypoints: [(x, 0.0)],
        };
        let mut input = vec![det(0.6, 0.62), det(0.9, 0.0), det(0.3, 0.02), det(0.8, 0.6)];
        let mut kept = Vec::new();
        weighted_nms(&mut input, 0.3, &mut kept);
        assert_eq!(kept.len(), 2);
        assert!((kept[0].score - 0.9).abs() < 1e-6);
        assert!((kept[0].bbox.xmin - 0.02 * 0.3 / 1.2).abs() < 1e-6);
        assert!((kept[1].keypoints[0].0 - (0.6 * 0.8 + 0.62 * 0.6) / 1.4).abs() < 1e-6);
    }
}
