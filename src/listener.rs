use log::info;
use std::io::{Cursor, Read};
use std::sync::{Arc, Mutex};
use vorbis_rs::VorbisDecoder;

use crate::service::RadioServiceClient;

#[cfg(feature = "playback")]
use crate::audio_player::AudioPlayer;

pub struct RadioListener {
    client: RadioServiceClient,
}

impl RadioListener {
    pub fn new(client: RadioServiceClient) -> Self {
        Self { client }
    }

    pub async fn get_station_info(&self) -> anyhow::Result<()> {
        let info = self.client.get_info().await?;
        println!("\n=== Station Info ===");
        println!("Name: {}", info.name);
        println!("Description: {}", info.description);
        println!("Bitrate: {} kbps", info.bitrate / 1000);
        println!("Sample Rate: {} Hz", info.sample_rate);
        println!("Channels: {}", info.channels);
        println!("Listeners: {}", info.listeners);
        println!("====================\n");
        Ok(())
    }

    pub async fn listen(&self, duration_secs: Option<u64>) -> anyhow::Result<()> {
        info!("[Listener] Connecting...");

        let (_send, recv) = self.client.listen().await?;

        info!("[Listener] Stream opened, creating async reader...");

        // Create a bridge between async RecvStream and sync Read for VorbisDecoder
        let recv_wrapper = Arc::new(Mutex::new(recv));
        let sync_reader = SyncStreamReader::new(recv_wrapper);

        // Create decoder with the sync reader
        let mut decoder = VorbisDecoder::new(sync_reader)?;

        let sample_rate = decoder.sampling_frequency().get();
        let channels = decoder.channels().get();
        info!("[Listener] Format: {} Hz, {} ch", sample_rate, channels);

        #[cfg(feature = "playback")]
        {
            let mut player = AudioPlayer::new(sample_rate, channels)?;
            info!("[Listener] Playing...");

            let start = std::time::Instant::now();

            // Decode and play
            while let Some(samples) = decoder.decode_audio_block()? {
                player.play_samples(samples.samples())?;

                if let Some(max) = duration_secs {
                    if start.elapsed().as_secs() >= max {
                        break;
                    }
                }
            }

            player.finish();
        }

        #[cfg(not(feature = "playback"))]
        {
            info!("[Listener] Playback disabled, counting samples...");

            let mut total_samples = 0;
            let start = std::time::Instant::now();

            while let Some(samples) = decoder.decode_audio_block()? {
                total_samples += samples.samples()[0].len();

                if let Some(max) = duration_secs {
                    if start.elapsed().as_secs() >= max {
                        break;
                    }
                }
            }

            info!("[Listener] Processed {} samples", total_samples);
        }

        Ok(())
    }
}

/// Bridges async RecvStream to sync Read for VorbisDecoder
struct SyncStreamReader {
    recv: Arc<Mutex<iroh::endpoint::RecvStream>>,
    runtime: tokio::runtime::Handle,
}

impl SyncStreamReader {
    fn new(recv: Arc<Mutex<iroh::endpoint::RecvStream>>) -> Self {
        Self {
            recv,
            runtime: tokio::runtime::Handle::current(),
        }
    }
}

impl Read for SyncStreamReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        // Block on async read
        self.runtime.block_on(async {
            let mut recv = self.recv.lock().unwrap();
            match recv.read(buf).await {
                Ok(Some(n)) => Ok(n),
                Ok(None) => Ok(0), // EOF
                Err(e) => Err(std::io::Error::new(std::io::ErrorKind::Other, e)),
            }
        })
    }
}
