use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use cpal::{
    FromSample, Sample, SampleFormat, SizedSample, Stream,
    traits::{DeviceTrait, HostTrait, StreamTrait},
};
use once_cell::sync::Lazy;

use crate::{devices::SYSTEM_DEFAULT, pcm_audio::PcmAudio};

pub static EDITOR_PLAYBACK: Lazy<EditorPlayback> = Lazy::new(EditorPlayback::default);

#[derive(Default)]
pub struct EditorPlayback {
    inner: Mutex<PlaybackInner>,
    state: Arc<Mutex<PlaybackState>>,
}

#[derive(Default)]
struct PlaybackInner {
    stream: Option<Stream>,
    device_name: Option<String>,
}

#[derive(Clone, Debug)]
struct PlaybackState {
    samples: Arc<Vec<f32>>,
    source_sample_rate: u32,
    position: f64,
    rate_ratio: f64,
    playing: bool,
    stop_at: Option<f64>,
    restore_to: Option<f64>,
}

impl Default for PlaybackState {
    fn default() -> Self {
        Self {
            samples: Arc::new(Vec::new()),
            source_sample_rate: 0,
            position: 0.0,
            rate_ratio: 1.0,
            playing: false,
            stop_at: None,
            restore_to: None,
        }
    }
}

impl EditorPlayback {
    pub fn load_audio_position(&self, pcm_audio: &PcmAudio) {
        let mut state = self.state.lock().expect("playback state lock poisoned");
        state.samples = Arc::new(pcm_audio.samples.clone());
        state.source_sample_rate = pcm_audio.sample_rate;
        state.position = 0.0;
        state.rate_ratio = 1.0;
        state.playing = false;
        state.stop_at = None;
        state.restore_to = None;
    }

    pub fn play(&self, pcm_audio: PcmAudio, output_device_name: &str) -> Result<(), PlaybackError> {
        let start_sample = self.position_samples();
        self.start_playback(pcm_audio, output_device_name, start_sample, None, None)
    }

    pub fn play_range(
        &self,
        pcm_audio: PcmAudio,
        output_device_name: &str,
        start_sample: usize,
        stop_sample: usize,
        restore_sample: Option<usize>,
    ) -> Result<(), PlaybackError> {
        if start_sample >= stop_sample {
            return Err(PlaybackError::InvalidPreviewRange);
        }

        self.start_playback(
            pcm_audio,
            output_device_name,
            start_sample,
            Some(stop_sample),
            restore_sample,
        )
    }

    fn start_playback(
        &self,
        pcm_audio: PcmAudio,
        output_device_name: &str,
        start_sample: usize,
        stop_sample: Option<usize>,
        restore_sample: Option<usize>,
    ) -> Result<(), PlaybackError> {
        if pcm_audio.is_empty() || pcm_audio.sample_rate == 0 {
            return Err(PlaybackError::NoAudioLoaded);
        }

        let (device, device_label) = find_output_device(output_device_name)?;
        let supported_config = device.default_output_config()?;
        let output_sample_rate = supported_config.sample_rate().0;
        let config = supported_config.config();

        {
            let mut state = self.state.lock().expect("playback state lock poisoned");
            state.samples = Arc::new(pcm_audio.samples);
            state.source_sample_rate = pcm_audio.sample_rate;
            state.position = (start_sample as f64).min(state.samples.len() as f64);
            if state.position >= state.samples.len() as f64 {
                state.position = 0.0;
            }
            state.rate_ratio = state.source_sample_rate as f64 / output_sample_rate as f64;
            state.playing = true;
            state.stop_at =
                stop_sample.map(|sample| (sample as f64).min(state.samples.len() as f64));
            state.restore_to =
                restore_sample.map(|sample| (sample as f64).min(state.samples.len() as f64));
        }

        let mut inner = self.inner.lock().expect("playback inner lock poisoned");
        if inner.stream.is_none() || inner.device_name.as_deref() != Some(device_label.as_str()) {
            let stream = build_stream(
                &device,
                supported_config.sample_format(),
                &config,
                self.state.clone(),
            )?;
            stream.play()?;
            inner.stream = Some(stream);
            inner.device_name = Some(device_label);
        }

        Ok(())
    }

    pub fn stop(&self) {
        let mut state = self.state.lock().expect("playback state lock poisoned");
        state.playing = false;
        state.stop_at = None;
        state.restore_to = None;
    }

    pub fn reset_for_new_audio(&self) {
        let mut state = self.state.lock().expect("playback state lock poisoned");
        state.position = 0.0;
        state.playing = false;
        state.stop_at = None;
        state.restore_to = None;
    }

    pub fn seek_relative(&self, seconds: f64) {
        let mut state = self.state.lock().expect("playback state lock poisoned");
        let target = state.position + seconds * state.source_sample_rate as f64;
        state.position = target.clamp(0.0, state.samples.len() as f64);
        state.restore_to = None;
    }

    pub fn seek_absolute_samples(&self, sample_pos: usize) {
        let mut state = self.state.lock().expect("playback state lock poisoned");
        state.position = (sample_pos as f64).clamp(0.0, state.samples.len() as f64);
        state.playing = false;
        state.stop_at = None;
        state.restore_to = None;
    }

    pub fn position_seconds(&self) -> f64 {
        let state = self.state.lock().expect("playback state lock poisoned");
        if state.source_sample_rate == 0 {
            0.0
        } else {
            state.position / state.source_sample_rate as f64
        }
    }

    pub fn position_samples(&self) -> usize {
        let state = self.state.lock().expect("playback state lock poisoned");
        state.position.floor().max(0.0) as usize
    }

    pub fn is_playing(&self) -> bool {
        self.state
            .lock()
            .expect("playback state lock poisoned")
            .playing
    }

}

fn find_output_device(name: &str) -> Result<(cpal::Device, String), PlaybackError> {
    let host = cpal::default_host();

    if name.is_empty() || name == SYSTEM_DEFAULT {
        let device = host
            .default_output_device()
            .ok_or(PlaybackError::NoDefaultOutputDevice)?;
        let label = device.name().unwrap_or_else(|_| SYSTEM_DEFAULT.to_string());
        return Ok((device, label));
    }

    let devices = host.output_devices()?;
    for device in devices {
        let Ok(device_name) = device.name() else {
            continue;
        };

        if device_name == name {
            return Ok((device, device_name));
        }
    }

    Err(PlaybackError::OutputDeviceNotFound(name.to_string()))
}

fn build_stream(
    device: &cpal::Device,
    sample_format: SampleFormat,
    config: &cpal::StreamConfig,
    state: Arc<Mutex<PlaybackState>>,
) -> Result<Stream, PlaybackError> {
    match sample_format {
        SampleFormat::I8 => build_typed_stream::<i8>(device, config, state),
        SampleFormat::I16 => build_typed_stream::<i16>(device, config, state),
        SampleFormat::I24 => build_typed_stream::<cpal::I24>(device, config, state),
        SampleFormat::I32 => build_typed_stream::<i32>(device, config, state),
        SampleFormat::I64 => build_typed_stream::<i64>(device, config, state),
        SampleFormat::U8 => build_typed_stream::<u8>(device, config, state),
        SampleFormat::U16 => build_typed_stream::<u16>(device, config, state),
        SampleFormat::U32 => build_typed_stream::<u32>(device, config, state),
        SampleFormat::U64 => build_typed_stream::<u64>(device, config, state),
        SampleFormat::F32 => build_typed_stream::<f32>(device, config, state),
        SampleFormat::F64 => build_typed_stream::<f64>(device, config, state),
        format => Err(PlaybackError::UnsupportedSampleFormat(format)),
    }
}

fn build_typed_stream<T>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    state: Arc<Mutex<PlaybackState>>,
) -> Result<Stream, PlaybackError>
where
    T: Sample + SizedSample + FromSample<f32>,
{
    let channels = config.channels as usize;
    let err_fn = |error| eprintln!("audio playback stream error: {error}");

    Ok(device.build_output_stream(
        config,
        move |data: &mut [T], _| write_output(data, channels, &state),
        err_fn,
        Some(Duration::from_millis(100)),
    )?)
}

fn write_output<T>(data: &mut [T], channels: usize, state: &Arc<Mutex<PlaybackState>>)
where
    T: Sample + FromSample<f32>,
{
    let mut state = state.lock().expect("playback state lock poisoned");

    for frame in data.chunks_mut(channels) {
        let value = if state.playing {
            next_sample(&mut state)
        } else {
            0.0
        };
        let value = T::from_sample(value);

        for sample in frame {
            *sample = value;
        }
    }
}

fn next_sample(state: &mut PlaybackState) -> f32 {
    if state
        .stop_at
        .is_some_and(|stop_at| state.position >= stop_at)
    {
        finish_range(state);
        return 0.0;
    }

    let index = state.position.floor() as usize;

    if index >= state.samples.len() {
        state.position = state.samples.len() as f64;
        finish_range(state);
        return 0.0;
    }

    let next_index = (index + 1).min(state.samples.len() - 1);
    let fraction = (state.position - index as f64) as f32;
    let current = state.samples[index];
    let next = state.samples[next_index];
    let value = current + (next - current) * fraction;

    state.position += state.rate_ratio;
    if state
        .stop_at
        .is_some_and(|stop_at| state.position >= stop_at)
    {
        finish_range(state);
    }

    value
}

fn finish_range(state: &mut PlaybackState) {
    state.playing = false;
    let stop_at = state.stop_at.take();
    if let Some(restore_to) = state.restore_to.take() {
        state.position = restore_to;
    } else if let Some(stop_at) = stop_at {
        state.position = stop_at;
    }
}

#[derive(Debug)]
pub enum PlaybackError {
    BuildStream(cpal::BuildStreamError),
    DefaultConfig(cpal::DefaultStreamConfigError),
    Devices(cpal::DevicesError),
    InvalidPreviewRange,
    NoAudioLoaded,
    NoDefaultOutputDevice,
    OutputDeviceNotFound(String),
    PlayStream(cpal::PlayStreamError),
    UnsupportedSampleFormat(SampleFormat),
}

impl std::fmt::Display for PlaybackError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BuildStream(error) => write!(formatter, "could not build audio stream: {error}"),
            Self::DefaultConfig(error) => {
                write!(formatter, "could not get output device config: {error}")
            }
            Self::Devices(error) => {
                write!(formatter, "could not enumerate output devices: {error}")
            }
            Self::InvalidPreviewRange => write!(formatter, "preview range is empty"),
            Self::NoAudioLoaded => write!(formatter, "no decoded audio is loaded"),
            Self::NoDefaultOutputDevice => {
                write!(formatter, "no default output device is available")
            }
            Self::OutputDeviceNotFound(name) => {
                write!(formatter, "output device not found: {name}")
            }
            Self::PlayStream(error) => write!(formatter, "could not start audio stream: {error}"),
            Self::UnsupportedSampleFormat(format) => {
                write!(formatter, "unsupported output sample format: {format:?}")
            }
        }
    }
}

impl std::error::Error for PlaybackError {}

impl From<cpal::BuildStreamError> for PlaybackError {
    fn from(error: cpal::BuildStreamError) -> Self {
        Self::BuildStream(error)
    }
}

impl From<cpal::DefaultStreamConfigError> for PlaybackError {
    fn from(error: cpal::DefaultStreamConfigError) -> Self {
        Self::DefaultConfig(error)
    }
}

impl From<cpal::DevicesError> for PlaybackError {
    fn from(error: cpal::DevicesError) -> Self {
        Self::Devices(error)
    }
}

impl From<cpal::PlayStreamError> for PlaybackError {
    fn from(error: cpal::PlayStreamError) -> Self {
        Self::PlayStream(error)
    }
}
