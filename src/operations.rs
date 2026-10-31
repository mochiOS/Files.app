//! Cooperative filesystem jobs: bounded copy chunks and one directory entry per step.
//! A transfer is staged beside its destination; cancellation never publishes a partial copy.
use std::fs::{self, File, OpenOptions, ReadDir};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    Copy,
    Move,
    Delete,
}
#[derive(Clone, Copy)]
pub(crate) enum Collision {
    Replace,
    KeepBoth,
    Skip,
}
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Outcome {
    Completed(Option<PathBuf>),
    Cancelled,
    Skipped,
}

enum Work {
    Copy(PathBuf, PathBuf),
    Directory(ReadDir, PathBuf),
    Bytes(File, File),
    Delete(PathBuf),
    DeleteDirectory(ReadDir, PathBuf),
    RemoveDirectory(PathBuf),
    SyncDirectory(PathBuf),
}
#[derive(Clone, Copy)]
enum Phase {
    Copying,
    Committing,
    Cleaning,
    Cancelling,
    Deleting,
}

pub(crate) struct Operation {
    pub(crate) kind: Kind,
    pub(crate) bytes: u64,
    pub(crate) items: u64,
    pub(crate) source: PathBuf,
    destination: Option<PathBuf>,
    stage: Option<PathBuf>,
    backup: Option<PathBuf>,
    work: Vec<Work>,
    phase: Phase,
    conflict: bool,
    replace: bool,
    result: Option<Result<Outcome, String>>,
    failure: Option<String>,
    skipped: bool,
    copied_move: bool,
}

fn exists(path: &Path) -> io::Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e),
    }
}

impl Operation {
    pub(crate) fn new(kind: Kind, source: PathBuf, directory: &Path) -> io::Result<Self> {
        let metadata = fs::symlink_metadata(&source)?;
        let mut operation = Self {
            kind,
            bytes: 0,
            items: 0,
            source: source.clone(),
            destination: None,
            stage: None,
            backup: None,
            work: Vec::new(),
            phase: Phase::Deleting,
            conflict: false,
            replace: false,
            result: None,
            failure: None,
            skipped: false,
            copied_move: false,
        };
        if kind == Kind::Delete {
            operation.work.push(Work::Delete(source));
            return Ok(operation);
        }
        let directory = fs::canonicalize(directory)?;
        let name = source
            .file_name()
            .ok_or_else(|| io::Error::other("Cannot transfer a filesystem root"))?;
        let destination = directory.join(name);
        let canonical_source = fs::canonicalize(&source)?;
        if metadata.is_dir() && directory.starts_with(&canonical_source) {
            return Err(io::Error::other("A folder cannot be placed inside itself"));
        }
        if kind == Kind::Move && destination == canonical_source {
            return Err(io::Error::other("The item is already in this folder"));
        }
        operation.conflict = exists(&destination)?;
        operation.destination = Some(destination);
        operation.phase = Phase::Copying;
        Ok(operation)
    }

    pub(crate) fn conflict(&self) -> Option<&Path> {
        self.conflict.then(|| self.destination.as_deref()).flatten()
    }
    pub(crate) fn cancellable(&self) -> bool {
        self.result.is_none()
            && (self.conflict || matches!(self.phase, Phase::Copying | Phase::Deleting))
    }
    pub(crate) fn label(&self) -> &'static str {
        match self.phase {
            Phase::Cancelling => "Cleaning up",
            Phase::Cleaning | Phase::Committing => "Finishing",
            _ => match self.kind {
                Kind::Copy => "Copying",
                Kind::Move => "Moving",
                Kind::Delete => "Deleting",
            },
        }
    }
    pub(crate) fn take_result(&mut self) -> Option<Result<Outcome, String>> {
        self.result.take()
    }

    pub(crate) fn resolve(&mut self, choice: Collision) -> io::Result<()> {
        if !self.conflict {
            return Ok(());
        }
        match choice {
            Collision::Skip => {
                self.cancel();
                self.skipped = true;
            }
            Collision::Replace => {
                let destination = self.destination.as_ref().unwrap();
                let target = fs::canonicalize(destination)?;
                let source = fs::canonicalize(&self.source)?;
                if source == target || source.starts_with(&target) {
                    return Err(io::Error::other(
                        "Cannot replace the source or a containing folder; choose Keep Both",
                    ));
                }
                self.replace = true;
            }
            Collision::KeepBoth => {
                let target = self.destination.as_ref().unwrap();
                let parent = target.parent().unwrap();
                let stem = target.file_stem().unwrap_or_default().to_string_lossy();
                let extension = target
                    .extension()
                    .map(|e| format!(".{}", e.to_string_lossy()))
                    .unwrap_or_default();
                for index in 2..u32::MAX {
                    let candidate = parent.join(format!("{stem} {index}{extension}"));
                    if !exists(&candidate)? {
                        self.destination = Some(candidate);
                        break;
                    }
                }
            }
        }
        self.conflict = false;
        Ok(())
    }

    pub(crate) fn cancel(&mut self) {
        if !self.cancellable() {
            return;
        }
        self.conflict = false;
        self.work.clear(); // Close open file handles before removing staging files.
        if let Some(stage) = self.stage.take() {
            self.work.push(Work::Delete(stage));
        }
        self.phase = Phase::Cancelling;
    }

    pub(crate) fn tick(&mut self) {
        if self.result.is_some() || self.conflict {
            return;
        }
        let start = Instant::now();
        for _ in 0..32 {
            if let Err(error) = self.step() {
                // Keep source and previous destination on errors. Remove staging cooperatively.
                let detail = format!("{}: {error}", self.source.display());
                self.work.clear();
                if self.failure.is_some()
                    || self.backup.is_some()
                    || (self.copied_move
                        && matches!(self.phase, Phase::Committing | Phase::Cleaning))
                {
                    self.result = Some(Err(format!(
                        "{detail}. Recovery data: {}. Destination: {}. Completed deletions cannot be undone",
                        self.stage.as_deref().unwrap_or(&self.source).display(),
                        self.destination
                            .as_deref()
                            .unwrap_or(&self.source)
                            .display()
                    )));
                } else {
                    self.failure = Some(detail);
                    if let Some(stage) = self.stage.take() {
                        self.work.push(Work::Delete(stage));
                    }
                    self.phase = Phase::Cancelling;
                }
                break;
            }
            if self.result.is_some() || start.elapsed() >= Duration::from_millis(3) {
                break;
            }
        }
    }

    fn step(&mut self) -> io::Result<()> {
        if let Some(work) = self.work.pop() {
            match work {
                Work::Copy(source, target) => {
                    let metadata = fs::symlink_metadata(&source)?;
                    if metadata.file_type().is_symlink() {
                        return Err(io::Error::other(
                            "Copying symbolic links is not supported yet",
                        ));
                    }
                    if metadata.is_dir() {
                        fs::create_dir(&target)?;
                        self.items += 1;
                        self.work
                            .push(Work::Directory(fs::read_dir(source)?, target));
                    } else if metadata.is_file() {
                        let input = File::open(source)?;
                        let output = OpenOptions::new()
                            .write(true)
                            .create_new(true)
                            .open(&target)?;
                        self.work.push(Work::Bytes(input, output));
                    } else {
                        return Err(io::Error::other(
                            "Only regular files and folders can be copied",
                        ));
                    }
                }
                Work::Directory(mut entries, target) => {
                    if let Some(entry) = entries.next() {
                        let entry = entry?;
                        let child = target.join(entry.file_name());
                        self.work.push(Work::Directory(entries, target));
                        self.work.push(Work::Copy(entry.path(), child));
                    } else if self.copied_move {
                        self.work.push(Work::SyncDirectory(target));
                    }
                }
                Work::Bytes(mut input, mut output) => {
                    let mut buffer = [0u8; 64 * 1024];
                    let count = input.read(&mut buffer)?;
                    if count == 0 {
                        output.flush()?;
                        output.set_permissions(input.metadata()?.permissions())?;
                        if self.copied_move {
                            output.sync_all()?;
                        }
                        self.items += 1;
                    } else {
                        output.write_all(&buffer[..count])?;
                        self.bytes += count as u64;
                        self.work.push(Work::Bytes(input, output));
                    }
                }
                Work::Delete(path) => {
                    let metadata = fs::symlink_metadata(&path)?;
                    if metadata.is_dir() && !metadata.file_type().is_symlink() {
                        self.work
                            .push(Work::DeleteDirectory(fs::read_dir(&path)?, path));
                    } else {
                        fs::remove_file(path)?;
                        self.items += 1;
                    }
                }
                Work::DeleteDirectory(mut entries, path) => {
                    if let Some(entry) = entries.next() {
                        let entry = entry?;
                        self.work.push(Work::DeleteDirectory(entries, path));
                        self.work.push(Work::Delete(entry.path()));
                    } else {
                        self.work.push(Work::RemoveDirectory(path));
                    }
                }
                Work::SyncDirectory(path) => File::open(path)?.sync_all()?,
                Work::RemoveDirectory(path) => {
                    fs::remove_dir(path)?;
                    self.items += 1;
                }
            }
            return Ok(());
        }
        match self.phase {
            Phase::Copying => {
                if self.stage.is_none() {
                    let parent = self.destination.as_ref().unwrap().parent().unwrap();
                    // Reserve a private directory atomically, rather than racing on an unused name.
                    for index in 0..u32::MAX {
                        let stage =
                            parent.join(format!(".files-transfer-{}-{index}", std::process::id()));
                        match fs::create_dir(&stage) {
                            Ok(()) => {
                                if self.kind == Kind::Copy {
                                    self.work
                                        .push(Work::Copy(self.source.clone(), stage.join("item")));
                                } else {
                                    self.phase = Phase::Committing;
                                }
                                self.stage = Some(stage);
                                return Ok(());
                            }
                            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                            Err(e) => return Err(e),
                        }
                    }
                }
                self.phase = Phase::Committing;
            }
            Phase::Committing => {
                let target = self.destination.as_ref().unwrap();
                let stage = self.stage.as_ref().unwrap();
                if self.copied_move {
                    File::open(stage)?.sync_all()?;
                }
                if exists(target)? {
                    if !self.replace {
                        self.conflict = true;
                        return Ok(());
                    }
                    // Keep the old item until the staged replacement has been published.
                    let backup = stage.join("previous");
                    fs::rename(target, &backup)?;
                    self.backup = Some(backup);
                    if self.copied_move {
                        File::open(stage)?.sync_all()?;
                    }
                }
                let source = if self.kind == Kind::Move && !self.copied_move {
                    self.source.clone()
                } else {
                    stage.join("item")
                };
                if let Err(error) = fs::rename(source, target) {
                    if let Some(backup) = &self.backup {
                        fs::rename(backup, target)?;
                        self.backup = None;
                    }
                    if self.kind == Kind::Move
                        && !self.copied_move
                        && error.kind() == io::ErrorKind::CrossesDevices
                    {
                        self.copied_move = true;
                        self.phase = Phase::Copying;
                        self.work
                            .push(Work::Copy(self.source.clone(), stage.join("item")));
                        return Ok(());
                    }
                    return Err(error);
                }
                if self.copied_move {
                    // Never remove the source until all copied bytes and the
                    // published directory entry have been synced successfully.
                    File::open(target.parent().unwrap())?.sync_all()?;
                    self.work.push(Work::Delete(self.source.clone()));
                }
                if let Some(backup) = self.backup.take() {
                    self.work.push(Work::Delete(backup));
                }
                self.phase = Phase::Cleaning;
            }
            Phase::Cleaning => {
                if let Some(stage) = self.stage.take() {
                    fs::remove_dir(stage)?;
                }
                self.result = Some(Ok(Outcome::Completed(self.destination.clone())));
            }
            Phase::Cancelling => {
                self.result = Some(match self.failure.take() {
                    Some(error) => Err(error),
                    None => Ok(if self.skipped {
                        Outcome::Skipped
                    } else {
                        Outcome::Cancelled
                    }),
                })
            }
            Phase::Deleting => self.result = Some(Ok(Outcome::Completed(None))),
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    struct Sandbox(PathBuf);
    impl Sandbox {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "files-jobs-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
        fn dir(&self, name: &str) -> PathBuf {
            let p = self.0.join(name);
            fs::create_dir_all(&p).unwrap();
            p
        }
    }
    impl Drop for Sandbox {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    fn finish(job: &mut Operation) -> Result<Outcome, String> {
        for _ in 0..10000 {
            job.tick();
            if let Some(result) = job.take_result() {
                return result;
            }
            assert!(job.conflict().is_none(), "unexpected conflict");
        }
        panic!("job did not finish")
    }
    fn clean(directory: &Path) {
        assert!(!fs::read_dir(directory).unwrap().any(|e| {
            e.unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".files-transfer-")
        }));
    }

    #[test]
    fn copies_nested_files_and_preserves_source() {
        let s = Sandbox::new();
        let source = s.dir("source/nested");
        let dest = s.dir("destination");
        fs::write(source.join("bytes"), b"payload").unwrap();
        let root = source.parent().unwrap().to_owned();
        let mut job = Operation::new(Kind::Copy, root.clone(), &dest).unwrap();
        assert_eq!(
            finish(&mut job).unwrap(),
            Outcome::Completed(Some(dest.join("source")))
        );
        assert_eq!(
            fs::read(dest.join("source/nested/bytes")).unwrap(),
            b"payload"
        );
        assert!(root.join("nested/bytes").is_file());
        clean(&dest);
    }
    #[test]
    fn cancellation_removes_partial_copy_without_touching_source() {
        let s = Sandbox::new();
        let dest = s.dir("destination");
        let source = s.0.join("big");
        fs::write(&source, vec![42; 8 * 1024 * 1024]).unwrap();
        let mut job = Operation::new(Kind::Copy, source.clone(), &dest).unwrap();
        job.tick();
        assert!(job.bytes > 0);
        assert!(!dest.join("big").exists());
        job.cancel();
        assert_eq!(finish(&mut job).unwrap(), Outcome::Cancelled);
        assert_eq!(fs::metadata(source).unwrap().len(), 8 * 1024 * 1024);
        clean(&dest);
    }
    #[test]
    fn conflict_waits_and_keep_both_preserves_both_files() {
        let s = Sandbox::new();
        let dest = s.dir("destination");
        let source = s.0.join("name.txt");
        fs::write(&source, b"new").unwrap();
        fs::write(dest.join("name.txt"), b"old").unwrap();
        let mut job = Operation::new(Kind::Copy, source, &dest).unwrap();
        job.tick();
        assert_eq!(job.bytes, 0);
        assert!(job.conflict().is_some());
        job.resolve(Collision::KeepBoth).unwrap();
        finish(&mut job).unwrap();
        assert_eq!(fs::read(dest.join("name.txt")).unwrap(), b"old");
        assert_eq!(fs::read(dest.join("name 2.txt")).unwrap(), b"new");
        clean(&dest);
    }
    #[test]
    fn replace_moves_whole_directory_and_cleans_old_destination() {
        let s = Sandbox::new();
        let source = s.dir("source/tree");
        let dest = s.dir("destination");
        fs::write(source.join("new"), b"new").unwrap();
        fs::create_dir(dest.join("tree")).unwrap();
        fs::write(dest.join("tree/old"), b"old").unwrap();
        let mut job = Operation::new(Kind::Move, source.clone(), &dest).unwrap();
        job.resolve(Collision::Replace).unwrap();
        finish(&mut job).unwrap();
        assert!(!source.exists());
        assert!(dest.join("tree/new").exists());
        assert!(!dest.join("tree/old").exists());
        clean(&dest);
    }
    #[test]
    fn prevents_self_descendant_and_ancestor_replacement() {
        let s = Sandbox::new();
        let source = s.dir("tree/nested/tree");
        assert!(Operation::new(Kind::Copy, s.0.join("tree"), &source).is_err());
        let mut job = Operation::new(Kind::Copy, source.clone(), &s.0).unwrap();
        assert!(job.resolve(Collision::Replace).is_err());
        job.cancel();
        finish(&mut job).unwrap();
        assert!(source.is_dir());
    }
    #[test]
    fn late_conflict_skip_cleans_staging() {
        let s = Sandbox::new();
        let dest = s.dir("destination");
        let source = s.0.join("a");
        fs::write(&source, b"new").unwrap();
        let mut job = Operation::new(Kind::Copy, source, &dest).unwrap();
        // Create destination after staging starts, before publication.
        job.step().unwrap();
        fs::write(dest.join("a"), b"arrived").unwrap();
        for _ in 0..20 {
            job.tick();
            if job.conflict().is_some() {
                break;
            }
        }
        assert!(job.conflict().is_some());
        job.resolve(Collision::Skip).unwrap();
        assert_eq!(finish(&mut job).unwrap(), Outcome::Skipped);
        assert_eq!(fs::read(dest.join("a")).unwrap(), b"arrived");
        clean(&dest);
    }
    #[test]
    fn failed_publication_restores_previous_destination() {
        let s = Sandbox::new();
        let dest = s.dir("destination");
        let source = s.0.join("a");
        fs::write(&source, b"new").unwrap();
        fs::write(dest.join("a"), b"old").unwrap();
        let mut job = Operation::new(Kind::Move, source.clone(), &dest).unwrap();
        job.resolve(Collision::Replace).unwrap();
        job.step().unwrap();
        fs::remove_file(source).unwrap();
        assert!(finish(&mut job).is_err());
        assert_eq!(fs::read(dest.join("a")).unwrap(), b"old");
        clean(&dest);
    }
    #[test]
    fn deletes_tree_incrementally_and_can_stop_before_next_item() {
        let s = Sandbox::new();
        let source = s.dir("tree");
        for n in 0..100 {
            fs::write(source.join(n.to_string()), b"x").unwrap();
        }
        let mut job = Operation::new(Kind::Delete, source.clone(), &s.0).unwrap();
        job.tick();
        assert!(job.items > 0);
        assert!(source.is_dir());
        job.cancel();
        assert_eq!(finish(&mut job).unwrap(), Outcome::Cancelled);
        assert!(fs::read_dir(&source).unwrap().count() > 0);
        let mut job = Operation::new(Kind::Delete, source.clone(), &s.0).unwrap();
        finish(&mut job).unwrap();
        assert!(!source.exists());
    }
    #[cfg(unix)]
    #[test]
    fn symlinks_are_never_followed_during_delete_and_failed_copy_is_cleaned() {
        let s = Sandbox::new();
        let source = s.dir("tree");
        let dest = s.dir("destination");
        let external = s.dir("external");
        fs::write(external.join("keep"), b"safe").unwrap();
        std::os::unix::fs::symlink(&external, source.join("link")).unwrap();
        let mut job = Operation::new(Kind::Copy, source.clone(), &dest).unwrap();
        assert!(finish(&mut job).is_err());
        assert!(!dest.join("tree").exists());
        clean(&dest);
        let mut job = Operation::new(Kind::Delete, source, &s.0).unwrap();
        finish(&mut job).unwrap();
        assert_eq!(fs::read(external.join("keep")).unwrap(), b"safe");
    }
}

#[cfg(all(test, target_os = "linux"))]
mod cross_device_tests {
    use super::*;
    use std::os::unix::fs::MetadataExt;
    struct Cleanup(PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    #[test]
    fn cross_device_move_syncs_and_publishes_before_removing_source() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let source = Cleanup(
            std::env::temp_dir().join(format!("files-cross-source-{}-{nonce}", std::process::id())),
        );
        let target = Cleanup(
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join(format!(".cross-device-test-{}-{nonce}", std::process::id())),
        );
        fs::create_dir(&source.0).unwrap();
        fs::create_dir(&target.0).unwrap();
        if fs::metadata(&source.0).unwrap().dev() == fs::metadata(&target.0).unwrap().dev() {
            return;
        }
        let directory = source.0.join("folder");
        fs::create_dir(&directory).unwrap();
        let bytes = vec![42; 512 * 1024];
        fs::write(directory.join("data.bin"), &bytes).unwrap();
        let mut operation = Operation::new(Kind::Move, directory.clone(), &target.0).unwrap();
        let mut used_copy = false;
        let mut result = None;
        for _ in 0..10_000 {
            operation.tick();
            used_copy |= operation.copied_move;
            if operation.copied_move && matches!(operation.phase, Phase::Copying) {
                assert!(directory.join("data.bin").exists());
            }
            if let Some(value) = operation.take_result() {
                result = Some(value.unwrap());
                break;
            }
        }
        let result = result.expect("cross-device move should finish");
        assert!(used_copy);
        assert_eq!(
            result,
            Outcome::Completed(Some(fs::canonicalize(&target.0).unwrap().join("folder")))
        );
        assert!(!directory.exists());
        assert_eq!(fs::read(target.0.join("folder/data.bin")).unwrap(), bytes);
        assert!(!fs::read_dir(&target.0).unwrap().any(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".files-transfer-")
        }));
    }
}
