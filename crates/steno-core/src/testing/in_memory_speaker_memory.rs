//! Known voices over an in-memory list of people.
//! Swift: `Sources/StenoCore/Testing/InMemorySpeakerMemory.swift`.

use std::collections::BTreeMap;
use std::sync::Mutex;

use async_trait::async_trait;
use uuid::Uuid;

use super::lock;
use crate::{BoundaryResult, Embedding, Person, SpeakerMatch, SpeakerMemory};

/// A `SpeakerMemory` over an in-memory list of people: cosine ranking over
/// the voices it was given, independent of any store.
#[derive(Debug, Default)]
pub struct InMemorySpeakerMemory {
    people: Mutex<BTreeMap<Uuid, Person>>,
}

impl InMemorySpeakerMemory {
    #[must_use]
    pub fn new(people: impl IntoIterator<Item = Person>) -> Self {
        InMemorySpeakerMemory {
            people: Mutex::new(
                people
                    .into_iter()
                    .map(|person| (person.id, person))
                    .collect(),
            ),
        }
    }

    /// Adds or replaces one person.
    pub fn insert(&self, person: Person) {
        lock(&self.people).insert(person.id, person);
    }

    /// Every person, by id.
    #[must_use]
    pub fn people(&self) -> Vec<Person> {
        lock(&self.people).values().cloned().collect()
    }
}

#[async_trait]
impl SpeakerMemory for InMemorySpeakerMemory {
    async fn candidates(
        &self,
        embedding: &Embedding,
        limit: usize,
    ) -> BoundaryResult<Vec<SpeakerMatch>> {
        let mut ranked: Vec<SpeakerMatch> = lock(&self.people)
            .values()
            .filter_map(|person| {
                let known = person.embedding.as_ref()?;
                Some(SpeakerMatch {
                    person: person.clone(),
                    similarity: known.cosine_similarity(embedding),
                })
            })
            .collect();
        // Best first; ties broken by id. Swift compares the uppercase id
        // text, which orders like the bytes.
        ranked.sort_by(|left, right| {
            right
                .similarity
                .total_cmp(&left.similarity)
                .then_with(|| left.person.id.cmp(&right.person.id))
        });
        ranked.truncate(limit);
        Ok(ranked)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocols::DEFAULT_MATCH_MARGIN;
    use crate::testing::sample_data;

    #[tokio::test]
    async fn candidates_rank_by_cosine_and_match_needs_threshold_and_margin() {
        let memory = InMemorySpeakerMemory::new([
            sample_data::person(0, "Anna"),
            sample_data::person(1, "Ben"),
            Person {
                embedding: None,
                ..sample_data::person(2, "Voiceless")
            },
        ]);
        let mut voice = sample_data::embedding(0);
        voice.0[1] = 0.5;
        let ranked = memory.candidates(&voice, 5).await.unwrap();
        assert_eq!(ranked.len(), 2);
        assert_eq!(ranked[0].person.display_name, "Anna");
        assert!(ranked[0].similarity > ranked[1].similarity);

        let matched = memory
            .match_voice(&voice, 0.8, DEFAULT_MATCH_MARGIN)
            .await
            .unwrap();
        assert_eq!(matched.unwrap().person.display_name, "Anna");
        assert!(
            memory
                .match_voice(&voice, 0.95, DEFAULT_MATCH_MARGIN)
                .await
                .unwrap()
                .is_none()
        );
        // Equidistant from both: the margin rejects it.
        let mut between = sample_data::embedding(0);
        between.0[1] = 1.0;
        assert!(
            memory
                .match_voice(&between, 0.5, DEFAULT_MATCH_MARGIN)
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            InMemorySpeakerMemory::default()
                .match_voice(&voice, 0.0, 0.0)
                .await
                .unwrap()
                .is_none()
        );
    }
}
