use log::{error, info};
use std::fs::File;
use std::path::Path;
use tokio::sync::broadcast;

use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::{DecoderOptions, CODEC_TYPE_NULL};
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

type AudioBlock = Vec<Vec<f32>>; // [channels][samples]

/// Runs the audio pipeline: Decode any format → Broadcast PCM blocks
pub fn audio_decode_loop(
    file_path: impl AsRef<Path>,
    pcm_tx: broadcast::Sender<AudioBlock>,
) -> anyhow::Result<()> {
    loop {
        info!("[Audio] Decoding: {}", file_path.as_ref().display());

        match decode_file(&file_path, &pcm_tx) {
            Ok(_) => {
                info!("[Audio] File complete, looping...");
            }
            Err(e) => {
                error!("[Audio] Error: {}", e);
                std::thread::sleep(std::time::Duration::from_secs(1));
            }
        }
    }
}

fn decode_file(
    file_path: impl AsRef<Path>,
    pcm_tx: &broadcast::Sender<AudioBlock>,
) -> anyhow::Result<()> {
    // Open the media source
    let file = File::open(file_path.as_ref())?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());

    // Create a hint for format detection
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
        "[Audio] Format: {} Hz, {} channels",
        codec_params.sample_rate.unwrap_or(44100),
        codec_params.channels.map(|c| c.count()).unwrap_or(2)
    );

    // Create decoder
    let mut decoder =
        symphonia::default::get_codecs().make(&codec_params, &DecoderOptions::default())?;

    let mut sample_buf: Option<SampleBuffer<f32>> = None;
    let mut audio_spec = None;

    // Decode all packets and broadcast PCM
    loop {
        let packet = match format.next_packet() {
            Ok(packet) => packet,
            Err(SymphoniaError::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                break;
            }
            Err(SymphoniaError::ResetRequired) => break,
            Err(e) => return Err(e.into()),
        };

        if packet.track_id() != track_id {
            continue;
        }

        let decoded = match decoder.decode(&packet) {
            Ok(buf) => buf,
            Err(SymphoniaError::DecodeError(_)) => continue,
            Err(e) => return Err(e.into()),
        };

        // Create sample buffer on first packet
        if sample_buf.is_none() {
            let spec = *decoded.spec();
            audio_spec = Some(spec);
            let duration = decoded.capacity() as u64;
            sample_buf = Some(SampleBuffer::<f32>::new(duration, spec));
        }

        // Convert to f32 and extract as planar
        if let Some(buf) = &mut sample_buf {
            buf.copy_interleaved_ref(decoded);

            // Convert interleaved to planar
            let interleaved = buf.samples();
            let num_channels = audio_spec.unwrap().channels.count();
            let frames = interleaved.len() / num_channels;

            let mut planar = vec![Vec::with_capacity(frames); num_channels];
            for (i, &sample) in interleaved.iter().enumerate() {
                planar[i % num_channels].push(sample);
            }

            // Broadcast PCM to all listeners
            let _ = pcm_tx.send(planar);
        }
    }

    Ok(())
}
