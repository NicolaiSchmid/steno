//! Decodes one lane of an asset to 16 kHz mono.
//! Swift: `AudioDecoder` in `Sources/StenoCore/Protocols/PipelineBoundaries.swift`.

use std::path::Path;

use async_trait::async_trait;

use super::BoundaryResult;
use crate::{AudioAsset, AudioBuffer16k, AudioFormat, AudioLane};

/// Decodes one lane of an asset to 16 kHz mono; the implementations live in
/// the audio crate.
#[async_trait]
pub trait AudioDecoder: Send + Sync {
    /// The lane's sidecar when present, else decoded from the master.
    async fn decode(&self, asset: &AudioAsset, lane: AudioLane) -> BoundaryResult<AudioBuffer16k>;

    /// The container `mixdown` writes. The persist stage names the file
    /// from it so `mixdownURL` never carries the wrong extension.
    fn mixdown_format(&self) -> AudioFormat;

    /// Mono mixdown in `mixdown_format` for the optional audio export.
    async fn mixdown(&self, asset: &AudioAsset, to: &Path) -> BoundaryResult<()>;
}
