use std::time::{Duration, Instant};

use crate::config::AppConfig;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PttState {
    Inactive,
    WarmUp,
    Active,
    Reset,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DesiredPttState {
    Off,
    On,
}

#[derive(Clone, Copy, Debug)]
pub struct PttTiming {
    pub timeout: Duration,
    pub warmup: Duration,
    pub reset: Duration,
}

impl PttTiming {
    pub fn from_config(config: &AppConfig) -> Self {
        Self {
            timeout: Duration::from_secs(config.repeater_timeout_seconds as u64),
            warmup: Duration::from_millis(config.repeater_warmup_tenths as u64 * 100),
            reset: Duration::from_millis(config.repeater_reset_tenths as u64 * 100),
        }
    }
}

pub struct Ptt {
    state: PttState,
    desired_state: DesiredPttState,
    timing: PttTiming,
    port_name: Option<String>,
    port: Option<Box<dyn serialport::SerialPort>>,
    timeout_deadline: Option<Instant>,
    warmup_deadline: Option<Instant>,
    reset_deadline: Option<Instant>,
}

impl Ptt {
    pub fn new(timing: PttTiming, port_name: &str) -> Result<Self, PttError> {
        if port_name.is_empty()
            || port_name.starts_with("Select ")
            || port_name.starts_with("No COM ")
        {
            return Err(PttError::MissingPort);
        }

        let mut port = serialport::new(port_name, 9600)
            .timeout(Duration::from_millis(100))
            .open()?;
        port.write_request_to_send(false)?;

        Ok(Self {
            state: PttState::Inactive,
            desired_state: DesiredPttState::Off,
            timing,
            port_name: Some(port_name.to_string()),
            port: Some(port),
            timeout_deadline: None,
            warmup_deadline: None,
            reset_deadline: None,
        })
    }

    pub fn new_without_port(timing: PttTiming) -> Self {
        Self {
            state: PttState::Inactive,
            desired_state: DesiredPttState::Off,
            timing,
            port_name: None,
            port: None,
            timeout_deadline: None,
            warmup_deadline: None,
            reset_deadline: None,
        }
    }

    pub fn state(&self) -> PttState {
        self.state
    }

    pub fn desired_state(&self) -> DesiredPttState {
        self.desired_state
    }

    pub fn set_desired_state(&mut self, desired_state: DesiredPttState) {
        self.set_desired_state_at(desired_state, Instant::now());
    }

    pub fn set_desired_state_at(&mut self, desired_state: DesiredPttState, now: Instant) {
        self.desired_state = desired_state;
        self.update_at(now);
    }

    pub fn update(&mut self) {
        self.update_at(Instant::now());
    }

    pub fn update_at(&mut self, now: Instant) {
        match self.state {
            PttState::Inactive => {
                if self.desired_state == DesiredPttState::On {
                    self.enter_warmup(now);
                }
            }
            PttState::WarmUp => {
                if self.timeout_expired(now) || self.desired_state == DesiredPttState::Off {
                    self.enter_reset(now);
                } else if self.warmup_deadline.is_some_and(|deadline| now >= deadline) {
                    self.enter_active();
                }
            }
            PttState::Active => {
                if self.timeout_expired(now) || self.desired_state == DesiredPttState::Off {
                    self.enter_reset(now);
                }
            }
            PttState::Reset => {
                if self.reset_deadline.is_some_and(|deadline| now >= deadline) {
                    self.restart_timeout(now);
                    match self.desired_state {
                        DesiredPttState::Off => self.enter_inactive(),
                        DesiredPttState::On => self.enter_warmup(now),
                    }
                }
            }
        }
    }

    pub fn status_text(&self) -> String {
        self.status_text_at(Instant::now())
    }

    pub fn status_text_at(&self, now: Instant) -> String {
        let desired = match self.desired_state {
            DesiredPttState::Off => "Off",
            DesiredPttState::On => "On",
        };
        let port = self.port_name.as_deref().unwrap_or("no port");

        match self.state {
            PttState::Inactive => format!("Inactive, desired {desired}, {port}"),
            PttState::WarmUp => format!(
                "WarmUp, desired {desired}, warmup {}, timeout {}, {port}",
                format_remaining(self.warmup_deadline, now),
                format_remaining(self.timeout_deadline, now),
            ),
            PttState::Active => format!(
                "Active, desired {desired}, timeout {}, {port}",
                format_remaining(self.timeout_deadline, now),
            ),
            PttState::Reset => format!(
                "Reset, desired {desired}, reset {}, {port}",
                format_remaining(self.reset_deadline, now),
            ),
        }
    }

    fn enter_inactive(&mut self) {
        self.state = PttState::Inactive;
        self.timeout_deadline = None;
        self.warmup_deadline = None;
        self.reset_deadline = None;
    }

    fn enter_warmup(&mut self, now: Instant) {
        self.state = PttState::WarmUp;
        self.set_rts(true);
        self.timeout_deadline = Some(now + self.timing.timeout);
        self.warmup_deadline = Some(now + self.timing.warmup);
        self.reset_deadline = None;
    }

    fn enter_active(&mut self) {
        self.state = PttState::Active;
        self.warmup_deadline = None;
        self.reset_deadline = None;
    }

    fn enter_reset(&mut self, now: Instant) {
        self.state = PttState::Reset;
        self.set_rts(false);
        self.warmup_deadline = None;
        self.reset_deadline = Some(now + self.timing.reset);
    }

    fn restart_timeout(&mut self, now: Instant) {
        self.timeout_deadline = Some(now + self.timing.timeout);
    }

    fn timeout_expired(&self, now: Instant) -> bool {
        self.timeout_deadline
            .is_some_and(|deadline| now >= deadline)
    }

    fn set_rts(&mut self, active: bool) {
        if let Some(port) = self.port.as_mut() {
            let _ = port.write_request_to_send(active);
        }
    }
}

impl Drop for Ptt {
    fn drop(&mut self) {
        self.set_rts(false);
    }
}

fn format_remaining(deadline: Option<Instant>, now: Instant) -> String {
    let remaining = deadline
        .map(|deadline| deadline.saturating_duration_since(now))
        .unwrap_or_default();
    format!("{:.1}s", remaining.as_secs_f64())
}

#[derive(Debug)]
pub enum PttError {
    MissingPort,
    Serial(serialport::Error),
}

impl std::fmt::Display for PttError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingPort => write!(formatter, "no PTT COM port selected"),
            Self::Serial(error) => write!(formatter, "serial port error: {error}"),
        }
    }
}

impl std::error::Error for PttError {}

impl From<serialport::Error> for PttError {
    fn from(error: serialport::Error) -> Self {
        Self::Serial(error)
    }
}

#[cfg(test)]
mod tests {
    use super::{DesiredPttState, Ptt, PttState, PttTiming};
    use std::time::{Duration, Instant};

    fn timing() -> PttTiming {
        PttTiming {
            timeout: Duration::from_secs(10),
            warmup: Duration::from_secs(2),
            reset: Duration::from_secs(3),
        }
    }

    #[test]
    fn inactive_off_stays_inactive() {
        let now = Instant::now();
        let mut ptt = Ptt::new_without_port(timing());

        ptt.set_desired_state_at(DesiredPttState::Off, now);

        assert_eq!(ptt.state(), PttState::Inactive);
    }

    #[test]
    fn inactive_on_enters_warmup_then_active_after_warmup() {
        let now = Instant::now();
        let mut ptt = Ptt::new_without_port(timing());

        ptt.set_desired_state_at(DesiredPttState::On, now);
        assert_eq!(ptt.state(), PttState::WarmUp);

        ptt.update_at(now + Duration::from_millis(1999));
        assert_eq!(ptt.state(), PttState::WarmUp);

        ptt.update_at(now + Duration::from_secs(2));
        assert_eq!(ptt.state(), PttState::Active);
    }

    #[test]
    fn warmup_off_enters_reset_and_ignores_on_until_reset_expires() {
        let now = Instant::now();
        let mut ptt = Ptt::new_without_port(timing());

        ptt.set_desired_state_at(DesiredPttState::On, now);
        ptt.set_desired_state_at(DesiredPttState::Off, now + Duration::from_secs(1));
        assert_eq!(ptt.state(), PttState::Reset);

        ptt.set_desired_state_at(DesiredPttState::On, now + Duration::from_secs(2));
        assert_eq!(ptt.state(), PttState::Reset);

        ptt.update_at(now + Duration::from_secs(4));
        assert_eq!(ptt.state(), PttState::WarmUp);
    }

    #[test]
    fn active_off_enters_reset_then_inactive_after_reset_if_off() {
        let now = Instant::now();
        let mut ptt = Ptt::new_without_port(timing());

        ptt.set_desired_state_at(DesiredPttState::On, now);
        ptt.update_at(now + Duration::from_secs(2));
        assert_eq!(ptt.state(), PttState::Active);

        ptt.set_desired_state_at(DesiredPttState::Off, now + Duration::from_secs(3));
        assert_eq!(ptt.state(), PttState::Reset);

        ptt.update_at(now + Duration::from_secs(6));
        assert_eq!(ptt.state(), PttState::Inactive);
    }

    #[test]
    fn timeout_forces_reset() {
        let now = Instant::now();
        let mut ptt = Ptt::new_without_port(timing());

        ptt.set_desired_state_at(DesiredPttState::On, now);
        ptt.update_at(now + Duration::from_secs(2));
        assert_eq!(ptt.state(), PttState::Active);

        ptt.update_at(now + Duration::from_secs(10));
        assert_eq!(ptt.state(), PttState::Reset);
    }

    #[test]
    fn status_includes_timers() {
        let now = Instant::now();
        let mut ptt = Ptt::new_without_port(timing());

        ptt.set_desired_state_at(DesiredPttState::On, now);

        let status = ptt.status_text_at(now + Duration::from_secs(1));
        assert!(status.contains("WarmUp"));
        assert!(status.contains("warmup 1.0s"));
        assert!(status.contains("timeout 9.0s"));
    }
}
