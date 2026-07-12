use std::sync::Arc;

use librespot_connect::{ConnectConfig, Spirc};
use librespot_core::Session;
use librespot_core::authentication::Credentials;
use librespot_core::config::DeviceType;
use librespot_core::dealer::Subscription;
use librespot_playback::config::{
    Bitrate, NormalisationMethod, NormalisationType, PlayerConfig, VolumeCtrl,
};
use librespot_playback::mixer::softmixer::SoftMixer;
use librespot_playback::mixer::{Mixer, MixerConfig};
use librespot_playback::player::Player;

use crate::errors::AuthError;

/// Output of the Spirc bootstrap. The caller is expected to spawn
/// `spirc_task` on a tokio runtime — without that the device registration
/// and dealer subscriptions go nowhere. `cluster_sub` streams
/// `hm://connect-state/v1/cluster` updates in parallel to Spirc's own
/// internal handling.
pub struct SpircBootstrap {
    pub spirc: Spirc,
    pub spirc_task: std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>,
    pub cluster_sub: Subscription,
    /// Local player event stream — the UI's source of truth while
    /// Opal itself is the active device (the dealer doesn't echo our
    /// own connect-state back; see `local_player`).
    pub player_events: librespot_playback::player::PlayerEventChannel,
}

/// `initial_volume` is the device volume Opal advertises before any
/// volume command lands (0..=1, from the persisted preference) — without
/// it, every transfer to Opal snaps the user back to librespot's
/// 50% default. `quality` maps the persisted streaming-quality pref to
/// the librespot bitrate tier (applies from the next session, i.e. app
/// start — the player is built once here).
pub async fn start(
    session: Session,
    credentials: Credentials,
    initial_volume: f32,
    quality: crate::prefs::AudioQuality,
    normalize: bool,
    eq: Arc<crate::audio_eq::EqShared>,
) -> Result<SpircBootstrap, AuthError> {
    // External cluster subscription must land BEFORE Spirc's own
    // dealer subs to be sure we register first in the listener map.
    // (Dealer allows multi-subscribe; this is belt-and-braces.)
    let cluster_sub = session
        .dealer()
        .add_listen_for("hm://connect-state/v1/cluster")
        .map_err(|e| AuthError::Server(format!("dealer cluster subscribe: {e}")))?;

    let connect_config = ConnectConfig {
        name: "Opal".to_string(),
        device_type: DeviceType::Computer,
        initial_volume: (initial_volume.clamp(0.0, 1.0) * u16::MAX as f32) as u16,
        ..Default::default()
    };

    // Cubic volume mapping (the alsamixer/official-client feel): the
    // slider tracks perceived loudness roughly linearly. The default
    // Log(60 dB) puts -30 dB at half-slider — almost everything audible
    // happens in the top third, which reads as "the volume dies by 70%".
    let mixer_config = MixerConfig {
        volume_ctrl: VolumeCtrl::Cubic(VolumeCtrl::DEFAULT_DB_RANGE),
        ..MixerConfig::default()
    };
    let mixer: Arc<dyn Mixer> = Arc::new(
        SoftMixer::open(mixer_config)
            .map_err(|e| AuthError::Server(format!("softmixer open: {e}")))?,
    );
    let volume_getter = mixer.get_soft_volume();

    // Real audio output (rodio → cpal → the OS default device): Opal
    // is an audible player, not just a Connect remote. Bitrate320 (the
    // "High" pref) is the highest tier librespot can stream (premium
    // "Very High", 320 kbps OGG Vorbis). Spotify's lossless FLAC tier is
    // never provisioned to a librespot session — the account entitlement
    // returned to this client is `high-bitrate` only (verified by probe),
    // so 320 is the ceiling for any third-party Connect client today.
    let player_config = PlayerConfig {
        bitrate: match quality {
            crate::prefs::AudioQuality::Low => Bitrate::Bitrate96,
            crate::prefs::AudioQuality::Normal => Bitrate::Bitrate160,
            crate::prefs::AudioQuality::High => Bitrate::Bitrate320,
        },
        // Keep album playback seamless; tracks decode ahead of the seam.
        gapless: true,
        // Volume normalisation + the DYNAMIC peak limiter — mirrors
        // Spotify's "Normalize volume". The gain is a lossless f64
        // multiply; the limiter only engages on peaks that would
        // otherwise hard-clip, replacing harsh clipping distortion with a
        // momentary inaudible dip. `Album` keeps an album's intended
        // track-to-track dynamics rather than flattening every song to one
        // loudness. Threshold/attack/release/knee keep librespot's
        // sensible defaults (-2 dBFS, 5 ms, 100 ms, 5 dB).
        normalisation: normalize,
        normalisation_method: NormalisationMethod::Dynamic,
        normalisation_type: NormalisationType::Album,
        ..PlayerConfig::default()
    };

    // Output F32, not the S16 default: librespot decodes/processes in f64,
    // and Windows' audio mixer runs in float, so F32 carries essentially
    // the full decoded precision to the OS instead of quantising to 16-bit
    // at our sink. No downside, strictly more faithful to the source.
    // Our own sink (not the stock rodio backend) so the output FOLLOWS
    // the OS default device — the stock backend binds one WASAPI stream
    // to whatever was default at startup, forever (see audio_sink.rs).
    let player = Player::new(player_config, session.clone(), volume_getter, move || {
        Box::new(crate::audio_sink::SwitchingSink::new(eq.clone()))
    });
    // Grab the event stream before Spirc consumes the player.
    let player_events = player.get_player_event_channel();

    let (spirc, task) = Spirc::new(connect_config, session, credentials, player, mixer)
        .await
        .map_err(|e| AuthError::Server(format!("spirc init: {e}")))?;

    Ok(SpircBootstrap {
        spirc,
        spirc_task: Box::pin(task),
        cluster_sub,
        player_events,
    })
}
