use log::{error, info};
use std::fs::File;
use std::io::Write;
use std::num::{NonZeroU32, NonZeroU8};
use std::path::Path;
use tokio::sync::broadcast;
use vorbis_rs::{VorbisBitrateManagementStrategy, VorbisEncoderBuilder};

use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::{DecoderOptions, CODEC_TYPE_NULL};
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

/// Custom Write implementation that broadcasts chunks to listeners
struct BroadcastWriter {
    tx: broadcast::Sender<Vec<u8>>,
    buffer: Vec<u8>,
    chunk_size: usize,
}

impl BroadcastWriter {
    fn new(tx: broadcast::Sender<Vec<u8>>) -> Self {
        Self {
            tx,
            buffer: Vec::new(),
            chunk_size: 8192,
        }
    }

    fn flush_if_needed(&mut self) -> std::io::Result<()> {
        if self.buffer.len() >= self.chunk_size {
            let chunk = self.buffer.clone();
            self.buffer.clear();
            // Ignore broadcast errors (no listeners)
            let _ = self.tx.send(chunk);
        }
        Ok(())
    }
}

impl Write for BroadcastWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.buffer.extend_from_slice(buf);
        self.flush_if_needed()?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        if !self.buffer.is_empty() {
            let chunk = self.buffer.clone();
            self.buffer.clear();
            let _ = self.tx.send(chunk);
        }
        Ok(())
    }
}

impl Drop for BroadcastWriter {
    fn drop(&mut self) {
        let _ = self.flush();
    }
}

/// Runs the audio pipeline: Decode any format → Encode to Vorbis → Broadcast bytes
pub fn audio_encode_loop(
    file_path: impl AsRef<Path>,
    sample_rate: u32,
    channels: u8,
    ogg_tx: broadcast::Sender<Vec<u8>>,
) -> anyhow::Result<()> {
    loop {
        info!("[Audio] Decoding: {}", file_path.as_ref().display());

        // Decode source file using symphonia
        match decode_and_stream(&file_path, sample_rate, channels, &ogg_tx) {
            Ok(_) => {
                info!("[Audio] File complete, looping...");
            }
            Err(e) => {
                error!("[Audio] Error: {}", e);
                // Continue looping even on error
                std::thread::sleep(std::time::Duration::from_secs(1));
            }
        }
    }
}

fn decode_and_stream(
    file_path: impl AsRef<Path>,
    target_sample_rate: u32,
    target_channels: u8,
    ogg_tx: &broadcast::Sender<Vec<u8>>,
) -> anyhow::Result<()> {
    // Open the media source
    let file = File::open(file_path.as_ref())?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());

    // Create a hint to help the format registry
    let mut hint = Hint::new();
    if let Some(ext) = file_path.as_ref().extension() {
        if let Some(ext_str) = ext.to_str() {
            hint.with_extension(ext_str);
        }
    }

    // Probe the media source
    let probed = symphonia::default::get_probe().format(
        &hint,
        mss,
        &FormatOptions::default(),
        &MetadataOptions::default(),
    )?;

    let mut format = probed.format;

    // Find the first audio track
    let track = format
        .tracks()
        .iter()
        .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)
        .ok_or_else(|| anyhow::anyhow!("No audio track found"))?;

    let track_id = track.id;
    let codec_params = &track.codec_params;

    info!(
        "[Audio] Source: {} Hz, {} channels",
        codec_params.sample_rate.unwrap_or(target_sample_rate),
        codec_params
            .channels
            .map(|c| c.count())
            .unwrap_or(target_channels as usize)
    );

    // Create decoder
    let mut decoder =
        symphonia::default::get_codecs().make(&codec_params, &DecoderOptions::default())?;

    let mut sample_buf: Option<SampleBuffer<f32>> = None;
    let mut spec = None;

    // Create encoder that writes directly to broadcast writer
    let broadcast_writer = BroadcastWriter::new(ogg_tx.clone());
    let mut encoder = VorbisEncoderBuilder::new(
        NonZeroU32::new(target_sample_rate).unwrap(),
        NonZeroU8::new(target_channels).unwrap(),
        broadcast_writer,
    )?
    .bitrate_management_strategy(VorbisBitrateManagementStrategy::QualityVbr {
        target_quality: 0.5,
    })
    .build()?;

    // Decode all packets
    loop {
        // Get the next packet
        let packet = match format.next_packet() {
            Ok(packet) => packet,
            Err(SymphoniaError::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                break; // End of stream
            }
            Err(SymphoniaError::ResetRequired) => {
                break; // Track list changed (chained streams)
            }
            Err(e) => return Err(e.into()),
        };

        // Skip packets from other tracks
        if packet.track_id() != track_id {
            continue;
        }

        // Decode the packet
        let decoded = match decoder.decode(&packet) {
            Ok(buf) => buf,
            Err(SymphoniaError::DecodeError(_)) => continue, // Skip bad packets
            Err(e) => return Err(e.into()),
        };

        // Create sample buffer on first packet
        if sample_buf.is_none() {
            spec = Some(*decoded.spec());
            let duration = decoded.capacity() as u64;
            sample_buf = Some(SampleBuffer::<f32>::new(duration, spec.unwrap()));
        }

        // Convert to f32 interleaved samples
        if let Some(buf) = &mut sample_buf {
            buf.copy_interleaved_ref(decoded);

            // Convert interleaved to planar format for Vorbis encoding
            let interleaved_samples = buf.samples();
            let num_channels = spec.unwrap().channels.count().min(target_channels as usize);
            let frames = interleaved_samples.len() / num_channels;

            let mut planar = vec![Vec::with_capacity(frames); num_channels];
            for (i, sample) in interleaved_samples.iter().enumerate() {
                let channel = i % num_channels;
                planar[channel].push(*sample);
            }

            // Pad with silence if needed
            while planar.len() < target_channels as usize {
                planar.push(vec![0.0; frames]);
            }

            // Encode this block - broadcasts automatically via BroadcastWriter
            encoder.encode_audio_block(&planar)?;
        }
    }

    // Finish encoder - flushes final data
    encoder.finish()?;

    Ok(())
}
