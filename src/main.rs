pub mod audio;
mod config;
mod devices;
pub mod editor;
pub mod editor_playback;
pub mod morse;
pub mod pcm_audio;
pub mod ptt;

use config::{APP_STATE, AppConfig};
use devices::{SYSTEM_DEFAULT, discover_devices};
use editor::EDITOR_CONTEXT;
use editor_playback::EDITOR_PLAYBACK;
use morse::callsign_to_morse_audio;
use pcm_audio::SearchDirection;
use ptt::{DesiredPttState, Ptt, PttState, PttTiming};
use slint::{ModelRc, SharedString, Timer, TimerMode, VecModel};
use std::{cell::RefCell, path::Path, rc::Rc, time::Duration};

slint::include_modules!();

fn main() -> Result<(), slint::PlatformError> {
    if let Err(error) = APP_STATE.load_from_disk() {
        eprintln!("Failed to load config: {error}");
    }

    let device_lists = discover_devices();
    let config = normalize_config(APP_STATE.config(), &device_lists);
    APP_STATE.replace(config.clone());

    let window = MainWindow::new()?;
    populate_setup(&window, &config, &device_lists);
    populate_editor(&window, &config);
    update_editor_transport(&window);
    update_player_ptt_status(&window, &Rc::new(RefCell::new(None)));

    let weak_window = window.as_weak();
    let ptt = Rc::new(RefCell::new(None::<Ptt>));
    let save_setup_window = weak_window.clone();
    window.on_save_setup(
        move |callsign,
              auto_output,
              auto_input,
              radio_output,
              radio_input,
              ptt_port,
              timeout_value,
              warmup_seconds,
              reset_seconds| {
            let timeout_seconds = match parse_timeout(&timeout_value) {
                Ok(value) => value,
                Err(message) => {
                    if let Some(window) = save_setup_window.upgrade() {
                        window.set_setup_status(format!("Timeout {message}").into());
                    }
                    return;
                }
            };

            let warmup_tenths = match parse_seconds_to_tenths(&warmup_seconds) {
                Ok(value) => value,
                Err(message) => {
                    if let Some(window) = save_setup_window.upgrade() {
                        window.set_setup_status(format!("Warmup {message}").into());
                    }
                    return;
                }
            };

            let reset_tenths = match parse_seconds_to_tenths(&reset_seconds) {
                Ok(value) => value,
                Err(message) => {
                    if let Some(window) = save_setup_window.upgrade() {
                        window.set_setup_status(format!("Reset {message}").into());
                    }
                    return;
                }
            };

            let current_config = APP_STATE.config();
            let config = AppConfig {
                callsign: callsign.to_string(),
                auto_output: auto_output.to_string(),
                auto_input: auto_input.to_string(),
                radio_output: radio_output.to_string(),
                radio_input: radio_input.to_string(),
                ptt_port: ptt_port.to_string(),
                repeater_timeout_seconds: timeout_seconds,
                repeater_warmup_tenths: warmup_tenths,
                repeater_reset_tenths: reset_tenths,
                editor_mp3_path: current_config.editor_mp3_path,
            };

            match APP_STATE.save(config.clone()) {
                Ok(path) => {
                    if let Some(window) = save_setup_window.upgrade() {
                        set_saved_setup(&window, &config);
                        window.set_setup_status(format!("Saved {}", path.display()).into());
                    }
                }
                Err(error) => {
                    if let Some(window) = save_setup_window.upgrade() {
                        window.set_setup_status(format!("Save failed: {error}").into());
                    }
                }
            }
        },
    );

    let browse_window = weak_window.clone();
    window.on_browse_editor_file(move || {
        let Some(path) = rfd::FileDialog::new()
            .add_filter("MP3 audio", &["mp3"])
            .pick_file()
        else {
            return;
        };

        if let Some(window) = browse_window.upgrade() {
            load_editor_file(&window, &path, true);
        }
    });

    let rewind_window = weak_window.clone();
    window.on_editor_rewind(move |seconds| {
        if let Some(window) = rewind_window.upgrade() {
            EDITOR_PLAYBACK.seek_relative(-(seconds as f64));
            update_editor_transport(&window);
        }
    });

    let play_window = weak_window.clone();
    window.on_editor_toggle_playback(move || {
        if let Some(window) = play_window.upgrade() {
            if EDITOR_PLAYBACK.is_playing() {
                EDITOR_PLAYBACK.stop();
                update_editor_transport(&window);
                window.set_editor_status("Stopped".into());
                return;
            }

            let Some(pcm_audio) = EDITOR_CONTEXT.with_pcm_audio(|audio| audio.cloned()) else {
                window.set_editor_status("No MP3 loaded".into());
                return;
            };

            let output_name = APP_STATE.config().auto_output;
            match EDITOR_PLAYBACK.play(pcm_audio, &output_name) {
                Ok(()) => {
                    update_editor_transport(&window);
                    window.set_editor_status(format!("Playing on {output_name}").into());
                }
                Err(error) => {
                    update_editor_transport(&window);
                    window.set_editor_status(format!("Playback failed: {error}").into());
                }
            }
        }
    });

    let forward_window = weak_window.clone();
    window.on_editor_forward(move |seconds| {
        if let Some(window) = forward_window.upgrade() {
            EDITOR_PLAYBACK.seek_relative(seconds as f64);
            update_editor_transport(&window);
        }
    });

    let last_five_window = weak_window.clone();
    window.on_editor_last_five_seconds(move || {
        if let Some(window) = last_five_window.upgrade() {
            match play_editor_preview(&window, PreviewKind::LastFiveSeconds) {
                Ok(()) => window.set_editor_status("Playing last 5 seconds".into()),
                Err(error) => window.set_editor_status(format!("Preview failed: {error}").into()),
            }
        }
    });

    let prev_gap_window = weak_window.clone();
    window.on_editor_prev_gap(move || {
        if let Some(window) = prev_gap_window.upgrade() {
            match move_to_gap(SearchDirection::Backward) {
                Some(sample_pos) => {
                    EDITOR_PLAYBACK.seek_absolute_samples(sample_pos);
                    update_editor_transport(&window);
                    window.set_editor_status("Moved to previous gap".into());
                }
                None => window.set_editor_status("Previous gap not found".into()),
            }
        }
    });

    let split_window = weak_window.clone();
    window.on_editor_split(move || {
        if let Some(window) = split_window.upgrade() {
            match EDITOR_CONTEXT.split_at(EDITOR_PLAYBACK.position_samples()) {
                Ok(()) => {
                    update_editor_segments(&window);
                    update_player_segments(&window);
                    window.set_editor_status("Segment split at play cursor".into());
                }
                Err(error) => {
                    window.set_editor_status(format!("Split failed: {error}").into());
                }
            }
        }
    });

    let next_gap_window = weak_window.clone();
    window.on_editor_next_gap(move || {
        if let Some(window) = next_gap_window.upgrade() {
            match move_to_gap(SearchDirection::Forward) {
                Some(sample_pos) => {
                    EDITOR_PLAYBACK.seek_absolute_samples(sample_pos);
                    update_editor_transport(&window);
                    window.set_editor_status("Moved to next gap".into());
                }
                None => window.set_editor_status("Next gap not found".into()),
            }
        }
    });

    let first_five_window = weak_window.clone();
    window.on_editor_first_five_seconds(move || {
        if let Some(window) = first_five_window.upgrade() {
            match play_editor_preview(&window, PreviewKind::FirstFiveSeconds) {
                Ok(()) => window.set_editor_status("Playing first 5 seconds".into()),
                Err(error) => window.set_editor_status(format!("Preview failed: {error}").into()),
            }
        }
    });

    let segment_double_click_window = weak_window.clone();
    window.on_editor_segment_double_clicked(move |segment_index| {
        if let Some(window) = segment_double_click_window.upgrade() {
            let segment_index = segment_index.max(0) as usize;
            let Some(sample_pos) = EDITOR_CONTEXT.segment_start(segment_index) else {
                window.set_editor_status("Segment not found".into());
                return;
            };

            EDITOR_PLAYBACK.seek_absolute_samples(sample_pos);
            update_editor_transport(&window);
            window.set_editor_status(format!("Moved to segment {}", segment_index + 1).into());
        }
    });

    let delete_segment_window = weak_window.clone();
    window.on_editor_delete_segment(move |segment_index| {
        if let Some(window) = delete_segment_window.upgrade() {
            let segment_index = segment_index.max(0) as usize;
            match EDITOR_CONTEXT.delete_segment(segment_index) {
                Ok(()) => {
                    update_editor_segments(&window);
                    update_player_segments(&window);
                    window
                        .set_editor_status(format!("Deleted segment {}", segment_index + 1).into());
                }
                Err(error) => {
                    window.set_editor_status(format!("Delete failed: {error}").into());
                }
            }
        }
    });

    let tab_ptt = ptt.clone();
    window.on_active_tab_changed(move |active_tab| {
        if let Some(window) = weak_window.upgrade() {
            if active_tab == 2 {
                create_player_ptt(&window, &tab_ptt);
            } else {
                destroy_player_ptt(&window, &tab_ptt);
            }
        }
    });

    let player_ptt_window = window.as_weak();
    let player_ptt = ptt.clone();
    window.on_player_ptt(move || {
        if let Some(window) = player_ptt_window.upgrade() {
            let mut ptt = player_ptt.borrow_mut();
            let Some(ptt) = ptt.as_mut() else {
                window.set_player_status("PTT is not available".into());
                return;
            };

            let desired_state = match ptt.desired_state() {
                DesiredPttState::Off => DesiredPttState::On,
                DesiredPttState::On => DesiredPttState::Off,
            };
            ptt.set_desired_state(desired_state);
            window.set_player_status(ptt.status_text().into());
        }
    });

    let id_sequence = Rc::new(RefCell::new(None::<IdSequence>));
    let player_sequence = Rc::new(RefCell::new(None::<PlayerSequence>));
    let player_id_window = window.as_weak();
    let player_id_ptt = ptt.clone();
    let player_id_sequence = id_sequence.clone();
    let player_id_player_sequence = player_sequence.clone();
    window.on_player_id(move || {
        if let Some(window) = player_id_window.upgrade() {
            *player_id_player_sequence.borrow_mut() = None;
            EDITOR_PLAYBACK.stop();
            if player_id_ptt.borrow().is_none() {
                create_player_ptt(&window, &player_id_ptt);
            }

            let mut ptt = player_id_ptt.borrow_mut();
            let Some(ptt) = ptt.as_mut() else {
                window.set_player_status("PTT is not available".into());
                return;
            };

            let config = APP_STATE.config();
            ptt.set_desired_state(DesiredPttState::On);
            *player_id_sequence.borrow_mut() = Some(IdSequence {
                state: IdSequenceState::WaitingForPtt,
                callsign: config.callsign,
                radio_output: config.radio_output,
                restore_sample: EDITOR_PLAYBACK.position_samples(),
            });
            window.set_player_status(format!("{} | ID waiting for PTT", ptt.status_text()).into());
        }
    });

    let player_rewind_window = window.as_weak();
    window.on_player_rewind(move |seconds| {
        if let Some(window) = player_rewind_window.upgrade() {
            EDITOR_PLAYBACK.seek_relative(-(seconds as f64));
            update_player_transport(&window);
            window.set_player_status(format!("Rewind {seconds} seconds").into());
        }
    });

    let player_play_window = window.as_weak();
    let player_play_ptt = ptt.clone();
    let player_play_sequence = player_sequence.clone();
    let player_play_id_sequence = id_sequence.clone();
    window.on_player_toggle_playback(move || {
        if let Some(window) = player_play_window.upgrade() {
            if player_play_sequence.borrow().is_some() || EDITOR_PLAYBACK.is_playing() {
                EDITOR_PLAYBACK.stop();
                *player_play_sequence.borrow_mut() = None;
                *player_play_id_sequence.borrow_mut() = None;
                if let Some(ptt) = player_play_ptt.borrow_mut().as_mut() {
                    ptt.set_desired_state(DesiredPttState::Off);
                    window.set_player_status(format!("{} | Stopped", ptt.status_text()).into());
                } else {
                    window.set_player_status("Stopped".into());
                }
                update_player_transport(&window);
                return;
            }

            if player_play_ptt.borrow().is_none() {
                create_player_ptt(&window, &player_play_ptt);
            }

            let mut ptt = player_play_ptt.borrow_mut();
            let Some(ptt) = ptt.as_mut() else {
                window.set_player_status("PTT is not available".into());
                return;
            };

            let cursor = EDITOR_PLAYBACK.position_samples();
            let Some(segment_index) = EDITOR_CONTEXT.active_segment_index(cursor) else {
                window.set_player_status("No segment at play cursor".into());
                return;
            };
            let Some(stop_sample) = EDITOR_CONTEXT.segment_end(segment_index) else {
                window.set_player_status("Segment end not found".into());
                return;
            };
            if cursor >= stop_sample {
                window.set_player_status("Play cursor is at the end of the segment".into());
                return;
            }

            ptt.set_desired_state(DesiredPttState::On);
            *player_play_sequence.borrow_mut() = Some(PlayerSequence {
                state: PlayerSequenceState::WaitingForPtt,
                start_sample: cursor,
                stop_sample,
                segment_index,
            });
            window.set_player_playing(true);
            window.set_player_status(
                format!(
                    "{} | Waiting to play segment {}",
                    ptt.status_text(),
                    segment_index + 1
                )
                .into(),
            );
        }
    });

    let player_forward_window = window.as_weak();
    window.on_player_forward(move |seconds| {
        if let Some(window) = player_forward_window.upgrade() {
            EDITOR_PLAYBACK.seek_relative(seconds as f64);
            update_player_transport(&window);
            window.set_player_status(format!("Forward {seconds} seconds").into());
        }
    });

    let player_segment_select_window = window.as_weak();
    window.on_player_segment_selected(move |segment_index| {
        if let Some(window) = player_segment_select_window.upgrade() {
            move_player_cursor_to_segment(&window, segment_index);
        }
    });

    let player_segment_window = window.as_weak();
    window.on_player_segment_double_clicked(move |segment_index| {
        if let Some(window) = player_segment_window.upgrade() {
            move_player_cursor_to_segment(&window, segment_index);
        }
    });

    let playback_timer = Timer::default();
    let timer_window = window.as_weak();
    let timer_ptt = ptt.clone();
    let timer_id_sequence = id_sequence.clone();
    let timer_player_sequence = player_sequence.clone();
    playback_timer.start(TimerMode::Repeated, Duration::from_millis(50), move || {
        if let Some(window) = timer_window.upgrade() {
            update_editor_transport(&window);
            if timer_id_sequence.borrow().is_none() {
                update_player_transport(&window);
            }
            update_player_ptt_status(&window, &timer_ptt);
            update_id_sequence(&window, &timer_ptt, &timer_id_sequence);
            update_player_sequence(&window, &timer_ptt, &timer_player_sequence);
        }
    });

    window.run()
}

fn populate_setup(window: &MainWindow, config: &AppConfig, devices: &devices::DeviceLists) {
    window.set_audio_output_options(model_from_strings(&devices.auto_outputs));
    window.set_audio_input_options(model_from_strings(&devices.auto_inputs));
    window.set_radio_output_options(model_from_strings(&devices.radio_outputs));
    window.set_radio_input_options(model_from_strings(&devices.radio_inputs));
    window.set_ptt_port_options(model_from_strings(&devices.ptt_ports));

    window.set_callsign(config.callsign.clone().into());
    window.set_auto_output_index(index_of(&devices.auto_outputs, &config.auto_output));
    window.set_auto_input_index(index_of(&devices.auto_inputs, &config.auto_input));
    window.set_radio_output_index(index_of(&devices.radio_outputs, &config.radio_output));
    window.set_radio_input_index(index_of(&devices.radio_inputs, &config.radio_input));
    window.set_ptt_port_index(index_of(&devices.ptt_ports, &config.ptt_port));
    window.set_timeout_value(format_timeout(config.repeater_timeout_seconds).into());
    window.set_warmup_seconds(format_tenths(config.repeater_warmup_tenths).into());
    window.set_reset_seconds(format_tenths(config.repeater_reset_tenths).into());

    set_saved_setup(window, config);
    window.set_setup_status(format!("Config file: {}", config::config_path_display()).into());
}

fn set_saved_setup(window: &MainWindow, config: &AppConfig) {
    window.set_saved_callsign(config.callsign.clone().into());
    window.set_saved_auto_output(config.auto_output.clone().into());
    window.set_saved_auto_input(config.auto_input.clone().into());
    window.set_saved_radio_output(config.radio_output.clone().into());
    window.set_saved_radio_input(config.radio_input.clone().into());
    window.set_saved_ptt_port(config.ptt_port.clone().into());
    window.set_timeout_value(format_timeout(config.repeater_timeout_seconds).into());
    window.set_saved_timeout_value(format_timeout(config.repeater_timeout_seconds).into());
    window.set_warmup_seconds(format_tenths(config.repeater_warmup_tenths).into());
    window.set_reset_seconds(format_tenths(config.repeater_reset_tenths).into());
    window.set_saved_warmup_seconds(format_tenths(config.repeater_warmup_tenths).into());
    window.set_saved_reset_seconds(format_tenths(config.repeater_reset_tenths).into());
}

fn populate_editor(window: &MainWindow, config: &AppConfig) {
    if config.editor_mp3_path.is_empty() {
        window.set_editor_file_path(String::new().into());
        window.set_editor_status("No MP3 loaded".into());
        update_editor_segments(window);
        update_player_segments(window);
        return;
    }

    load_editor_file(window, Path::new(&config.editor_mp3_path), false);
}

fn load_editor_file(window: &MainWindow, path: &Path, persist_selection: bool) {
    window.set_editor_file_path(path.display().to_string().into());
    window.set_editor_status("Loading MP3...".into());

    match EDITOR_CONTEXT.load_mp3(path) {
        Ok(snapshot) => {
            EDITOR_PLAYBACK.reset_for_new_audio();
            EDITOR_CONTEXT.with_pcm_audio(|audio| {
                if let Some(audio) = audio {
                    EDITOR_PLAYBACK.load_audio_position(audio);
                }
            });
            let path_string = path.display().to_string();
            window.set_editor_file_path(path_string.clone().into());
            update_editor_transport(window);
            update_player_transport(window);
            update_editor_segments(window);
            update_player_segments(window);
            window.set_editor_status(
                format!(
                    "Loaded {:.1} seconds, {} samples",
                    snapshot.duration_seconds, snapshot.pcm_frame_count
                )
                .into(),
            );

            if persist_selection {
                let mut config = APP_STATE.config();
                config.editor_mp3_path = path_string;
                if let Err(error) = APP_STATE.save(config) {
                    window.set_editor_status(
                        format!("Loaded MP3, but config save failed: {error}").into(),
                    );
                }
            }
        }
        Err(error) => {
            EDITOR_CONTEXT.clear();
            EDITOR_PLAYBACK.reset_for_new_audio();
            update_editor_transport(window);
            update_player_transport(window);
            update_editor_segments(window);
            update_player_segments(window);
            window.set_editor_status(format!("Failed to load MP3: {error}").into());
        }
    }
}

fn update_editor_transport(window: &MainWindow) {
    window.set_editor_playing(EDITOR_PLAYBACK.is_playing());
    window.set_editor_play_cursor(format_play_cursor(EDITOR_PLAYBACK.position_seconds()).into());
}

fn update_player_transport(window: &MainWindow) {
    let sample_pos = EDITOR_PLAYBACK.position_samples();
    window.set_player_playing(EDITOR_PLAYBACK.is_playing());
    window.set_player_play_cursor(format_play_cursor(EDITOR_PLAYBACK.position_seconds()).into());

    if let Some(segment_index) = EDITOR_CONTEXT.active_segment_index(sample_pos) {
        window.set_selected_player_segment(segment_index as i32);
        update_player_segment_remaining(window, segment_index, sample_pos);
    }
}

fn update_player_segment_remaining(window: &MainWindow, segment_index: usize, sample_pos: usize) {
    let remaining_seconds = EDITOR_CONTEXT.with_pcm_audio(|audio| {
        let Some(audio) = audio else {
            return 0.0;
        };
        let Some(&segment_end) = audio.segments.get(segment_index) else {
            return 0.0;
        };

        let remaining_samples = (segment_end as usize).saturating_sub(sample_pos);
        remaining_samples as f64 / audio.sample_rate as f64
    });

    window.set_player_segment_remaining(format_play_cursor(remaining_seconds).into());
}

fn move_player_cursor_to_segment(window: &MainWindow, segment_index: i32) {
    let segment_index = segment_index.max(0) as usize;
    let Some(sample_pos) = EDITOR_CONTEXT.segment_start(segment_index) else {
        window.set_player_status("Segment not found".into());
        return;
    };

    EDITOR_PLAYBACK.seek_absolute_samples(sample_pos);
    update_player_transport(window);
    window.set_player_status(format!("Moved to segment {}", segment_index + 1).into());
}

fn update_editor_segments(window: &MainWindow) {
    window.set_editor_segments(model_from_strings(&EDITOR_CONTEXT.segment_rows()));
    window.set_selected_editor_segment(-1);
}

fn update_player_segments(window: &MainWindow) {
    window.set_player_segments(model_from_strings(&EDITOR_CONTEXT.segment_rows()));
    update_player_transport(window);
}

fn create_player_ptt(window: &MainWindow, ptt: &Rc<RefCell<Option<Ptt>>>) {
    if ptt.borrow().is_some() {
        return;
    }

    let config = APP_STATE.config();
    let timing = PttTiming::from_config(&config);
    match Ptt::new(timing, &config.ptt_port) {
        Ok(new_ptt) => {
            *ptt.borrow_mut() = Some(new_ptt);
            update_player_ptt_status(window, ptt);
        }
        Err(error) => {
            window.set_player_status(format!("PTT unavailable: {error}").into());
        }
    }
}

fn destroy_player_ptt(window: &MainWindow, ptt: &Rc<RefCell<Option<Ptt>>>) {
    if ptt.borrow_mut().take().is_some() {
        window.set_player_status("PTT inactive".into());
    }
}

fn update_player_ptt_status(window: &MainWindow, ptt: &Rc<RefCell<Option<Ptt>>>) {
    let mut ptt = ptt.borrow_mut();
    if let Some(ptt) = ptt.as_mut() {
        ptt.update();
        window.set_player_status(ptt.status_text().into());
    }
}

struct IdSequence {
    state: IdSequenceState,
    callsign: String,
    radio_output: String,
    restore_sample: usize,
}

enum IdSequenceState {
    WaitingForPtt,
    Playing,
}

struct PlayerSequence {
    state: PlayerSequenceState,
    start_sample: usize,
    stop_sample: usize,
    segment_index: usize,
}

enum PlayerSequenceState {
    WaitingForPtt,
    Playing,
}

fn update_id_sequence(
    window: &MainWindow,
    ptt: &Rc<RefCell<Option<Ptt>>>,
    id_sequence: &Rc<RefCell<Option<IdSequence>>>,
) {
    let mut id_sequence = id_sequence.borrow_mut();
    let Some(sequence) = id_sequence.as_mut() else {
        return;
    };

    let mut ptt = ptt.borrow_mut();
    let Some(ptt) = ptt.as_mut() else {
        window.set_player_status("ID cancelled: PTT is not available".into());
        *id_sequence = None;
        return;
    };

    match sequence.state {
        IdSequenceState::WaitingForPtt => {
            if ptt.state() == PttState::Active {
                let id_audio = callsign_to_morse_audio(&sequence.callsign, 48_000);
                match EDITOR_PLAYBACK.play(id_audio, &sequence.radio_output) {
                    Ok(()) => {
                        sequence.state = IdSequenceState::Playing;
                        window.set_player_status(
                            format!("{} | ID playing", ptt.status_text()).into(),
                        );
                    }
                    Err(error) => {
                        restore_editor_playback_audio(sequence.restore_sample);
                        ptt.set_desired_state(DesiredPttState::Off);
                        window.set_player_status(
                            format!("{} | ID failed: {error}", ptt.status_text()).into(),
                        );
                        *id_sequence = None;
                    }
                }
            } else {
                window.set_player_status(
                    format!("{} | ID waiting for PTT", ptt.status_text()).into(),
                );
            }
        }
        IdSequenceState::Playing => {
            if EDITOR_PLAYBACK.is_playing() {
                window.set_player_status(format!("{} | ID playing", ptt.status_text()).into());
            } else {
                restore_editor_playback_audio(sequence.restore_sample);
                ptt.set_desired_state(DesiredPttState::Off);
                window.set_player_status(format!("{} | ID complete", ptt.status_text()).into());
                *id_sequence = None;
            }
        }
    }
}

fn update_player_sequence(
    window: &MainWindow,
    ptt: &Rc<RefCell<Option<Ptt>>>,
    player_sequence: &Rc<RefCell<Option<PlayerSequence>>>,
) {
    let mut player_sequence = player_sequence.borrow_mut();
    let Some(sequence) = player_sequence.as_mut() else {
        return;
    };

    let mut ptt = ptt.borrow_mut();
    let Some(ptt) = ptt.as_mut() else {
        EDITOR_PLAYBACK.stop();
        window.set_player_playing(false);
        window.set_player_status("Playback cancelled: PTT is not available".into());
        *player_sequence = None;
        return;
    };

    match sequence.state {
        PlayerSequenceState::WaitingForPtt => {
            if ptt.state() == PttState::Active {
                let Some(pcm_audio) = EDITOR_CONTEXT.with_pcm_audio(|audio| audio.cloned()) else {
                    ptt.set_desired_state(DesiredPttState::Off);
                    window.set_player_playing(false);
                    window.set_player_status("No MP3 loaded".into());
                    *player_sequence = None;
                    return;
                };

                let output_name = APP_STATE.config().radio_output;
                match EDITOR_PLAYBACK.play_range(
                    pcm_audio,
                    &output_name,
                    sequence.start_sample,
                    sequence.stop_sample,
                    None,
                ) {
                    Ok(()) => {
                        sequence.state = PlayerSequenceState::Playing;
                        window.set_player_playing(true);
                        window.set_player_status(
                            format!(
                                "{} | Playing segment {}",
                                ptt.status_text(),
                                sequence.segment_index + 1
                            )
                            .into(),
                        );
                    }
                    Err(error) => {
                        ptt.set_desired_state(DesiredPttState::Off);
                        window.set_player_playing(false);
                        window.set_player_status(
                            format!("{} | Playback failed: {error}", ptt.status_text()).into(),
                        );
                        *player_sequence = None;
                    }
                }
            } else {
                window.set_player_playing(true);
                window.set_player_status(
                    format!(
                        "{} | Waiting to play segment {}",
                        ptt.status_text(),
                        sequence.segment_index + 1
                    )
                    .into(),
                );
            }
        }
        PlayerSequenceState::Playing => {
            if EDITOR_PLAYBACK.is_playing() {
                window.set_player_playing(true);
                window.set_player_status(
                    format!(
                        "{} | Playing segment {}",
                        ptt.status_text(),
                        sequence.segment_index + 1
                    )
                    .into(),
                );
            } else {
                ptt.set_desired_state(DesiredPttState::Off);
                skip_short_segments_after_playback(sequence.segment_index);
                window.set_player_playing(false);
                window.set_player_status(
                    format!(
                        "{} | Finished segment {}",
                        ptt.status_text(),
                        sequence.segment_index + 1
                    )
                    .into(),
                );
                *player_sequence = None;
            }
        }
    }
}

fn restore_editor_playback_audio(sample_pos: usize) {
    EDITOR_CONTEXT.with_pcm_audio(|audio| {
        if let Some(audio) = audio {
            EDITOR_PLAYBACK.load_audio_position(audio);
            EDITOR_PLAYBACK.seek_absolute_samples(sample_pos);
        }
    });
}

fn skip_short_segments_after_playback(finished_segment_index: usize) {
    const MIN_PLAYABLE_SEGMENT_SECONDS: f64 = 10.0;

    let mut next_segment_index = finished_segment_index + 1;

    while let Some(duration) = EDITOR_CONTEXT.segment_duration_seconds(next_segment_index) {
        if duration >= MIN_PLAYABLE_SEGMENT_SECONDS {
            if let Some((start, _)) = EDITOR_CONTEXT.segment_bounds(next_segment_index) {
                EDITOR_PLAYBACK.seek_absolute_samples(start);
            }
            return;
        }

        next_segment_index += 1;
    }
}

fn move_to_gap(direction: SearchDirection) -> Option<usize> {
    const GAP_MIN_DURATION_SECONDS: f64 = 0.1;
    const GAP_THRESHOLD: f32 = 1.0 / 32.0;

    EDITOR_CONTEXT.search_gap(
        EDITOR_PLAYBACK.position_samples(),
        GAP_MIN_DURATION_SECONDS,
        GAP_THRESHOLD,
        direction,
    )
}

enum PreviewKind {
    FirstFiveSeconds,
    LastFiveSeconds,
}

fn play_editor_preview(window: &MainWindow, kind: PreviewKind) -> Result<(), String> {
    let Some(pcm_audio) = EDITOR_CONTEXT.with_pcm_audio(|audio| audio.cloned()) else {
        return Err("No MP3 loaded".to_string());
    };

    let cursor = EDITOR_PLAYBACK.position_samples();
    let five_seconds = 5 * pcm_audio.sample_rate as usize;
    let sample_count = pcm_audio.samples.len();
    let output_name = APP_STATE.config().auto_output;

    let (start_sample, stop_sample, restore_sample) = match kind {
        PreviewKind::FirstFiveSeconds => {
            let stop_sample = cursor.saturating_add(five_seconds).min(sample_count);
            (cursor, stop_sample, Some(cursor))
        }
        PreviewKind::LastFiveSeconds => {
            let start_sample = cursor.saturating_sub(five_seconds);
            (start_sample, cursor.min(sample_count), None)
        }
    };

    EDITOR_PLAYBACK
        .play_range(
            pcm_audio,
            &output_name,
            start_sample,
            stop_sample,
            restore_sample,
        )
        .map_err(|error| error.to_string())?;
    update_editor_transport(window);
    Ok(())
}

fn normalize_config(mut config: AppConfig, devices: &devices::DeviceLists) -> AppConfig {
    config.auto_output =
        valid_or_default(config.auto_output, &devices.auto_outputs, SYSTEM_DEFAULT);
    config.auto_input = valid_or_default(config.auto_input, &devices.auto_inputs, SYSTEM_DEFAULT);
    config.radio_output = valid_or_default(
        config.radio_output,
        &devices.radio_outputs,
        &devices.radio_outputs[0],
    );
    config.radio_input = valid_or_default(
        config.radio_input,
        &devices.radio_inputs,
        &devices.radio_inputs[0],
    );
    config.ptt_port = valid_or_default(config.ptt_port, &devices.ptt_ports, &devices.ptt_ports[0]);
    config.repeater_timeout_seconds = config.repeater_timeout_seconds.min(59 * 60 + 59);
    config.repeater_warmup_tenths = config.repeater_warmup_tenths.min(600);
    config.repeater_reset_tenths = config.repeater_reset_tenths.min(600);
    config
}

fn valid_or_default(value: String, options: &[String], default: &str) -> String {
    if options.iter().any(|option| option == &value) {
        value
    } else {
        default.to_string()
    }
}

fn index_of(options: &[String], value: &str) -> i32 {
    options
        .iter()
        .position(|option| option == value)
        .unwrap_or_default() as i32
}

fn model_from_strings(values: &[String]) -> ModelRc<SharedString> {
    let values = values
        .iter()
        .map(|value| SharedString::from(value.as_str()))
        .collect::<Vec<_>>();

    ModelRc::from(Rc::new(VecModel::from(values)))
}

fn parse_seconds_to_tenths(value: &str) -> Result<u32, &'static str> {
    let seconds = value
        .trim()
        .parse::<f64>()
        .map_err(|_| "must be a number of seconds")?;

    if !seconds.is_finite() || seconds < 0.0 {
        return Err("must be zero or greater");
    }

    if seconds > 60.0 {
        return Err("must be 60 seconds or less");
    }

    Ok((seconds * 10.0).round() as u32)
}

fn format_tenths(value: u32) -> String {
    format!("{}.{:01}", value / 10, value % 10)
}

fn parse_timeout(value: &str) -> Result<u32, &'static str> {
    let trimmed = value.trim();
    let Some((minutes, seconds)) = trimmed.split_once(':') else {
        return Err("must use minute:second format, for example 5:00");
    };

    if seconds.contains(':') {
        return Err("must use one colon, for example 5:00");
    }

    let minutes = minutes
        .trim()
        .parse::<u32>()
        .map_err(|_| "minutes must be a whole number")?;
    let seconds = seconds
        .trim()
        .parse::<u32>()
        .map_err(|_| "seconds must be a whole number")?;

    if seconds > 59 {
        return Err("seconds must be between 0 and 59");
    }

    let total_seconds = minutes
        .checked_mul(60)
        .and_then(|minutes| minutes.checked_add(seconds))
        .ok_or("is too large")?;

    if total_seconds > 59 * 60 + 59 {
        return Err("must be 59:59 or less");
    }

    Ok(total_seconds)
}

fn format_timeout(total_seconds: u32) -> String {
    format!("{}:{:02}", total_seconds / 60, total_seconds % 60)
}

fn format_play_cursor(seconds: f64) -> String {
    let seconds = seconds.max(0.0);
    let minutes = (seconds / 60.0).floor() as u64;
    let seconds = seconds - minutes as f64 * 60.0;

    format!("{minutes}:{seconds:05.2}")
}
