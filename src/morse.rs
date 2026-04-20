use crate::pcm_audio::PcmAudio;

const TONE_FREQUENCY_HZ: f32 = 700.0;
const WORDS_PER_MINUTE: f32 = 20.0;

pub fn callsign_to_morse_audio(callsign: &str, sample_rate: u32) -> PcmAudio {
    let dot_samples = dot_samples(sample_rate);
    let mut samples = Vec::new();
    let mut first_symbol = true;

    for symbol in callsign
        .chars()
        .filter_map(|character| morse_symbol(character.to_ascii_uppercase()))
    {
        if !first_symbol {
            append_silence(&mut samples, dot_samples * 3);
        }
        first_symbol = false;

        append_symbol(&mut samples, symbol, sample_rate, dot_samples);
    }

    PcmAudio::new(sample_rate, samples)
}

fn append_symbol(samples: &mut Vec<f32>, symbol: &str, sample_rate: u32, dot_samples: usize) {
    for (index, mark) in symbol.chars().enumerate() {
        if index > 0 {
            append_silence(samples, dot_samples);
        }

        match mark {
            '.' => append_tone(samples, sample_rate, dot_samples),
            '-' => append_tone(samples, sample_rate, dot_samples * 3),
            _ => {}
        }
    }
}

fn append_tone(samples: &mut Vec<f32>, sample_rate: u32, sample_count: usize) {
    let start = samples.len();
    let sample_rate = sample_rate as f32;

    for index in 0..sample_count {
        let time = (start + index) as f32 / sample_rate;
        let phase = time * TONE_FREQUENCY_HZ * std::f32::consts::TAU;
        samples.push(phase.sin() * 0.5);
    }
}

fn append_silence(samples: &mut Vec<f32>, sample_count: usize) {
    samples.extend(std::iter::repeat_n(0.0, sample_count));
}

fn dot_samples(sample_rate: u32) -> usize {
    (sample_rate as f32 * dot_duration_seconds()).round() as usize
}

fn dot_duration_seconds() -> f32 {
    1.2 / WORDS_PER_MINUTE
}

fn morse_symbol(character: char) -> Option<&'static str> {
    match character {
        'A' => Some(".-"),
        'B' => Some("-..."),
        'C' => Some("-.-."),
        'D' => Some("-.."),
        'E' => Some("."),
        'F' => Some("..-."),
        'G' => Some("--."),
        'H' => Some("...."),
        'I' => Some(".."),
        'J' => Some(".---"),
        'K' => Some("-.-"),
        'L' => Some(".-.."),
        'M' => Some("--"),
        'N' => Some("-."),
        'O' => Some("---"),
        'P' => Some(".--."),
        'Q' => Some("--.-"),
        'R' => Some(".-."),
        'S' => Some("..."),
        'T' => Some("-"),
        'U' => Some("..-"),
        'V' => Some("...-"),
        'W' => Some(".--"),
        'X' => Some("-..-"),
        'Y' => Some("-.--"),
        'Z' => Some("--.."),
        '0' => Some("-----"),
        '1' => Some(".----"),
        '2' => Some("..---"),
        '3' => Some("...--"),
        '4' => Some("....-"),
        '5' => Some("....."),
        '6' => Some("-...."),
        '7' => Some("--..."),
        '8' => Some("---.."),
        '9' => Some("----."),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{callsign_to_morse_audio, dot_samples};

    #[test]
    fn single_dot_is_one_dot_long() {
        let audio = callsign_to_morse_audio("E", 8_000);

        assert_eq!(audio.samples.len(), dot_samples(8_000));
    }

    #[test]
    fn dash_is_three_dots_long() {
        let audio = callsign_to_morse_audio("T", 8_000);

        assert_eq!(audio.samples.len(), dot_samples(8_000) * 3);
    }

    #[test]
    fn inserts_symbol_gap_between_characters() {
        let audio = callsign_to_morse_audio("EE", 8_000);

        assert_eq!(audio.samples.len(), dot_samples(8_000) * 5);
    }

    #[test]
    fn skips_unsupported_characters() {
        let audio = callsign_to_morse_audio("@", 8_000);

        assert!(audio.samples.is_empty());
    }
}
