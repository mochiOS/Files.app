use std::cmp::Ordering;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ViewMode {
    List,
    Grid,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EntryKind {
    Directory,
    Application,
    Image,
    Archive,
    Document,
    File,
}

impl EntryKind {
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Directory => "Folder",
            Self::Application => "Application",
            Self::Image => "Image",
            Self::Archive => "Archive",
            Self::Document => "Document",
            Self::File => "File",
        }
    }

    const fn sorts_as_directory(self) -> bool {
        matches!(self, Self::Directory | Self::Application)
    }
}

#[derive(Clone, Debug)]
pub(crate) struct FileEntry {
    pub(crate) path: PathBuf,
    pub(crate) name: String,
    pub(crate) kind: EntryKind,
    pub(crate) size: u64,
    pub(crate) modified: String,
}

impl FileEntry {
    pub(crate) fn is_directory(&self) -> bool {
        self.kind.sorts_as_directory()
    }

    pub(crate) fn size_label(&self) -> String {
        if self.is_directory() {
            return "--".to_owned();
        }

        format_size(self.size)
    }
}

#[derive(Debug)]
pub(crate) struct Browser {
    current_dir: PathBuf,
    entries: Vec<FileEntry>,
    history: Vec<PathBuf>,
    history_index: usize,
    search: String,
    selected: Option<PathBuf>,
    view_mode: ViewMode,
    error: Option<String>,
}

impl Browser {
    pub(crate) fn new(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let mut browser = Self {
            current_dir: path.clone(),
            entries: Vec::new(),
            history: vec![path],
            history_index: 0,
            search: String::new(),
            selected: None,
            view_mode: ViewMode::Grid,
            error: None,
        };
        browser.reload();
        browser
    }

    pub(crate) fn current_dir(&self) -> &Path {
        &self.current_dir
    }

    pub(crate) fn entries(&self) -> Vec<&FileEntry> {
        if self.search.is_empty() {
            return self.entries.iter().collect();
        }

        let query = self.search.to_ascii_lowercase();
        self.entries
            .iter()
            .filter(|entry| entry.name.to_ascii_lowercase().contains(&query))
            .collect()
    }

    pub(crate) fn selected(&self) -> Option<&Path> {
        self.selected.as_deref()
    }

    pub(crate) fn select(&mut self, path: PathBuf) {
        self.selected = Some(path);
    }

    pub(crate) fn clear_selection(&mut self) {
        self.selected = None;
    }

    pub(crate) fn search(&self) -> &str {
        &self.search
    }

    pub(crate) fn push_search(&mut self, text: &str) {
        self.search.push_str(text);
        self.selected = None;
    }

    pub(crate) fn pop_search(&mut self) {
        self.search.pop();
        self.selected = None;
    }

    pub(crate) fn clear_search(&mut self) {
        self.search.clear();
        self.selected = None;
    }

    pub(crate) fn view_mode(&self) -> ViewMode {
        self.view_mode
    }

    pub(crate) fn set_view_mode(&mut self, view_mode: ViewMode) {
        self.view_mode = view_mode;
    }

    pub(crate) fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    pub(crate) fn can_go_back(&self) -> bool {
        self.history_index > 0
    }

    pub(crate) fn can_go_forward(&self) -> bool {
        self.history_index + 1 < self.history.len()
    }

    pub(crate) fn navigate(&mut self, path: impl Into<PathBuf>) -> bool {
        let path = normalize_path(path.into());
        let Ok(entries) = read_entries(&path) else {
            self.error = Some(format!("Cannot open {}", path.display()));
            return false;
        };

        self.history.truncate(self.history_index + 1);
        self.history.push(path.clone());
        self.history_index = self.history.len() - 1;
        self.set_directory(path, entries);
        true
    }

    pub(crate) fn go_back(&mut self) -> bool {
        if !self.can_go_back() {
            return false;
        }
        self.load_history(self.history_index - 1)
    }

    pub(crate) fn go_forward(&mut self) -> bool {
        if !self.can_go_forward() {
            return false;
        }
        self.load_history(self.history_index + 1)
    }

    pub(crate) fn go_up(&mut self) -> bool {
        let Some(parent) = self.current_dir.parent() else {
            return false;
        };
        self.navigate(parent.to_path_buf())
    }

    pub(crate) fn reload(&mut self) {
        match read_entries(&self.current_dir) {
            Ok(entries) => self.set_directory(self.current_dir.clone(), entries),
            Err(error) => {
                self.entries.clear();
                self.selected = None;
                self.error = Some(format!(
                    "Cannot read {}: {error}",
                    self.current_dir.display()
                ));
            }
        }
    }

    pub(crate) fn select_relative(&mut self, offset: isize) {
        let entries = self.entries();
        if entries.is_empty() {
            self.selected = None;
            return;
        }

        let current = self
            .selected
            .as_ref()
            .and_then(|selected| entries.iter().position(|entry| &entry.path == selected));
        let next = match current {
            Some(index) => (index as isize + offset).clamp(0, entries.len() as isize - 1) as usize,
            None if offset < 0 => entries.len() - 1,
            None => 0,
        };
        self.selected = Some(entries[next].path.clone());
    }

    pub(crate) fn open_selected(&mut self) -> bool {
        let Some(selected) = self.selected.clone() else {
            return false;
        };
        let is_directory = self
            .entries
            .iter()
            .find(|entry| entry.path == selected)
            .is_some_and(FileEntry::is_directory);
        is_directory && self.navigate(selected)
    }

    pub(crate) fn create_folder(&mut self) -> Option<PathBuf> {
        let mut suffix = 1u32;
        let path = loop {
            let name = if suffix == 1 {
                "New Folder".to_owned()
            } else {
                format!("New Folder {suffix}")
            };
            let candidate = self.current_dir.join(name);
            if !candidate.exists() {
                break candidate;
            }
            suffix = suffix.checked_add(1)?;
        };
        match fs::create_dir(&path) {
            Ok(()) => {
                self.reload_select(Some(path.clone()));
                Some(path)
            }
            Err(error) => {
                self.set_operation_error("create folder", &path, error);
                None
            }
        }
    }

    pub(crate) fn rename_selected(&mut self, name: &str) -> bool {
        let Some(source) = self.selected.clone() else {
            return false;
        };
        if !valid_entry_name(name) || source.parent() != Some(self.current_dir.as_path()) {
            self.error = Some("The name is not valid".to_owned());
            return false;
        }
        let destination = self.current_dir.join(name);
        if destination == source {
            self.error = None;
            return true;
        }
        if destination.exists() {
            self.error = Some(format!("{} already exists", destination.display()));
            return false;
        }
        match fs::rename(&source, &destination) {
            Ok(()) => {
                self.reload_select(Some(destination));
                true
            }
            Err(error) => {
                self.set_operation_error("rename", &source, error);
                false
            }
        }
    }

    pub(crate) fn delete_selected(&mut self) -> bool {
        let Some(path) = self.selected.clone() else {
            return false;
        };
        if path.parent() != Some(self.current_dir.as_path()) {
            self.error = Some("The selected item cannot be deleted".to_owned());
            return false;
        }
        match remove_path_tree(&path) {
            Ok(()) => {
                self.reload_select(None);
                true
            }
            Err(error) => {
                self.set_operation_error("delete", &path, error);
                false
            }
        }
    }

    fn reload_select(&mut self, selected: Option<PathBuf>) {
        match read_entries(&self.current_dir) {
            Ok(entries) => {
                self.entries = entries;
                self.search.clear();
                self.selected = selected;
                self.error = None;
            }
            Err(error) => {
                self.entries.clear();
                self.selected = None;
                self.error = Some(format!(
                    "Cannot read {}: {error}",
                    self.current_dir.display()
                ));
            }
        }
    }

    fn set_operation_error(&mut self, operation: &str, path: &Path, error: std::io::Error) {
        self.error = Some(format!("Cannot {operation} {}: {error}", path.display()));
    }

    fn load_history(&mut self, index: usize) -> bool {
        let Some(path) = self.history.get(index).cloned() else {
            return false;
        };
        let Ok(entries) = read_entries(&path) else {
            self.error = Some(format!("Cannot open {}", path.display()));
            return false;
        };
        self.history_index = index;
        self.set_directory(path, entries);
        true
    }

    fn set_directory(&mut self, path: PathBuf, entries: Vec<FileEntry>) {
        self.current_dir = path;
        self.entries = entries;
        self.search.clear();
        self.selected = None;
        self.error = None;
    }
}

fn valid_entry_name(name: &str) -> bool {
    !name.is_empty() && name != "." && name != ".." && !name.contains('/') && !name.contains('\0')
}

fn remove_path_tree(path: &Path) -> std::io::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return fs::remove_file(path);
    }
    for entry in fs::read_dir(path)? {
        remove_path_tree(&entry?.path())?;
    }
    fs::remove_dir(path)
}

fn read_entries(path: &Path) -> std::io::Result<Vec<FileEntry>> {
    let mut entries = Vec::new();
    for result in fs::read_dir(path)? {
        let entry = result?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }

        let metadata = entry.metadata()?;
        let kind = classify(&name, metadata.is_dir());
        let modified = metadata
            .modified()
            .ok()
            .and_then(format_modified)
            .unwrap_or_else(|| "--".to_owned());
        entries.push(FileEntry {
            path: entry.path(),
            name,
            kind,
            size: metadata.len(),
            modified,
        });
    }

    entries.sort_by(compare_entries);
    Ok(entries)
}

fn compare_entries(left: &FileEntry, right: &FileEntry) -> Ordering {
    match (
        left.kind.sorts_as_directory(),
        right.kind.sorts_as_directory(),
    ) {
        (true, false) => Ordering::Less,
        (false, true) => Ordering::Greater,
        _ => left
            .name
            .to_ascii_lowercase()
            .cmp(&right.name.to_ascii_lowercase()),
    }
}

fn classify(name: &str, is_directory: bool) -> EntryKind {
    if is_directory {
        return if name.to_ascii_lowercase().ends_with(".app") {
            EntryKind::Application
        } else {
            EntryKind::Directory
        };
    }

    let extension = Path::new(name)
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match extension.as_str() {
        "png" | "jpg" | "jpeg" | "gif" | "bmp" | "webp" | "svg" => EntryKind::Image,
        "zip" | "tar" | "gz" | "xz" | "mpkg" => EntryKind::Archive,
        "txt" | "md" | "toml" | "json" | "xml" | "log" => EntryKind::Document,
        _ => EntryKind::File,
    }
}

fn normalize_path(path: PathBuf) -> PathBuf {
    let mut normalized = PathBuf::from("/");
    for component in path.components() {
        use std::path::Component;
        match component {
            Component::ParentDir => {
                normalized.pop();
            }
            Component::Normal(part) => normalized.push(part),
            Component::RootDir | Component::Prefix(_) | Component::CurDir => {}
        }
    }
    normalized
}

fn format_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["bytes", "KB", "MB", "GB", "TB"];
    if bytes < 1_000 {
        return format!("{bytes} bytes");
    }

    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1_000.0 && unit + 1 < UNITS.len() {
        value /= 1_000.0;
        unit += 1;
    }
    if value >= 10.0 {
        format!("{value:.0} {}", UNITS[unit])
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

fn format_modified(time: std::time::SystemTime) -> Option<String> {
    let seconds = time.duration_since(UNIX_EPOCH).ok()?.as_secs();
    let days = (seconds / 86_400) as i64;
    let day_seconds = seconds % 86_400;
    let (year, month, day) = civil_from_days(days);
    let hour = day_seconds / 3_600;
    let minute = day_seconds % 3_600 / 60;
    Some(format!(
        "{month:02}/{day:02}/{year:04} {hour:02}:{minute:02}"
    ))
}

fn civil_from_days(days_since_epoch: i64) -> (i64, u32, u32) {
    let z = days_since_epoch + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let day_of_era = z - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let mut year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);
    (year, month as u32, day as u32)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_TEST_DIRECTORY: AtomicU64 = AtomicU64::new(1);

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new() -> std::io::Result<Self> {
            let sequence = NEXT_TEST_DIRECTORY.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir()
                .join(format!("mochios-files-{}-{sequence}", std::process::id()));
            fs::create_dir(&path)?;
            Ok(Self(path))
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn defaults_to_grid_view() {
        assert_eq!(Browser::new("/").view_mode(), ViewMode::Grid);
    }

    #[test]
    fn classifies_common_file_types() {
        assert_eq!(classify("Files.app", true), EntryKind::Application);
        assert_eq!(classify("Pictures", true), EntryKind::Directory);
        assert_eq!(classify("photo.PNG", false), EntryKind::Image);
        assert_eq!(classify("notes.md", false), EntryKind::Document);
        assert_eq!(classify("bundle.mpkg", false), EntryKind::Archive);
    }

    #[test]
    fn formats_file_sizes() {
        assert_eq!(format_size(999), "999 bytes");
        assert_eq!(format_size(1_500), "1.5 KB");
        assert_eq!(format_size(12_500_000), "12 MB");
    }

    #[test]
    fn converts_unix_epoch_date() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(20_000), (2024, 10, 4));
    }

    #[test]
    fn normalizes_parent_components_without_leaving_root() {
        assert_eq!(
            normalize_path(PathBuf::from("/system/../applications")),
            Path::new("/applications")
        );
        assert_eq!(
            normalize_path(PathBuf::from("/../../tmp")),
            Path::new("/tmp")
        );
    }

    #[test]
    fn creates_uniquely_named_folders() -> std::io::Result<()> {
        let directory = TestDirectory::new()?;
        let mut browser = Browser::new(directory.path());
        let first = browser.create_folder();
        let second = browser.create_folder();
        let expected_first = directory.path().join("New Folder");
        let expected_second = directory.path().join("New Folder 2");
        assert_eq!(first.as_deref(), Some(expected_first.as_path()));
        assert_eq!(second.as_deref(), Some(expected_second.as_path()));
        Ok(())
    }

    #[test]
    fn renames_without_replacing_an_existing_item() -> std::io::Result<()> {
        let directory = TestDirectory::new()?;
        let old = directory.path().join("old.txt");
        let occupied = directory.path().join("occupied.txt");
        fs::write(&old, b"old")?;
        fs::write(&occupied, b"occupied")?;
        let mut browser = Browser::new(directory.path());
        browser.select(old.clone());
        assert!(!browser.rename_selected("occupied.txt"));
        assert!(old.exists());
        assert!(browser.rename_selected("new.txt"));
        assert!(!old.exists());
        assert!(directory.path().join("new.txt").exists());
        Ok(())
    }

    #[test]
    fn deletes_a_directory_tree() -> std::io::Result<()> {
        let directory = TestDirectory::new()?;
        let target = directory.path().join("target");
        fs::create_dir(&target)?;
        fs::write(target.join("child.txt"), b"child")?;
        let mut browser = Browser::new(directory.path());
        browser.select(target.clone());
        assert!(browser.delete_selected());
        assert!(!target.exists());
        Ok(())
    }

    #[test]
    fn rejects_invalid_names() -> std::io::Result<()> {
        let directory = TestDirectory::new()?;
        let target = directory.path().join("target");
        fs::write(&target, b"target")?;
        let mut browser = Browser::new(directory.path());
        browser.select(target.clone());
        assert!(!browser.rename_selected("../outside"));
        assert!(target.exists());
        Ok(())
    }
}
