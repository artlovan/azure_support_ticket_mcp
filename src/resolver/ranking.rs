//! Ranking primitives.

use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct RankedCandidate<T> {
    pub item: T,
    pub confidence: f32,
    pub reason: String,
}

pub fn rank_by<T, F>(items: Vec<T>, score: F) -> Vec<RankedCandidate<T>>
where
    F: Fn(&T) -> (f32, String),
{
    let mut out: Vec<RankedCandidate<T>> = items
        .into_iter()
        .map(|t| {
            let (c, r) = score(&t);
            RankedCandidate {
                item: t,
                confidence: c,
                reason: r,
            }
        })
        .filter(|c| c.confidence > 0.0)
        .collect();
    out.sort_by(|a, b| {
        b.confidence
            .partial_cmp(&a.confidence)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    out
}
