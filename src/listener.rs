use log::info;
use std::io::Cursor;
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

        let (_send, mut recv) = self.client.listen().await?;

        info!("[Listener] Stream opened, buffering...");

        // Buffer incoming OGG data
        let mut ogg_buffer = Vec::new();
        let mut chunk = vec![0u8; 8192];

        // Collect minimum buffer
        const MIN_BUFFER_SIZE: usize = 32768;
        while ogg_buffer.len() < MIN_BUFFER_SIZE {
            match recv.read(&mut chunk).await? {
                None => return Err(anyhow::anyhow!("Stream ended early")),
                Some(n) => ogg_buffer.extend_from_slice(&chunk[..n]),
            }
        }

        info!(
            "[Listener] Decoding ({} bytes buffered)...",
            ogg_buffer.len()
        );

        // Create decoder
        let cursor = Cursor::new(ogg_buffer.clone());
        let mut decoder = VorbisDecoder::new(cursor)?;

        let sample_rate = decoder.sampling_frequency().get();
        let channels = decoder.channels().get();
        info!("[Listener] Format: {} Hz, {} ch", sample_rate, channels);

        #[cfg(feature = "playback")]
        {
            let mut player = AudioPlayer::new(sample_rate, channels)?;

            // Decode and play initial buffer
            while let Some(samples) = decoder.decode_audio_block()? {
                player.play_samples(samples.samples())?;
            }

            info!("[Listener] Playing live stream...");

            // Continue streaming
            let start = std::time::Instant::now();
            loop {
                match recv.read(&mut chunk).await? {
                    None => break,
                    Some(n) => {
                        ogg_buffer.extend_from_slice(&chunk[..n]);

                        // Try decoding accumulated data
                        let cursor = Cursor::new(ogg_buffer.clone());
                        if let Ok(mut new_decoder) = VorbisDecoder::new(cursor) {
                            while let Some(samples) = new_decoder.decode_audio_block()? {
                                player.play_samples(samples.samples())?;
                            }
                        }
                    }
                }

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
            info!("[Listener] Playback disabled, consuming stream silently");

            let mut total_samples = 0;
            while let Some(samples) = decoder.decode_audio_block()? {
                total_samples += samples.samples()[0].len();
            }

            // Keep connection alive
            let start = std::time::Instant::now();
            loop {
                match recv.read(&mut chunk).await {
                    Ok(None) => break,
                    Ok(Some(_)) => {}
                    Err(_) => break,
                }

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
