use cpal::traits::{DeviceTrait, HostTrait};

pub const SYSTEM_DEFAULT: &str = "System default";

pub struct DeviceLists {
    pub auto_outputs: Vec<String>,
    pub auto_inputs: Vec<String>,
    pub radio_outputs: Vec<String>,
    pub radio_inputs: Vec<String>,
    pub ptt_ports: Vec<String>,
}

pub fn discover_devices() -> DeviceLists {
    let host = cpal::default_host();
    let outputs = audio_device_names(host.output_devices().ok());
    let inputs = audio_device_names(host.input_devices().ok());
    let ports = serialport::available_ports()
        .map(|ports| ports.into_iter().map(|port| port.port_name).collect())
        .unwrap_or_else(|_| vec![String::from("No COM ports found")]);

    DeviceLists {
        auto_outputs: with_system_default(outputs.clone()),
        auto_inputs: with_system_default(inputs.clone()),
        radio_outputs: with_empty_choice(outputs, "Select radio output"),
        radio_inputs: with_empty_choice(inputs, "Select radio input"),
        ptt_ports: with_empty_choice(ports, "Select PTT COM port"),
    }
}

fn audio_device_names<I>(devices: Option<I>) -> Vec<String>
where
    I: IntoIterator<Item = cpal::Device>,
{
    let mut names: Vec<String> = devices
        .into_iter()
        .flatten()
        .filter_map(|device| device.name().ok())
        .collect();

    names.sort();
    names.dedup();

    if names.is_empty() {
        names.push(String::from("No audio devices found"));
    }

    names
}

fn with_system_default(mut devices: Vec<String>) -> Vec<String> {
    devices.insert(0, String::from(SYSTEM_DEFAULT));
    devices
}

fn with_empty_choice(mut devices: Vec<String>, label: &str) -> Vec<String> {
    devices.insert(0, String::from(label));
    devices
}
