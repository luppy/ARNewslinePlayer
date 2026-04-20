use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};
use std::{
    fs, io,
    path::{Path, PathBuf},
    sync::Mutex,
};

const CONFIG_FILE_NAME: &str = "ar_newsline_player_config.json";

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct AppConfig {
    pub callsign: String,
    pub auto_output: String,
    pub auto_input: String,
    pub radio_output: String,
    pub radio_input: String,
    pub ptt_port: String,
    pub repeater_timeout_seconds: u32,
    pub repeater_warmup_tenths: u32,
    pub repeater_reset_tenths: u32,
    pub editor_mp3_path: String,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            callsign: String::new(),
            auto_output: String::from("System default"),
            auto_input: String::from("System default"),
            radio_output: String::new(),
            radio_input: String::new(),
            ptt_port: String::new(),
            repeater_timeout_seconds: 5 * 60,
            repeater_warmup_tenths: 10,
            repeater_reset_tenths: 50,
            editor_mp3_path: String::new(),
        }
    }
}

pub struct AppState {
    config: Mutex<AppConfig>,
}

pub static APP_STATE: Lazy<AppState> = Lazy::new(|| AppState {
    config: Mutex::new(AppConfig::default()),
});

impl AppState {
    pub fn load_from_disk(&self) -> io::Result<()> {
        let path = config_path()?;

        let config = match fs::read_to_string(&path) {
            Ok(contents) => serde_json::from_str(&contents).unwrap_or_default(),
            Err(error) if error.kind() == io::ErrorKind::NotFound => AppConfig::default(),
            Err(error) => return Err(error),
        };

        self.replace(config);
        Ok(())
    }

    pub fn config(&self) -> AppConfig {
        self.config
            .lock()
            .expect("app config lock poisoned")
            .clone()
    }

    pub fn replace(&self, config: AppConfig) {
        *self.config.lock().expect("app config lock poisoned") = config;
    }

    pub fn save(&self, config: AppConfig) -> io::Result<PathBuf> {
        let path = config_path()?;
        let contents = serde_json::to_string_pretty(&config)?;

        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }

        fs::write(&path, contents)?;
        self.replace(config);
        Ok(path)
    }
}

pub fn config_path() -> io::Result<PathBuf> {
    Ok(std::env::current_dir()?.join(CONFIG_FILE_NAME))
}

pub fn config_path_display() -> String {
    config_path()
        .unwrap_or_else(|_| Path::new(CONFIG_FILE_NAME).to_path_buf())
        .display()
        .to_string()
}
