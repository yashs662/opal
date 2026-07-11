//! Default-device-following audio sink.
//!
//! librespot's stock rodio backend opens **one** `OutputStream` on
//! whatever the OS default output is at construction and holds it for
//! the player's whole life — switching the Windows default device then
//! has no effect on Opal (a WASAPI stream stays bound to the endpoint it
//! opened). This sink re-resolves the default device instead:
//!
//! - on every `start()` (a fresh playback session picks up the current
//!   default immediately), and
//! - on a **time-gated identity check** in `write()` (every
//!   [`DEVICE_RECHECK`]), so a switch made mid-song follows within half
//!   a second.
//!
//! Cost discipline: the check is a name compare against a cached string;
//! the default-device COM query runs at most twice a second and only on
//! the librespot player thread — the UI thread is never involved. The
//! stream is rebuilt only when the default actually changed (or opening
//! previously failed); rodio's ~0.5 s in-flight buffer on the old stream
//! is discarded, which is the expected blip of an output switch.

use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait};
use librespot_playback::audio_backend::{Sink, SinkError, SinkResult};
use librespot_playback::convert::Converter;
use librespot_playback::decoder::AudioPacket;
use librespot_playback::{NUM_CHANNELS, SAMPLE_RATE};
use log::{info, warn};

/// How often `write()` re-checks the default output device's identity.
const DEVICE_RECHECK: Duration = Duration::from_millis(500);

/// An open output: the rodio sink, the stream keeping it alive, and the
/// name of the device it was opened on (the identity the recheck
/// compares against).
struct Out {
    sink: rodio::Sink,
    _stream: rodio::OutputStream,
    device_name: Option<String>,
}

pub struct SwitchingSink {
    out: Option<Out>,
    last_check: Instant,
}

impl SwitchingSink {
    pub fn new() -> Self {
        Self {
            out: None,
            last_check: Instant::now(),
        }
    }

    /// Name of the current OS default output device (`None` when there
    /// is no default or the name can't be read — treated as "unknown",
    /// which never matches an open stream, forcing a rebuild attempt).
    fn default_device_name() -> Option<String> {
        cpal::default_host()
            .default_output_device()
            .and_then(|d| d.name().ok())
    }

    /// Open a stream + sink on the current default device. Mirrors the
    /// stock backend's config choice: native stereo 44.1 kHz, falling
    /// back to the device's default sample rate, then its full default
    /// config. Output is always F32 (Opal decodes to f64 and Windows
    /// mixes in float — see `spirc_bootstrap`).
    fn open() -> Result<Out, String> {
        let host = cpal::default_host();
        let device = host
            .default_output_device()
            .ok_or_else(|| "no default output device".to_string())?;
        let device_name = device.name().ok();
        let default_config = device
            .default_output_config()
            .map_err(|e| format!("default config: {e}"))?;
        let config = device
            .supported_output_configs()
            .map_err(|e| format!("supported configs: {e}"))?
            .find(|c| c.channels() == NUM_CHANNELS as cpal::ChannelCount)
            .and_then(|c| {
                c.try_with_sample_rate(cpal::SampleRate(SAMPLE_RATE))
                    .or_else(|| c.try_with_sample_rate(default_config.sample_rate()))
            })
            .unwrap_or(default_config);
        let mut stream = rodio::OutputStreamBuilder::default()
            .with_device(device.clone())
            .with_config(&config.config())
            .with_sample_format(cpal::SampleFormat::F32)
            .open_stream()
            .or_else(|e| {
                warn!("audio: exact stream config failed, falling back: {e}");
                rodio::OutputStreamBuilder::from_device(device)
                    .map_err(|e| format!("stream builder: {e}"))?
                    .open_stream_or_fallback()
                    .map_err(|e| format!("open stream: {e}"))
            })?;
        stream.log_on_drop(false);
        let sink = rodio::Sink::connect_new(stream.mixer());
        info!(
            "audio output: {}",
            device_name.as_deref().unwrap_or("[unknown device]")
        );
        Ok(Out {
            sink,
            _stream: stream,
            device_name,
        })
    }

    /// Ensure an open stream on the **current** default device: opens
    /// when absent, rebuilds when the default moved elsewhere.
    fn ensure_current(&mut self) -> Result<(), String> {
        let current = Self::default_device_name();
        let stale = match &self.out {
            Some(out) => out.device_name != current || current.is_none(),
            None => true,
        };
        if stale {
            // Drop first so WASAPI releases the old endpoint before the
            // new stream opens.
            self.out = None;
            self.out = Some(Self::open()?);
        }
        Ok(())
    }
}

impl Sink for SwitchingSink {
    fn start(&mut self) -> SinkResult<()> {
        self.ensure_current()
            .map_err(SinkError::ConnectionRefused)?;
        if let Some(out) = &self.out {
            out.sink.play();
        }
        Ok(())
    }

    fn stop(&mut self) -> SinkResult<()> {
        if let Some(out) = &self.out {
            out.sink.sleep_until_end();
            out.sink.pause();
        }
        Ok(())
    }

    fn write(&mut self, packet: AudioPacket, converter: &mut Converter) -> SinkResult<()> {
        // Follow a default-device switch made mid-song. Time-gated so the
        // COM query runs at most ~2×/s, on this (player) thread only.
        let now = Instant::now();
        if now.duration_since(self.last_check) >= DEVICE_RECHECK {
            self.last_check = now;
            let current = Self::default_device_name();
            let moved = self
                .out
                .as_ref()
                .map(|o| o.device_name != current)
                .unwrap_or(true);
            if moved {
                info!(
                    "audio: default output changed → {}",
                    current.as_deref().unwrap_or("[none]")
                );
                self.out = None;
                match Self::open() {
                    Ok(out) => {
                        out.sink.play();
                        self.out = Some(out);
                    }
                    // Transient (device unplugged mid-switch): stay
                    // silent and retry on the next gated check rather
                    // than killing the playback session.
                    Err(e) => warn!("audio: reopen failed ({e}) — retrying"),
                }
            }
        }
        let Some(out) = &self.out else {
            // No output right now — drop the packet (silence) instead of
            // erroring the player into teardown.
            return Ok(());
        };
        let samples = packet
            .samples()
            .map_err(|e| SinkError::OnWrite(e.to_string()))?;
        let samples_f32: &[f32] = &converter.f64_to_f32(samples);
        let source = rodio::buffer::SamplesBuffer::new(
            NUM_CHANNELS as cpal::ChannelCount,
            SAMPLE_RATE,
            samples_f32,
        );
        out.sink.append(source);
        // Backpressure: cap the queued chunks (~0.5 s) so pause/seek stay
        // responsive — same pacing as the stock backend.
        while out.sink.len() > 26 {
            std::thread::sleep(Duration::from_millis(10));
        }
        Ok(())
    }
}
