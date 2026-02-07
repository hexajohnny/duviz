use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc::{self, Receiver, Sender},
    Arc,
};
use std::thread;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemKind {
    Dir,
    File,
    FilesAggregate,
}

#[derive(Debug, Clone)]
pub struct Item {
    pub name: String,
    pub path: PathBuf,
    pub size: u64,
    pub kind: ItemKind,
    pub count: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ViewMode {
    Dirs,
    Files,
}

#[derive(Debug)]
pub enum ScanMsg {
    Progress { scanned: u64, errors: u64 },
    Done { items: Vec<Item>, total: u64, errors: u64 },
    Error(String),
}

pub struct ScanHandle {
    pub cancel: Arc<AtomicBool>,
    pub rx: Receiver<ScanMsg>,
}

pub fn start_scan(path: PathBuf, view: ViewMode) -> ScanHandle {
    let (tx, rx) = mpsc::channel();
    let cancel = Arc::new(AtomicBool::new(false));
    let cancel_thread = cancel.clone();

    let tx_thread = tx.clone();
    thread::spawn(move || {
        let result = match view {
            ViewMode::Dirs => scan_dir_approx(&path, tx_thread, &cancel_thread),
            ViewMode::Files => scan_files_direct(&path, tx_thread, &cancel_thread),
        };
        if let Err(err) = result {
            let _ = tx.send(ScanMsg::Error(err));
        }
    });

    ScanHandle { cancel, rx }
}

fn scan_dir_approx(path: &Path, tx: Sender<ScanMsg>, cancel: &Arc<AtomicBool>) -> Result<(), String> {
    if is_proc_path(path) {
        return Err("/proc is excluded".to_string());
    }
    let base = path.to_path_buf();
    let base_canon = fs::canonicalize(&base).unwrap_or(base.clone());
    let mut items: Vec<Item> = Vec::new();
    let mut errors = 0u64;
    let mut scanned = 0u64;

    let read_dir = fs::read_dir(path).map_err(|e| format!("Failed to read dir: {}", e))?;

    let mut dir_names: HashMap<PathBuf, usize> = HashMap::new();
    let mut files_total = 0u64;
    let mut files_count = 0u64;

    for entry in read_dir {
        if cancel.load(Ordering::Relaxed) {
            return Ok(());
        }
        let entry = match entry {
            Ok(e) => e,
            Err(_) => {
                errors += 1;
                continue;
            }
        };
        let child_path = {
            let p = entry.path();
            if p.is_absolute() {
                p
            } else {
                base_canon.join(entry.file_name())
            }
        };
        if is_proc_path(&child_path) {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();

        let file_type = match entry.file_type() {
            Ok(t) => t,
            Err(_) => {
                errors += 1;
                continue;
            }
        };

        if file_type.is_symlink() {
            continue;
        }

        if file_type.is_file() {
            match entry.metadata() {
                Ok(m) => files_total = files_total.saturating_add(m.len()),
                Err(_) => errors += 1,
            }
            files_count += 1;
            scanned += 1;
            if scanned % 2000 == 0 {
                let _ = tx.send(ScanMsg::Progress { scanned, errors });
            }
            continue;
        }

        if file_type.is_dir() {
            let idx = items.len();
            items.push(Item {
                name,
                path: child_path.clone(),
                size: 0,
                kind: ItemKind::Dir,
                count: 0,
            });
            let key = normalize_path(&base_canon, &child_path);
            dir_names.insert(key, idx);
            scanned += 1;
            if scanned % 2000 == 0 {
                let _ = tx.send(ScanMsg::Progress { scanned, errors });
            }
        }
    }

    let files_label = format!("(Files: {})", files_count);
    items.push(Item {
        name: files_label,
        path: base_canon.clone(),
        size: files_total,
        kind: ItemKind::FilesAggregate,
        count: files_count,
    });

    if !dir_names.is_empty() {
        if cancel.load(Ordering::Relaxed) {
            return Ok(());
        }
        let dir_paths: Vec<PathBuf> = items
            .iter()
            .filter(|i| i.kind == ItemKind::Dir)
            .map(|i| i.path.clone())
            .collect();
        match du_sizes_parallel(&dir_paths, cancel) {
            Ok(batch_sizes) => {
                for (p, size) in batch_sizes {
                    let key = normalize_path(&base_canon, &p);
                    if let Some(idx) = dir_names.get(&key) {
                        if let Some(item) = items.get_mut(*idx) {
                            item.size = size;
                        }
                    }
                }
            }
            Err(_) => {
                errors += dir_names.len() as u64;
            }
        }
        let _ = tx.send(ScanMsg::Progress { scanned, errors });
    }

    let total: u64 = items.iter().map(|i| i.size).sum();
    items.sort_by(|a, b| b.size.cmp(&a.size));

    let _ = tx.send(ScanMsg::Done { items, total, errors });
    Ok(())
}

fn scan_files_direct(path: &Path, tx: Sender<ScanMsg>, cancel: &Arc<AtomicBool>) -> Result<(), String> {
    if is_proc_path(path) {
        return Err("/proc is excluded".to_string());
    }
    let base = path.to_path_buf();
    let base_canon = fs::canonicalize(&base).unwrap_or(base);
    let mut items: Vec<Item> = Vec::new();
    let mut errors = 0u64;
    let mut scanned = 0u64;

    let read_dir = fs::read_dir(path).map_err(|e| format!("Failed to read dir: {}", e))?;

    for entry in read_dir {
        if cancel.load(Ordering::Relaxed) {
            return Ok(());
        }
        let entry = match entry {
            Ok(e) => e,
            Err(_) => {
                errors += 1;
                continue;
            }
        };
        let child_path = {
            let p = entry.path();
            if p.is_absolute() {
                p
            } else {
                base_canon.join(entry.file_name())
            }
        };
        if is_proc_path(&child_path) {
            continue;
        }
        let file_type = match entry.file_type() {
            Ok(t) => t,
            Err(_) => {
                errors += 1;
                continue;
            }
        };
        if file_type.is_symlink() || file_type.is_dir() {
            continue;
        }
        let size = match entry.metadata() {
            Ok(m) => m.len(),
            Err(_) => {
                errors += 1;
                0
            }
        };
        let name = entry.file_name().to_string_lossy().to_string();
        items.push(Item {
            name,
            path: child_path,
            size,
            kind: ItemKind::File,
            count: 0,
        });
        scanned += 1;
        if scanned % 2000 == 0 {
            let _ = tx.send(ScanMsg::Progress { scanned, errors });
        }
    }

    let total: u64 = items.iter().map(|i| i.size).sum();
    items.sort_by(|a, b| b.size.cmp(&a.size));

    let _ = tx.send(ScanMsg::Done { items, total, errors });
    Ok(())
}

fn du_sizes_parallel(paths: &[PathBuf], cancel: &Arc<AtomicBool>) -> Result<Vec<(PathBuf, u64)>, String> {
    if paths.is_empty() {
        return Ok(Vec::new());
    }
    let workers = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(2)
        .min(8);
    let work = Arc::new(std::sync::Mutex::new(paths.to_vec()));
    let (tx, rx) = mpsc::channel();

    let mut handles = Vec::new();
    for _ in 0..workers {
        let work = Arc::clone(&work);
        let tx = tx.clone();
        let cancel = Arc::clone(cancel);
        handles.push(thread::spawn(move || {
            loop {
                if cancel.load(Ordering::Relaxed) {
                    break;
                }
                let next = {
                    let mut guard = work.lock().unwrap();
                    guard.pop()
                };
                let Some(path) = next else { break };
                let size = du_size_single(&path).unwrap_or(0);
                let _ = tx.send((path, size));
            }
        }));
    }
    drop(tx);

    let mut out = Vec::with_capacity(paths.len());
    for item in rx.iter() {
        out.push(item);
    }
    for h in handles {
        let _ = h.join();
    }
    Ok(out)
}

fn du_size_single(path: &Path) -> Result<u64, String> {
    let output = Command::new("du")
        .arg("-k")
        .arg("-x")
        .arg("--apparent-size")
        .arg("-s")
        .arg("--")
        .arg(path)
        .output()
        .map_err(|e| format!("du failed: {}", e))?;
    if !output.status.success() {
        return Err("du returned non-zero status".to_string());
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut parts = stdout.lines().next().unwrap_or("").splitn(2, '\t');
    let size_kb = parts.next().unwrap_or("0").trim();
    let size: u64 = size_kb.parse::<u64>().unwrap_or(0).saturating_mul(1024);
    Ok(size)
}

fn is_proc_path(path: &Path) -> bool {
    path.starts_with("/proc")
}

fn normalize_path(base: &Path, p: &Path) -> PathBuf {
    let joined = if p.is_absolute() {
        p.to_path_buf()
    } else {
        base.join(p)
    };
    fs::canonicalize(&joined).unwrap_or(joined)
}
