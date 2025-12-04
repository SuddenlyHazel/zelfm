use async_trait::async_trait;
use log::{error, info};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use tokio::sync::broadcast;

use crate::service::{RadioServiceServer, StationInfo};
use zel_core::protocol::RequestContext;

#[derive(Clone)]
pub struct RadioBroadcaster {
    station_name: String,
    station_desc: String,
    sample_rate: u32,
    channels: u8,
    ogg_broadcast_tx: broadcast::Sender<Vec<u8>>, // Broadcast OGG bytes
    listener_count: Arc<AtomicUsize>,
}

impl RadioBroadcaster {
    pub fn new(
        name: impl Into<String>,
        desc: impl Into<String>,
        sample_rate: u32,
        channels: u8,
    ) -> (Self, broadcast::Sender<Vec<u8>>) {
        // Broadcast channel for OGG bytes
        let (ogg_broadcast_tx, _) = broadcast::channel(100);
        let tx_clone = ogg_broadcast_tx.clone();

        let broadcaster = Self {
            station_name: name.into(),
            station_desc: desc.into(),
            sample_rate,
            channels,
            ogg_broadcast_tx,
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
            bitrate: 128000, // Fixed medium quality
            sample_rate: self.sample_rate,
            channels: self.channels,
            listeners: self.listener_count.load(Ordering::Relaxed),
        })
    }

    async fn listen(
        &self,
        _ctx: RequestContext,
        mut send: iroh::endpoint::SendStream,
        _recv: iroh::endpoint::RecvStream,
    ) -> Result<(), String> {
        let listener_id = self.listener_count.fetch_add(1, Ordering::Relaxed);
        info!("[Broadcaster] Listener {} connected", listener_id);

        // Subscribe to OGG broadcast - all listeners get same encoded stream
        let mut ogg_rx = self.ogg_broadcast_tx.subscribe();

        // Stream OGG chunks to client
        while let Ok(ogg_chunk) = ogg_rx.recv().await {
            if let Err(e) = send.write_all(&ogg_chunk).await {
                error!("Send error to listener {}: {}", listener_id, e);
                break;
            }
        }

        // Cleanup
        let _ = send.finish();
        self.listener_count.fetch_sub(1, Ordering::Relaxed);
        info!("[Broadcaster] Listener {} disconnected", listener_id);

        Ok(())
    }
}
