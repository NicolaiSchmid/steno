//! Known voices over an in-memory list of people.
//! Swift: `Sources/StenoCore/Testing/InMemorySpeakerMemory.swift`.

use std::collections::BTreeMap;
use std::sync::Mutex;

use async_trait::async_trait;
use uuid::Uuid;

use super::lock;
use crate::{BoundaryResult, Embedding, Person, SpeakerMatch, SpeakerMemory};

/// A `SpeakerMemory` over an in-memory list of people: cosine ranking over
/// the voices it was given, independent of any store. Ties rank by person
/// id, the order Swift gets from the uppercase id text. A similarity that
/// is not finite (an embedding holding `inf` or NaN) is dropped before
/// ranking, so the default `match_voice` only ever sees numbers.
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
                let similarity = known.cosine_similarity(embedding);
                similarity.is_finite().then(|| SpeakerMatch {
                    person: person.clone(),
                    similarity,
                })
            })
            .collect();
        // Best first; ties broken by id, which orders like Swift's
        // uppercase id text. `total_cmp` puts +0.0 before -0.0 whatever
        // the ids, where Swift breaks that tie by id too; a zero similarity
        // never passes a positive threshold, so no match differs.
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

    #[tokio::test]
    async fn equal_similarities_rank_by_id_and_the_list_stops_at_the_limit() {
        // Three people sharing one voice, inserted out of id order.
        let memory = InMemorySpeakerMemory::new([
            Person {
                id: Uuid::from_u128(3),
                ..sample_data::person(0, "Third")
            },
            Person {
                id: Uuid::from_u128(1),
                ..sample_data::person(0, "First")
            },
            Person {
                id: Uuid::from_u128(2),
                ..sample_data::person(0, "Second")
            },
        ]);
        let voice = sample_data::embedding(0);
        let names = |ranked: Vec<SpeakerMatch>| -> Vec<String> {
            ranked
                .into_iter()
                .map(|found| found.person.display_name)
                .collect()
        };
        let all = memory.candidates(&voice, 10).await.unwrap();
        // One shared voice: every similarity is the same bit pattern.
        assert!(
            all.iter()
                .all(|found| found.similarity.to_bits() == all[0].similarity.to_bits())
        );
        assert_eq!(names(all), ["First", "Second", "Third"]);
        assert_eq!(
            names(memory.candidates(&voice, 2).await.unwrap()),
            ["First", "Second"]
        );
        assert_eq!(names(memory.candidates(&voice, 0).await.unwrap()), [""; 0]);
    }

    #[tokio::test]
    async fn a_voice_with_an_infinite_component_matches_nobody() {
        let memory = InMemorySpeakerMemory::new([
            sample_data::person(0, "Anna"),
            sample_data::person(1, "Ben"),
        ]);
        // `inf / inf` and `0 * inf` are NaN: every similarity is non-finite.
        let mut voice = sample_data::embedding(0);
        voice.0[0] = f32::INFINITY;
        assert_eq!(memory.candidates(&voice, 5).await.unwrap(), []);
        assert_eq!(memory.match_voice(&voice, 0.0, 0.0).await.unwrap(), None);
    }
}
