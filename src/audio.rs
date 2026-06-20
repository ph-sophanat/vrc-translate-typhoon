use anyhow::{anyhow, Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::SampleFormat;
use rubato::{
    Resampler, SincFixedIn, SincInterpolationParameters, SincInterpolationType, WindowFunction,
};
use std::sync::mpsc::{channel, Receiver};

const TARGET_RATE: u32 = 16_000; // Whisper native rate

/// Start continuous capture from the default input device.
/// Returns the live stream (keep it alive!), a receiver of mono f32 sample
/// batches at the device sample rate, and that device rate.
pub fn start_capture() -> Result<(cpal::Stream, Receiver<Vec<f32>>, u32)> {
    let host = cpal::default_host();
    let device = host
        .default_input_device()
        .ok_or_else(|| anyhow!("no default input device found"))?;
    let config = device
        .default_input_config()
        .context("getting default input config")?;

    let device_rate = config.sample_rate();
    let channels = config.channels() as usize;
    eprintln!(
        "  [mic] {} | {} Hz | {} ch | {:?}",
        device
            .id()
            .map(|i| i.to_string())
            .unwrap_or_else(|_| "default".into()),
        device_rate,
        channels,
        config.sample_format()
    );

    let (tx, rx) = channel::<Vec<f32>>();
    let err_fn = |err| eprintln!("  [mic] stream error: {err}");

    let stream = match config.sample_format() {
        SampleFormat::F32 => device.build_input_stream(
            config.into(),
            move |data: &[f32], _: &cpal::InputCallbackInfo| {
                let mut mono = Vec::with_capacity(data.len() / channels.max(1));
                for frame in data.chunks(channels) {
                    let sum: f32 = frame.iter().copied().sum();
                    mono.push(sum / channels as f32);
                }
                let _ = tx.send(mono);
            },
            err_fn,
            None,
        )?,
        SampleFormat::I16 => device.build_input_stream(
            config.into(),
            move |data: &[i16], _: &cpal::InputCallbackInfo| {
                let mut mono = Vec::with_capacity(data.len() / channels.max(1));
                for frame in data.chunks(channels) {
                    let sum: f32 = frame.iter().map(|&s| s as f32 / 32768.0).sum();
                    mono.push(sum / channels as f32);
                }
                let _ = tx.send(mono);
            },
            err_fn,
            None,
        )?,
        other => return Err(anyhow!("unsupported input sample format: {other:?}")),
    };

    stream.play().context("starting input stream")?;
    Ok((stream, rx, device_rate))
}

/// Streaming resampler: push device-rate mono samples, get 16 kHz samples out.
pub struct StreamResampler {
    resampler: Option<SincFixedIn<f32>>,
    chunk: usize,
    in_buf: Vec<f32>,
}

impl StreamResampler {
    pub fn new(in_rate: u32) -> Result<StreamResampler> {
        if in_rate == TARGET_RATE {
            return Ok(StreamResampler {
                resampler: None,
                chunk: 0,
                in_buf: Vec::new(),
            });
        }
        let params = SincInterpolationParameters {
            sinc_len: 256,
            f_cutoff: 0.95,
            interpolation: SincInterpolationType::Linear,
            oversampling_factor: 256,
            window: WindowFunction::BlackmanHarris2,
        };
        let chunk = 1024usize;
        let resampler = SincFixedIn::<f32>::new(
            TARGET_RATE as f64 / in_rate as f64,
            2.0,
            params,
            chunk,
            1,
        )
        .context("constructing resampler")?;
        Ok(StreamResampler {
            resampler: Some(resampler),
            chunk,
            in_buf: Vec::new(),
        })
    }

    /// Resample a batch; leftover (< chunk) is retained for the next call.
    pub fn push(&mut self, samples: &[f32]) -> Vec<f32> {
        let Some(res) = self.resampler.as_mut() else {
            return samples.to_vec();
        };
        self.in_buf.extend_from_slice(samples);
        let mut out = Vec::new();
        while self.in_buf.len() >= self.chunk {
            let block: Vec<f32> = self.in_buf.drain(..self.chunk).collect();
            match res.process(&vec![block], None) {
                Ok(r) => out.extend_from_slice(&r[0]),
                Err(e) => {
                    eprintln!("  [resample] error: {e}");
                    break;
                }
            }
        }
        out
    }
}
