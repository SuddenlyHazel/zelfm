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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub listener_id: usize,
    pub nickname: Option<String>,
    pub message: String,
    pub timestamp: u64,
}

/// Connection-level extension to track listener identity
#[derive(Debug, Clone)]
pub struct ListenerInfo {
    pub id: usize,
    pub nickname: Option<String>,
}

#[zel_service(name = "radio")]
pub trait RadioService {
    #[method(name = "info")]
    async fn get_info(&self) -> Result<StationInfo, String>;

    #[method(name = "send_chat")]
    async fn send_chat(&self, message: String) -> Result<(), String>;

    #[subscription(name = "chat_stream", item = "ChatMessage")]
    async fn chat_stream(&self) -> Result<(), String>;

    #[stream(name = "listen")]
    async fn listen(&self) -> Result<(), String>;
}
