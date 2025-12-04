#[cfg(feature = "live-input")]
use cpal::traits::{DeviceTrait, HostTrait};

#[cfg(feature = "live-input")]
pub fn list_input_devices() -> anyhow::Result<()> {
    let host = cpal::default_host();

    println!("\n=== Available Input Devices ===\n");

    let mut found_any = false;
    for (idx, device) in host.input_devices()?.enumerate() {
        if let Ok(name) = device.name() {
            if let Ok(config) = device.default_input_config() {
                println!(
                    "  [{}] {} ({} Hz, {} ch)",
                    idx,
                    name,
                    config.sample_rate().0,
                    config.channels()
                );
                found_any = true;
            }
        }
    }

    if !found_any {
        println!("  No input devices found");
    }

    println!();
    Ok(())
}

#[cfg(feature = "live-input")]
pub fn find_device_by_name(host: &cpal::Host, search: &str) -> anyhow::Result<cpal::Device> {
    host.input_devices()?
        .find(|d| {
            d.name()
                .map(|n| n.to_lowercase().contains(&search.to_lowercase()))
                .unwrap_or(false)
        })
        .ok_or_else(|| anyhow::anyhow!("No device matching '{}' found", search))
}
