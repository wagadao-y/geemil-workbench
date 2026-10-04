//! Benchmark sampling only: Windows process working set/commit and staging
//! bytes every 50 ms. Report observed peaks, not an application memory limit.
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};

pub fn disk_bytes(path: &Path) -> u64 {
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return 0;
    };
    if !meta.is_dir() {
        return meta.len();
    }
    std::fs::read_dir(path).map_or(0, |entries| {
        entries
            .filter_map(Result::ok)
            .map(|e| disk_bytes(&e.path()))
            .sum()
    })
}

#[cfg(windows)]
fn process_memory() -> (u64, u64, u64, u64) {
    #[repr(C)]
    #[derive(Default)]
    struct Counters {
        cb: u32,
        faults: u32,
        peak_working: usize,
        working: usize,
        peak_paged: usize,
        paged: usize,
        peak_nonpaged: usize,
        nonpaged: usize,
        commit: usize,
        peak_commit: usize,
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetCurrentProcess() -> *mut std::ffi::c_void;
    }
    #[link(name = "psapi")]
    unsafe extern "system" {
        fn GetProcessMemoryInfo(
            process: *mut std::ffi::c_void,
            counters: *mut Counters,
            cb: u32,
        ) -> i32;
    }
    let mut counters = Counters {
        cb: std::mem::size_of::<Counters>() as u32,
        ..Default::default()
    };
    // SAFETY: Win32's documented C layout and a valid current-process handle;
    // the output buffer is live and has the size passed to the API.
    unsafe {
        if GetProcessMemoryInfo(GetCurrentProcess(), &mut counters, counters.cb) == 0 {
            return (0, 0, 0, 0);
        }
    }
    (
        counters.working as u64,
        counters.commit as u64,
        counters.peak_working as u64,
        counters.peak_commit as u64,
    )
}
#[cfg(not(windows))]
fn process_memory() -> (u64, u64, u64, u64) {
    (0, 0, 0, 0)
}

pub fn phase<T>(
    name: &str,
    root: &Path,
    work: impl FnOnce() -> anyhow::Result<T>,
) -> anyhow::Result<T> {
    let stop = Arc::new(AtomicBool::new(false));
    let working = Arc::new(AtomicU64::new(0));
    let commit = Arc::new(AtomicU64::new(0));
    let staging = Arc::new(AtomicU64::new(0));
    let thread: JoinHandle<()> = {
        let (stop, working, commit, staging) = (
            stop.clone(),
            working.clone(),
            commit.clone(),
            staging.clone(),
        );
        let stage: PathBuf = root.join("staging");
        std::thread::spawn(move || {
            loop {
                let (w, c, _, _) = process_memory();
                working.fetch_max(w, Ordering::Relaxed);
                commit.fetch_max(c, Ordering::Relaxed);
                staging.fetch_max(disk_bytes(&stage), Ordering::Relaxed);
                if stop.load(Ordering::Relaxed) {
                    break;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        })
    };
    let start = Instant::now();
    let result = work();
    let seconds = start.elapsed().as_secs_f64();
    let (_, _, peak_working, peak_commit) = process_memory();
    stop.store(true, Ordering::Relaxed);
    thread.join().expect("benchmark sampler");
    println!(
        "{}",
        serde_json::json!({ "phase": name, "seconds": seconds,
        "sampled_peak_working_bytes": working.load(Ordering::Relaxed),
        "sampled_peak_commit_bytes": commit.load(Ordering::Relaxed),
        "sampled_peak_staging_bytes": staging.load(Ordering::Relaxed),
        "process_cumulative_peak_working_bytes": peak_working,
        "process_cumulative_peak_commit_bytes": peak_commit,
        "project_disk_bytes": disk_bytes(root), "ok": result.is_ok() })
    );
    result
}
