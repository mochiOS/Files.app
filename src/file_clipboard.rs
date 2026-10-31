use crate::operations::Kind;
use std::path::{Path, PathBuf};

const COPY_TYPE: &str = "application/x-mochios-file-copy";
const MOVE_TYPE: &str = "application/x-mochios-file-move";

const MAX_ITEMS: usize = 1024;
const MAX_BYTES: usize = 1024 * 1024;
const MAGIC: &[u8] = b"MFILES2\0";

#[cfg(test)]
pub(crate) fn set(kind: Kind, path: &Path) -> Result<(), String> {
    set_many(kind, &[path.to_owned()])
}
pub(crate) fn set_many(kind: Kind, paths: &[PathBuf]) -> Result<(), String> {
    encode(paths)?; // Validate lengths and syntax before touching filesystem paths.
    let paths = paths
        .iter()
        .map(|path| {
            let parent = std::fs::canonicalize(path.parent().ok_or("Invalid source path")?)
                .map_err(|error| error.to_string())?;
            Ok(parent.join(path.file_name().ok_or("Invalid source path")?))
        })
        .collect::<Result<Vec<_>, String>>()?;
    let bytes = encode(&paths)?;
    write(
        if kind == Kind::Move {
            MOVE_TYPE
        } else {
            COPY_TYPE
        },
        &bytes,
    )
}
fn encode(paths: &[PathBuf]) -> Result<Vec<u8>, String> {
    if paths.is_empty() || paths.len() > MAX_ITEMS {
        return Err("Select between 1 and 1024 items".into());
    }
    let mut bytes = MAGIC.to_vec();
    bytes.extend_from_slice(&(paths.len() as u32).to_le_bytes());
    for path in paths {
        let value = path
            .to_str()
            .ok_or("This path cannot be placed on the clipboard")?;
        if !valid(path) || value.contains('\0') || value.len() > 4095 {
            return Err("Invalid file clipboard path".into());
        }
        bytes.extend_from_slice(&(value.len() as u32).to_le_bytes());
        bytes.extend_from_slice(value.as_bytes());
        if bytes.len() > MAX_BYTES {
            return Err("Too many file paths for the clipboard".into());
        }
    }
    Ok(bytes)
}
fn valid(path: &Path) -> bool {
    path.is_absolute() && path.file_name().is_some()
}
fn decode(bytes: &[u8]) -> Option<Vec<PathBuf>> {
    if bytes.len() > MAX_BYTES {
        return None;
    }
    if !bytes.starts_with(MAGIC) {
        let text = std::str::from_utf8(bytes).ok()?;
        let path = PathBuf::from(text);
        return (valid(&path) && !text.contains('\0')).then_some(vec![path]);
    }
    let mut rest = &bytes[MAGIC.len()..];
    fn length(rest: &mut &[u8]) -> Option<usize> {
        let value = u32::from_le_bytes(rest.get(..4)?.try_into().ok()?) as usize;
        *rest = &rest[4..];
        Some(value)
    }
    let count = length(&mut rest)?;
    if count == 0 || count > MAX_ITEMS {
        return None;
    }
    let mut paths = Vec::new();
    for _ in 0..count {
        let size = length(&mut rest)?;
        if size > 4095 {
            return None;
        }
        let text = std::str::from_utf8(rest.get(..size)?).ok()?;
        rest = &rest[size..];
        let path = PathBuf::from(text);
        if !valid(&path) || text.contains('\0') {
            return None;
        }
        if !paths.contains(&path) {
            paths.push(path);
        }
    }
    rest.is_empty().then_some(paths)
}
pub(crate) fn get() -> Option<(Kind, Vec<PathBuf>)> {
    let (content_type, data) = read()?;
    let kind = match content_type.as_str() {
        COPY_TYPE => Kind::Copy,
        MOVE_TYPE => Kind::Move,
        _ => return None,
    };
    Some((kind, decode(&data)?))
}
pub(crate) fn clear_moved(snapshot: &[PathBuf], completed: &[PathBuf]) {
    if let Some((Kind::Move, paths)) = get() {
        if paths == snapshot {
            let remaining: Vec<_> = paths
                .into_iter()
                .filter(|path| {
                    !completed
                        .iter()
                        .any(|source| path == source || path.starts_with(source))
                })
                .collect();
            if remaining.is_empty() {
                let _ = write("text/plain;charset=utf-8", b"");
            } else {
                let _ = set_many(Kind::Move, &remaining);
            }
        }
    }
}
#[cfg(target_os = "mochios")]
fn write(content_type: &str, bytes: &[u8]) -> Result<(), String> {
    appcore::clipboard::set(content_type, bytes)
        .map_err(|e| format!("Cannot write clipboard: {e:?}"))
}
#[cfg(target_os = "mochios")]
fn read() -> Option<(String, Vec<u8>)> {
    appcore::clipboard::content().ok().flatten()
}

// Share the preview's existing clipboard slot. An invalid UTF-8 marker keeps file
// references out of text fields, and subsequent text copying replaces the slot.
#[cfg(not(target_os = "mochios"))]
fn write(content_type: &str, bytes: &[u8]) -> Result<(), String> {
    let mut data = b"\xffFILES\0".to_vec();
    data.extend_from_slice(content_type.as_bytes());
    data.push(0);
    data.extend_from_slice(bytes);
    if let Some(root) = std::env::var_os("MOCHIOS_PREVIEW_CONFIG_ROOT") {
        std::fs::create_dir_all(&root).map_err(|e| e.to_string())?;
        std::fs::write(PathBuf::from(root).join("clipboard.txt"), data).map_err(|e| e.to_string())
    } else {
        LOCAL.with(|slot| *slot.borrow_mut() = Some(data));
        Ok(())
    }
}
#[cfg(not(target_os = "mochios"))]
thread_local! { static LOCAL: std::cell::RefCell<Option<Vec<u8>>> = const { std::cell::RefCell::new(None) }; }
#[cfg(not(target_os = "mochios"))]
fn read() -> Option<(String, Vec<u8>)> {
    let data = if let Some(root) = std::env::var_os("MOCHIOS_PREVIEW_CONFIG_ROOT") {
        std::fs::read(PathBuf::from(root).join("clipboard.txt")).ok()?
    } else {
        LOCAL.with(|slot| slot.borrow().clone())?
    };
    let data = data.strip_prefix(b"\xffFILES\0")?;
    let split = data.iter().position(|b| *b == 0)?;
    Some((
        std::str::from_utf8(&data[..split]).ok()?.to_owned(),
        data[split + 1..].to_vec(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn path_lists_round_trip_unicode_and_newlines() {
        let paths = vec![
            PathBuf::from("/home/user/日本語.txt"),
            PathBuf::from("/home/user/line\nbreak.txt"),
        ];
        assert_eq!(decode(&encode(&paths).unwrap()).unwrap(), paths);
        assert_eq!(
            decode(b"/home/user/legacy.txt").unwrap(),
            vec![PathBuf::from("/home/user/legacy.txt")]
        );
    }
    #[test]
    fn malformed_oversized_and_relative_paths_are_rejected() {
        assert!(encode(&[]).is_err());
        assert!(encode(&["relative.txt".into()]).is_err());
        let mut data = encode(&["/home/user/a".into()]).unwrap();
        data.push(0);
        assert!(decode(&data).is_none());
        assert!(decode(MAGIC).is_none());
        assert!(decode(&vec![0; MAX_BYTES + 1]).is_none());
        let mut data = MAGIC.to_vec();
        data.extend_from_slice(&u32::MAX.to_le_bytes());
        assert!(decode(&data).is_none());
    }
    #[test]
    fn completed_moves_do_not_clear_a_new_clipboard_snapshot() {
        let dir = std::env::temp_dir();
        let a = dir.join("a");
        let b = dir.join("b");
        set_many(Kind::Move, std::slice::from_ref(&a)).unwrap();
        set_many(Kind::Copy, std::slice::from_ref(&b)).unwrap();
        clear_moved(std::slice::from_ref(&a), std::slice::from_ref(&a));
        assert_eq!(get().unwrap().0, Kind::Copy);
        assert_eq!(
            get().unwrap().1,
            vec![std::fs::canonicalize(dir).unwrap().join("b")]
        );
    }
}
