use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, Weak};

use crate::PersistenceError;

use super::PersistedNodeState;
use super::codec::{CHECKSUM_SIZE, MAX_SNAPSHOT_FILE_SIZE, decode_snapshot};

type PathLockMap = BTreeMap<PathBuf, Weak<Mutex<()>>>;

pub(super) fn shared_path_lock(path: &Path) -> Arc<Mutex<()>> {
    static LOCKS: OnceLock<Mutex<PathLockMap>> = OnceLock::new();

    let mut locks = LOCKS
        .get_or_init(|| Mutex::new(BTreeMap::new()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());

    if let Some(existing) = locks.get(path).and_then(Weak::upgrade) {
        return existing;
    }

    locks.retain(|_, lock| lock.strong_count() > 0);
    let lock = Arc::new(Mutex::new(()));
    locks.insert(path.to_path_buf(), Arc::downgrade(&lock));
    lock
}

pub(super) fn lock_store_file(base_path: &Path) -> Result<File, PersistenceError> {
    ensure_parent_dir(base_path)?;

    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(base_path)
        .map_err(PersistenceError::from_io)?;
    File::lock(&file).map_err(PersistenceError::from_io)?;
    Ok(file)
}

pub(super) fn slot_path(base_path: &Path, generation: u64) -> PathBuf {
    if generation % 2 == 1 {
        append_suffix(base_path, ".a")
    } else {
        append_suffix(base_path, ".b")
    }
}

pub(super) fn remove_slots(base_path: &Path) -> Result<(), PersistenceError> {
    for path in [
        append_suffix(base_path, ".a"),
        append_suffix(base_path, ".b"),
    ] {
        match fs::remove_file(path) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(PersistenceError::from_io(error)),
        }
    }
    Ok(())
}

struct ValidSlot {
    snapshot: PersistedNodeState,
    checksum: [u8; CHECKSUM_SIZE],
}

pub(super) fn load_latest(
    base_path: &Path,
) -> Result<Option<PersistedNodeState>, PersistenceError> {
    let mut any_file_exists = false;
    let mut valid = Vec::new();

    for path in [
        append_suffix(base_path, ".a"),
        append_suffix(base_path, ".b"),
    ] {
        match read_slot(&path) {
            Ok(Some(snapshot)) => {
                any_file_exists = true;
                valid.push(snapshot);
            }
            Ok(None) => {}
            Err(PersistenceError::Io(io::ErrorKind::NotFound)) => {}
            Err(PersistenceError::Io(kind)) => return Err(PersistenceError::Io(kind)),
            Err(_) => {
                any_file_exists = true;
            }
        }
    }

    if valid.len() == 2 && valid[0].snapshot.generation == valid[1].snapshot.generation {
        let generation = valid[0].snapshot.generation;
        if valid[0].checksum != valid[1].checksum {
            return Err(PersistenceError::ConflictingSnapshotGeneration(generation));
        }
    }

    if let Some(latest) = valid
        .into_iter()
        .max_by_key(|slot| slot.snapshot.generation)
    {
        return Ok(Some(latest.snapshot));
    }

    if any_file_exists {
        Err(PersistenceError::NoValidSnapshot)
    } else {
        Ok(None)
    }
}

pub(super) fn write_slots(
    base_path: &Path,
    generation: u64,
    bytes: &[u8],
) -> Result<(), PersistenceError> {
    ensure_parent_dir(base_path)?;

    write_snapshot_file(&slot_path(base_path, generation), bytes)?;
    write_snapshot_file(&mirror_slot_path(base_path, generation), bytes)
}

fn write_snapshot_file(path: &Path, bytes: &[u8]) -> Result<(), PersistenceError> {
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(path)
        .map_err(PersistenceError::from_io)?;
    file.write_all(bytes).map_err(PersistenceError::from_io)?;
    file.sync_all().map_err(PersistenceError::from_io)?;
    Ok(())
}

fn mirror_slot_path(base_path: &Path, generation: u64) -> PathBuf {
    if generation % 2 == 1 {
        append_suffix(base_path, ".b")
    } else {
        append_suffix(base_path, ".a")
    }
}

fn ensure_parent_dir(path: &Path) -> Result<(), PersistenceError> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent).map_err(PersistenceError::from_io)?;
    }
    Ok(())
}

fn append_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut value: OsString = path.as_os_str().to_owned();
    value.push(suffix);
    PathBuf::from(value)
}

fn read_slot(path: &Path) -> Result<Option<ValidSlot>, PersistenceError> {
    let mut file = File::open(path).map_err(PersistenceError::from_io)?;
    let file_len = file.metadata().map_err(PersistenceError::from_io)?.len();
    if file_len > MAX_SNAPSHOT_FILE_SIZE {
        return Err(PersistenceError::SnapshotTooLarge);
    }

    let capacity = usize::try_from(file_len).map_err(|_| PersistenceError::SnapshotTooLarge)?;
    let mut bytes = Vec::with_capacity(capacity);
    file.read_to_end(&mut bytes)
        .map_err(PersistenceError::from_io)?;

    let snapshot = decode_snapshot(&bytes)?;
    let checksum = bytes
        .get(bytes.len().saturating_sub(CHECKSUM_SIZE)..)
        .and_then(|checksum| checksum.try_into().ok())
        .ok_or(PersistenceError::InvalidSnapshot)?;

    Ok(Some(ValidSlot { snapshot, checksum }))
}

#[cfg(test)]
mod tests {
    use std::process::Command;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::mpsc;
    use std::thread;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use crate::StateStore;

    use super::*;

    #[test]
    #[ignore = "subprocess helper for cross-process store locking"]
    fn cross_process_store_lock_helper() {
        let Some(base_path) = std::env::var_os("SECOND_TEST_STORE_LOCK_PATH") else {
            return;
        };
        let ready_path = PathBuf::from(
            std::env::var_os("SECOND_TEST_STORE_LOCK_READY")
                .expect("lock helper requires ready path"),
        );
        let release_path = PathBuf::from(
            std::env::var_os("SECOND_TEST_STORE_LOCK_RELEASE")
                .expect("lock helper requires release path"),
        );

        let file = lock_store_file(Path::new(&base_path)).expect("helper must acquire store lock");
        fs::write(&ready_path, b"ready").expect("helper must publish readiness");

        while !release_path.exists() {
            thread::sleep(Duration::from_millis(10));
        }

        File::unlock(&file).expect("helper must release store lock");
    }

    #[test]
    fn state_store_lock_is_exclusive_across_processes() {
        static NEXT_ID: AtomicU64 = AtomicU64::new(0);

        let unique = format!(
            "second-cross-process-lock-{}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system clock must be after unix epoch")
                .as_nanos(),
            NEXT_ID.fetch_add(1, Ordering::Relaxed),
        );
        let base_path = std::env::temp_dir().join(unique);
        let ready_path = base_path.with_extension("ready");
        let release_path = base_path.with_extension("release");

        let mut child = Command::new(std::env::current_exe().expect("test executable must exist"))
            .arg("--exact")
            .arg("persistence::slot::tests::cross_process_store_lock_helper")
            .arg("--ignored")
            .env("SECOND_TEST_STORE_LOCK_PATH", &base_path)
            .env("SECOND_TEST_STORE_LOCK_READY", &ready_path)
            .env("SECOND_TEST_STORE_LOCK_RELEASE", &release_path)
            .spawn()
            .expect("lock helper process must start");

        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !ready_path.exists() {
            assert!(
                std::time::Instant::now() < deadline,
                "lock helper did not acquire the store lock"
            );
            thread::sleep(Duration::from_millis(10));
        }

        let store = StateStore::new(&base_path);
        let (sender, receiver) = mpsc::channel();
        let loader = thread::spawn(move || {
            sender
                .send(store.load())
                .expect("receiver must remain alive");
        });

        assert!(
            receiver.recv_timeout(Duration::from_millis(200)).is_err(),
            "StateStore::load must wait while another process owns the store lock"
        );

        fs::write(&release_path, b"release").expect("parent must release helper");
        assert!(child.wait().expect("helper wait must succeed").success());
        assert!(
            receiver
                .recv_timeout(Duration::from_secs(5))
                .expect("load must complete after cross-process lock release")
                .expect("load must succeed")
                .is_none()
        );
        loader.join().expect("loader thread must exit");

        for path in [&base_path, &ready_path, &release_path] {
            match fs::remove_file(path) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => panic!("failed to clean test file {}: {error}", path.display()),
            }
        }
    }
}
