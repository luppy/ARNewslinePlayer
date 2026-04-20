#[derive(Clone, Debug)]
pub struct PcmAudio {
    pub sample_rate: u32,
    pub samples: Vec<f32>,
    pub segments: Vec<u32>,
}

impl PcmAudio {
    pub fn new(sample_rate: u32, samples: Vec<f32>) -> Self {
        let sample_count = samples.len().min(u32::MAX as usize) as u32;

        Self {
            sample_rate,
            samples,
            segments: vec![sample_count],
        }
    }

    pub fn frame_count(&self) -> usize {
        self.samples.len()
    }

    pub fn duration_seconds(&self) -> f64 {
        if self.sample_rate == 0 {
            0.0
        } else {
            self.frame_count() as f64 / self.sample_rate as f64
        }
    }

    pub fn is_empty(&self) -> bool {
        self.samples.is_empty()
    }

    pub fn split_segment(&mut self, pos: u32) -> Result<(), SegmentError> {
        let sample_count = self.sample_count_u32();

        if pos == 0 {
            return Err(SegmentError::StartOfAudio);
        }

        if pos >= sample_count {
            return Err(SegmentError::EndOfAudio);
        }

        match self.segments.binary_search(&pos) {
            Ok(_) => Err(SegmentError::AlreadyExists),
            Err(index) => {
                self.segments.insert(index, pos);
                Ok(())
            }
        }
    }

    pub fn remove_segment(&mut self, segment_index: usize) -> Result<(), SegmentError> {
        let segment_count = self.segments.len();

        if segment_index >= segment_count {
            return Err(SegmentError::OutOfRange);
        }

        if segment_index == 0 {
            return Err(SegmentError::FirstSegment);
        }

        self.segments.remove(segment_index - 1);
        Ok(())
    }

    pub fn set_segments(&mut self, segments: Vec<u32>) -> Result<(), SegmentError> {
        let sample_count = self.sample_count_u32();

        if segments.is_empty() || segments.last() != Some(&sample_count) {
            return Err(SegmentError::InvalidSegmentList);
        }

        let mut previous = 0;
        for &segment in &segments {
            if segment <= previous || segment > sample_count {
                return Err(SegmentError::InvalidSegmentList);
            }
            previous = segment;
        }

        self.segments = segments;
        Ok(())
    }

    pub fn search_gap(
        &self,
        start_pos: usize,
        min_length: usize,
        threshold: f32,
        direction: SearchDirection,
    ) -> Option<usize> {
        if self.samples.is_empty() || min_length == 0 {
            return None;
        }

        let threshold = threshold.abs();
        let start_pos = start_pos.min(self.samples.len().saturating_sub(1));

        match direction {
            SearchDirection::Forward => self.search_gap_forward(start_pos, min_length, threshold),
            SearchDirection::Backward => self.search_gap_backward(start_pos, min_length, threshold),
        }
    }

    fn sample_count_u32(&self) -> u32 {
        self.samples.len().min(u32::MAX as usize) as u32
    }

    fn is_gap_sample(&self, index: usize, threshold: f32) -> bool {
        self.samples[index].abs() <= threshold
    }

    fn search_gap_forward(
        &self,
        start_pos: usize,
        min_length: usize,
        threshold: f32,
    ) -> Option<usize> {
        let mut index = start_pos;

        if self.is_gap_sample(index, threshold) {
            while index < self.samples.len() && self.is_gap_sample(index, threshold) {
                index += 1;
            }
        }

        while index < self.samples.len() {
            while index < self.samples.len() && !self.is_gap_sample(index, threshold) {
                index += 1;
            }

            let gap_start = index;
            while index < self.samples.len() && self.is_gap_sample(index, threshold) {
                index += 1;
            }

            let gap_end = index;
            if gap_end.saturating_sub(gap_start) >= min_length {
                return center_of_gap(gap_start, gap_end);
            }
        }

        None
    }

    fn search_gap_backward(
        &self,
        start_pos: usize,
        min_length: usize,
        threshold: f32,
    ) -> Option<usize> {
        let mut index = start_pos;

        if self.is_gap_sample(index, threshold) {
            while index > 0 && self.is_gap_sample(index, threshold) {
                index -= 1;
            }

            if index == 0 && self.is_gap_sample(index, threshold) {
                return None;
            }
        }

        loop {
            while index > 0 && !self.is_gap_sample(index, threshold) {
                index -= 1;
            }

            if index == 0 && !self.is_gap_sample(index, threshold) {
                return None;
            }

            let gap_end = index + 1;
            while index > 0 && self.is_gap_sample(index - 1, threshold) {
                index -= 1;
            }

            let gap_start = index;
            if gap_end.saturating_sub(gap_start) >= min_length {
                return center_of_gap(gap_start, gap_end);
            }

            if gap_start == 0 {
                return None;
            }

            index = gap_start - 1;
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SearchDirection {
    Forward,
    Backward,
}

fn center_of_gap(start: usize, end_exclusive: usize) -> Option<usize> {
    let center = start + (end_exclusive - start - 1) / 2;
    Some(center)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SegmentError {
    AlreadyExists,
    EndOfAudio,
    FirstSegment,
    InvalidSegmentList,
    OutOfRange,
    StartOfAudio,
}

impl std::fmt::Display for SegmentError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AlreadyExists => write!(formatter, "segment already exists at that position"),
            Self::EndOfAudio => write!(formatter, "cannot split at or after the end of the audio"),
            Self::FirstSegment => write!(formatter, "the first segment cannot be merged backward"),
            Self::InvalidSegmentList => write!(formatter, "invalid segment list"),
            Self::OutOfRange => write!(formatter, "segment index is out of range"),
            Self::StartOfAudio => write!(formatter, "cannot split at the start of the audio"),
        }
    }
}

impl std::error::Error for SegmentError {}

#[cfg(test)]
mod tests {
    use super::{PcmAudio, SearchDirection, SegmentError};

    #[test]
    fn starts_with_end_boundary() {
        let audio = PcmAudio::new(48_000, vec![0.0; 42]);

        assert_eq!(audio.segments, vec![42]);
    }

    #[test]
    fn split_segment_keeps_boundaries_ordered() {
        let mut audio = PcmAudio::new(48_000, vec![0.0; 100]);

        audio.split_segment(75).unwrap();
        audio.split_segment(25).unwrap();
        audio.split_segment(50).unwrap();

        assert_eq!(audio.segments, vec![25, 50, 75, 100]);
    }

    #[test]
    fn split_segment_rejects_invalid_boundaries() {
        let mut audio = PcmAudio::new(48_000, vec![0.0; 100]);

        assert_eq!(audio.split_segment(0), Err(SegmentError::StartOfAudio));
        assert_eq!(audio.split_segment(100), Err(SegmentError::EndOfAudio));
        audio.split_segment(50).unwrap();
        assert_eq!(audio.split_segment(50), Err(SegmentError::AlreadyExists));
    }

    #[test]
    fn remove_segment_merges_with_previous_segment() {
        let mut audio = PcmAudio::new(48_000, vec![0.0; 100]);
        audio.split_segment(25).unwrap();
        audio.split_segment(50).unwrap();
        audio.split_segment(75).unwrap();

        audio.remove_segment(2).unwrap();

        assert_eq!(audio.segments, vec![25, 75, 100]);
    }

    #[test]
    fn remove_segment_can_merge_final_segment_backward() {
        let mut audio = PcmAudio::new(48_000, vec![0.0; 100]);
        audio.split_segment(50).unwrap();

        audio.remove_segment(1).unwrap();

        assert_eq!(audio.segments, vec![100]);
    }

    #[test]
    fn remove_segment_rejects_first_only_and_out_of_range() {
        let mut audio = PcmAudio::new(48_000, vec![0.0; 100]);

        assert_eq!(audio.remove_segment(0), Err(SegmentError::FirstSegment));

        audio.split_segment(50).unwrap();

        assert_eq!(audio.remove_segment(0), Err(SegmentError::FirstSegment));
        assert_eq!(audio.remove_segment(2), Err(SegmentError::OutOfRange));
    }

    #[test]
    fn set_segments_accepts_valid_boundaries() {
        let mut audio = PcmAudio::new(48_000, vec![0.0; 100]);

        audio.set_segments(vec![25, 50, 100]).unwrap();

        assert_eq!(audio.segments, vec![25, 50, 100]);
    }

    #[test]
    fn set_segments_rejects_invalid_boundaries() {
        let mut audio = PcmAudio::new(48_000, vec![0.0; 100]);

        assert_eq!(
            audio.set_segments(vec![]),
            Err(SegmentError::InvalidSegmentList)
        );
        assert_eq!(
            audio.set_segments(vec![25, 50]),
            Err(SegmentError::InvalidSegmentList)
        );
        assert_eq!(
            audio.set_segments(vec![50, 25, 100]),
            Err(SegmentError::InvalidSegmentList)
        );
    }

    #[test]
    fn search_gap_forward_finds_center_of_first_long_enough_gap() {
        let audio = PcmAudio::new(48_000, vec![1.0, 0.0, 0.0, 1.0, 0.05, 0.04, 0.03, 1.0]);

        assert_eq!(
            audio.search_gap(0, 3, 0.05, SearchDirection::Forward),
            Some(5)
        );
    }

    #[test]
    fn search_gap_forward_leaves_current_gap_before_searching() {
        let audio = PcmAudio::new(48_000, vec![0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0]);

        assert_eq!(
            audio.search_gap(1, 3, 0.0, SearchDirection::Forward),
            Some(5)
        );
    }

    #[test]
    fn search_gap_backward_finds_center_of_first_long_enough_gap() {
        let audio = PcmAudio::new(48_000, vec![1.0, 0.05, 0.04, 0.03, 1.0, 0.0, 0.0, 1.0]);

        assert_eq!(
            audio.search_gap(7, 3, 0.05, SearchDirection::Backward),
            Some(2)
        );
    }

    #[test]
    fn search_gap_backward_leaves_current_gap_before_searching() {
        let audio = PcmAudio::new(48_000, vec![1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0]);

        assert_eq!(
            audio.search_gap(6, 3, 0.0, SearchDirection::Backward),
            Some(2)
        );
    }

    #[test]
    fn search_gap_returns_none_when_no_gap_is_long_enough() {
        let audio = PcmAudio::new(48_000, vec![1.0, 0.0, 0.0, 1.0]);

        assert_eq!(audio.search_gap(0, 3, 0.0, SearchDirection::Forward), None);
        assert_eq!(audio.search_gap(3, 3, 0.0, SearchDirection::Backward), None);
    }
}
