//! Decodes one lane of an asset to 16 kHz mono.

use std::path::Path;

use async_trait::async_trait;

use super::BoundaryResult;
use crate::{AudioAsset, AudioBuffer16k, AudioFormat, AudioLane};

/// Decodes one lane of an asset to 16 kHz mono.
#[async_trait]
pub trait AudioDecoder: Send + Sync {
    /// The lane's sidecar when present, else decoded from the master.
    async fn decode(&self, asset: &AudioAsset, lane: AudioLane) -> BoundaryResult<AudioBuffer16k>;

    /// The container `mixdown` writes, so `mixdownURL` gets the right extension.
    fn mixdown_format(&self) -> AudioFormat;

    /// Mono mixdown in `mixdown_format` for the optional audio export.
    async fn mixdown(&self, asset: &AudioAsset, to: &Path) -> BoundaryResult<()>;
}
