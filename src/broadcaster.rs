use async_trait::async_trait;
use log::{error, info, warn};
use std::num::{NonZeroU32, NonZeroU8};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use tokio::io::AsyncWriteExt;
use tokio::sync::broadcast;
use tokio::time::{timeout, Duration};
use vorbis_rs::{VorbisBitrateManagementStrategy, VorbisEncoderBuilder};

use crate::service::{ChatMessage, RadioServiceServer, StationInfo};
use zel_core::protocol::RequestContext;

type AudioBlock = Vec<Vec<f32>>;

#[derive(Clone)]
pub struct RadioBroadcaster {
    station_name: String,
    station_desc: String,
    sample_rate: u32,
    channels: u8,
    pcm_broadcast_tx: broadcast::Sender<AudioBlock>, // Broadcast PCM audio blocks
    chat_broadcast_tx: broadcast::Sender<ChatMessage>, // Broadcast chat messages
    listener_count: Arc<AtomicUsize>,
}

impl RadioBroadcaster {
    pub fn new(
        name: impl Into<String>,
        desc: impl Into<String>,
        sample_rate: u32,
        channels: u8,
    ) -> (Self, broadcast::Sender<AudioBlock>) {
        // Broadcast channel for PCM audio blocks
        let (pcm_broadcast_tx, _) = broadcast::channel(100);
        let tx_clone = pcm_broadcast_tx.clone();

        // Broadcast channel for chat messages
        let (chat_broadcast_tx, _) = broadcast::channel(100);

        let broadcaster = Self {
            station_name: name.into(),
            station_desc: desc.into(),
            sample_rate,
            channels,
            pcm_broadcast_tx,
            chat_broadcast_tx,
            listener_count: Arc::new(AtomicUsize::new(0)),
        };

        (broadcaster, tx_clone)
    }
}

#[async_trait]
impl RadioServiceServer for RadioBroadcaster {
    async fn get_info(&self, _ctx: RequestContext) -> Result<StationInfo, String> {
        Ok(StationInfo {
            name: self.station_name.clone(),
            description: self.station_desc.clone(),
            bitrate: 128000,
            sample_rate: self.sample_rate,
            channels: self.channels,
            listeners: self.listener_count.load(Ordering::Relaxed),
        })
    }

    async fn send_chat(&self, ctx: RequestContext, message: String) -> Result<(), String> {
        use std::time::SystemTime;

        // Get listener info from connection extensions
        let listener_info = ctx
            .connection_extensions()
            .get::<crate::service::ListenerInfo>()
            .ok_or("Listener info not found")?;

        let chat = ChatMessage {
            listener_id: listener_info.id,
            nickname: listener_info.nickname.clone(),
            message,
            timestamp: SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_secs(),
        };

        // Broadcast to all chat subscribers
        let _ = self.chat_broadcast_tx.send(chat);
        Ok(())
    }

    async fn chat_stream(
        &self,
        _ctx: RequestContext,
        mut sink: crate::service::RadioServiceChatStreamSink,
    ) -> Result<(), String> {
        let mut chat_rx = self.chat_broadcast_tx.subscribe();

        while let Ok(msg) = chat_rx.recv().await {
            if sink.send(msg).await.is_err() {
                break;
            }
        }

        Ok(())
    }

    async fn listen(
        &self,
        _ctx: RequestContext,
        mut send: iroh::endpoint::SendStream,
        _recv: iroh::endpoint::RecvStream,
    ) -> Result<(), String> {
        let listener_id = self.listener_count.fetch_add(1, Ordering::Relaxed);
        info!("[Broadcaster] Listener {} connected", listener_id);

        // Subscribe to PCM broadcast - each listener gets ALL audio blocks
        let mut pcm_rx = self.pcm_broadcast_tx.subscribe();

        // Spawn encoder task for THIS listener
        let sample_rate = self.sample_rate;
        let channels = self.channels;

        let (ogg_tx, mut ogg_rx) = tokio::sync::mpsc::channel::<Vec<u8>>(10);

        let encoder_task = tokio::task::spawn_blocking(move || {
            // Custom Write impl that sends to channel
            struct ChannelWriter {
                tx: tokio::sync::mpsc::Sender<Vec<u8>>,
                buffer: Vec<u8>,
            }

            impl std::io::Write for ChannelWriter {
                fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                    self.buffer.extend_from_slice(buf);
                    if self.buffer.len() >= 8192 {
                        let chunk = self.buffer.clone();
                        self.buffer.clear();
                        // If send fails, listener disconnected - return error to stop encoder
                        self.tx.blocking_send(chunk).map_err(|_| {
                            std::io::Error::new(
                                std::io::ErrorKind::BrokenPipe,
                                "Listener disconnected",
                            )
                        })?;
                    }
                    Ok(buf.len())
                }

                fn flush(&mut self) -> std::io::Result<()> {
                    if !self.buffer.is_empty() {
                        let chunk = self.buffer.clone();
                        self.buffer.clear();
                        // If send fails, listener disconnected - return error to stop encoder
                        self.tx.blocking_send(chunk).map_err(|_| {
                            std::io::Error::new(
                                std::io::ErrorKind::BrokenPipe,
                                "Listener disconnected",
                            )
                        })?;
                    }
                    Ok(())
                }
            }

            impl Drop for ChannelWriter {
                fn drop(&mut self) {
                    let _ = std::io::Write::flush(self);
                }
            }

            let writer = ChannelWriter {
                tx: ogg_tx,
                buffer: Vec::new(),
            };

            let mut encoder = VorbisEncoderBuilder::new(
                NonZeroU32::new(sample_rate).unwrap(),
                NonZeroU8::new(channels).unwrap(),
                writer,
            )
            .map_err(|e| format!("Encoder setup: {}", e))?
            .bitrate_management_strategy(VorbisBitrateManagementStrategy::QualityVbr {
                target_quality: 0.5,
            })
            .build()
            .map_err(|e| format!("Encoder build: {}", e))?;

            // Encode PCM blocks as they arrive
            info!("[Encoder {}] Starting encoding loop", listener_id);
            let mut block_count = 0;
            while let Ok(pcm_block) = pcm_rx.blocking_recv() {
                block_count += 1;
                if block_count % 100 == 0 {
                    info!("[Encoder {}] Encoded {} blocks", listener_id, block_count);
                }
                if let Err(e) = encoder.encode_audio_block(&pcm_block) {
                    error!("[Encoder {}] Encoding error: {}", listener_id, e);
                    break;
                }
            }
            info!(
                "[Encoder {}] Encoding loop ended, total blocks: {}",
                listener_id, block_count
            );

            // Finish encoder
            let _ = encoder.finish();

            Ok::<_, String>(())
        });

        // Send encoded OGG chunks to client with stall detection
        const SEND_TIMEOUT: Duration = Duration::from_secs(30);

        while let Some(chunk) = ogg_rx.recv().await {
            match timeout(SEND_TIMEOUT, send.write_all(&chunk)).await {
                Ok(Ok(())) => {
                    // Successfully sent chunk
                }
                Ok(Err(e)) => {
                    error!("Send error to listener {}: {}", listener_id, e);
                    break;
                }
                Err(_) => {
                    warn!(
                        "Listener {} stalled (no progress for {} seconds), disconnecting",
                        listener_id,
                        SEND_TIMEOUT.as_secs()
                    );
                    break;
                }
            }
        }

        // Cleanup
        let _ = send.finish();
        encoder_task.abort();

        self.listener_count.fetch_sub(1, Ordering::Relaxed);
        info!("[Broadcaster] Listener {} disconnected", listener_id);

        Ok(())
    }
}
