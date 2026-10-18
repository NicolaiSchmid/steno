//! What `store_waits` shares: a WAV decoder, no destinations, and a
//! pipeline's dependencies over a store with an audio folder.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use steno_core::paths::{file_url, file_url_path};
use steno_core::protocols::BoundaryResult;
use steno_core::testing::{FakeDiarizer, InMemorySpeakerMemory};
use steno_core::{
    AudioAsset, AudioBuffer16k, AudioFormat, AudioLane, Delivery, DeliveryDispatcher, SpeechEngine,
    Store, async_trait,
};
use steno_pipeline::{MeetingEventBus, PipelineDependencies};
use uuid::Uuid;

/// Reads the 16 kHz mono WAV fixtures.
struct WavDecoder;

#[async_trait]
impl steno_core::AudioDecoder for WavDecoder {
    async fn decode(&self, asset: &AudioAsset, lane: AudioLane) -> BoundaryResult<AudioBuffer16k> {
        let url = asset.sidecars_16k.get(&lane).unwrap_or(&asset.url);
        let bytes = std::fs::read(file_url_path(url).ok_or("not a file URL")?)?;
        Ok(AudioBuffer16k::new(
            bytes[44..]
                .as_chunks::<2>()
                .0
                .iter()
                .map(|pair| f32::from(i16::from_le_bytes(*pair)) / 32768.0)
                .collect(),
        ))
    }

    fn mixdown_format(&self) -> AudioFormat {
        AudioFormat::Wav16kInt16
    }

    async fn mixdown(&self, asset: &AudioAsset, to: &Path) -> BoundaryResult<()> {
        std::fs::copy(file_url_path(&asset.url).ok_or("not a file URL")?, to)?;
        Ok(())
    }
}

/// Delivers to no destination.
struct NoDestinations;

#[async_trait]
impl DeliveryDispatcher for NoDestinations {
    async fn deliver_all(&self, _meeting_id: Uuid) -> Vec<Delivery> {
        Vec::new()
    }
}

/// The store at `database`, its audio folder `audio` beside it.
pub fn store_with_audio(database: &Path) -> (Arc<Store>, PathBuf) {
    let audio = database.with_file_name("audio");
    let store = Arc::new(Store::open(database).unwrap());
    let mut settings = store.settings().unwrap();
    settings.audio_folder = file_url(&audio, true);
    store.save_settings(&settings).unwrap();
    (store, audio)
}

/// A pipeline's dependencies over `store` with `engine` and the fakes.
pub fn dependencies(store: &Arc<Store>, engine: Arc<dyn SpeechEngine>) -> PipelineDependencies {
    PipelineDependencies::new(
        Arc::new(WavDecoder),
        engine,
        Arc::new(FakeDiarizer::default()),
        Arc::new(InMemorySpeakerMemory::new(Vec::new())),
        Arc::new(NoDestinations),
        store.clone(),
        MeetingEventBus::new(),
    )
}
