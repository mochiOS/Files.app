//! Sequential batches reuse the single-item staging/commit machinery.
use crate::operations::{Collision, Kind, Operation, Outcome};
use std::{
    collections::{BTreeSet, VecDeque},
    fs, io,
    path::{Path, PathBuf},
};
#[derive(Default, Debug)]
pub(crate) struct Report {
    pub completed: Vec<(PathBuf, Option<PathBuf>)>,
    pub skipped: usize,
    pub errors: Vec<String>,
    pub failed: usize,
    pub cancelled: bool,
    pub total: usize,
}
pub(crate) struct Batch {
    pub kind: Kind,
    pub source: PathBuf,
    pub bytes: u64,
    pub items: u64,
    pub sources: Vec<PathBuf>,
    directory: PathBuf,
    pending: VecDeque<PathBuf>,
    current: Option<Operation>,
    report: Report,
    result: Option<Report>,
    all: Option<Collision>,
    cancelled: bool,
    done_bytes: u64,
    done_items: u64,
    total: usize,
    current_failure: Option<String>,
}
impl Batch {
    pub(crate) fn new_many(
        kind: Kind,
        sources: Vec<PathBuf>,
        directory: &Path,
    ) -> io::Result<Self> {
        if sources.len() > 1024 {
            return Err(io::Error::other("Select up to 1024 items per batch"));
        }
        let mut unique = BTreeSet::new();
        let mut normalized = Vec::new();
        for source in sources {
            let name = source
                .file_name()
                .ok_or_else(|| io::Error::other("A filesystem root cannot be selected"))?;
            let parent = fs::canonicalize(
                source
                    .parent()
                    .ok_or_else(|| io::Error::other("Invalid source path"))?,
            )?;
            let path = parent.join(name);
            fs::symlink_metadata(&path)?;
            if unique.insert(path.clone()) {
                normalized.push(path);
            }
        }
        let snapshot = normalized.clone();
        let parents: Vec<_> = normalized
            .iter()
            .filter(|path| fs::symlink_metadata(path).is_ok_and(|metadata| metadata.is_dir()))
            .cloned()
            .collect();
        normalized.retain(|path| {
            !parents
                .iter()
                .any(|parent| path != parent && path.starts_with(parent))
        });
        let source = normalized
            .first()
            .cloned()
            .ok_or_else(|| io::Error::other("No items selected"))?;
        let directory = if kind == Kind::Delete {
            directory.to_owned()
        } else {
            fs::canonicalize(directory)?
        };
        // Validate all roots before the first mutation; preparation has no writes.
        for source in &normalized {
            Operation::new(kind, source.clone(), &directory)?;
        }
        let total = normalized.len();
        let mut batch = Self {
            kind,
            source,
            bytes: 0,
            items: 0,
            sources: snapshot,
            directory,
            pending: normalized.into(),
            current: None,
            report: Report::default(),
            result: None,
            all: None,
            cancelled: false,
            done_bytes: 0,
            done_items: 0,
            total,
            current_failure: None,
        };
        batch.report.total = total;
        batch.start_next();
        Ok(batch)
    }
    fn start_next(&mut self) {
        while !self.cancelled {
            let Some(source) = self.pending.pop_front() else {
                break;
            };
            self.source = source.clone();
            match Operation::new(self.kind, source, &self.directory) {
                Ok(operation) => {
                    self.current = Some(operation);
                    return;
                }
                Err(error) => {
                    self.report.failed += 1;
                    self.report
                        .errors
                        .push(format!("{}: {error}", self.source.display()));
                    if !self.pending.is_empty() {
                        return;
                    }
                }
            }
        }
        self.report.cancelled = self.cancelled;
        self.result = Some(std::mem::take(&mut self.report));
    }
    pub(crate) fn total(&self) -> usize {
        self.total
    }
    pub(crate) fn processed(&self) -> usize {
        self.report.completed.len() + self.report.skipped + self.report.failed
    }
    pub(crate) fn conflict(&self) -> Option<&Path> {
        if self.all.is_some() {
            None
        } else {
            self.current.as_ref().and_then(Operation::conflict)
        }
    }
    pub(crate) fn cancellable(&self) -> bool {
        !self.cancelled
            && self.result.is_none()
            && (!self.pending.is_empty()
                || self.current.as_ref().is_some_and(Operation::cancellable))
    }
    pub(crate) fn label(&self) -> &'static str {
        if self.cancelled {
            "Cancelling"
        } else {
            self.current.as_ref().map_or("Finishing", Operation::label)
        }
    }
    pub(crate) fn resolve(&mut self, choice: Collision) -> io::Result<()> {
        if let Some(operation) = &mut self.current {
            operation.resolve(choice)?;
        }
        Ok(())
    }
    pub(crate) fn resolve_all(&mut self, choice: Collision) -> io::Result<()> {
        self.resolve(choice)?;
        self.all = Some(choice);
        Ok(())
    }
    pub(crate) fn cancel(&mut self) {
        if !self.cancellable() {
            return;
        }
        self.cancelled = true;
        if let Some(operation) = &mut self.current {
            operation.cancel();
        }
    }
    pub(crate) fn take_result(&mut self) -> Option<Report> {
        self.result.take()
    }
    pub(crate) fn tick(&mut self) {
        if self.result.is_some() {
            return;
        }
        let Some(operation) = &mut self.current else {
            self.start_next();
            return;
        };
        if operation.conflict().is_some() && !self.cancelled {
            if let Some(choice) = self.all {
                if let Err(error) = operation.resolve(choice) {
                    self.all = None;
                    self.current_failure = Some(format!("{}: {error}", self.source.display()));
                    operation.resolve(Collision::Skip).ok();
                }
            } else {
                return;
            }
        }
        operation.tick();
        self.bytes = self.done_bytes + operation.bytes;
        self.items = self.done_items + operation.items;
        if let Some(result) = operation.take_result() {
            let source = operation.source.clone();
            self.done_bytes = self.bytes;
            self.done_items = self.items;
            match result {
                Ok(Outcome::Completed(destination)) => {
                    self.report.completed.push((source, destination))
                }
                Ok(Outcome::Skipped) => {
                    if let Some(error) = self.current_failure.take() {
                        self.report.failed += 1;
                        self.report.errors.push(error);
                    } else {
                        self.report.skipped += 1;
                    }
                }
                Ok(Outcome::Cancelled) => self.cancelled = true,
                Err(error) => {
                    self.current_failure = None;
                    self.report.failed += 1;
                    self.report.errors.push(error);
                }
            }
            self.current = None;
            self.start_next();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    struct Sandbox(PathBuf);
    impl Sandbox {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "files-batch-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(path.join("source")).unwrap();
            fs::create_dir(path.join("target")).unwrap();
            Self(path)
        }
        fn source(&self, name: &str) -> PathBuf {
            let path = self.0.join("source").join(name);
            fs::write(&path, name).unwrap();
            path
        }
        fn target(&self) -> PathBuf {
            self.0.join("target")
        }
    }
    impl Drop for Sandbox {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    fn finish(batch: &mut Batch) -> Report {
        for _ in 0..10000 {
            batch.tick();
            if let Some(report) = batch.take_result() {
                return report;
            }
            assert!(batch.conflict().is_none(), "unresolved conflict");
        }
        panic!("batch did not finish")
    }
    #[test]
    fn copies_multiple_roots_and_selects_all_completed_destinations() {
        let s = Sandbox::new();
        let paths = vec![s.source("a.txt"), s.source("b.txt")];
        let mut batch = Batch::new_many(Kind::Copy, paths.clone(), &s.target()).unwrap();
        let report = finish(&mut batch);
        assert_eq!(report.total, 2);
        assert_eq!(report.completed.len(), 2);
        assert_eq!(report.failed, 0);
        for path in paths {
            assert!(path.exists());
            assert!(s.target().join(path.file_name().unwrap()).exists());
        }
        assert!(!fs::read_dir(s.target()).unwrap().any(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".files-transfer-")
        }));
    }
    #[test]
    fn replace_all_and_skip_all_apply_to_later_conflicts() {
        for replace in [false, true] {
            let s = Sandbox::new();
            let paths = vec![s.source("a.txt"), s.source("b.txt")];
            for path in &paths {
                fs::write(s.target().join(path.file_name().unwrap()), "old").unwrap();
            }
            let mut batch = Batch::new_many(Kind::Copy, paths, &s.target()).unwrap();
            assert!(batch.conflict().is_some());
            batch
                .resolve_all(if replace {
                    Collision::Replace
                } else {
                    Collision::Skip
                })
                .unwrap();
            let report = finish(&mut batch);
            assert_eq!(report.completed.len(), if replace { 2 } else { 0 });
            assert_eq!(report.skipped, if replace { 0 } else { 2 });
            assert_eq!(
                fs::read_to_string(s.target().join("b.txt")).unwrap(),
                if replace { "b.txt" } else { "old" }
            );
        }
    }
    #[test]
    fn cancellation_keeps_committed_work_and_never_starts_remaining_roots() {
        let s = Sandbox::new();
        let paths = vec![s.source("a"), s.source("b"), s.source("c")];
        let mut batch = Batch::new_many(Kind::Copy, paths.clone(), &s.target()).unwrap();
        for _ in 0..100 {
            batch.tick();
            if batch.processed() == 1 {
                break;
            }
        }
        assert_eq!(batch.processed(), 1);
        batch.cancel();
        let report = finish(&mut batch);
        assert!(report.cancelled);
        assert_eq!(report.completed.len(), 1);
        assert_eq!(report.total, 3);
        assert!(s.target().join("a").exists());
        assert!(!s.target().join("b").exists());
        assert!(!s.target().join("c").exists());
        assert!(paths.iter().all(|path| path.exists()));
    }
    #[test]
    fn a_missing_source_is_reported_without_faking_success_or_losing_other_items() {
        let s = Sandbox::new();
        let a = s.source("a");
        let b = s.source("b");
        let mut batch = Batch::new_many(Kind::Copy, vec![a.clone(), b], &s.target()).unwrap();
        fs::remove_file(a).unwrap();
        let report = finish(&mut batch);
        assert_eq!(report.failed, 1);
        assert_eq!(report.errors.len(), 1);
        assert_eq!(report.completed.len(), 1);
        assert!(s.target().join("b").exists());
        assert!(!s.target().join("a").exists());
        assert!(!report.cancelled);
    }
    #[test]
    fn moving_with_skip_preserves_unmoved_clipboard_items() {
        let s = Sandbox::new();
        let a = s.source("a");
        let b = s.source("b");
        fs::write(s.target().join("b"), "existing").unwrap();
        crate::file_clipboard::set_many(Kind::Move, &[a.clone(), b.clone()]).unwrap();
        let (_, sources) = crate::file_clipboard::get().unwrap();
        let mut batch = Batch::new_many(Kind::Move, sources, &s.target()).unwrap();
        loop {
            batch.tick();
            if batch.conflict().is_some() {
                batch.resolve(Collision::Skip).unwrap();
                break;
            }
        }
        let report = finish(&mut batch);
        let moved = report
            .completed
            .iter()
            .map(|(path, _)| path.clone())
            .collect::<Vec<_>>();
        crate::file_clipboard::clear_moved(&batch.sources, &moved);
        assert!(!a.exists());
        assert!(b.exists());
        assert_eq!(crate::file_clipboard::get().unwrap(), (Kind::Move, vec![b]));
    }
    #[test]
    fn overlapping_selection_only_processes_the_parent_once() {
        let s = Sandbox::new();
        let parent = s.0.join("source/folder");
        fs::create_dir(&parent).unwrap();
        let child = parent.join("child");
        fs::write(&child, "content").unwrap();
        let mut batch = Batch::new_many(Kind::Copy, vec![parent, child], &s.target()).unwrap();
        assert_eq!(batch.total(), 1);
        let report = finish(&mut batch);
        assert_eq!(report.completed.len(), 1);
        assert!(s.target().join("folder/child").exists());
        assert!(!s.target().join("child").exists());
    }
    #[test]
    fn all_roots_are_validated_before_any_destructive_work() {
        let s = Sandbox::new();
        let a = s.source("a");
        assert!(
            Batch::new_many(
                Kind::Delete,
                vec![a.clone(), s.0.join("missing")],
                &s.target()
            )
            .is_err()
        );
        assert!(a.exists());
    }
}
