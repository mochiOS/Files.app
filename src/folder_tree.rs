//! Lazily enumerated sidebar folders. Only explicitly expanded directories are read.
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub(crate) struct FolderRow {
    pub path: PathBuf,
    pub depth: usize,
    pub expanded: bool,
    pub expandable: bool,
}

pub(crate) struct FolderTree {
    root: PathBuf,
    expanded: BTreeSet<PathBuf>,
    children: BTreeMap<PathBuf, Vec<PathBuf>>,
}
impl FolderTree {
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            expanded: BTreeSet::new(),
            children: BTreeMap::new(),
        }
    }
    pub fn toggle(&mut self, path: &Path) -> io::Result<()> {
        if self.expanded.remove(path) {
            return Ok(());
        }
        // Do not follow symlink directories or enumerate an entire filesystem.
        let mut children = fs::read_dir(path)?
            .filter_map(Result::ok)
            .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
            .map(|entry| entry.path())
            .collect::<Vec<_>>();
        children.sort_by(|a, b| {
            a.file_name()
                .map(|name| name.to_string_lossy().to_lowercase())
                .cmp(
                    &b.file_name()
                        .map(|name| name.to_string_lossy().to_lowercase()),
                )
                .then(a.cmp(b))
        });
        self.children.insert(path.to_path_buf(), children);
        self.expanded.insert(path.to_path_buf());
        Ok(())
    }
    pub fn rows(&self) -> Vec<FolderRow> {
        let mut rows = Vec::new();
        self.append(&self.root, 0, &mut rows);
        rows
    }
    fn append(&self, path: &Path, depth: usize, rows: &mut Vec<FolderRow>) {
        let expanded = self.expanded.contains(path);
        rows.push(FolderRow {
            path: path.to_path_buf(),
            depth,
            expanded,
            expandable: self
                .children
                .get(path)
                .is_none_or(|children| !children.is_empty()),
        });
        if expanded && let Some(children) = self.children.get(path) {
            for child in children {
                self.append(child, depth + 1, rows);
            }
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn expands_only_folders_and_preserves_nested_expansion_when_collapsed() {
        let root = std::env::temp_dir().join(format!("mochios-folder-tree-{}", std::process::id()));
        fs::create_dir_all(root.join("System/Library")).unwrap();
        fs::create_dir_all(root.join("Applications")).unwrap();
        fs::write(root.join("file.txt"), "text").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&root, root.join("cycle")).unwrap();
        let mut tree = FolderTree::new(root.clone());
        assert_eq!(tree.rows().len(), 1);
        tree.toggle(&root).unwrap();
        assert_eq!(tree.rows().len(), 3);
        assert!(tree.rows()[1].path.ends_with("Applications"));
        tree.toggle(&root.join("System")).unwrap();
        assert_eq!(tree.rows().last().unwrap().depth, 2);
        tree.toggle(&root).unwrap();
        assert_eq!(tree.rows().len(), 1);
        tree.toggle(&root).unwrap();
        assert_eq!(tree.rows().len(), 4);
        assert!(tree.toggle(&root.join("missing")).is_err());
        fs::remove_dir_all(root).unwrap();
    }
}
