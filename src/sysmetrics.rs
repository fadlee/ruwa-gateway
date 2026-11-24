//! What an operator needs to size and watch an instance (`/app/metrics`,
//! GOWA `pkg/sysmetrics`): memory of the process and of its container, CPU
//! time, disk used by media and storages, and free space on the volume.
//!
//! Everything is read on demand from `/proc`, the cgroup files and `statvfs`;
//! nothing is sampled in the background. Values that do not exist on the
//! current platform (non-Linux) or are unlimited (container memory limit) are
//! `null`.

use std::{
    alloc::{GlobalAlloc, Layout, System},
    collections::BTreeMap,
    path::Path,
    sync::{
        Mutex, OnceLock,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant, SystemTime},
};

use chrono::{DateTime, SecondsFormat, Utc};
use serde::Serialize;

/// Folder walks are expensive (media folders hold thousands of files), so a
/// walk is reused for this long.
const FOLDER_CACHE_FOR: Duration = Duration::from_secs(60);
/// `USER_HZ` on every Linux the images run on.
const TICKS_PER_SECOND: f64 = 100.0;

static STARTED: OnceLock<(Instant, SystemTime)> = OnceLock::new();
static FOLDER_CACHE: Mutex<Option<(Instant, BTreeMap<String, Folder>)>> = Mutex::new(None);

/// Bytes currently allocated on the heap, maintained by [`CountingAlloc`].
static HEAP_BYTES: AtomicUsize = AtomicUsize::new(0);

/// Global allocator wrapper that tracks live heap bytes (the equivalent of Go's
/// `HeapAlloc`). Register it in the binary with `#[global_allocator]`.
pub struct CountingAlloc;

// SAFETY: every call is forwarded unchanged to `System`; the counter is only
// bookkeeping.
unsafe impl GlobalAlloc for CountingAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let ptr = unsafe { System.alloc(layout) };
        if !ptr.is_null() {
            HEAP_BYTES.fetch_add(layout.size(), Ordering::Relaxed);
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) };
        HEAP_BYTES.fetch_sub(layout.size(), Ordering::Relaxed);
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let ptr = unsafe { System.alloc_zeroed(layout) };
        if !ptr.is_null() {
            HEAP_BYTES.fetch_add(layout.size(), Ordering::Relaxed);
        }
        ptr
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let new = unsafe { System.realloc(ptr, layout, new_size) };
        if !new.is_null() {
            HEAP_BYTES.fetch_add(new_size, Ordering::Relaxed);
            HEAP_BYTES.fetch_sub(layout.size(), Ordering::Relaxed);
        }
        new
    }
}

/// Records the process start time. Call first thing in `main` so uptime
/// includes initialization; otherwise it starts at the first metrics read.
pub fn init() {
    STARTED.get_or_init(|| (Instant::now(), SystemTime::now()));
}

/// Size of a directory tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Folder {
    pub bytes: u64,
    pub files: u64,
}

/// The volume a path lives on; `None` = unknown on this platform.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct Filesystem {
    pub total_bytes: Option<u64>,
    pub free_bytes: Option<u64>,
}

/// Keys keep their GOWA names (`runtime_sys_bytes`, `goroutines`, `gc_cycles`)
/// because clients such as gowa-ui read them; see [`snapshot`] for the Rust
/// meaning of each.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Memory {
    pub heap_alloc_bytes: u64,
    pub runtime_sys_bytes: u64,
    pub process_rss_bytes: Option<u64>,
    pub container_bytes: Option<u64>,
    pub container_limit_bytes: Option<u64>,
    pub goroutines: u64,
    pub gc_cycles: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Snapshot {
    pub started_at: String,
    pub uptime_seconds: u64,
    /// Compiler version (GOWA reports the Go toolchain here).
    pub go_version: String,
    pub cpu_seconds: Option<f64>,
    pub memory: Memory,
    pub folders: BTreeMap<String, Folder>,
    pub filesystem: Filesystem,
}

/// Builds a snapshot. `folders` maps a label to a path; `fs_path` is the path
/// whose volume is reported. Must run inside a tokio runtime.
///
/// Rust equivalents of the Go runtime fields: `heap_alloc_bytes` = live bytes
/// seen by [`CountingAlloc`] (0 if it is not installed), `runtime_sys_bytes` =
/// virtual size of the process (`VmSize`), `goroutines` = live tokio tasks,
/// `gc_cycles` = 0 (no GC).
pub async fn snapshot(folders: Vec<(String, std::path::PathBuf)>, fs_path: &Path) -> Snapshot {
    let (started, started_at) = *STARTED.get_or_init(|| (Instant::now(), SystemTime::now()));
    let status = read("/proc/self/status");
    let (container_bytes, container_limit_bytes) = container_memory();
    let fs_path = fs_path.to_owned();
    let (folders, filesystem) =
        tokio::task::spawn_blocking(move || (folder_sizes(&folders), filesystem(&fs_path)))
            .await
            .unwrap_or_default();

    Snapshot {
        started_at: DateTime::<Utc>::from(started_at).to_rfc3339_opts(SecondsFormat::Millis, true),
        uptime_seconds: started.elapsed().as_secs(),
        go_version: env!("RUWA_RUSTC_VERSION").to_owned(),
        cpu_seconds: read("/proc/self/stat").and_then(|s| parse_cpu_seconds(&s)),
        memory: Memory {
            heap_alloc_bytes: HEAP_BYTES.load(Ordering::Relaxed) as u64,
            runtime_sys_bytes: status
                .as_deref()
                .and_then(|s| status_kib(s, "VmSize:"))
                .unwrap_or_default(),
            process_rss_bytes: status.as_deref().and_then(|s| status_kib(s, "VmRSS:")),
            container_bytes,
            container_limit_bytes,
            goroutines: tokio::runtime::Handle::current()
                .metrics()
                .num_alive_tasks() as u64,
            gc_cycles: 0,
        },
        folders,
        filesystem,
    }
}

fn read(path: &str) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

/// `VmRSS:  1234 kB` → bytes.
fn status_kib(status: &str, key: &str) -> Option<u64> {
    let line = status.lines().find(|l| l.starts_with(key))?;
    let kib: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
    Some(kib * 1024)
}

/// User + system CPU time from `/proc/self/stat`. The command name may contain
/// spaces, so fields are counted after its closing `)`.
fn parse_cpu_seconds(stat: &str) -> Option<f64> {
    let rest = &stat[stat.rfind(')')? + 1..];
    let fields: Vec<&str> = rest.split_whitespace().collect();
    let user: f64 = fields.get(11)?.parse().ok()?;
    let system: f64 = fields.get(12)?.parse().ok()?;
    Some((user + system) / TICKS_PER_SECOND)
}

/// One cgroup value; `max` (no limit) and garbage are `None`.
fn parse_cgroup_value(text: &str) -> Option<u64> {
    let text = text.trim();
    if text == "max" {
        return None;
    }
    text.parse().ok()
}

/// cgroup v2, then v1. v1 reports "no limit" as a huge number.
fn container_memory() -> (Option<u64>, Option<u64>) {
    let value = |path: &str| read(path).and_then(|t| parse_cgroup_value(&t));
    if let Some(current) = value("/sys/fs/cgroup/memory.current") {
        return (Some(current), value("/sys/fs/cgroup/memory.max"));
    }
    let current = value("/sys/fs/cgroup/memory/memory.usage_in_bytes");
    let limit = value("/sys/fs/cgroup/memory/memory.limit_in_bytes").filter(|l| *l < 1 << 62);
    (current, limit)
}

fn folder_sizes(folders: &[(String, std::path::PathBuf)]) -> BTreeMap<String, Folder> {
    let mut cache = FOLDER_CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((at, sizes)) = cache.as_ref()
        && at.elapsed() < FOLDER_CACHE_FOR
    {
        return sizes.clone();
    }
    let sizes: BTreeMap<String, Folder> = folders
        .iter()
        .map(|(label, root)| (label.clone(), walk(root)))
        .collect();
    *cache = Some((Instant::now(), sizes.clone()));
    sizes
}

/// Best effort: unreadable entries are skipped and symlinks are counted by
/// their own size, never followed.
fn walk(root: &Path) -> Folder {
    let mut total = Folder { bytes: 0, files: 0 };
    let mut pending = vec![root.to_owned()];
    while let Some(dir) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() {
                pending.push(entry.path());
            } else if let Ok(meta) = entry.metadata() {
                total.bytes += meta.len();
                total.files += 1;
            }
        }
    }
    total
}

// The statvfs fields are u32 on some unixes (macOS) and u64 on Linux, so the casts
// are needed for portability even where clippy sees them as no-ops.
#[cfg(unix)]
#[allow(clippy::unnecessary_cast)]
fn filesystem(path: &Path) -> Filesystem {
    use std::{ffi::CString, mem::MaybeUninit, os::unix::ffi::OsStrExt};

    let Ok(path) = CString::new(path.as_os_str().as_bytes()) else {
        return Filesystem::default();
    };
    let mut stat = MaybeUninit::<libc::statvfs>::uninit();
    // SAFETY: `path` is a valid NUL-terminated string and `stat` is a valid
    // out-pointer; it is only read after statvfs reports success.
    if unsafe { libc::statvfs(path.as_ptr(), stat.as_mut_ptr()) } != 0 {
        return Filesystem::default();
    }
    let stat = unsafe { stat.assume_init() };
    let unit = stat.f_frsize as u64;
    Filesystem {
        total_bytes: Some(stat.f_blocks as u64 * unit),
        free_bytes: Some(stat.f_bavail as u64 * unit),
    }
}

#[cfg(not(unix))]
fn filesystem(_path: &Path) -> Filesystem {
    Filesystem::default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vm_fields_are_kib() {
        let status = "Name:\tx\nVmSize:\t  2048 kB\nVmRSS:\t   100 kB\n";
        assert_eq!(status_kib(status, "VmRSS:"), Some(100 * 1024));
        assert_eq!(status_kib(status, "VmSize:"), Some(2048 * 1024));
        assert_eq!(status_kib(status, "VmSwap:"), None);
    }

    #[test]
    fn cpu_time_survives_spaces_in_the_command_name() {
        // fields after ')': state ppid pgrp session tty tpgid flags minflt
        // cminflt majflt cmajflt utime stime ...
        let stat = "42 (my (odd) name) S 1 2 3 4 5 6 7 8 9 10 250 150 0 0";
        assert_eq!(parse_cpu_seconds(stat), Some(4.0));
        assert_eq!(parse_cpu_seconds("42 (x) S 1"), None);
        assert_eq!(parse_cpu_seconds("garbage"), None);
    }

    #[test]
    fn cgroup_values() {
        assert_eq!(parse_cgroup_value("1048576\n"), Some(1_048_576));
        assert_eq!(parse_cgroup_value("max\n"), None);
        assert_eq!(parse_cgroup_value("nope"), None);
    }

    #[test]
    fn walk_counts_files_and_ignores_missing_roots() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("a/b")).unwrap();
        std::fs::write(dir.path().join("one"), [0u8; 10]).unwrap();
        std::fs::write(dir.path().join("a/b/two"), [0u8; 5]).unwrap();
        assert_eq!(
            walk(dir.path()),
            Folder {
                bytes: 15,
                files: 2
            }
        );
        assert_eq!(
            walk(&dir.path().join("missing")),
            Folder { bytes: 0, files: 0 }
        );
    }

    #[cfg(unix)]
    #[test]
    fn filesystem_reports_the_volume() {
        let dir = tempfile::tempdir().unwrap();
        let fs = filesystem(dir.path());
        assert!(fs.total_bytes.unwrap() >= fs.free_bytes.unwrap());
        assert_eq!(
            filesystem(&dir.path().join("missing")),
            Filesystem::default()
        );
    }

    #[tokio::test]
    async fn snapshot_has_the_gowa_shape() {
        init();
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("f"), [0u8; 3]).unwrap();
        let snap = snapshot(vec![("media".into(), dir.path().to_owned())], dir.path()).await;
        let value = serde_json::to_value(&snap).unwrap();
        for key in [
            "started_at",
            "uptime_seconds",
            "go_version",
            "cpu_seconds",
            "memory",
            "folders",
            "filesystem",
        ] {
            assert!(value.get(key).is_some(), "missing {key}");
        }
        for key in [
            "heap_alloc_bytes",
            "runtime_sys_bytes",
            "process_rss_bytes",
            "container_bytes",
            "container_limit_bytes",
            "goroutines",
            "gc_cycles",
        ] {
            assert!(value["memory"].get(key).is_some(), "missing memory.{key}");
        }
        assert!(snap.started_at.ends_with('Z'));
    }
}
