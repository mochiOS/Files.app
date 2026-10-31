//! Asynchronous app-provided thumbnails. File decoding belongs to the provider.
use appcore::{
    document::DocumentIconResolver,
    task::{TaskScope, TaskSlot},
    viewkit::{dispatch::UiSubscription, image::ImageData},
};
use std::{
    collections::{BTreeMap, VecDeque},
    fs,
    io::Read,
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::SystemTime,
};
const EDGE: u32 = 144;
const CAPACITY: usize = 128;
#[derive(Clone, Debug, PartialEq, Eq)]
struct Stamp {
    length: u64,
    modified: SystemTime,
}
impl Stamp {
    fn read(path: &Path) -> Option<Self> {
        let metadata = fs::metadata(path).ok()?;
        if !metadata.is_file() {
            return None;
        }
        Some(Self {
            length: metadata.len(),
            modified: metadata.modified().ok()?,
        })
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
struct Key {
    path: PathBuf,
    source: Stamp,
    provider: String,
    executable: Stamp,
}
struct Cached {
    key: Key,
    image: Option<ImageData>,
}
#[derive(Default)]
struct Shared {
    generation: u64,
    ready: BTreeMap<PathBuf, ImageData>,
    cache: VecDeque<Cached>,
}
impl Shared {
    fn publish(&mut self, generation: u64, key: Key, image: Option<ImageData>) -> bool {
        if self.generation != generation {
            return false;
        }
        if let Some(image) = &image {
            self.ready.insert(key.path.clone(), image.clone());
        }
        self.cache.retain(|entry| entry.key.path != key.path);
        self.cache.push_back(Cached { key, image });
        while self.cache.len() > CAPACITY {
            self.cache.pop_front();
        }
        true
    }
}

pub(crate) struct Thumbnails {
    shared: Arc<Mutex<Shared>>,
    tasks: Option<TaskScope>,
    slot: TaskSlot,
    subscription: Option<UiSubscription>,
    requested: Vec<PathBuf>,
}
impl Thumbnails {
    pub(crate) fn new() -> Self {
        Self {
            shared: Arc::new(Mutex::new(Shared::default())),
            tasks: None,
            slot: TaskSlot::new(),
            subscription: UiSubscription::new(|| {}).ok(),
            requested: Vec::new(),
        }
    }
    pub(crate) fn image(&self, path: &Path) -> Option<ImageData> {
        self.shared.lock().ok()?.ready.get(path).cloned()
    }
    pub(crate) fn invalidate(&mut self) {
        self.requested.clear();
        self.slot = TaskSlot::new();
        if let Ok(mut shared) = self.shared.lock() {
            shared.generation = shared.generation.wrapping_add(1);
            shared.ready.clear();
        }
    }
    pub(crate) fn request(&mut self, paths: Vec<PathBuf>) {
        if self.requested == paths {
            return;
        }
        self.requested = paths.clone();
        let generation = {
            let Ok(mut shared) = self.shared.lock() else {
                return;
            };
            shared.generation = shared.generation.wrapping_add(1);
            shared.ready.retain(|path, _| paths.contains(path));
            shared.generation
        };
        // Replacing the slot cancels the previous directory even if task setup fails.
        self.slot = TaskSlot::new();
        if self.tasks.is_none() {
            self.tasks = TaskScope::new().ok();
        }
        let (Some(tasks), Some(subscription)) = (&self.tasks, &self.subscription) else {
            return;
        };
        let notify = subscription.notifier();
        let shared = self.shared.clone();
        let _ = self.slot.spawn(
            tasks,
            move |task| -> appcore::task::TaskResult<(), std::io::Error> {
                let Ok(resolver) = DocumentIconResolver::new() else {
                    return Ok(());
                };
                let mut requested = 0;
                for path in paths {
                    task.check_cancelled()?;
                    let Some(extension) = path
                        .extension()
                        .and_then(|value| value.to_str())
                        .map(str::to_ascii_lowercase)
                    else {
                        continue;
                    };
                    let content_type =
                        appcore::content_type::ContentType::from_extension(&extension);
                    let Ok(Some(provider)) =
                        resolver.thumbnail_provider(&extension, content_type.identifier())
                    else {
                        continue;
                    };
                    if requested >= CAPACITY {
                        break;
                    }
                    requested += 1;
                    let (Some(source), Some(executable)) =
                        (Stamp::read(&path), Stamp::read(provider.executable()))
                    else {
                        continue;
                    };
                    if source.length > appcore::document::thumbnail::MAX_INPUT_BYTES as u64 {
                        continue;
                    }
                    let key = Key {
                        path: path.clone(),
                        source,
                        provider: provider.bundle_id.clone(),
                        executable,
                    };
                    let cached = shared.lock().ok().and_then(|shared| {
                        shared
                            .cache
                            .iter()
                            .find(|entry| entry.key == key)
                            .map(|entry| entry.image.clone())
                    });
                    let image = match cached {
                        Some(image) => image,
                        None => {
                            let result = (|| {
                                let file = fs::OpenOptions::new()
                                    .read(true)
                                    .custom_flags(libc::O_NONBLOCK)
                                    .open(&path)
                                    .ok()?;
                                let metadata = file.metadata().ok()?;
                                if !metadata.is_file()
                                    || metadata.len()
                                        > appcore::document::thumbnail::MAX_INPUT_BYTES as u64
                                {
                                    return None;
                                }
                                let mut bytes = Vec::new();
                                file.take(appcore::document::thumbnail::MAX_INPUT_BYTES as u64 + 1)
                                    .read_to_end(&mut bytes)
                                    .ok()?;
                                if bytes.len() > appcore::document::thumbnail::MAX_INPUT_BYTES {
                                    return None;
                                }
                                let thumbnail = provider
                                    .generate(
                                        &bytes,
                                        &extension,
                                        content_type.identifier(),
                                        EDGE,
                                        || task.is_cancel_requested(),
                                    )
                                    .ok()?;
                                ImageData::from_premultiplied_rgba8(
                                    thumbnail.width(),
                                    thumbnail.height(),
                                    thumbnail.pixels().to_vec(),
                                )
                                .ok()
                            })();
                            result
                        }
                    };
                    task.check_cancelled()?;
                    // Replacement during generation must not publish stale pixels.
                    if Stamp::read(&path).as_ref() != Some(&key.source)
                        || Stamp::read(provider.executable()).as_ref() != Some(&key.executable)
                    {
                        continue;
                    }
                    if let Ok(mut shared) = shared.lock() {
                        if shared.publish(generation, key, image) {
                            notify.notify();
                        }
                    }
                }
                Ok(())
            },
            |_| {},
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn key(path: &str, length: u64) -> Key {
        Key {
            path: path.into(),
            source: Stamp {
                length,
                modified: SystemTime::UNIX_EPOCH,
            },
            provider: "org.example.viewer".into(),
            executable: Stamp {
                length: 1,
                modified: SystemTime::UNIX_EPOCH,
            },
        }
    }
    #[test]
    fn stale_directory_results_never_enter_cache() {
        let mut shared = Shared {
            generation: 2,
            ..Shared::default()
        };
        let image = ImageData::from_rgba8(1, 1, vec![255; 4]).unwrap();
        assert!(!shared.publish(1, key("old.png", 1), Some(image.clone())));
        assert!(shared.ready.is_empty() && shared.cache.is_empty());
        assert!(shared.publish(2, key("new.png", 1), Some(image)));
        assert!(shared.ready.contains_key(Path::new("new.png")));
    }
    #[test]
    fn cache_is_bounded_and_source_replacement_changes_key() {
        let mut shared = Shared::default();
        for index in 0..CAPACITY + 5 {
            shared.publish(0, key(&format!("{index}.png"), 1), None);
        }
        assert_eq!(shared.cache.len(), CAPACITY);
        assert_ne!(key("file.png", 1), key("file.png", 2));
        shared.publish(0, key("file.png", 2), None);
        shared.publish(0, key("file.png", 3), None);
        assert_eq!(
            shared
                .cache
                .iter()
                .filter(|entry| entry.key.path == Path::new("file.png"))
                .count(),
            1
        );
    }
}
