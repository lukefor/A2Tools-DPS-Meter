//! CPU and disk work, separate from both GTK and Tokio's async workers.
//! Each queue runs one job at a time; callers wait for their turn.
use tokio::sync::Semaphore;

pub(crate) static CALCULATIONS: WorkQueue = WorkQueue::new();
/// The 500 ms meter tick. Details readers do not take the calculator mutex,
/// so the tick must not wait behind them in CALCULATIONS.
pub(crate) static DPS_TICK: WorkQueue = WorkQueue::new();
pub(crate) static HISTORY: WorkQueue = WorkQueue::new();
/// The skill icon cache: small files, many at once when a fight opens, so
/// they queue apart from history and never behind a fight being read.
pub(crate) static ICONS: WorkQueue = WorkQueue::new();

pub(crate) struct WorkQueue {
    running: Semaphore,
}

impl WorkQueue {
    const fn new() -> Self {
        Self {
            running: Semaphore::const_new(1),
        }
    }

    pub(crate) async fn run<T, F>(&'static self, work: F) -> Result<T, String>
    where
        T: Send + 'static,
        F: FnOnce() -> T + Send + 'static,
    {
        let running = self.running.acquire().await.map_err(|e| e.to_string())?;
        tauri::async_runtime::spawn_blocking(move || {
            // Keep the permit until the work ends, even if the awaiting
            // request is cancelled: blocking jobs cannot be aborted.
            let _running = running;
            work()
        })
        .await
        .map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(flavor = "current_thread")]
    async fn blocked_work_does_not_block_timers_and_cancel_keeps_the_queue_busy() {
        static QUEUE: WorkQueue = WorkQueue::new();
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let job = tokio::spawn(QUEUE.run(move || {
            started_tx.send(()).unwrap();
            release_rx.recv().unwrap();
        }));
        started_rx.await.unwrap();
        tokio::time::timeout(
            std::time::Duration::from_secs(1),
            tokio::time::sleep(std::time::Duration::from_millis(10)),
        )
        .await
        .unwrap();
        job.abort();
        let _ = job.await;
        // The cancelled job still runs, so the next one waits instead of failing.
        let next = tokio::spawn(QUEUE.run(|| 42));
        tokio::task::yield_now().await;
        assert!(!next.is_finished());
        release_tx.send(()).unwrap();
        assert_eq!(
            tokio::time::timeout(std::time::Duration::from_secs(1), next)
                .await
                .unwrap()
                .unwrap()
                .unwrap(),
            42
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn many_waiting_jobs_all_complete() {
        static QUEUE: WorkQueue = WorkQueue::new();
        let order = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let jobs: Vec<_> = (0..40)
            .map(|i| {
                let order = order.clone();
                tokio::spawn(QUEUE.run(move || order.lock().unwrap().push(i)))
            })
            .collect();
        for job in jobs {
            job.await.unwrap().unwrap();
        }
        assert_eq!(order.lock().unwrap().len(), 40);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn tick_queue_runs_while_details_queue_is_busy() {
        static DETAILS: WorkQueue = WorkQueue::new();
        static TICK: WorkQueue = WorkQueue::new();
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let details = tokio::spawn(DETAILS.run(move || {
            started_tx.send(()).unwrap();
            release_rx.recv().unwrap();
        }));
        started_rx.await.unwrap();
        let queued_details = tokio::spawn(DETAILS.run(|| 1));
        assert_eq!(
            tokio::time::timeout(std::time::Duration::from_secs(1), TICK.run(|| 42))
                .await
                .expect("tick waited behind details work")
                .unwrap(),
            42
        );
        release_tx.send(()).unwrap();
        details.await.unwrap().unwrap();
        assert_eq!(queued_details.await.unwrap().unwrap(), 1);
    }
}
