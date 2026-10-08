//! Immutable PCM blocks shared by the receive store, live cursor and batch jobs.
//! Cloning a recording clones block references, never the sample buffers.
use std::{ops::Range, sync::Arc};

#[derive(Clone, Debug)]
struct Block {
    samples: Arc<Vec<i16>>,
    range: Range<usize>,
    /// Exclusive logical end in this PCM view, for logarithmic cursor lookup.
    end: usize,
}

#[derive(Clone, Debug, Default)]
pub struct Pcm {
    blocks: Vec<Block>,
    len: usize,
}

impl Pcm {
    pub fn len(&self) -> usize {
        self.len
    }
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
    pub fn append(&mut self, other: &Self) {
        self.blocks.extend(other.blocks.iter().map(|block| Block {
            samples: block.samples.clone(),
            range: block.range.clone(),
            end: self.len + block.end,
        }));
        self.len += other.len;
    }
    pub fn slices(&self) -> impl Iterator<Item = &[i16]> {
        self.blocks.iter().map(|b| &b.samples[b.range.clone()])
    }
    pub fn chunks(&self, max_samples: usize) -> impl Iterator<Item = &[i16]> {
        assert!(max_samples > 0);
        self.slices().flat_map(move |s| s.chunks(max_samples))
    }
    pub fn iter(&self) -> impl Iterator<Item = &i16> + Clone {
        self.blocks
            .iter()
            .flat_map(|b| b.samples[b.range.clone()].iter())
    }
    /// A cursor view, including partial first/last blocks. No samples are copied.
    pub fn range(&self, range: Range<usize>) -> Self {
        assert!(range.start <= range.end && range.end <= self.len);
        let mut result = Self::default();
        let first = self
            .blocks
            .partition_point(|block| block.end <= range.start);
        for block in &self.blocks[first..] {
            let offset = block.end - block.range.len();
            let start_in_block = range.start.saturating_sub(offset);
            let end_in_block = range.end.saturating_sub(offset).min(block.range.len());
            if start_in_block < end_in_block {
                result.blocks.push(Block {
                    samples: block.samples.clone(),
                    range: block.range.start + start_in_block..block.range.start + end_in_block,
                    end: result.len + end_in_block - start_in_block,
                });
                result.len += end_in_block - start_in_block;
            }
            if block.end >= range.end {
                break;
            }
        }
        result
    }
}

impl From<Vec<i16>> for Pcm {
    fn from(samples: Vec<i16>) -> Self {
        let len = samples.len();
        if len == 0 {
            return Self::default();
        }
        Self {
            blocks: vec![Block {
                samples: Arc::new(samples),
                range: 0..len,
                end: len,
            }],
            len,
        }
    }
}
impl FromIterator<i16> for Pcm {
    fn from_iter<T: IntoIterator<Item = i16>>(iter: T) -> Self {
        Vec::from_iter(iter).into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn full_recording_and_cursor_share_all_sample_allocations() {
        let first = Pcm::from(vec![1, 2, 3]);
        let last = Pcm::from(vec![4, 5]);
        let mut whole = first.clone();
        whole.append(&last);
        let view = whole.range(2..4);
        assert_eq!(view.iter().copied().collect::<Vec<_>>(), [3, 4]);
        assert!(Arc::ptr_eq(
            &first.blocks[0].samples,
            &whole.blocks[0].samples
        ));
        assert!(Arc::ptr_eq(
            &view.blocks[1].samples,
            &last.blocks[0].samples
        ));
        drop(whole);
        drop(first);
        drop(last);
        assert_eq!(view.iter().copied().collect::<Vec<_>>(), [3, 4]);
    }
    #[test]
    fn all_cursor_ranges_and_bounded_chunks_preserve_samples() {
        let mut pcm = Pcm::default();
        for chunk in [vec![1, 2], vec![], vec![3], vec![4, 5]] {
            pcm.append(&Pcm::from(chunk));
        }
        for start in 0..=5 {
            for end in start..=5 {
                let view = pcm.range(start..end);
                assert_eq!(view.len(), end - start);
                assert_eq!(
                    view.chunks(1).flatten().copied().collect::<Vec<_>>(),
                    [1, 2, 3, 4, 5][start..end]
                );
            }
        }
    }
    #[test]
    fn appended_subranges_keep_their_logical_index_and_storage() {
        let mut input = Pcm::default();
        for n in 0..1000 {
            input.append(&Pcm::from(vec![n; 10]));
        }
        let mut joined = input.range(9873..9892);
        joined.append(&input.range(15..28));
        assert_eq!(joined.len(), 32);
        let expected: Vec<_> = input
            .iter()
            .skip(9873)
            .take(19)
            .chain(input.iter().skip(15).take(13))
            .copied()
            .collect();
        for from in 0..=joined.len() {
            for to in from..=joined.len() {
                assert_eq!(
                    joined.range(from..to).iter().copied().collect::<Vec<_>>(),
                    expected[from..to]
                );
            }
        }
        assert!(Arc::ptr_eq(
            &joined.blocks[0].samples,
            &input.blocks[987].samples
        ));
    }
}
