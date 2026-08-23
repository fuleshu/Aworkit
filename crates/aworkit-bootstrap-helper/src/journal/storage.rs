//! Durable storage behind the journal.
//!
//! [`JournalStorage`] hides the crash-safe file layout (create-new maintenance
//! lock, atomic same-volume publication, file and directory durability,
//! owner-only modes) so the journal logic is platform-neutral and hermetic.
//! [`FilesystemJournalStorage`] is the real implementation;
//! [`InMemoryJournalStorage`] is a test double that can inject torn writes and
//! dropped durability for fault-injection tests.

use std::{
    fs::{self, File, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

use aworkit_protocol::ManualRecoveryNoticeV1;
use fs2::FileExt;
use serde::Serialize;

use super::error::BootstrapJournalError;
use super::model::{JournalHeaderV1, JournalRecordV1, JournalSnapshotV1, TerminalReceiptV1};

/// Crash-safe persistence for one journal root.
pub trait JournalStorage: Send + Sync {
    /// Claims the single-flight maintenance lock.
    fn try_acquire_lock(&self) -> Result<(), BootstrapJournalError>;

    /// Whether the maintenance lock is currently present.
    fn is_locked(&self) -> bool;

    /// Releases the maintenance lock.
    fn release_lock(&self);

    /// Writes the durable header (create-once; the journal checks absence first).
    fn write_header(&self, header: &JournalHeaderV1) -> Result<(), BootstrapJournalError>;

    fn read_header(&self) -> Result<Option<JournalHeaderV1>, BootstrapJournalError>;

    /// Appends one record at its ordinal using create-new semantics.
    fn append_record(&self, record: &JournalRecordV1) -> Result<(), BootstrapJournalError>;

    /// Returns the longest parseable, ordinal-contiguous prefix of the chain.
    fn load_chain(&self) -> Result<Vec<JournalRecordV1>, BootstrapJournalError>;

    /// Replace-writes the compact snapshot after its source record is durable.
    fn write_snapshot(&self, snapshot: &JournalSnapshotV1) -> Result<(), BootstrapJournalError>;

    fn read_snapshot(&self) -> Result<Option<JournalSnapshotV1>, BootstrapJournalError>;

    /// Seals the terminal receipt; fails if one is already durable.
    fn seal_receipt(&self, receipt: &TerminalReceiptV1) -> Result<(), BootstrapJournalError>;

    fn read_receipt(&self) -> Result<Option<TerminalReceiptV1>, BootstrapJournalError>;

    /// Writes the optional manual-recovery notice (at most one).
    fn write_notice(&self, notice: &ManualRecoveryNoticeV1) -> Result<(), BootstrapJournalError>;

    fn read_notice(&self) -> Result<Option<ManualRecoveryNoticeV1>, BootstrapJournalError>;

    /// Clears a journal root so a fresh transaction can start.
    fn reset(&self) -> Result<(), BootstrapJournalError>;
}

/// Clears a journal root and its record directory.
fn clear_root(root: &Path) -> io::Result<()> {
    for name in [
        "HEADER.json",
        "SNAPSHOT.json",
        "RECEIPT.json",
        "NOTICE.json",
    ] {
        let _ = fs::remove_file(root.join(name));
    }
    let _ = fs::remove_dir_all(root.join("records"));
    sync_directory(root)?;
    Ok(())
}

fn encode<T: Serialize>(value: &T) -> Result<Vec<u8>, BootstrapJournalError> {
    serde_json::to_vec(value).map_err(|_| BootstrapJournalError::Encoding)
}

fn read_opt(path: &Path) -> io::Result<Option<Vec<u8>>> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

/// Durably persists bytes under the owner-only journal root on one volume.
#[derive(Debug)]
pub struct FilesystemJournalStorage {
    root: PathBuf,
    lock_file: Mutex<Option<File>>,
}

impl FilesystemJournalStorage {
    /// Opens (creating if needed) the helper-controlled journal root.
    pub fn open(root: impl Into<PathBuf>) -> Result<Self, BootstrapJournalError> {
        let root = root.into();
        fs::create_dir_all(&root)?;
        // Owner-only directory on unix; the native hardened port (M12) adds
        // anchored no-follow identity and ACLs.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = fs::metadata(&root)?.permissions();
            perms.set_mode(0o700);
            fs::set_permissions(&root, perms)?;
        }
        Ok(Self {
            root,
            lock_file: Mutex::new(None),
        })
    }

    /// The journal root directory.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    fn records_dir(&self) -> PathBuf {
        self.root.join("records")
    }
}

impl JournalStorage for FilesystemJournalStorage {
    fn try_acquire_lock(&self) -> Result<(), BootstrapJournalError> {
        let lock = self.root.join("LOCK");
        let file = owner_only_options()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&lock)?;
        file.try_lock_exclusive().map_err(|error| {
            if error.kind() == io::ErrorKind::WouldBlock {
                BootstrapJournalError::Busy
            } else {
                BootstrapJournalError::Io(error)
            }
        })?;
        sync_directory(&self.root)?;
        *self.lock_file.lock().expect("journal lock-file mutex") = Some(file);
        Ok(())
    }

    fn is_locked(&self) -> bool {
        self.lock_file
            .lock()
            .expect("journal lock-file mutex")
            .is_some()
    }

    fn release_lock(&self) {
        if let Some(file) = self
            .lock_file
            .lock()
            .expect("journal lock-file mutex")
            .take()
        {
            let _ = FileExt::unlock(&file);
        }
    }

    fn write_header(&self, header: &JournalHeaderV1) -> Result<(), BootstrapJournalError> {
        let path = self.root.join("HEADER.json");
        let bytes = encode(header)?;
        atomic_create_once(&path, &bytes, BootstrapJournalError::IdentityConflict)
    }

    fn read_header(&self) -> Result<Option<JournalHeaderV1>, BootstrapJournalError> {
        let path = self.root.join("HEADER.json");
        let Some(bytes) = read_opt(&path)? else {
            return Ok(None);
        };
        serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|_| BootstrapJournalError::HeaderCorrupt)
    }

    fn append_record(&self, record: &JournalRecordV1) -> Result<(), BootstrapJournalError> {
        let dir = self.records_dir();
        fs::create_dir_all(&dir)?;
        let path = dir.join(format!("{:010}.json", record.ordinal));
        let bytes = encode(record)?;
        atomic_create_once(
            &path,
            &bytes,
            BootstrapJournalError::Invalid("record ordinal is already durable"),
        )
    }

    fn load_chain(&self) -> Result<Vec<JournalRecordV1>, BootstrapJournalError> {
        let dir = self.records_dir();
        let entries = match fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error.into()),
        };
        let mut ordinals = Vec::new();
        for entry in entries {
            let entry = entry?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            if let Some(stem) = name.strip_suffix(".json") {
                if stem.len() == 10 && stem.bytes().all(|byte| byte.is_ascii_digit()) {
                    if let Ok(ordinal) = stem.parse::<u64>() {
                        ordinals.push(ordinal);
                    }
                }
            } else if name.ends_with(".tmp") {
                // An unpublished temporary file has no durable meaning.
                fs::remove_file(entry.path())?;
            }
        }
        ordinals.sort_unstable();
        ordinals.dedup();
        let mut records = Vec::new();
        for (index, ordinal) in ordinals.iter().copied().enumerate() {
            let expected = u64::try_from(index).expect("journal index fits in u64");
            if ordinal != expected {
                return Err(BootstrapJournalError::ChainBroken { ordinal: expected });
            }
            let path = dir.join(format!("{ordinal:010}.json"));
            let bytes = fs::read(&path)?;
            match serde_json::from_slice::<JournalRecordV1>(&bytes) {
                Ok(record) if record.ordinal == ordinal => records.push(record),
                // Only the final, unparsable file can be an uncommitted torn
                // tail. A malformed middle record is durable corruption.
                _ if index + 1 == ordinals.len() => {
                    fs::remove_file(&path)?;
                    sync_directory(&dir)?;
                    break;
                }
                _ => return Err(BootstrapJournalError::ChainBroken { ordinal }),
            }
        }
        Ok(records)
    }

    fn write_snapshot(&self, snapshot: &JournalSnapshotV1) -> Result<(), BootstrapJournalError> {
        let target = self.root.join("SNAPSHOT.json");
        let temporary = self.root.join(".SNAPSHOT.json.tmp");
        let bytes = encode(snapshot)?;
        let mut file = owner_only_options()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&temporary)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        fs::rename(&temporary, &target)?;
        sync_directory(&self.root)?;
        Ok(())
    }

    fn read_snapshot(&self) -> Result<Option<JournalSnapshotV1>, BootstrapJournalError> {
        let path = self.root.join("SNAPSHOT.json");
        let Some(bytes) = read_opt(&path)? else {
            return Ok(None);
        };
        serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|_| BootstrapJournalError::Invalid("snapshot is corrupt"))
    }

    fn seal_receipt(&self, receipt: &TerminalReceiptV1) -> Result<(), BootstrapJournalError> {
        let path = self.root.join("RECEIPT.json");
        let bytes = encode(receipt)?;
        atomic_create_once(&path, &bytes, BootstrapJournalError::TerminalSealed)
    }

    fn read_receipt(&self) -> Result<Option<TerminalReceiptV1>, BootstrapJournalError> {
        let path = self.root.join("RECEIPT.json");
        let Some(bytes) = read_opt(&path)? else {
            return Ok(None);
        };
        serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|_| BootstrapJournalError::Invalid("receipt is corrupt"))
    }

    fn write_notice(&self, notice: &ManualRecoveryNoticeV1) -> Result<(), BootstrapJournalError> {
        let path = self.root.join("NOTICE.json");
        let bytes = encode(notice)?;
        if path.exists() {
            // Idempotent if the same notice is rewritten verbatim.
            let existing = self.read_notice()?;
            if existing.as_ref() == Some(notice) {
                return Ok(());
            }
            return Err(BootstrapJournalError::Invalid(
                "a manual-recovery notice is already durable",
            ));
        }
        atomic_create_once(
            &path,
            &bytes,
            BootstrapJournalError::Invalid("a manual-recovery notice is already durable"),
        )
    }

    fn read_notice(&self) -> Result<Option<ManualRecoveryNoticeV1>, BootstrapJournalError> {
        let path = self.root.join("NOTICE.json");
        let Some(bytes) = read_opt(&path)? else {
            return Ok(None);
        };
        serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|_| BootstrapJournalError::Invalid("manual-recovery notice is corrupt"))
    }

    fn reset(&self) -> Result<(), BootstrapJournalError> {
        clear_root(&self.root).map_err(BootstrapJournalError::Io)?;
        Ok(())
    }
}

fn sync_directory(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        File::open(path)?.sync_all()
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Ok(())
    }
}

fn owner_only_options() -> OpenOptions {
    let mut options = OpenOptions::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
}

/// Publishes an immutable file through a same-directory, owner-only temporary
/// file. The maintenance lock serializes the absence check and rename.
fn atomic_create_once(
    target: &Path,
    bytes: &[u8],
    already_exists: BootstrapJournalError,
) -> Result<(), BootstrapJournalError> {
    if target.exists() {
        return Err(already_exists);
    }
    let parent = target
        .parent()
        .ok_or(BootstrapJournalError::Invalid("journal path has no parent"))?;
    let name =
        target
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or(BootstrapJournalError::Invalid(
                "journal filename is invalid",
            ))?;
    let temporary = parent.join(format!(".{name}.tmp"));
    match fs::remove_file(&temporary) {
        Ok(()) => sync_directory(parent)?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    let mut file = owner_only_options()
        .create_new(true)
        .write(true)
        .open(&temporary)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    if target.exists() {
        fs::remove_file(&temporary)?;
        return Err(already_exists);
    }
    fs::rename(&temporary, target)?;
    sync_directory(parent)?;
    Ok(())
}

/// Hermetic in-memory journal storage for unit and fault-injection tests.
#[derive(Debug, Default)]
pub struct InMemoryJournalStorage {
    state: Mutex<InMemoryState>,
    /// When set, the next append is not committed and fails, simulating a crash
    /// before the record became durable.
    pub fail_next_append: AtomicBool,
    /// When set, the next appended record is committed with a wrong hash so the
    /// chain verifies as broken on the next read.
    pub corrupt_next_record_hash: AtomicBool,
}

#[derive(Debug, Default)]
struct InMemoryState {
    lock: bool,
    header: Option<JournalHeaderV1>,
    records: Vec<JournalRecordV1>,
    snapshot: Option<JournalSnapshotV1>,
    receipt: Option<TerminalReceiptV1>,
    notice: Option<ManualRecoveryNoticeV1>,
}

impl InMemoryJournalStorage {
    /// A copy of the committed records, in ordinal order.
    #[must_use]
    pub fn records(&self) -> Vec<JournalRecordV1> {
        self.state
            .lock()
            .expect("journal storage lock")
            .records
            .clone()
    }

    /// The number of committed records.
    #[must_use]
    pub fn record_count(&self) -> u64 {
        u64::try_from(
            self.state
                .lock()
                .expect("journal storage lock")
                .records
                .len(),
        )
        .expect("in-memory record count fits in u64")
    }
}

impl JournalStorage for InMemoryJournalStorage {
    fn try_acquire_lock(&self) -> Result<(), BootstrapJournalError> {
        let mut state = self.state.lock().expect("journal storage lock");
        if state.lock {
            // A terminal receipt makes the prior lock stale.
            if state.receipt.is_some() {
                state.lock = true;
                return Ok(());
            }
            return Err(BootstrapJournalError::Busy);
        }
        state.lock = true;
        Ok(())
    }

    fn is_locked(&self) -> bool {
        self.state.lock().expect("journal storage lock").lock
    }

    fn release_lock(&self) {
        self.state.lock().expect("journal storage lock").lock = false;
    }

    fn write_header(&self, header: &JournalHeaderV1) -> Result<(), BootstrapJournalError> {
        let mut state = self.state.lock().expect("journal storage lock");
        if state.header.is_some() {
            return Err(BootstrapJournalError::IdentityConflict);
        }
        state.header = Some(header.clone());
        Ok(())
    }

    fn read_header(&self) -> Result<Option<JournalHeaderV1>, BootstrapJournalError> {
        Ok(self
            .state
            .lock()
            .expect("journal storage lock")
            .header
            .clone())
    }

    fn append_record(&self, record: &JournalRecordV1) -> Result<(), BootstrapJournalError> {
        if self.fail_next_append.swap(false, Ordering::SeqCst) {
            return Err(BootstrapJournalError::Io(io::Error::new(
                io::ErrorKind::Other,
                "injected durability failure",
            )));
        }
        let mut state = self.state.lock().expect("journal storage lock");
        let mut record = record.clone();
        if self.corrupt_next_record_hash.swap(false, Ordering::SeqCst) {
            record.record_hash = "sha256:".to_string() + "f".repeat(64).as_str();
        }
        if state.records.len() > i64::MAX as usize {
            return Err(BootstrapJournalError::RecordCapExceeded);
        }
        if record.ordinal
            != u64::try_from(state.records.len()).expect("in-memory record count fits in u64")
        {
            return Err(BootstrapJournalError::Invalid(
                "record ordinal is not the next durable ordinal",
            ));
        }
        state.records.push(record);
        Ok(())
    }

    fn load_chain(&self) -> Result<Vec<JournalRecordV1>, BootstrapJournalError> {
        Ok(self
            .state
            .lock()
            .expect("journal storage lock")
            .records
            .clone())
    }

    fn write_snapshot(&self, snapshot: &JournalSnapshotV1) -> Result<(), BootstrapJournalError> {
        self.state.lock().expect("journal storage lock").snapshot = Some(snapshot.clone());
        Ok(())
    }

    fn read_snapshot(&self) -> Result<Option<JournalSnapshotV1>, BootstrapJournalError> {
        Ok(self
            .state
            .lock()
            .expect("journal storage lock")
            .snapshot
            .clone())
    }

    fn seal_receipt(&self, receipt: &TerminalReceiptV1) -> Result<(), BootstrapJournalError> {
        let mut state = self.state.lock().expect("journal storage lock");
        if state.receipt.is_some() {
            return Err(BootstrapJournalError::TerminalSealed);
        }
        state.receipt = Some(receipt.clone());
        Ok(())
    }

    fn read_receipt(&self) -> Result<Option<TerminalReceiptV1>, BootstrapJournalError> {
        Ok(self
            .state
            .lock()
            .expect("journal storage lock")
            .receipt
            .clone())
    }

    fn write_notice(&self, notice: &ManualRecoveryNoticeV1) -> Result<(), BootstrapJournalError> {
        let mut state = self.state.lock().expect("journal storage lock");
        match state.notice.as_ref() {
            Some(existing) if existing == notice => Ok(()),
            Some(_) => Err(BootstrapJournalError::Invalid(
                "a manual-recovery notice is already durable",
            )),
            None => {
                state.notice = Some(notice.clone());
                Ok(())
            }
        }
    }

    fn read_notice(&self) -> Result<Option<ManualRecoveryNoticeV1>, BootstrapJournalError> {
        Ok(self
            .state
            .lock()
            .expect("journal storage lock")
            .notice
            .clone())
    }

    fn reset(&self) -> Result<(), BootstrapJournalError> {
        *self.state.lock().expect("journal storage lock") = InMemoryState::default();
        Ok(())
    }
}

/// Shared owner handle so the journal and its tests can hold one instance.
pub type ArcJournalStorage = Arc<dyn JournalStorage>;
