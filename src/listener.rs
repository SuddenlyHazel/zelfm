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

        info!("[Listener] Stream opened, buffering OGG data...");

        // Spawn a task to collect streaming data
        // Small buffer (10 chunks = ~80KB = ~5 seconds at 128kbps) for responsive shutdown
        let (data_tx, data_rx) = tokio::sync::mpsc::channel::<Vec<u8>>(10);

        let recv_task = tokio::spawn(async move {
            let mut chunk = vec![0u8; 8192];
            loop {
                match recv.read(&mut chunk).await {
                    Ok(Some(n)) => {
                        if data_tx.send(chunk[..n].to_vec()).await.is_err() {
                            break;
                        }
                    }
                    Ok(None) => break,
                    Err(_) => break,
                }
            }
        });

        // Decode and play in blocking task
        let result = tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
            // Create a streaming reader that pulls from the channel
            struct ChannelReader {
                rx: tokio::sync::mpsc::Receiver<Vec<u8>>,
                buffer: Vec<u8>,
                position: usize,
            }

            impl ChannelReader {
                fn new(rx: tokio::sync::mpsc::Receiver<Vec<u8>>) -> Self {
                    Self {
                        rx,
                        buffer: Vec::new(),
                        position: 0,
                    }
                }
            }

            impl std::io::Read for ChannelReader {
                fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
                    // Fill from current buffer first
                    if self.position < self.buffer.len() {
                        let available = self.buffer.len() - self.position;
                        let to_copy = available.min(buf.len());
                        buf[..to_copy]
                            .copy_from_slice(&self.buffer[self.position..self.position + to_copy]);
                        self.position += to_copy;
                        return Ok(to_copy);
                    }

                    // Need more data from channel
                    match self.rx.blocking_recv() {
                        Some(chunk) => {
                            self.buffer = chunk;
                            self.position = 0;
                            self.read(buf) // Try again with new buffer
                        }
                        None => Ok(0), // EOF
                    }
                }
            }

            let reader = ChannelReader::new(data_rx);
            let mut decoder = VorbisDecoder::new(reader)?;

            let sample_rate = decoder.sampling_frequency().get();
            let channels = decoder.channels().get();
            info!("[Listener] Format: {} Hz, {} ch", sample_rate, channels);

            #[cfg(feature = "playback")]
            {
                let mut player = AudioPlayer::new(sample_rate, channels)?;
                info!("[Listener] Playing...");

                let start = std::time::Instant::now();

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
        })
        .await??;

        recv_task.abort();

        Ok(result)
    }
}
