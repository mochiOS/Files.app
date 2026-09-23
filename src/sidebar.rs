use std::collections::HashSet;
use std::ffi::OsString;
use std::fs;
use std::io::{self, Write};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};

const MAGIC: &[u8] = b"MOCHIOS-FILES-SIDEBAR\0\x01";
const MAX_BOOKMARKS: usize = 128;
const MAX_PATH_BYTES: usize = 4096;

#[cfg(target_os = "mochios")]
const STORAGE_ROOT: &str = "/libraries/applications/org.mochios.files/users";

#[cfg(not(target_os = "mochios"))]
const STORAGE_ROOT: &str = "/tmp/mochios-files/users";

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct SidebarBookmarks {
    paths: Vec<PathBuf>,
    hidden_defaults: Vec<PathBuf>,
}

impl SidebarBookmarks {
    pub(crate) fn load() -> Self {
        let Some(path) = storage_path() else {
            return Self::default();
        };
        let _ = recover(&path);
        fs::read(path)
            .ok()
            .and_then(|bytes| Self::decode(&bytes).ok())
            .unwrap_or_default()
    }

    pub(crate) fn paths(&self) -> &[PathBuf] {
        &self.paths
    }

    pub(crate) fn contains(&self, path: &Path) -> bool {
        self.paths.iter().any(|candidate| candidate == path)
    }

    pub(crate) fn hides_default(&self, path: &Path) -> bool {
        self.hidden_defaults
            .iter()
            .any(|candidate| candidate == path)
    }

    pub(crate) fn add(&mut self, path: PathBuf) -> bool {
        if self.paths.len() >= MAX_BOOKMARKS || self.contains(&path) {
            return false;
        }
        self.paths.push(path);
        true
    }

    pub(crate) fn remove(&mut self, path: &Path) -> bool {
        let Some(index) = self.paths.iter().position(|candidate| candidate == path) else {
            return false;
        };
        self.paths.remove(index);
        true
    }

    pub(crate) fn hide_default(&mut self, path: PathBuf) -> bool {
        if self.hides_default(&path) {
            return false;
        }
        self.hidden_defaults.push(path);
        true
    }

    pub(crate) fn restore_default(&mut self, path: &Path) -> bool {
        let Some(index) = self
            .hidden_defaults
            .iter()
            .position(|candidate| candidate == path)
        else {
            return false;
        };
        self.hidden_defaults.remove(index);
        true
    }

    pub(crate) fn save(&self) -> io::Result<()> {
        let path = storage_path().ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "invalid session user name")
        })?;
        let parent = path.parent().ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "invalid sidebar storage path")
        })?;
        fs::create_dir_all(parent)?;

        let temporary = parent.join(format!(".sidebar-v1-{}.new", std::process::id()));
        let backup = parent.join(".sidebar-v1.backup");
        remove_if_present(&temporary)?;
        remove_if_present(&backup)?;

        let result = (|| {
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temporary)?;
            file.write_all(&self.encode()?)?;
            file.sync_all()?;
            drop(file);

            let had_original = match fs::rename(&path, &backup) {
                Ok(()) => true,
                Err(error) if error.kind() == io::ErrorKind::NotFound => false,
                Err(error) => return Err(error),
            };
            if let Err(error) = fs::rename(&temporary, &path) {
                if had_original {
                    let _ = fs::rename(&backup, &path);
                }
                return Err(error);
            }
            if had_original {
                remove_if_present(&backup)?;
            }
            Ok(())
        })();
        if result.is_err() {
            let _ = remove_if_present(&temporary);
        }
        result
    }

    fn encode(&self) -> io::Result<Vec<u8>> {
        let mut bytes = Vec::with_capacity(
            MAGIC.len() + 8 + (self.paths.len() + self.hidden_defaults.len()) * 32,
        );
        bytes.extend_from_slice(MAGIC);
        encode_paths(&mut bytes, &self.paths)?;
        encode_paths(&mut bytes, &self.hidden_defaults)?;
        Ok(bytes)
    }

    fn decode(bytes: &[u8]) -> io::Result<Self> {
        if !bytes.starts_with(MAGIC) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid sidebar bookmark header",
            ));
        }
        let mut cursor = MAGIC.len();
        let paths = decode_paths(bytes, &mut cursor)?;
        let hidden_defaults = decode_paths(bytes, &mut cursor)?;
        if cursor != bytes.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "trailing sidebar bookmark data",
            ));
        }
        Ok(Self {
            paths,
            hidden_defaults,
        })
    }
}

fn encode_paths(bytes: &mut Vec<u8>, paths: &[PathBuf]) -> io::Result<()> {
    let count = u32::try_from(paths.len())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "too many bookmarks"))?;
    bytes.extend_from_slice(&count.to_le_bytes());
    for path in paths {
            let raw = path.as_os_str().as_bytes();
            if raw.is_empty() || raw.len() > MAX_PATH_BYTES || !path.is_absolute() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "bookmark path is invalid",
                ));
            }
            let length = u32::try_from(raw.len()).map_err(|_| {
                io::Error::new(io::ErrorKind::InvalidData, "bookmark path is too long")
            })?;
            bytes.extend_from_slice(&length.to_le_bytes());
            bytes.extend_from_slice(raw);
    }
    Ok(())
}

fn decode_paths(bytes: &[u8], cursor: &mut usize) -> io::Result<Vec<PathBuf>> {
        let count = read_u32(bytes, cursor)? as usize;
        if count > MAX_BOOKMARKS {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "too many sidebar bookmarks",
            ));
        }

        let mut paths = Vec::with_capacity(count);
        let mut seen = HashSet::new();
        for _ in 0..count {
            let length = read_u32(bytes, cursor)? as usize;
            if length == 0 || length > MAX_PATH_BYTES || *cursor + length > bytes.len() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "invalid sidebar bookmark length",
                ));
            }
            let path = PathBuf::from(OsString::from_vec(
                bytes[*cursor..*cursor + length].to_vec(),
            ));
            *cursor += length;
            if !path.is_absolute() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "sidebar bookmark is not absolute",
                ));
            }
            if seen.insert(path.clone()) {
                paths.push(path);
            }
        }
        Ok(paths)
}

fn storage_path() -> Option<PathBuf> {
    let user = std::env::var("USER").unwrap_or_else(|_| String::from("root"));
    valid_user_name(&user)
        .then(|| Path::new(STORAGE_ROOT).join(user).join("sidebar-v1"))
}

fn valid_user_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

fn read_u32(bytes: &[u8], cursor: &mut usize) -> io::Result<u32> {
    let end = cursor.saturating_add(4);
    let raw = bytes.get(*cursor..end).ok_or_else(|| {
        io::Error::new(io::ErrorKind::UnexpectedEof, "truncated sidebar bookmarks")
    })?;
    *cursor = end;
    Ok(u32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]))
}

fn remove_if_present(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn recover(path: &Path) -> io::Result<()> {
    let Some(parent) = path.parent() else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid sidebar storage path",
        ));
    };
    let backup = parent.join(".sidebar-v1.backup");
    if !backup.exists() {
        return Ok(());
    }
    if path.exists() {
        remove_if_present(&backup)
    } else {
        fs::rename(backup, path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binary_format_round_trips_unicode_and_non_utf8_paths() {
        let mut bookmarks = SidebarBookmarks::default();
        assert!(bookmarks.add(PathBuf::from("/home/test/Documents/設計")));
        assert!(bookmarks.add(PathBuf::from(OsString::from_vec(vec![
            b'/', b't', b'm', b'p', b'/', 0x80,
        ]))));

        assert_eq!(
            SidebarBookmarks::decode(&bookmarks.encode().unwrap()).unwrap(),
            bookmarks
        );
    }

    #[test]
    fn decode_rejects_relative_truncated_and_trailing_data() {
        let relative = SidebarBookmarks {
            paths: vec![PathBuf::from("relative")],
            hidden_defaults: Vec::new(),
        };
        assert!(relative.encode().is_err());

        let mut valid = SidebarBookmarks {
            paths: vec![PathBuf::from("/absolute")],
            hidden_defaults: Vec::new(),
        }
        .encode()
        .unwrap();
        assert!(SidebarBookmarks::decode(&valid[..valid.len() - 1]).is_err());
        valid.push(0);
        assert!(SidebarBookmarks::decode(&valid).is_err());
    }

    #[test]
    fn duplicate_bookmarks_are_rejected_by_add_and_deduplicated_by_decode() {
        let mut bookmarks = SidebarBookmarks::default();
        assert!(bookmarks.add(PathBuf::from("/home/test/Desktop")));
        assert!(!bookmarks.add(PathBuf::from("/home/test/Desktop")));

        let raw_path = b"/home/test/Desktop";
        let mut bytes = MAGIC.to_vec();
        bytes.extend_from_slice(&2u32.to_le_bytes());
        for _ in 0..2 {
            bytes.extend_from_slice(&(raw_path.len() as u32).to_le_bytes());
            bytes.extend_from_slice(raw_path);
        }
        bytes.extend_from_slice(&0u32.to_le_bytes());
        assert_eq!(SidebarBookmarks::decode(&bytes).unwrap().paths.len(), 1);
    }

    #[test]
    fn custom_bookmarks_can_be_removed() {
        let path = PathBuf::from("/home/test/Documents/project");
        let mut bookmarks = SidebarBookmarks::default();
        assert!(bookmarks.add(path.clone()));
        assert!(bookmarks.remove(&path));
        assert!(!bookmarks.remove(&path));
        assert!(bookmarks.paths().is_empty());
    }

    #[test]
    fn default_bookmarks_can_be_hidden_and_restored() {
        let path = PathBuf::from("/home/test/Desktop");
        let mut bookmarks = SidebarBookmarks::default();
        assert!(bookmarks.hide_default(path.clone()));
        assert!(bookmarks.hides_default(&path));
        assert!(bookmarks.restore_default(&path));
        assert!(!bookmarks.hides_default(&path));
    }

    #[test]
    fn user_name_cannot_escape_application_storage() {
        assert!(valid_user_name("test-user_1"));
        assert!(!valid_user_name("../root"));
        assert!(!valid_user_name("user/name"));
    }
}
