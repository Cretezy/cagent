//! Per-call execution timing, excluding authorization (including queued prompts).
//!
//! Task-local scope follows the call future, so parallel calls cannot subtract
//! each other's approval waits. Nested authorization pauses count only once.

use std::cell::Cell;
use std::future::Future;
use std::time::{Duration, Instant};

#[derive(Default)]
struct AuthorizationTime {
    depth: Cell<usize>,
    elapsed: Cell<Duration>,
}

tokio::task_local! {
    static AUTHORIZATION_TIME: AuthorizationTime;
}

pub(super) async fn measure<T>(future: impl Future<Output = T>) -> (T, u64) {
    AUTHORIZATION_TIME
        .scope(AuthorizationTime::default(), async {
            let started = Instant::now();
            let result = future.await;
            let elapsed = started
                .elapsed()
                .saturating_sub(AUTHORIZATION_TIME.with(|time| time.elapsed.get()));
            (
                result,
                u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX),
            )
        })
        .await
}

pub(super) struct AuthorizationPause(Instant);

/// No-op outside a measured primary tool call (e.g. attachment approval).
pub(super) fn pause_for_authorization() -> AuthorizationPause {
    let _ = AUTHORIZATION_TIME.try_with(|time| time.depth.set(time.depth.get() + 1));
    AuthorizationPause(Instant::now())
}

impl Drop for AuthorizationPause {
    fn drop(&mut self) {
        let _ = AUTHORIZATION_TIME.try_with(|time| {
            let depth = time.depth.get().saturating_sub(1);
            time.depth.set(depth);
            if depth == 0 {
                time.elapsed.set(time.elapsed.get() + self.0.elapsed());
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn tool_timing_excludes_nested_and_repeated_authorization_waits() {
        let ((wall, excluded), millis) = measure(async {
            let started = Instant::now();
            for _ in 0..2 {
                let _outer = pause_for_authorization();
                tokio::time::sleep(Duration::from_millis(25)).await;
                let _inner = pause_for_authorization();
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
            (
                started.elapsed(),
                AUTHORIZATION_TIME.with(|time| time.elapsed.get()),
            )
        })
        .await;
        assert!(excluded >= Duration::from_millis(100));
        assert!(excluded <= wall, "nested waits must not be counted twice");
        assert!(Duration::from_millis(millis) < excluded);
    }

    #[tokio::test]
    async fn tool_timing_keeps_parallel_calls_independent() {
        let (approval, execution) = tokio::join!(
            measure(async {
                let _pause = pause_for_authorization();
                tokio::time::sleep(Duration::from_millis(100)).await;
            }),
            measure(tokio::time::sleep(Duration::from_millis(50))),
        );
        assert!(approval.1 < execution.1);
        assert!(execution.1 >= 50);
    }

    #[tokio::test]
    async fn tool_timing_resumes_after_denied_or_cancelled_authorization() {
        let (result, _) = measure(async {
            let _pause = pause_for_authorization();
            Err::<(), _>("denied")
        })
        .await;
        assert_eq!(result, Err("denied"));
        let (_, millis) = measure(tokio::time::sleep(Duration::from_millis(10))).await;
        assert!(millis >= 10);
        drop(pause_for_authorization()); // Non-tool authorization is supported.
    }
}
