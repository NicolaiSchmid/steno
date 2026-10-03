//! Bounded concurrency for the chunked passes.
//! Swift: `Sources/StenoLLM/Support/BoundedConcurrency.swift`.

use std::future::Future;

use futures_util::{StreamExt, TryStreamExt, stream};

/// `transform` over `items` with at most `limit` in flight, results in
/// input order. The first error cancels the rest and is returned.
pub async fn map_bounded<I, T, E, F, Fut>(
    items: impl IntoIterator<Item = I>,
    limit: usize,
    transform: F,
) -> Result<Vec<T>, E>
where
    F: Fn(I) -> Fut,
    Fut: Future<Output = Result<T, E>>,
{
    stream::iter(items)
        .map(transform)
        .buffered(limit.max(1))
        .try_collect()
        .await
}
