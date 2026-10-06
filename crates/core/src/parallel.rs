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

/// Runs `work` on every item on `workers` threads, started once, and hands
/// each result to `consume` on the calling thread as it arrives, in no
/// particular order. The first error, from either side, stops the rest.
pub(crate) fn for_each_unordered<T: Sync, R: Send>(
    items: &[T],
    workers: usize,
    job: &JobControl,
    work: impl Fn(&T) -> Result<R> + Sync,
    mut consume: impl FnMut(R) -> Result<()>,
) -> Result<()> {
    let workers = workers.clamp(1, items.len().max(1));
    let next = std::sync::atomic::AtomicUsize::new(0);
    let failed = AtomicBool::new(false);
    // Bounded, so results wait for the consumer instead of piling up.
    let (sender, receiver) = mpsc::sync_channel::<Result<R>>(workers * 2);
    thread::scope(|scope| {
        for _ in 0..workers {
            let sender = sender.clone();
            let (next, failed, work) = (&next, &failed, &work);
            scope.spawn(move || {
                while !failed.load(Ordering::Relaxed) {
                    let index = next.fetch_add(1, Ordering::Relaxed);
                    let Some(item) = items.get(index) else {
                        break;
                    };
                    let result = job.check().and_then(|_| work(item));
                    if result.is_err() {
                        failed.store(true, Ordering::Relaxed);
                    }
                    if sender.send(result).is_err() {
                        break;
                    }
                }
            });
        }
        drop(sender);
        // Drain until every worker has stopped, so none blocks on a full
        // channel after a failure.
        let mut first = None;
        for result in receiver {
            if first.is_some() {
                continue;
            }
            if let Err(e) = result.and_then(&mut consume) {
                failed.store(true, Ordering::Relaxed);
                first = Some(e);
            }
        }
        first.map_or(Ok(()), Err)
    })
}

/// Runs `work` on every item on `workers` threads, started once, and hands
/// the results to `consume` on the calling thread in item order. Workers
/// stay at most two items each ahead of `consume`, so results never pile
/// up. The first error, from either side, stops the rest.
pub(crate) fn for_each_ordered<T: Sync, R: Send>(
    items: &[T],
    workers: usize,
    job: &JobControl,
    work: impl Fn(usize, &T) -> Result<R> + Sync,
    mut consume: impl FnMut(usize, R) -> Result<()>,
) -> Result<()> {
    struct State<R> {
        next: usize,
        consumed: usize,
        ready: std::collections::HashMap<usize, Result<R>>,
        failed: bool,
    }
    let workers = workers.clamp(1, items.len().max(1));
    let window = workers * 2;
    let state = Mutex::new(State {
        next: 0,
        consumed: 0,
        ready: std::collections::HashMap::new(),
        failed: false,
    });
    let changed = std::sync::Condvar::new();
    thread::scope(|scope| {
        for _ in 0..workers {
            let (state, changed, work) = (&state, &changed, &work);
            scope.spawn(move || {
                let mut s = state.lock().unwrap();
                loop {
                    if s.failed || s.next >= items.len() {
                        return;
                    }
                    if s.next >= s.consumed + window {
                        s = changed.wait(s).unwrap();
                        continue;
                    }
                    let index = s.next;
                    s.next += 1;
                    drop(s);
                    let result = job.check().and_then(|_| work(index, &items[index]));
                    s = state.lock().unwrap();
                    s.failed |= result.is_err();
                    s.ready.insert(index, result);
                    changed.notify_all();
                }
            });
        }
        let fail = |e| {
            state.lock().unwrap().failed = true;
            changed.notify_all();
            Err(e)
        };
        for index in 0..items.len() {
            let result = {
                let mut s = state.lock().unwrap();
                // Items start in order, so every item before a failed one has
                // started and arrives; waiting for each in turn consumes all
                // of them and then reports the earliest error, however the
                // workers' timing falls.
                loop {
                    if let Some(result) = s.ready.remove(&index) {
                        break result;
                    }
                    s = changed.wait(s).unwrap();
                }
            };
            if let Err(e) = result.and_then(|r| consume(index, r)) {
                return fail(e);
            }
            state.lock().unwrap().consumed = index + 1;
            changed.notify_all();
        }
        Ok(())
    })
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
    fn ordered_jobs_arrive_in_order_and_stop_at_the_first_error() {
        let items: Vec<u64> = (0..1000).collect();
        let job = JobControl::default();
        let mut seen = vec![];
        for_each_ordered(
            &items,
            4,
            &job,
            |i, item| {
                // Uneven work, so later items often finish first.
                if i % 7 == 0 {
                    thread::sleep(Duration::from_micros(200));
                }
                Ok(*item * 2)
            },
            |i, value| {
                assert_eq!(value, i as u64 * 2);
                seen.push(i);
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(seen, (0..1000).collect::<Vec<_>>());
        let mut consumed = 0;
        let error = for_each_ordered(
            &items,
            4,
            &job,
            |i, _| {
                // The item before the failure is still running when it
                // fails; it is consumed all the same.
                if i == 499 {
                    thread::sleep(Duration::from_millis(20));
                }
                if i == 500 {
                    Err(anyhow!("work"))
                } else {
                    Ok(())
                }
            },
            |_, _| {
                consumed += 1;
                Ok(())
            },
        );
        assert_eq!(error.unwrap_err().to_string(), "work");
        assert_eq!(consumed, 500);
        let error = for_each_ordered(
            &items,
            4,
            &job,
            |_, _| Ok(()),
            |i, _| {
                if i == 10 {
                    Err(anyhow!("consume"))
                } else {
                    Ok(())
                }
            },
        );
        assert_eq!(error.unwrap_err().to_string(), "consume");
        job.cancel.store(true, Ordering::Relaxed);
        assert!(for_each_ordered(&items, 4, &job, |_, _| Ok(()), |_, _| Ok(())).is_err());
    }
    #[test]
    fn unordered_jobs_consume_every_result_and_stop_at_the_first_error() {
        let items: Vec<u64> = (0..1000).collect();
        let job = JobControl::default();
        let mut sum = 0;
        for_each_unordered(
            &items,
            4,
            &job,
            |i| Ok(*i),
            |i| {
                sum += i;
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(sum, 499_500);
        // Failing workers and a failing consumer both end the run, with
        // workers still producing while the consumer has stopped taking.
        let error = for_each_unordered(
            &items,
            4,
            &job,
            |i| {
                if *i == 500 {
                    Err(anyhow!("work"))
                } else {
                    Ok(*i)
                }
            },
            |_| Ok(()),
        );
        assert_eq!(error.unwrap_err().to_string(), "work");
        let error = for_each_unordered(&items, 4, &job, |i| Ok(*i), |_| Err(anyhow!("consume")));
        assert_eq!(error.unwrap_err().to_string(), "consume");
        job.cancel.store(true, Ordering::Relaxed);
        assert!(for_each_unordered(&items, 4, &job, |i| Ok(*i), |_| Ok(())).is_err());
    }
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
