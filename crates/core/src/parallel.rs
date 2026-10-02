//! Ordered, byte-bounded CPU jobs. Completed results retain their reservation
//! until consumed, so a slow writer cannot create an unbounded result backlog.
use crate::JobControl;
use anyhow::{Result, anyhow, ensure};
use std::{
    collections::VecDeque,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread::{self, JoinHandle},
    time::Duration,
};

struct Task<T, R> {
    input: T,
    result: mpsc::Sender<Result<R>>,
}

pub(crate) struct OrderedPool<T, R> {
    sender: Option<mpsc::Sender<Task<T, R>>>,
    pending: VecDeque<(usize, mpsc::Receiver<Result<R>>)>,
    threads: Vec<JoinHandle<()>>,
    stop: Arc<AtomicBool>,
    reserved: usize,
    budget: usize,
    max_jobs: usize,
    job: JobControl,
}

impl<T: Send + 'static, R: Send + 'static> OrderedPool<T, R> {
    pub fn new(
        workers: usize,
        budget: usize,
        job: &JobControl,
        processor: impl Fn(T) -> Result<R> + Send + Sync + 'static,
    ) -> Result<Self> {
        ensure!((1..=64).contains(&workers), "Invalid worker count");
        ensure!(budget > 0, "Invalid worker memory budget");
        let (sender, receiver) = mpsc::channel::<Task<T, R>>();
        let receiver = Arc::new(Mutex::new(receiver));
        let stop = Arc::new(AtomicBool::new(false));
        let processor = Arc::new(processor);
        let mut pool = Self {
            sender: Some(sender),
            pending: VecDeque::new(),
            threads: vec![],
            stop: stop.clone(),
            reserved: 0,
            budget,
            max_jobs: workers * 2,
            job: job.clone(),
        };
        for _ in 0..workers {
            let receiver = receiver.clone();
            let stop = stop.clone();
            let processor = processor.clone();
            let job = job.clone();
            pool.threads
                .push(
                    thread::Builder::new()
                        .name("point-convert".into())
                        .spawn(move || {
                            loop {
                                let task = match receiver.lock() {
                                    Ok(rx) => rx.recv(),
                                    Err(_) => break,
                                };
                                let Ok(task) = task else { break };
                                if stop.load(Ordering::Relaxed) {
                                    break;
                                }
                                let result =
                                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                        job.check()?;
                                        let value = processor(task.input)?;
                                        job.check()?;
                                        Ok(value)
                                    }))
                                    .unwrap_or_else(|_| Err(anyhow!("Conversion worker panicked")));
                                let _ = task.result.send(result);
                            }
                        })?,
                );
        }
        Ok(pool)
    }

    pub fn has_capacity(&self, bytes: usize) -> bool {
        bytes <= self.budget.saturating_sub(self.reserved) && self.pending.len() < self.max_jobs
    }

    pub fn submit(&mut self, input: T, bytes: usize) -> Result<()> {
        self.job.check()?;
        ensure!(self.has_capacity(bytes), "Conversion queue budget exceeded");
        let (result, receiver) = mpsc::channel();
        self.sender
            .as_ref()
            .unwrap()
            .send(Task { input, result })
            .map_err(|_| anyhow!("Conversion workers stopped"))?;
        self.pending.push_back((bytes, receiver));
        self.reserved += bytes;
        Ok(())
    }

    pub fn pop(&mut self) -> Result<Option<R>> {
        let Some((bytes, receiver)) = self.pending.pop_front() else {
            return Ok(None);
        };
        loop {
            self.job.check()?;
            match receiver.recv_timeout(Duration::from_millis(50)) {
                Ok(result) => {
                    self.reserved -= bytes;
                    return result.map(Some);
                }
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(anyhow!("Conversion worker disconnected"));
                }
            }
        }
    }
}

impl<T, R> Drop for OrderedPool<T, R> {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.sender.take();
        self.pending.clear();
        for handle in self.threads.drain(..) {
            let _ = handle.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn overlapping_jobs_return_in_order_and_completed_results_keep_their_budget() {
        let (release, wait) = mpsc::channel();
        let wait = Mutex::new(wait);
        let (completed, second) = mpsc::channel();
        let mut pool = OrderedPool::new(2, 100, &JobControl::default(), move |i| {
            if i == 0 {
                wait.lock().unwrap().recv().unwrap();
            } else {
                completed.send(()).unwrap();
            }
            Ok(i)
        })
        .unwrap();
        pool.submit(0, 50).unwrap();
        pool.submit(1, 50).unwrap();
        // Job 1 completes while job 0 is still running. Its completed result
        // must continue consuming queue budget until the ordered writer reads it.
        let arrived = second.recv_timeout(Duration::from_secs(5));
        release.send(()).unwrap();
        assert!(arrived.is_ok());
        assert!(!pool.has_capacity(1));
        assert_eq!(pool.pop().unwrap(), Some(0));
        assert!(pool.has_capacity(50));
        assert_eq!(pool.pop().unwrap(), Some(1));
        assert_eq!(pool.pop().unwrap(), None);
    }

    #[test]
    fn worker_failure_is_returned_and_pending_jobs_can_be_dropped() {
        let mut pool = OrderedPool::<u32, u32>::new(2, 100, &JobControl::default(), |_| {
            Err(anyhow!("failed block"))
        })
        .unwrap();
        pool.submit(0, 40).unwrap();
        pool.submit(1, 40).unwrap();
        assert!(pool.pop().unwrap_err().to_string().contains("failed block"));
    }
}
