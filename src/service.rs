use serde::{Deserialize, Serialize};
use zel_core::protocol::zel_service;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StationInfo {
    pub name: String,
    pub description: String,
    pub bitrate: u32,     // Fixed: 128000 (128 kbps)
    pub sample_rate: u32, // e.g., 44100 Hz
    pub channels: u8,     // e.g., 2 (stereo)
    pub listeners: usize,
}

// No StreamRequest needed - all listeners get same quality
#[zel_service(name = "radio")]
pub trait RadioService {
    #[method(name = "info")]
    async fn get_info(&self) -> Result<StationInfo, String>;

    #[stream(name = "listen")]
    async fn listen(&self) -> Result<(), String>;
}
