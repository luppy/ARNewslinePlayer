use std::{
    io::Cursor,
    path::{Path, PathBuf},
};

use crate::pcm_audio::PcmAudio;
use symphonia::core::{
    audio::{AudioBufferRef, SampleBuffer},
    codecs::DecoderOptions,
    errors::Error as SymphoniaError,
    formats::FormatOptions,
    io::MediaSourceStream,
    meta::MetadataOptions,
    probe::Hint,
};

#[derive(Clone, Debug)]
pub struct CompressedAudio {
    source_path: PathBuf,
    data: Vec<u8>,
}

impl CompressedAudio {
    pub fn from_file(path: impl AsRef<Path>) -> Result<Self, AudioError> {
        let source_path = path.as_ref().to_path_buf();
        let data = std::fs::read(&source_path)?;

        Ok(Self { source_path, data })
    }

    pub fn source_path(&self) -> &Path {
        &self.source_path
    }

    pub fn compressed_len(&self) -> usize {
        self.data.len()
    }

    pub fn decode_to_pcm(&self) -> Result<PcmAudio, AudioError> {
        let cursor = Cursor::new(self.data.clone());
        let media_source = MediaSourceStream::new(Box::new(cursor), Default::default());

        let mut hint = Hint::new();
        if let Some(extension) = self
            .source_path
            .extension()
            .and_then(|extension| extension.to_str())
        {
            hint.with_extension(extension);
        }

        let probed = symphonia::default::get_probe().format(
            &hint,
            media_source,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )?;

        let mut format = probed.format;
        let track = format
            .default_track()
            .ok_or(AudioError::MissingDefaultTrack)?;
        let track_id = track.id;
        let codec_params = &track.codec_params;
        let sample_rate = codec_params.sample_rate.unwrap_or(0);

        let mut decoder =
            symphonia::default::get_codecs().make(codec_params, &DecoderOptions::default())?;
        let mut samples = Vec::new();

        loop {
            let packet = match format.next_packet() {
                Ok(packet) => packet,
                Err(SymphoniaError::ResetRequired) => {
                    decoder.reset();
                    continue;
                }
                Err(SymphoniaError::IoError(error))
                    if error.kind() == std::io::ErrorKind::UnexpectedEof =>
                {
                    break;
                }
                Err(error) => return Err(error.into()),
            };

            if packet.track_id() != track_id {
                continue;
            }

            let decoded = match decoder.decode(&packet) {
                Ok(decoded) => decoded,
                Err(SymphoniaError::DecodeError(_)) => continue,
                Err(error) => return Err(error.into()),
            };

            copy_decoded_to_f32(&decoded, &mut samples);
        }

        Ok(PcmAudio::new(sample_rate, samples))
    }
}

fn copy_decoded_to_f32(decoded: &AudioBufferRef<'_>, samples: &mut Vec<f32>) {
    let mut buffer = SampleBuffer::<f32>::new(decoded.capacity() as u64, *decoded.spec());
    buffer.copy_interleaved_ref(decoded.clone());

    let channels = decoded.spec().channels.count();
    if channels <= 1 {
        samples.extend_from_slice(buffer.samples());
        return;
    }

    for frame in buffer.samples().chunks_exact(channels) {
        let sum: f32 = frame.iter().sum();
        samples.push(sum / channels as f32);
    }
}

#[derive(Debug)]
pub enum AudioError {
    Io(std::io::Error),
    Decode(SymphoniaError),
    MissingDefaultTrack,
}

impl std::fmt::Display for AudioError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "I/O error: {error}"),
            Self::Decode(error) => write!(formatter, "audio decode error: {error}"),
            Self::MissingDefaultTrack => write!(formatter, "audio file has no default track"),
        }
    }
}

impl std::error::Error for AudioError {}

impl From<std::io::Error> for AudioError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<SymphoniaError> for AudioError {
    fn from(error: SymphoniaError) -> Self {
        Self::Decode(error)
    }
}
