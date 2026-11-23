//! The worker threads of streaming: a small pool built on `std` only.
//!
//! Jobs go in through one channel whose receiver is shared behind a mutex, results come back
//! on a second channel. A thread pool and an MPMC channel are a few dozen lines of `std`, not
//! worth a dependency yet (decision 0010).

use std::error::Error;
use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use vox_world::{ChunkMesh, ChunkPos};

/// Why streaming stopped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StreamError {
    /// A worker thread has stopped, which only happens when its job panicked.
    WorkerStopped,
}

impl fmt::Display for StreamError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WorkerStopped => write!(f, "a chunk worker thread stopped unexpectedly"),
        }
    }
}

impl Error for StreamError {}

/// A finished job.
#[derive(Debug)]
pub struct JobResult<T> {
    /// The chunk the job was for.
    pub pos: ChunkPos,
    /// What the job produced for it.
    pub output: T,
    /// How long the job took on its worker.
    pub took: Duration,
}

/// A pool of threads that run one function on every position sent to it and return its
/// output of type `T`.
pub struct Workers<T> {
    /// Closed on drop, which ends the worker loops. `None` only while dropping.
    jobs: Option<Sender<ChunkPos>>,
    results: Receiver<JobResult<T>>,
    /// Set on drop so workers skip the jobs still waiting in the channel.
    stop: Arc<AtomicBool>,
    threads: Vec<JoinHandle<()>>,
}

impl<T: Send + 'static> Workers<T> {
    /// Starts `count` threads (at least 1), each with its own scratch mesh, that run `work`
    /// on every position sent and report the result and how long it took.
    pub fn spawn(
        count: usize,
        work: impl Fn(ChunkPos, &mut ChunkMesh) -> T + Send + Sync + 'static,
    ) -> Self {
        let (job_sender, job_receiver) = mpsc::channel::<ChunkPos>();
        let (result_sender, results) = mpsc::channel::<JobResult<T>>();
        let job_receiver = Arc::new(Mutex::new(job_receiver));
        let work = Arc::new(work);
        let stop = Arc::new(AtomicBool::new(false));

        let threads = (0..count.max(1))
            .map(|_| {
                let job_receiver = Arc::clone(&job_receiver);
                let results = result_sender.clone();
                let work = Arc::clone(&work);
                let stop = Arc::clone(&stop);
                thread::spawn(move || run_worker(&job_receiver, &results, &*work, &stop))
            })
            .collect();
        Self {
            jobs: Some(job_sender),
            results,
            stop,
            threads,
        }
    }

    /// Queues `pos` for the next free worker.
    ///
    /// # Errors
    /// [`StreamError::WorkerStopped`] when no worker is left to take it.
    pub fn send(&self, pos: ChunkPos) -> Result<(), StreamError> {
        let jobs = self.jobs.as_ref().ok_or(StreamError::WorkerStopped)?;
        jobs.send(pos).map_err(|_| StreamError::WorkerStopped)
    }

    /// A finished job, if any, without blocking.
    ///
    /// # Errors
    /// [`StreamError::WorkerStopped`] when every worker is gone and nothing is left to read.
    pub fn try_recv(&self) -> Result<Option<JobResult<T>>, StreamError> {
        match self.results.try_recv() {
            Ok(result) => Ok(Some(result)),
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) => Err(StreamError::WorkerStopped),
        }
    }

    /// Checks that every worker thread still runs. A worker only ends before the pool is
    /// dropped when its job panicked, and the chunk it held would never load.
    ///
    /// # Errors
    /// [`StreamError::WorkerStopped`] when any worker thread has finished.
    pub fn check(&self) -> Result<(), StreamError> {
        if self.threads.iter().any(JoinHandle::is_finished) {
            Err(StreamError::WorkerStopped)
        } else {
            Ok(())
        }
    }
}

impl<T> Drop for Workers<T> {
    /// Stops the workers and waits for them: each finishes the job it is running, at most.
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        // Closing the job channel wakes the workers that wait for a job.
        self.jobs = None;
        for thread in self.threads.drain(..) {
            // A worker that panicked was reported by `check`; there is nothing more to do.
            let _ = thread.join();
        }
    }
}

/// The loop of one worker thread: take a job, run it, report it, until the pool shuts down.
fn run_worker<T>(
    jobs: &Mutex<Receiver<ChunkPos>>,
    results: &Sender<JobResult<T>>,
    work: &(impl Fn(ChunkPos, &mut ChunkMesh) -> T + ?Sized),
    stop: &AtomicBool,
) {
    let mut scratch = ChunkMesh::default();
    loop {
        // The lock is held only while waiting for a job, never while running one. A worker
        // that waits here blocks the others on the mutex instead, which is the same thing.
        let job = match jobs.lock() {
            Ok(receiver) => receiver.recv(),
            Err(_) => return,
        };
        let Ok(pos) = job else {
            return;
        };
        if stop.load(Ordering::Acquire) {
            return;
        }
        let started = Instant::now();
        let output = work(pos, &mut scratch);
        let result = JobResult {
            pos,
            output,
            took: started.elapsed(),
        };
        if results.send(result).is_err() {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::sync::atomic::AtomicUsize;

    use super::*;

    /// How long a test waits for the workers before it fails.
    const TIMEOUT: Duration = Duration::from_secs(10);
    /// Pause between two polls of the pool.
    const POLL: Duration = Duration::from_millis(1);
    /// Positions sent in the throughput tests.
    const JOBS: i32 = 200;

    fn at(x: i32) -> ChunkPos {
        ChunkPos::new(x, 0, 0)
    }

    /// A job whose mesh carries its x coordinate, so a result can be checked.
    fn marked(pos: ChunkPos, _scratch: &mut ChunkMesh) -> ChunkMesh {
        ChunkMesh {
            vertices: Vec::new(),
            indices: vec![pos.x.unsigned_abs()],
        }
    }

    #[test]
    fn every_sent_position_comes_back_exactly_once() {
        let workers = Workers::spawn(3, marked);
        for x in 0..JOBS {
            workers.send(at(x)).expect("workers run");
        }
        let started = Instant::now();
        let mut seen = HashSet::new();
        while seen.len() < JOBS as usize {
            assert!(started.elapsed() < TIMEOUT, "timed out");
            match workers.try_recv().expect("workers run") {
                Some(result) => {
                    assert_eq!(result.output.indices, vec![result.pos.x.unsigned_abs()]);
                    assert!(seen.insert(result.pos), "{:?} came back twice", result.pos);
                }
                None => thread::sleep(POLL),
            }
        }
        thread::sleep(10 * POLL);
        assert!(workers.try_recv().expect("workers run").is_none());
        assert!(workers.check().is_ok());
    }

    #[test]
    fn zero_threads_still_start_one() {
        let workers = Workers::spawn(0, marked);
        workers.send(at(1)).expect("workers run");
        let started = Instant::now();
        while workers.try_recv().expect("workers run").is_none() {
            assert!(started.elapsed() < TIMEOUT, "timed out");
            thread::sleep(POLL);
        }
    }

    #[test]
    fn a_panicking_job_is_reported_by_check() {
        let workers = Workers::spawn(2, |pos, scratch| {
            assert!(pos.x != 0, "the test job panics on purpose");
            marked(pos, scratch)
        });
        assert!(workers.check().is_ok());
        workers.send(at(0)).expect("workers run");
        let started = Instant::now();
        while workers.check().is_ok() {
            assert!(started.elapsed() < TIMEOUT, "timed out");
            thread::sleep(POLL);
        }
        assert_eq!(workers.check(), Err(StreamError::WorkerStopped));
    }

    #[test]
    fn dropping_the_pool_joins_the_threads_with_jobs_still_queued() {
        /// How long each job of the test takes.
        const JOB_TIME: Duration = Duration::from_millis(20);
        let run = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&run);
        let workers = Workers::spawn(1, move |pos, scratch| {
            counter.fetch_add(1, Ordering::SeqCst);
            thread::sleep(JOB_TIME);
            marked(pos, scratch)
        });
        for x in 0..JOBS {
            workers.send(at(x)).expect("workers run");
        }
        let started = Instant::now();
        drop(workers);
        // Waiting for every job would take JOBS * JOB_TIME (4 s); stopping takes about one job.
        assert!(started.elapsed() < JOB_TIME * (JOBS as u32) / 4);
        assert!(run.load(Ordering::SeqCst) < JOBS as usize);
    }

    #[test]
    fn the_error_has_a_message() {
        assert!(!StreamError::WorkerStopped.to_string().is_empty());
    }
}
