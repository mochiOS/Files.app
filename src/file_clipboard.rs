use crate::operations::Kind;
use std::path::{Path, PathBuf};

const COPY_TYPE: &str = "application/x-mochios-file-copy";
const MOVE_TYPE: &str = "application/x-mochios-file-move";

pub(crate) fn set(kind: Kind, path: &Path) -> Result<(), String> {
    let content_type = if kind == Kind::Move {
        MOVE_TYPE
    } else {
        COPY_TYPE
    };
    let path = path
        .to_str()
        .ok_or("This path cannot be placed on the clipboard")?;
    write(content_type, path.as_bytes())
}
pub(crate) fn get() -> Option<(Kind, PathBuf)> {
    let (content_type, data) = read()?;
    let kind = match content_type.as_str() {
        COPY_TYPE => Kind::Copy,
        MOVE_TYPE => Kind::Move,
        _ => return None,
    };
    let path = PathBuf::from(String::from_utf8(data).ok()?);
    (path.is_absolute() && path.file_name().is_some()).then_some((kind, path))
}
pub(crate) fn clear_move(source: &Path) {
    if get().is_some_and(|(kind, path)| kind == Kind::Move && path == source) {
        let _ = write("text/plain;charset=utf-8", b"");
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
