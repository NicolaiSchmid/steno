//! `SpeakerMemory` over the persons in the store: cosine ranking against
//! every stored voice. The threshold and margin live in the trait's
//! provided `match_voice`; this type only ranks. Voices themselves are
//! written by the store when speakers are confirmed or merged.
//! Swift: `Sources/StenoSpeech/Speakers/CosineSpeakerMemory.swift`.

use std::sync::Arc;

use steno_core::{
    Embedding, SpeakerMatch, SpeakerMemory, Store, async_trait, protocols::BoundaryResult,
};

#[derive(Debug, Clone)]
pub struct StoreSpeakerMemory {
    store: Arc<Store>,
}

impl StoreSpeakerMemory {
    #[must_use]
    pub fn new(store: Arc<Store>) -> Self {
        StoreSpeakerMemory { store }
    }
}

#[async_trait]
impl SpeakerMemory for StoreSpeakerMemory {
    /// Best first; ties broken by person id so the order is stable.
    async fn candidates(
        &self,
        embedding: &Embedding,
        limit: usize,
    ) -> BoundaryResult<Vec<SpeakerMatch>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let probe = embedding.normalized();
        let mut ranked: Vec<SpeakerMatch> = self
            .store
            .persons()?
            .into_iter()
            .filter_map(|person| {
                let known = person.embedding.as_ref()?;
                if known.0.len() != probe.0.len() {
                    return None;
                }
                let similarity = known.cosine_similarity(&probe);
                Some(SpeakerMatch { person, similarity })
            })
            .collect();
        ranked.sort_by(|a, b| {
            b.similarity
                .partial_cmp(&a.similarity)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.person.id.cmp(&b.person.id))
        });
        ranked.truncate(limit);
        Ok(ranked)
    }
}
