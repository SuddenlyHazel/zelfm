use log::{error, info};
use std::path::PathBuf;
use tokio::sync::broadcast;

type AudioBlock = Vec<Vec<f32>>; // [channels][samples]

/// Trait for audio sources that can broadcast PCM audio blocks
pub trait AudioSource: Send + 'static {
    fn start(self, pcm_tx: broadcast::Sender<AudioBlock>) -> anyhow::Result<()>;
}

// ============================================================================
// File Source (existing functionality)
// ============================================================================

pub struct FileSource {
    pub path: PathBuf,
}

impl FileSource {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }
}

impl AudioSource for FileSource {
    fn start(self, pcm_tx: broadcast::Sender<AudioBlock>) -> anyhow::Result<()> {
        info!(
            "[FileSource] Starting file decoder for: {}",
            self.path.display()
        );
        file_decode_loop(&self.path, pcm_tx)
    }
}

fn file_decode_loop(
    file_path: &PathBuf,
    pcm_tx: broadcast::Sender<AudioBlock>,
) -> anyhow::Result<()> {
    use std::fs::File;
    use symphonia::core::audio::SampleBuffer;
    use symphonia::core::codecs::{DecoderOptions, CODEC_TYPE_NULL};
    use symphonia::core::errors::Error as SymphoniaError;
    use symphonia::core::formats::FormatOptions;
    use symphonia::core::io::MediaSourceStream;
    use symphonia::core::meta::MetadataOptions;
    use symphonia::core::probe::Hint;

    info!("[File] Starting decode loop for: {}", file_path.display());

    loop {
        info!("[File] Decoding iteration starting...");

        match decode_file_once(file_path, &pcm_tx) {
            Ok(true) => {
                info!("[File] Decode complete, looping...");
            }
            Ok(false) => {
                info!("[File] Channel closed, shutting down...");
                break;
            }
            Err(e) => {
                error!("[File] Decode error: {}", e);
                std::thread::sleep(std::time::Duration::from_secs(1));
            }
        }
    }

    info!("[File] Decode loop exited");

    Ok(())
}

fn decode_file_once(
    file_path: &PathBuf,
    pcm_tx: &broadcast::Sender<AudioBlock>,
) -> anyhow::Result<bool> {
    use std::fs::File;
    use symphonia::core::audio::SampleBuffer;
    use symphonia::core::codecs::{DecoderOptions, CODEC_TYPE_NULL};
    use symphonia::core::errors::Error as SymphoniaError;
    use symphonia::core::formats::FormatOptions;
    use symphonia::core::io::MediaSourceStream;
    use symphonia::core::meta::MetadataOptions;
    use symphonia::core::probe::Hint;

    let file = File::open(file_path)?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());

    let mut hint = Hint::new();
    if let Some(ext) = file_path.extension() {
        if let Some(ext_str) = ext.to_str() {
            hint.with_extension(ext_str);
        }
    }

    let probed = symphonia::default::get_probe().format(
        &hint,
        mss,
        &FormatOptions::default(),
        &MetadataOptions::default(),
    )?;

    let mut format = probed.format;

    let track = format
        .tracks()
        .iter()
        .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)
        .ok_or_else(|| anyhow::anyhow!("No audio track"))?;

    let track_id = track.id;
    let codec_params = &track.codec_params;

    let detected_rate = codec_params.sample_rate.unwrap_or(44100);
    let detected_channels = codec_params.channels.map(|c| c.count()).unwrap_or(2);

    info!(
        "[File] Detected format: {} Hz, {} ch",
        detected_rate, detected_channels
    );

    let mut decoder =
        symphonia::default::get_codecs().make(&codec_params, &DecoderOptions::default())?;

    let mut sample_buf: Option<SampleBuffer<f32>> = None;
    let mut audio_spec = None;

    loop {
        let packet = match format.next_packet() {
            Ok(packet) => packet,
            Err(SymphoniaError::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                break
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

        if sample_buf.is_none() {
            audio_spec = Some(*decoded.spec());
            let duration = decoded.capacity() as u64;
            sample_buf = Some(SampleBuffer::<f32>::new(duration, audio_spec.unwrap()));
        }

        if let Some(buf) = &mut sample_buf {
            buf.copy_interleaved_ref(decoded);

            let interleaved = buf.samples();
            let num_channels = audio_spec.unwrap().channels.count();
            let frames = interleaved.len() / num_channels;

            let mut planar = vec![Vec::with_capacity(frames); num_channels];
            for (i, &sample) in interleaved.iter().enumerate() {
                planar[i % num_channels].push(sample);
            }

            // Check if send fails (channel closed = shutdown)
            if pcm_tx.send(planar).is_err() {
                info!("[File] Channel closed during decode");
                return Ok(false);
            }
        }
    }

    Ok(true)
}

// ============================================================================
// Live Source (CPAL input capture)
// ============================================================================

#[cfg(feature = "live-input")]
pub struct LiveSource {
    pub device_name: Option<String>,
}

#[cfg(feature = "live-input")]
impl LiveSource {
    pub fn new(device_name: Option<String>) -> Self {
        Self { device_name }
    }
}

#[cfg(feature = "live-input")]
impl AudioSource for LiveSource {
    fn start(self, pcm_tx: broadcast::Sender<AudioBlock>) -> anyhow::Result<()> {
        use crate::devices::find_device_by_name;
        use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

        let host = cpal::default_host();

        // Select device
        let device = if let Some(name) = &self.device_name {
            find_device_by_name(&host, name)?
        } else {
            host.default_input_device()
                .ok_or_else(|| anyhow::anyhow!("No default input device"))?
        };

        let device_name = device.name()?;
        let config = device.default_input_config()?;
        let sample_rate = config.sample_rate().0;
        let channels = config.channels() as usize;

        println!("[Live] Device: {}", device_name);
        println!("[Live] Format: {} Hz, {} ch", sample_rate, channels);

        // Build input stream
        let stream = device.build_input_stream(
            &config.into(),
            move |data: &[f32], _: &cpal::InputCallbackInfo| {
                // Convert interleaved to planar
                let frames = data.len() / channels;
                let mut planar = vec![Vec::with_capacity(frames); channels];
                for (i, &sample) in data.iter().enumerate() {
                    planar[i % channels].push(sample);
                }

                // Upmix mono to stereo if needed (broadcaster expects 2 channels)
                if channels == 1 && planar.len() == 1 {
                    let mono_channel = planar[0].clone();
                    planar.push(mono_channel); // Duplicate for stereo
                }

                // Broadcast to all listeners
                let _ = pcm_tx.send(planar);
            },
            |err| error!("[Live] Stream error: {}", err),
            None,
        )?;

        stream.play()?;

        println!("[Live] Streaming... (Press Ctrl+C to stop)");

        // Keep stream alive by moving it into the loop
        // Process exit will clean it up
        loop {
            std::thread::sleep(std::time::Duration::from_millis(100));

            // Stream is kept alive by this loop
            // When main thread exits (Ctrl+C), this thread is terminated
        }

        #[allow(unreachable_code)]
        {
            drop(stream);
            Ok(())
        }
    }
}
