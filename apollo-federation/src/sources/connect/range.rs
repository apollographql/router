//! Helpers for working with [`Range`]

use std::cmp::max;
use std::cmp::min;
use std::ops::Range;

pub(super) trait RangeExt {
    /// Narrow a range by applying another range whose values are relative to the start of this
    /// range. The result is cropped to the original range - that is, it cannot extend the original
    /// range. The original range also cannot be narrowed to zero - if the new range would be
    /// empty, the original range is returned. If the other range is `None`, the original range is
    /// not modified.
    fn narrow(&self, other: Option<&Self>) -> Self;
}

impl RangeExt for Range<usize> {
    fn narrow(&self, other: Option<&Self>) -> Self {
        if let Some(other) = other {
            // Normalize inputs to ensure start < end
            let normalized_other = if other.start < other.end {
                other
            } else {
                &(other.end..other.start)
            };
            let normalized_self = if self.start < self.end {
                self
            } else {
                &(self.end..self.start)
            };

            // Check for overflow
            let other_start = if normalized_other.start > usize::MAX - normalized_self.start {
                usize::MAX
            } else {
                normalized_other.start + normalized_self.start
            };
            let other_end = if normalized_other.end > usize::MAX - normalized_self.start {
                usize::MAX
            } else {
                normalized_other.end + normalized_self.start
            };

            // Narrow the range
            let start = max(normalized_self.start, other_start);
            let end = min(normalized_self.end, other_end);
            if start >= end {
                self.clone()
            } else {
                start..end
            }
        } else {
            self.clone()
        }
    }
}

#[cfg(test)]
mod test {
    use rstest::rstest;

    use super::*;

    #[rstest]
    #[case(0..10, Some(0..5), 0..5)]
    #[case(0..10, Some(5..15), 5..10)]
    #[case(0..10, Some(15..25), 0..10)]
    #[case(50..100, Some(25..50), 75..100)]
    #[case(50..100, Some(25..100), 75..100)]
    #[case(20..10, Some(2..4), 12..14)]
    #[case(20..10, Some(4..2), 12..14)]
    #[case(10..20, Some(4..2), 12..14)]
    #[case(0..10, Some(15..20), 0..10)]
    #[case(0..10, None, 0..10)]
    #[case(usize::MAX - 10..usize::MAX, Some(5..200), usize::MAX - 5..usize::MAX)]
    #[allow(clippy::reversed_empty_ranges)]
    fn test_narrow(
        #[case] range: Range<usize>,
        #[case] other: Option<Range<usize>>,
        #[case] expected: Range<usize>,
    ) {
        assert_eq!(range.narrow(other.as_ref()), expected);
    }
}
