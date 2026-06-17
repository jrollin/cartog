//! Bounded concurrent fan-out for the blocking HTTP embedding providers.
//!
//! The `EmbeddingProvider` trait is sync, so concurrency is a `std::thread::scope`
//! work-queue over indexed sub-batches feeding a slot-indexed collector that
//! reconstructs input order. `max_concurrent <= 1` (or a single sub-batch) takes
//! a serial path with no threads spawned.

use anyhow::Result;
use std::sync::Mutex;

/// One sub-batch's outcome slot: `None` until a worker writes it.
type Slot = Mutex<Option<Result<Vec<Vec<f32>>>>>;

/// Run `embed_one` over `sub_batches` with at most `max_concurrent` in flight,
/// returning the concatenated embeddings in input order. `embed_one` receives
/// each sub-batch's ordinal (its index in `sub_batches`) so retry backoff can
/// de-correlate per sub-batch.
///
/// On failure the lowest-ordinal error wins (deterministic, never wall-clock),
/// matching the serial "first chunk aborts" behavior.
pub(crate) fn run_concurrent<F>(
    sub_batches: Vec<&[&str]>,
    max_concurrent: usize,
    embed_one: F,
) -> Result<Vec<Vec<f32>>>
where
    F: Fn(usize, &[&str]) -> Result<Vec<Vec<f32>>> + Sync,
{
    if max_concurrent <= 1 || sub_batches.len() <= 1 {
        let mut out = Vec::new();
        for (idx, batch) in sub_batches.into_iter().enumerate() {
            out.extend(embed_one(idx, batch)?);
        }
        return Ok(out);
    }

    let n = sub_batches.len();
    let queue = Mutex::new(sub_batches.into_iter().enumerate());
    let results: Vec<Slot> = (0..n).map(|_| Mutex::new(None)).collect();

    std::thread::scope(|s| {
        for _ in 0..max_concurrent.min(n) {
            s.spawn(|| loop {
                // Hold the queue lock only to pop; release before the network call.
                let next = queue.lock().expect("queue mutex poisoned").next();
                let Some((idx, batch)) = next else { break };
                let r = embed_one(idx, batch);
                *results[idx].lock().expect("result mutex poisoned") = Some(r);
            });
        }
    });

    let mut out = Vec::new();
    for slot in results {
        match slot.into_inner().expect("result mutex poisoned") {
            Some(Ok(v)) => out.extend(v),
            Some(Err(e)) => return Err(e),
            None => unreachable!("every slot is written before the scope joins"),
        }
    }
    Ok(out)
}

/// Retry a request on transient HTTP failures (429/503, timeout, connect) with
/// capped exponential backoff plus deterministic per-(ordinal, attempt) jitter
/// to avoid a thundering herd. Up to 3 attempts.
///
/// `Retry-After` is not honored: `request()` consumes the response via
/// `error_for_status()`, so the header is unreachable from the resulting error.
pub(crate) fn with_retry<T, F>(ordinal: usize, op: F) -> Result<T>
where
    F: Fn() -> Result<T>,
{
    const MAX_ATTEMPTS: usize = 3;
    let mut attempt = 0;
    loop {
        match op() {
            Ok(v) => return Ok(v),
            Err(e) => {
                attempt += 1;
                if attempt >= MAX_ATTEMPTS || !is_retryable(&e) {
                    return Err(e);
                }
                std::thread::sleep(std::time::Duration::from_millis(backoff_ms(
                    ordinal, attempt,
                )));
            }
        }
    }
}

/// True if the error chain carries a retryable reqwest failure.
fn is_retryable(err: &anyhow::Error) -> bool {
    err.chain()
        .find_map(|c| c.downcast_ref::<reqwest::Error>())
        .is_some_and(|e| {
            e.is_timeout()
                || e.is_connect()
                || matches!(
                    e.status(),
                    Some(reqwest::StatusCode::TOO_MANY_REQUESTS)
                        | Some(reqwest::StatusCode::SERVICE_UNAVAILABLE)
                )
        })
}

/// Backoff for an attempt (1-based): `500ms << attempt`, capped at 4s, plus a
/// deterministic 0..250ms jitter derived from `(ordinal, attempt)`.
fn backoff_ms(ordinal: usize, attempt: usize) -> u64 {
    let base = (500u64 << attempt).min(4000);
    let jitter = ((ordinal.wrapping_mul(2654435761) ^ attempt.wrapping_mul(40503)) % 250) as u64;
    base + jitter
}

/// Clamp a requested in-flight cap to `1..=16`. `0` (auto/unset) maps to 1.
#[must_use]
pub(crate) fn clamp_concurrency(requested: usize) -> usize {
    if requested == 0 {
        1
    } else {
        requested.min(16)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vecs(start: usize, count: usize) -> Vec<Vec<f32>> {
        (0..count).map(|i| vec![(start + i) as f32]).collect()
    }

    #[test]
    fn clamp_concurrency_bounds() {
        assert_eq!(clamp_concurrency(0), 1, "0 = serial");
        assert_eq!(clamp_concurrency(1), 1);
        assert_eq!(clamp_concurrency(16), 16);
        assert_eq!(clamp_concurrency(17), 16);
        assert_eq!(clamp_concurrency(usize::MAX), 16);
    }

    #[test]
    fn preserves_input_order_when_completion_reorders() {
        // Each sub-batch is one text whose value encodes its position; later
        // ordinals sleep less so they finish first, yet output stays in order.
        let batches: Vec<Vec<&str>> = vec![vec!["0"], vec!["1"], vec!["2"], vec!["3"]];
        let refs: Vec<&[&str]> = batches.iter().map(|b| b.as_slice()).collect();
        let out = run_concurrent(refs, 4, |_idx, b| {
            let v: u64 = b[0].parse().unwrap();
            std::thread::sleep(std::time::Duration::from_millis((4 - v) * 5));
            Ok(vec![vec![v as f32]])
        })
        .unwrap();
        assert_eq!(out, vec![vec![0.0], vec![1.0], vec![2.0], vec![3.0]]);
    }

    #[test]
    fn serial_and_concurrent_agree() {
        let batches: Vec<Vec<&str>> = (0..10)
            .map(|i| vec![Box::leak(i.to_string().into_boxed_str()) as &str])
            .collect();
        let refs: Vec<&[&str]> = batches.iter().map(|b| b.as_slice()).collect();
        let embed = |_idx: usize, b: &[&str]| -> Result<Vec<Vec<f32>>> {
            let v: f32 = b[0].parse().unwrap();
            Ok(vec![vec![v]])
        };
        let serial = run_concurrent(refs.clone(), 1, embed).unwrap();
        let concurrent = run_concurrent(refs, 8, embed).unwrap();
        assert_eq!(serial, concurrent);
    }

    #[test]
    fn lowest_ordinal_error_wins() {
        let batches: Vec<Vec<&str>> = vec![vec!["ok"], vec!["fail-1"], vec!["ok"], vec!["fail-3"]];
        let refs: Vec<&[&str]> = batches.iter().map(|b| b.as_slice()).collect();
        let err = run_concurrent(refs, 4, |_idx, b| {
            if b[0].starts_with("fail") {
                anyhow::bail!("{}", b[0]);
            }
            Ok(vecs(0, 1))
        })
        .unwrap_err();
        assert_eq!(err.to_string(), "fail-1", "earliest failing ordinal wins");
    }

    #[test]
    fn backoff_jitter_is_deterministic_and_bounded() {
        assert_eq!(
            backoff_ms(3, 1),
            backoff_ms(3, 1),
            "same inputs → same delay"
        );
        assert!(backoff_ms(0, 1) < 1000 + 250);
        assert!(backoff_ms(7, 10) <= 4000 + 249, "capped base + jitter");
        // Distinct ordinals must de-correlate (else concurrent sub-batches
        // retry in lockstep — the thundering herd the jitter prevents).
        assert_ne!(
            backoff_ms(0, 1),
            backoff_ms(1, 1),
            "ordinal must vary the jitter on a given attempt"
        );
    }
}
