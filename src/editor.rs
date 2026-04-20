use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};
use std::{
    fs, io,
    path::{Path, PathBuf},
    sync::Mutex,
};

use crate::{
    audio::CompressedAudio,
    pcm_audio::{PcmAudio, SearchDirection},
};

#[derive(Clone, Debug, Default)]
pub struct EditorContextSnapshot {
    pub mp3_path: Option<PathBuf>,
    pub pcm_frame_count: usize,
    pub duration_seconds: f64,
}

#[derive(Default)]
pub struct EditorContext {
    state: Mutex<EditorState>,
}

#[derive(Default)]
struct EditorState {
    mp3_path: Option<PathBuf>,
    pcm_audio: Option<PcmAudio>,
}

pub static EDITOR_CONTEXT: Lazy<EditorContext> = Lazy::new(EditorContext::default);

impl EditorContext {
    pub fn load_mp3(&self, path: impl AsRef<Path>) -> Result<EditorContextSnapshot, EditorError> {
        let path = path.as_ref().to_path_buf();
        let compressed = CompressedAudio::from_file(&path)?;
        let mut pcm_audio = compressed.decode_to_pcm()?;
        load_segments(&path, &mut pcm_audio)?;

        let snapshot = EditorContextSnapshot {
            mp3_path: Some(path.clone()),
            pcm_frame_count: pcm_audio.frame_count(),
            duration_seconds: pcm_audio.duration_seconds(),
        };

        let mut state = self.state.lock().expect("editor context lock poisoned");
        state.mp3_path = Some(path);
        state.pcm_audio = Some(pcm_audio);

        Ok(snapshot)
    }

    pub fn snapshot(&self) -> EditorContextSnapshot {
        let state = self.state.lock().expect("editor context lock poisoned");
        let pcm_audio = state.pcm_audio.as_ref();

        EditorContextSnapshot {
            mp3_path: state.mp3_path.clone(),
            pcm_frame_count: pcm_audio.map(PcmAudio::frame_count).unwrap_or_default(),
            duration_seconds: pcm_audio
                .map(PcmAudio::duration_seconds)
                .unwrap_or_default(),
        }
    }

    pub fn with_pcm_audio<T>(&self, callback: impl FnOnce(Option<&PcmAudio>) -> T) -> T {
        let state = self.state.lock().expect("editor context lock poisoned");
        callback(state.pcm_audio.as_ref())
    }

    pub fn split_at(&self, sample_pos: usize) -> Result<(), EditorError> {
        let mut state = self.state.lock().expect("editor context lock poisoned");
        let path = state.mp3_path.clone().ok_or(EditorError::NoAudioLoaded)?;
        let Some(pcm_audio) = state.pcm_audio.as_mut() else {
            return Err(EditorError::NoAudioLoaded);
        };

        let sample_pos = u32::try_from(sample_pos).map_err(|_| EditorError::PositionTooLarge)?;
        pcm_audio.split_segment(sample_pos)?;
        save_segments(&path, pcm_audio)?;
        Ok(())
    }

    pub fn delete_segment(&self, segment_index: usize) -> Result<(), EditorError> {
        let mut state = self.state.lock().expect("editor context lock poisoned");
        let path = state.mp3_path.clone().ok_or(EditorError::NoAudioLoaded)?;
        let Some(pcm_audio) = state.pcm_audio.as_mut() else {
            return Err(EditorError::NoAudioLoaded);
        };

        pcm_audio.remove_segment(segment_index)?;
        save_segments(&path, pcm_audio)?;
        Ok(())
    }

    pub fn segment_rows(&self) -> Vec<String> {
        let state = self.state.lock().expect("editor context lock poisoned");
        let Some(pcm_audio) = state.pcm_audio.as_ref() else {
            return Vec::new();
        };

        let mut rows = Vec::with_capacity(pcm_audio.segments.len());
        let mut start = 0_u32;

        for &end in &pcm_audio.segments {
            rows.push(format!(
                "{}    {}    {}",
                format_sample_position(start, pcm_audio.sample_rate),
                format_sample_position(end, pcm_audio.sample_rate),
                format_sample_position(end.saturating_sub(start), pcm_audio.sample_rate),
            ));
            start = end;
        }

        rows
    }

    pub fn segment_start(&self, segment_index: usize) -> Option<usize> {
        let state = self.state.lock().expect("editor context lock poisoned");
        let pcm_audio = state.pcm_audio.as_ref()?;

        if segment_index >= pcm_audio.segments.len() {
            return None;
        }

        if segment_index == 0 {
            Some(0)
        } else {
            Some(pcm_audio.segments[segment_index - 1] as usize)
        }
    }

    pub fn active_segment_index(&self, sample_pos: usize) -> Option<usize> {
        let state = self.state.lock().expect("editor context lock poisoned");
        let pcm_audio = state.pcm_audio.as_ref()?;
        let sample_pos = sample_pos as u32;

        pcm_audio
            .segments
            .iter()
            .position(|&segment_end| sample_pos < segment_end)
            .or_else(|| pcm_audio.segments.len().checked_sub(1))
    }

    pub fn segment_end(&self, segment_index: usize) -> Option<usize> {
        let state = self.state.lock().expect("editor context lock poisoned");
        let pcm_audio = state.pcm_audio.as_ref()?;

        pcm_audio
            .segments
            .get(segment_index)
            .map(|&segment_end| segment_end as usize)
    }

    pub fn segment_bounds(&self, segment_index: usize) -> Option<(usize, usize)> {
        let state = self.state.lock().expect("editor context lock poisoned");
        let pcm_audio = state.pcm_audio.as_ref()?;
        let end = *pcm_audio.segments.get(segment_index)? as usize;
        let start = if segment_index == 0 {
            0
        } else {
            pcm_audio.segments[segment_index - 1] as usize
        };

        Some((start, end))
    }

    pub fn segment_duration_seconds(&self, segment_index: usize) -> Option<f64> {
        let state = self.state.lock().expect("editor context lock poisoned");
        let pcm_audio = state.pcm_audio.as_ref()?;
        let (start, end) = segment_bounds_for_audio(pcm_audio, segment_index)?;

        Some((end.saturating_sub(start)) as f64 / pcm_audio.sample_rate as f64)
    }

    pub fn search_gap(
        &self,
        start_pos: usize,
        min_duration_seconds: f64,
        threshold: f32,
        direction: SearchDirection,
    ) -> Option<usize> {
        let state = self.state.lock().expect("editor context lock poisoned");
        let pcm_audio = state.pcm_audio.as_ref()?;
        let min_length = (min_duration_seconds * pcm_audio.sample_rate as f64).ceil() as usize;

        pcm_audio.search_gap(start_pos, min_length, threshold, direction)
    }

    pub fn clear(&self) {
        let mut state = self.state.lock().expect("editor context lock poisoned");
        state.mp3_path = None;
        state.pcm_audio = None;
    }
}

#[derive(Debug, Deserialize, Serialize)]
struct SegmentSidecar {
    segments: Vec<u32>,
}

fn load_segments(path: &Path, pcm_audio: &mut PcmAudio) -> Result<(), EditorError> {
    let path = segments_path(path);
    let contents = match fs::read_to_string(&path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };

    let sidecar: SegmentSidecar = serde_json::from_str(&contents)?;
    pcm_audio.set_segments(sidecar.segments)?;
    Ok(())
}

fn save_segments(path: &Path, pcm_audio: &PcmAudio) -> Result<(), EditorError> {
    let path = segments_path(path);
    let sidecar = SegmentSidecar {
        segments: pcm_audio.segments.clone(),
    };
    let contents = serde_json::to_string_pretty(&sidecar)?;

    fs::write(path, contents)?;
    Ok(())
}

fn segments_path(path: &Path) -> PathBuf {
    path.with_extension("segments")
}

fn segment_bounds_for_audio(audio: &PcmAudio, segment_index: usize) -> Option<(usize, usize)> {
    let end = *audio.segments.get(segment_index)? as usize;
    let start = if segment_index == 0 {
        0
    } else {
        audio.segments[segment_index - 1] as usize
    };

    Some((start, end))
}

fn format_sample_position(sample_pos: u32, sample_rate: u32) -> String {
    if sample_rate == 0 {
        return "0:00.00".to_string();
    }

    let seconds = sample_pos as f64 / sample_rate as f64;
    let minutes = (seconds / 60.0).floor() as u64;
    let seconds = seconds - minutes as f64 * 60.0;

    format!("{minutes}:{seconds:05.2}")
}

#[derive(Debug)]
pub enum EditorError {
    Audio(crate::audio::AudioError),
    Io(std::io::Error),
    NoAudioLoaded,
    PositionTooLarge,
    Segment(crate::pcm_audio::SegmentError),
    SegmentJson(serde_json::Error),
}

impl std::fmt::Display for EditorError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Audio(error) => write!(formatter, "{error}"),
            Self::Io(error) => write!(formatter, "segment file I/O error: {error}"),
            Self::NoAudioLoaded => write!(formatter, "no decoded audio is loaded"),
            Self::PositionTooLarge => write!(formatter, "position is too large"),
            Self::Segment(error) => write!(formatter, "{error}"),
            Self::SegmentJson(error) => write!(formatter, "segment file JSON error: {error}"),
        }
    }
}

impl std::error::Error for EditorError {}

impl From<crate::audio::AudioError> for EditorError {
    fn from(error: crate::audio::AudioError) -> Self {
        Self::Audio(error)
    }
}

impl From<crate::pcm_audio::SegmentError> for EditorError {
    fn from(error: crate::pcm_audio::SegmentError) -> Self {
        Self::Segment(error)
    }
}

impl From<std::io::Error> for EditorError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<serde_json::Error> for EditorError {
    fn from(error: serde_json::Error) -> Self {
        Self::SegmentJson(error)
    }
}
