use geemil_core::{CoreError, Project};
use std::{
    path::Path,
    process::Command,
    time::{Duration, Instant},
};

#[test]
fn project_lock_probe() {
    let Some(root) = std::env::var_os("GEEMIL_LOCK_TEST_ROOT") else {
        return;
    };
    let mode = std::env::var("GEEMIL_LOCK_TEST_MODE").unwrap();
    let result = Project::load(Path::new(&root));
    if mode == "locked" {
        assert!(matches!(
            CoreError::find(&result.unwrap_err()),
            Some(CoreError::ProjectLocked(_))
        ));
    } else {
        let project = result.unwrap();
        if mode == "hold" {
            std::fs::write(project.root.join("test-lock-ready"), b"ready").unwrap();
            std::thread::sleep(Duration::from_secs(60));
        }
    }
}

fn probe(root: &Path, mode: &str) -> Command {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", "project_lock_probe", "--nocapture"])
        .env("GEEMIL_LOCK_TEST_ROOT", root)
        .env("GEEMIL_LOCK_TEST_MODE", mode);
    command
}

#[test]
fn project_lock_is_shared_by_clones_and_blocks_other_processes_until_drop() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("p");
    let project = Project::create(&root, "Lock test").unwrap();
    let clone = project.clone();
    drop(project);
    let mut reloaded = Project::load(&root).unwrap();
    reloaded.cleanup().unwrap();
    assert!(root.join("project.lock").is_file());
    assert!(probe(&root, "locked").status().unwrap().success());
    drop(reloaded);
    assert!(probe(&root, "locked").status().unwrap().success());
    drop(clone);
    assert!(probe(&root, "free").status().unwrap().success());
}

#[test]
fn interrupted_process_releases_the_os_lock_without_removing_the_lock_file() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("p");
    drop(Project::create(&root, "Crash test").unwrap());
    let mut child = probe(&root, "hold").spawn().unwrap();
    let start = Instant::now();
    while !root.join("test-lock-ready").exists() && start.elapsed() < Duration::from_secs(10) {
        std::thread::sleep(Duration::from_millis(10));
    }
    let ready = root.join("test-lock-ready").exists();
    child.kill().unwrap();
    child.wait().unwrap();
    assert!(ready, "child failed to acquire lock");
    assert!(Project::load(&root).is_ok());
}
