use std::collections::HashMap;
use std::future::Future;

use tokio::task::{JoinError, JoinSet};

/// Keep at most `concurrency` spawned jobs alive, returning results in input order.
pub(crate) async fn run_bounded<T, R, F, Fut>(
    items: impl IntoIterator<Item = T>,
    concurrency: usize,
    mut job: F,
) -> Vec<Result<R, JoinError>>
where
    R: Send + 'static,
    F: FnMut(T) -> Fut,
    Fut: Future<Output = R> + Send + 'static,
{
    assert!(concurrency > 0);
    let mut items = items.into_iter();
    let mut running = JoinSet::new();
    let mut indices = HashMap::new();
    let mut results = Vec::new();

    loop {
        while running.len() < concurrency {
            let Some(item) = items.next() else { break };
            let index = results.len();
            results.push(None);
            let handle = running.spawn(job(item));
            indices.insert(handle.id(), index);
        }

        let Some(completed) = running.join_next_with_id().await else {
            break;
        };
        let (id, result) = match completed {
            Ok((id, value)) => (id, Ok(value)),
            Err(error) => (error.id(), Err(error)),
        };
        let index = indices.remove(&id).expect("each job has an input index");
        results[index] = Some(result);
    }

    results
        .into_iter()
        .map(|result| result.expect("each job completed"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::run_bounded;
    use std::sync::{Arc, Mutex};
    use tokio::sync::oneshot;
    use tokio::time::{timeout, Duration};

    #[tokio::test]
    async fn bounded_jobs_create_only_the_active_window_and_refill_promptly() {
        let (release, blocked) = oneshot::channel();
        let mut blocked = Some(blocked);
        let (started, third_started) = oneshot::channel();
        let mut started = Some(started);
        let created = Arc::new(Mutex::new(Vec::new()));
        let observed = created.clone();
        let runner = tokio::spawn(async move {
            run_bounded(0..1000, 2, move |index| {
                created.lock().unwrap().push(index);
                let blocked = if index == 0 { blocked.take() } else { None };
                let started = if index == 2 { started.take() } else { None };
                async move {
                    if let Some(started) = started {
                        // Wait here so the number of created futures is deterministic.
                        let (release_third, wait) = oneshot::channel();
                        started.send(release_third).unwrap();
                        wait.await.unwrap();
                    }
                    if let Some(blocked) = blocked {
                        blocked.await.unwrap();
                    }
                    index
                }
            })
            .await
        });
        let release_third = timeout(Duration::from_secs(5), third_started)
            .await
            .unwrap()
            .unwrap();
        // Job 1 freed a slot while job 0 is still blocked. No futures for the
        // remaining 997 inputs should have been created yet.
        assert_eq!(*observed.lock().unwrap(), vec![0, 1, 2]);
        release.send(()).unwrap();
        release_third.send(()).unwrap();
        let results = runner
            .await
            .unwrap()
            .into_iter()
            .map(Result::unwrap)
            .collect::<Vec<_>>();
        assert_eq!(results, (0..1000).collect::<Vec<_>>());
    }

    #[tokio::test]
    async fn bounded_jobs_preserve_panic_positions_and_continue() {
        let results = run_bounded(0..10, 3, |index| async move {
            if index == 2 {
                panic!("test job failed");
            }
            tokio::task::yield_now().await;
            index
        })
        .await;
        assert_eq!(results.len(), 10);
        for (index, result) in results.into_iter().enumerate() {
            if index == 2 {
                assert!(result.unwrap_err().is_panic());
            } else {
                assert_eq!(result.unwrap(), index);
            }
        }
    }

    #[tokio::test]
    async fn bounded_jobs_accept_empty_input() {
        assert!(
            run_bounded(Vec::<usize>::new(), 2, |index| async move { index })
                .await
                .is_empty()
        );
    }
}
