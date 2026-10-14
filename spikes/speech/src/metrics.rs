//! Accuracy (word error rate) and latency statistics for the benchmark.

/// Lower-cases `text` and reduces it to bare words, so punctuation, capitals and curly quotes never count as errors.
///
/// Apostrophes stay inside words ("don't", "ass's"); every other non-alphanumeric character, hyphens included,
/// separates words ("she-goats" is two words), which is how Whisper tends to emit them as well.
#[must_use]
pub fn normalise_words(text: &str) -> Vec<String> {
    text.chars()
        .map(|c| match c {
            '\u{2019}' | '\u{2018}' => '\'',
            c if c.is_alphanumeric() || c == '\'' => c.to_ascii_lowercase(),
            _ => ' ',
        })
        .collect::<String>()
        .split_whitespace()
        // A quote used as a quotation mark ("'sire'") is not part of the word.
        .map(|w| fold_spelling(w.trim_matches('\'')).to_owned())
        .filter(|w| !w.is_empty())
        .collect()
}

/// Spelling variants that carry no recognition information, folded to one form on both sides of the comparison (the
/// idea behind Whisper's own English spelling normaliser). The 1912 translation used as reference is British and
/// Whisper writes American; an old text spells the interjection "O". The list stays tiny: a pair is added only when a
/// reference exposes it, so it cannot hide real errors.
fn fold_spelling(word: &str) -> &str {
    match word {
        "traveller" => "traveler",
        "belabouring" => "belaboring",
        "o" => "oh",
        other => other,
    }
}

/// One step of the best word alignment between a reference and a hypothesis.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Edit {
    /// The word matches.
    Match,
    /// The hypothesis has a different word here.
    Substitute,
    /// The hypothesis lacks a reference word.
    Delete,
    /// The hypothesis has an extra word.
    Insert,
}

/// Word error rate between a reference and a hypothesis, with the edit counts behind it.
#[derive(Clone, Debug, PartialEq)]
pub struct Wer {
    /// Words in the reference.
    pub reference_words: usize,
    /// Reference words replaced by another word.
    pub substitutions: usize,
    /// Reference words the hypothesis lacks.
    pub deletions: usize,
    /// Extra words in the hypothesis.
    pub insertions: usize,
    /// The alignment, one entry per step (matches included), for printing a diff.
    pub alignment: Vec<(Edit, Option<String>, Option<String>)>,
}

impl Wer {
    /// Total edits.
    #[must_use]
    pub fn errors(&self) -> usize {
        self.substitutions + self.deletions + self.insertions
    }

    /// `errors / reference_words`; 0.0 for an empty reference with an empty hypothesis.
    #[must_use]
    pub fn rate(&self) -> f64 {
        if self.reference_words == 0 {
            return if self.errors() == 0 { 0.0 } else { 1.0 };
        }
        self.errors() as f64 / self.reference_words as f64
    }
}

/// Computes the word error rate of `hypothesis` against `reference` (both normalised with [`normalise_words`]).
#[must_use]
pub fn wer(reference: &str, hypothesis: &str) -> Wer {
    let r = normalise_words(reference);
    let h = normalise_words(hypothesis);
    // Classic Levenshtein table; the transcripts are a few hundred words, so O(n*m) memory is fine.
    let mut cost = vec![vec![0usize; h.len() + 1]; r.len() + 1];
    for (i, row) in cost.iter_mut().enumerate() {
        row[0] = i;
    }
    for (j, c) in cost[0].iter_mut().enumerate() {
        *c = j;
    }
    for i in 1..=r.len() {
        for j in 1..=h.len() {
            let sub = cost[i - 1][j - 1] + usize::from(r[i - 1] != h[j - 1]);
            cost[i][j] = sub.min(cost[i - 1][j] + 1).min(cost[i][j - 1] + 1);
        }
    }
    let (mut i, mut j) = (r.len(), h.len());
    let mut alignment = Vec::new();
    let (mut substitutions, mut deletions, mut insertions) = (0, 0, 0);
    while i > 0 || j > 0 {
        if i > 0 && j > 0 && cost[i][j] == cost[i - 1][j - 1] + usize::from(r[i - 1] != h[j - 1]) {
            let matched = r[i - 1] == h[j - 1];
            substitutions += usize::from(!matched);
            let edit = if matched { Edit::Match } else { Edit::Substitute };
            alignment.push((edit, Some(r[i - 1].clone()), Some(h[j - 1].clone())));
            i -= 1;
            j -= 1;
        } else if i > 0 && cost[i][j] == cost[i - 1][j] + 1 {
            deletions += 1;
            alignment.push((Edit::Delete, Some(r[i - 1].clone()), None));
            i -= 1;
        } else {
            insertions += 1;
            alignment.push((Edit::Insert, None, Some(h[j - 1].clone())));
            j -= 1;
        }
    }
    alignment.reverse();
    Wer { reference_words: r.len(), substitutions, deletions, insertions, alignment }
}

/// Summary of a set of latency samples, in milliseconds.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LatencyStats {
    /// Number of samples.
    pub n: usize,
    /// Arithmetic mean.
    pub mean: f64,
    /// Median.
    pub p50: f64,
    /// 95th percentile (nearest rank).
    pub p95: f64,
    /// 99th percentile (nearest rank).
    pub p99: f64,
    /// Slowest sample.
    pub max: f64,
}

impl LatencyStats {
    /// Summarises `samples_ms`; `None` when there are no samples.
    #[must_use]
    pub fn from_samples(samples_ms: &[f64]) -> Option<Self> {
        if samples_ms.is_empty() {
            return None;
        }
        let mut sorted = samples_ms.to_vec();
        sorted.sort_by(f64::total_cmp);
        let rank = |p: f64| sorted[((p * sorted.len() as f64).ceil() as usize).clamp(1, sorted.len()) - 1];
        Some(Self {
            n: sorted.len(),
            mean: sorted.iter().sum::<f64>() / sorted.len() as f64,
            p50: rank(0.50),
            p95: rank(0.95),
            p99: rank(0.99),
            max: sorted[sorted.len() - 1],
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalise_strips_punctuation_and_case() {
        assert_eq!(
            normalise_words("\"Here is a dainty morsel,\" said the Ass's owner \u{2014} she-goats; 'sire'!"),
            ["here", "is", "a", "dainty", "morsel", "said", "the", "ass's", "owner", "she", "goats", "sire"]
        );
    }

    #[test]
    fn spelling_variants_fold_together() {
        assert_eq!(normalise_words("O traveller, belabouring"), normalise_words("Oh traveler belaboring"));
    }

    #[test]
    fn identical_text_has_zero_wer() {
        let w = wer("The quick brown fox.", "the quick, brown fox");
        assert_eq!((w.errors(), w.reference_words), (0, 4));
        assert!(w.rate().abs() < f64::EPSILON);
    }

    #[test]
    fn counts_each_edit_kind() {
        // sat->sit and the->very are substitutions.
        let w = wer("the cat sat on the mat", "the cat sit on very mat");
        assert_eq!((w.substitutions, w.deletions, w.insertions), (2, 0, 0));
        // "b" is deleted, "e" inserted.
        let w = wer("a b c d", "a c d e");
        assert_eq!((w.substitutions, w.deletions, w.insertions), (0, 1, 1));
        assert!((w.rate() - 0.5).abs() < 1e-12);
    }

    #[test]
    fn empty_reference_is_all_insertions() {
        let w = wer("", "hello there");
        assert_eq!((w.reference_words, w.insertions), (0, 2));
        assert!((w.rate() - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn latency_percentiles_use_nearest_rank() {
        let samples: Vec<f64> = (1..=100).map(f64::from).collect();
        let s = LatencyStats::from_samples(&samples).map(|s| (s.n, s.p50, s.p95, s.p99, s.max));
        assert_eq!(s, Some((100, 50.0, 95.0, 99.0, 100.0)));
        assert!(LatencyStats::from_samples(&[]).is_none());
    }
}
