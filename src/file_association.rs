use std::fs::File;
use std::io::Read;
use std::path::Path;

use appkit::document::{self, AssociationRoles};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Handler {
    pub(crate) bundle_id: String,
    pub(crate) name: String,
}

pub(crate) fn content_type(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
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
        _ if looks_like_utf8_text(path) => "text/plain",
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
        document::open_with(path, content_type, bundle_id, AssociationRoles::EDIT)
    } else {
        document::open(path, content_type, AssociationRoles::EDIT)
    };
    result
        .map(|_| ())
        .map_err(|error| format!("No application can open this file ({error:?})"))
}

#[cfg(not(target_os = "mochios"))]
pub(crate) fn open(_path: &Path, _bundle_id: Option<&str>) -> Result<(), String> {
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
    document::handlers(extension, content_type(path), AssociationRoles::EDIT)
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
        AssociationRoles::EDIT,
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
    }
}
