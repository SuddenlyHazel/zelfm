#[cfg(feature = "playback")]
use rodio::{Decoder, OutputStream, Sink};
#[cfg(feature = "playback")]
use std::io::Cursor;

#[cfg(feature = "playback")]
pub struct AudioPlayer {
    stream: OutputStream,
    sink: Sink,
    sample_rate: u32,
    channels: u8,
}

#[cfg(feature = "playback")]
impl AudioPlayer {
    pub fn new(sample_rate: u32, channels: u8) -> anyhow::Result<Self> {
        use rodio::OutputStreamBuilder;

        let stream = OutputStreamBuilder::open_default_stream()?;

        let mixer = stream.mixer();
        let sink = Sink::connect_new(mixer);

        Ok(Self {
            stream,
            sink,
            sample_rate,
            channels,
        })
    }

    pub fn play_samples(&mut self, samples: &[&[f32]]) -> anyhow::Result<()> {
        // Convert planar to interleaved
        let num_channels = samples.len();
        let num_samples = samples[0].len();

        let mut interleaved = Vec::with_capacity(num_channels * num_samples);
        for i in 0..num_samples {
            for channel in samples {
                interleaved.push(channel[i]);
            }
        }

        let source =
            rodio::buffer::SamplesBuffer::new(self.channels as u16, self.sample_rate, interleaved);

        self.sink.append(source);
        Ok(())
    }

    pub fn finish(self) {
        self.sink.sleep_until_end();
    }
}

// Stub when playback disabled
#[cfg(not(feature = "playback"))]
pub struct AudioPlayer;

#[cfg(not(feature = "playback"))]
impl AudioPlayer {
    pub fn new(_sample_rate: u32, _channels: u8) -> anyhow::Result<Self> {
        Ok(Self)
    }

    pub fn play_samples(&mut self, _samples: &[&[f32]]) -> anyhow::Result<()> {
        Ok(())
    }

    pub fn finish(self) {}
}
