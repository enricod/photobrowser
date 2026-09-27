use std::cmp::Ordering;
use std::collections::BinaryHeap;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Priority {
    Background,
    Prefetch,
    Visible,
    Current,
}

#[derive(Debug, Clone, Default)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    pub fn new() -> CancelToken {
        CancelToken::default()
    }

    pub fn cancel(&self) {
        self.0.store(true, AtomicOrdering::Relaxed);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(AtomicOrdering::Relaxed)
    }
}

type Work = Box<dyn FnOnce() + Send + 'static>;

struct Job {
    priority: Priority,
    seq: u64,
    token: CancelToken,
    work: Work,
}

impl PartialEq for Job {
    fn eq(&self, other: &Self) -> bool {
        self.priority == other.priority && self.seq == other.seq
    }
}

impl Eq for Job {}

impl Ord for Job {
    /// Max-heap: higher priority first; within a priority, lower seq (older) first.
    fn cmp(&self, other: &Self) -> Ordering {
        self.priority
            .cmp(&other.priority)
            .then_with(|| other.seq.cmp(&self.seq))
    }
}

impl PartialOrd for Job {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

struct Queue {
    heap: BinaryHeap<Job>,
    next_seq: u64,
    shutdown: bool,
}

struct Shared {
    queue: Mutex<Queue>,
    ready: Condvar,
}

pub struct JobPool {
    shared: Arc<Shared>,
    workers: Vec<JoinHandle<()>>,
}

impl JobPool {
    pub fn new(threads: usize) -> JobPool {
        let shared = Arc::new(Shared {
            queue: Mutex::new(Queue { heap: BinaryHeap::new(), next_seq: 0, shutdown: false }),
            ready: Condvar::new(),
        });
        let workers = (0..threads.max(1))
            .map(|i| {
                let shared = shared.clone();
                std::thread::Builder::new()
                    .name(format!("pb-worker-{i}"))
                    .spawn(move || worker_loop(&shared))
                    .expect("failed to spawn worker thread")
            })
            .collect();
        JobPool { shared, workers }
    }

    /// One core is left for the GTK main thread.
    pub fn default_threads() -> usize {
        std::thread::available_parallelism()
            .map(|n| n.get().saturating_sub(1).max(1))
            .unwrap_or(1)
    }

    pub fn submit(&self, priority: Priority, token: CancelToken, work: impl FnOnce() + Send + 'static) {
        let mut q = self.shared.queue.lock().unwrap();
        let seq = q.next_seq;
        q.next_seq += 1;
        q.heap.push(Job { priority, seq, token, work: Box::new(work) });
        drop(q);
        self.shared.ready.notify_one();
    }

    pub fn queued(&self) -> usize {
        self.shared.queue.lock().unwrap().heap.len()
    }

    pub fn clear(&self) {
        self.shared.queue.lock().unwrap().heap.clear();
    }
}

fn worker_loop(shared: &Shared) {
    loop {
        let job = {
            let mut q = shared.queue.lock().unwrap();
            loop {
                if q.shutdown {
                    return;
                }
                if let Some(job) = q.heap.pop() {
                    break job;
                }
                q = shared.ready.wait(q).unwrap();
            }
        };
        if job.token.is_cancelled() {
            continue;
        }
        let _ = catch_unwind(AssertUnwindSafe(job.work));
    }
}

impl Drop for JobPool {
    fn drop(&mut self) {
        self.shared.queue.lock().unwrap().shutdown = true;
        self.shared.ready.notify_all();
        for worker in self.workers.drain(..) {
            let _ = worker.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;

    /// Occupies the single worker until the returned sender is used.
    fn block_worker(pool: &JobPool) -> mpsc::Sender<()> {
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let (started_tx, started_rx) = mpsc::channel::<()>();
        pool.submit(Priority::Current, CancelToken::new(), move || {
            started_tx.send(()).unwrap();
            let _ = release_rx.recv();
        });
        started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        release_tx
    }

    #[test]
    fn runs_higher_priority_first_and_fifo_within_priority() {
        let pool = JobPool::new(1);
        let release = block_worker(&pool);
        let (tx, rx) = mpsc::channel();
        for (p, name) in [
            (Priority::Background, "bg"),
            (Priority::Visible, "vis1"),
            (Priority::Prefetch, "pre"),
            (Priority::Visible, "vis2"),
            (Priority::Current, "cur"),
        ] {
            let tx = tx.clone();
            pool.submit(p, CancelToken::new(), move || tx.send(name).unwrap());
        }
        release.send(()).unwrap();
        let order: Vec<_> = (0..5).map(|_| rx.recv_timeout(Duration::from_secs(5)).unwrap()).collect();
        assert_eq!(order, ["cur", "vis1", "vis2", "pre", "bg"]);
    }

    #[test]
    fn cancelled_jobs_do_not_run() {
        let pool = JobPool::new(1);
        let release = block_worker(&pool);
        let (tx, rx) = mpsc::channel();
        let token = CancelToken::new();
        let tx1 = tx.clone();
        pool.submit(Priority::Visible, token.clone(), move || tx1.send("cancelled").unwrap());
        pool.submit(Priority::Background, CancelToken::new(), move || tx.send("kept").unwrap());
        token.cancel();
        release.send(()).unwrap();
        assert_eq!(rx.recv_timeout(Duration::from_secs(5)).unwrap(), "kept");
        assert!(rx.recv_timeout(Duration::from_millis(100)).is_err());
    }

    #[test]
    fn panicking_job_does_not_kill_worker() {
        let pool = JobPool::new(1);
        let (tx, rx) = mpsc::channel();
        pool.submit(Priority::Visible, CancelToken::new(), || panic!("boom"));
        pool.submit(Priority::Background, CancelToken::new(), move || tx.send(1).unwrap());
        assert_eq!(rx.recv_timeout(Duration::from_secs(5)).unwrap(), 1);
    }

    #[test]
    fn clear_drops_queued_jobs() {
        let pool = JobPool::new(1);
        let release = block_worker(&pool);
        let (tx, rx) = mpsc::channel::<()>();
        pool.submit(Priority::Visible, CancelToken::new(), move || tx.send(()).unwrap());
        assert_eq!(pool.queued(), 1);
        pool.clear();
        assert_eq!(pool.queued(), 0);
        release.send(()).unwrap();
        assert!(rx.recv_timeout(Duration::from_millis(200)).is_err());
    }

    #[test]
    fn default_threads_at_least_one() {
        assert!(JobPool::default_threads() >= 1);
    }
}
