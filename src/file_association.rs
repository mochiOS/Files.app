use std::fs::File;
use std::io::Read;
use std::path::Path;

use appcore::document::{self, AssociationRoles};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Handler {
    pub(crate) bundle_id: String,
    pub(crate) name: String,
}

pub(crate) fn content_type(path: &Path) -> &'static str {
    let kind = declared_content_type(
        path.extension()
            .and_then(|value| value.to_str())
            .unwrap_or_default(),
    );
    if kind == "application/octet-stream" && looks_like_utf8_text(path) {
        "text/plain"
    } else {
        kind
    }
}

/// Classifies a filename extension without opening a filesystem path.
pub(crate) fn declared_content_type(extension: &str) -> &'static str {
    match extension.to_ascii_lowercase().as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "gif" => "image/gif",
        "bmp" => "image/bmp",
        "svg" => "image/svg+xml",
        "ttf" => appcore::content_type::TRUE_TYPE_FONT,
        "otf" => appcore::content_type::OPEN_TYPE_FONT,
        "ttc" | "otc" => appcore::content_type::FONT_COLLECTION,
        "json" => "application/json",
        "toml" => "application/toml",
        "xml" => "application/xml",
        "csv" => "text/csv",
        "md" | "markdown" => "text/markdown",
        "c" | "h" => "text/x-c",
        "cc" | "cpp" | "cxx" | "hh" | "hpp" => "text/x-c++",
        "rs" => "text/x-rust",
        "sh" => "text/x-shellscript",
        "yaml" | "yml" => "text/yaml",
        "txt" | "text" | "log" | "ini" | "conf" | "cfg" => "text/plain",
        _ => "application/octet-stream",
    }
}

fn looks_like_utf8_text(path: &Path) -> bool {
    let Ok(mut file) = File::open(path) else {
        return false;
    };
    let mut bytes = [0u8; 8192];
    let Ok(length) = file.read(&mut bytes) else {
        return false;
    };
    let sample = &bytes[..length];
    !sample.contains(&0) && std::str::from_utf8(sample).is_ok()
}

#[cfg(target_os = "mochios")]
pub(crate) fn open(path: &Path, bundle_id: Option<&str>) -> Result<(), String> {
    let path = path
        .to_str()
        .ok_or_else(|| String::from("The file path is not valid UTF-8"))?;
    let content_type = content_type(Path::new(path));
    let result = if let Some(bundle_id) = bundle_id {
        document::open_with(path, content_type, bundle_id, AssociationRoles::VIEW)
    } else {
        document::open(path, content_type, AssociationRoles::VIEW)
    };
    result
        .map(|_| ())
        .map_err(|error| format!("No application can open this file ({error:?})"))
}

#[cfg(not(target_os = "mochios"))]
pub(crate) fn open(_path: &Path, _bundle_id: Option<&str>) -> Result<(), String> {
    #[cfg(target_os = "linux")]
    {
        let path = _path
            .to_str()
            .ok_or("The document path is not valid UTF-8")?;
        let result = if let Some(bundle_id) = _bundle_id {
            document::open_with(path, content_type(_path), bundle_id, AssociationRoles::VIEW)
        } else {
            document::open(path, content_type(_path), AssociationRoles::VIEW)
        };
        return result
            .map(|_| ())
            .map_err(|error| format!("The document could not be opened ({error})"));
    }
    #[cfg(not(target_os = "linux"))]
    Err(String::from(
        "Application launching is only available on mochiOS",
    ))
}

#[cfg(target_os = "mochios")]
pub(crate) fn handlers(path: &Path) -> Result<Vec<Handler>, String> {
    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default();
    document::handlers(extension, content_type(path), AssociationRoles::VIEW)
        .map(|handlers| {
            handlers
                .into_iter()
                .map(|handler| Handler {
                    bundle_id: handler.bundle_id,
                    name: handler.name,
                })
                .collect()
        })
        .map_err(|error| format!("Cannot find applications for this file ({error:?})"))
}

#[cfg(not(target_os = "mochios"))]
pub(crate) fn handlers(_path: &Path) -> Result<Vec<Handler>, String> {
    Ok(Vec::new())
}

#[cfg(target_os = "mochios")]
pub(crate) fn set_default(path: &Path, bundle_id: &str) -> Result<(), String> {
    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default();
    document::set_default(
        extension,
        content_type(path),
        bundle_id,
        AssociationRoles::VIEW,
    )
    .map_err(|error| format!("Cannot change the default application ({error:?})"))
}

#[cfg(not(target_os = "mochios"))]
pub(crate) fn set_default(_path: &Path, _bundle_id: &str) -> Result<(), String> {
    Err(String::from(
        "Default applications can only be changed on mochiOS",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_extensions_have_stable_content_types() {
        assert_eq!(content_type(Path::new("document.json")), "application/json");
        assert_eq!(content_type(Path::new("document.toml")), "application/toml");
        assert_eq!(content_type(Path::new("README.md")), "text/markdown");
        assert_eq!(content_type(Path::new("main.rs")), "text/x-rust");
        for (name, mime) in [
            ("font.TTF", "font/ttf"),
            ("font.otf", "font/otf"),
            ("font.ttc", "font/collection"),
            ("font.otc", "font/collection"),
        ] {
            assert_eq!(content_type(Path::new(name)), mime);
            assert_eq!(
                content_type(Path::new(name)),
                appcore::content_type::ContentType::for_path(name).identifier()
            );
        }
    }
}
