//! Known voices over an in-memory list of people.

use std::collections::BTreeMap;
use std::sync::Mutex;

use async_trait::async_trait;
use uuid::Uuid;

use super::lock;
use crate::{BoundaryResult, Embedding, Person, SpeakerMatch, SpeakerMemory};

/// A `SpeakerMemory` over an in-memory list of people: cosine ranking, ties by
/// person id. A non-finite similarity is dropped before ranking.
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
        // Best first, ties by id. `total_cmp` puts +0.0 before -0.0, but a zero
        // similarity never passes a positive threshold.
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
