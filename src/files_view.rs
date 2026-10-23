use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

use crate::browser::{Browser, EntryKind, FileEntry, ViewMode};
use crate::file_association::{self, Handler as AssociationHandler};
use crate::file_clipboard;
use crate::folder_tree::FolderTree;
use crate::operations::{Collision, Kind, Operation, Outcome};
use crate::sidebar::SidebarBookmarks;
use viewkit::accessibility::{AccessibilityNode, AccessibilityRole};
use viewkit::components::{
    Button, ButtonSize, ButtonStyle, Icon, IconName, Image, Rectangle, RectangleColor, Svg, Text,
};
use viewkit::draw_command::DrawCommand;
use viewkit::event::{ContextMenuItem, ContextMenuRequest, EventContext, EventResult, ViewEvent};
use viewkit::geometry::{Point, Rect, Size};
use viewkit::platform::{CursorIcon, Key, PointerButton};
use viewkit::prelude::{ImageContentMode, ImageData, SvgData};
use viewkit::theme::{Color, Theme};
use viewkit::typography::{TextAlignment, TextRole};
use viewkit::view::{Constraints, MeasureContext, PaintContext, View};

const FOLDER_SVG: &[u8] = include_bytes!("../resources/icons/folder.svg");
const FILE_SVG: &[u8] = include_bytes!("../resources/icons/file.svg");
const APPLICATION_SVG: &[u8] = include_bytes!("../resources/icons/application.svg");

const DOUBLE_CLICK: Duration = Duration::from_millis(500);
const GRID_HEADER_HEIGHT: f32 = 60.0;
const GRID_SIDE_INSET: f32 = 24.0;
const CONTEXT_COMMAND_OPEN: u32 = 1;
const CONTEXT_COMMAND_RELOAD: u32 = 2;
const CONTEXT_COMMAND_NEW_FOLDER: u32 = 3;
const CONTEXT_COMMAND_RENAME: u32 = 4;
const CONTEXT_COMMAND_DELETE: u32 = 5;
const CONTEXT_COMMAND_CONFIRM_DELETE: u32 = 6;
const CONTEXT_COMMAND_CANCEL_DELETE: u32 = 7;
const CONTEXT_COMMAND_ADD_TO_SIDEBAR: u32 = 8;
const CONTEXT_COMMAND_REMOVE_FROM_SIDEBAR: u32 = 9;
const CONTEXT_COMMAND_OPEN_WITH: u32 = 10;
const CONTEXT_COMMAND_SET_DEFAULT: u32 = 11;
const CONTEXT_COMMAND_COPY: u32 = 12;
const CONTEXT_COMMAND_CUT: u32 = 13;
const CONTEXT_COMMAND_PASTE: u32 = 14;
const CONTEXT_COMMAND_OPEN_WITH_FIRST_HANDLER: u32 = 1_000;
const CONTEXT_COMMAND_SET_DEFAULT_FIRST_HANDLER: u32 = 2_000;

const DEFAULT_SIDEBAR_DIRECTORIES: [&str; 6] = [
    "Desktop",
    "Documents",
    "Downloads",
    "Movies",
    "Music",
    "Pictures",
];

fn colors() -> viewkit::theme::BrowserTokens {
    Theme::current().browser
}

#[derive(Clone, Copy)]
enum SidebarIcon {
    Application,
    Folder,
    File,
}

#[derive(Clone)]
struct SidebarItem {
    label: String,
    path: PathBuf,
    icon: SidebarIcon,
}

impl SidebarItem {
    fn new(label: impl Into<String>, path: PathBuf, icon: SidebarIcon) -> Self {
        Self {
            label: label.into(),
            path,
            icon,
        }
    }
}

struct FileIcons {
    application: Option<SvgData>,
    folder: Option<SvgData>,
    file: Option<SvgData>,
    documents: BTreeMap<String, ImageData>,
}

impl FileIcons {
    fn new() -> Self {
        Self {
            application: SvgData::decode(APPLICATION_SVG).ok(),
            folder: SvgData::decode(FOLDER_SVG).ok(),
            file: SvgData::decode(FILE_SVG).ok(),
            documents: BTreeMap::new(),
        }
    }

    fn refresh_documents(&mut self, entries: Vec<FileEntry>) {
        self.documents = load_default_document_icons(&entries);
    }

    fn document(&self, entry: &FileEntry) -> Option<&ImageData> {
        let extension = entry
            .path
            .extension()
            .and_then(|extension| extension.to_str())?
            .to_ascii_lowercase();
        self.documents.get(&extension)
    }

    fn entry(&self, entry: &FileEntry) -> Option<&SvgData> {
        if uses_application_icon(entry) {
            return self.application.as_ref();
        }
        match entry.kind {
            EntryKind::Directory => self.folder.as_ref(),
            EntryKind::Application => self.application.as_ref(),
            EntryKind::Image | EntryKind::Archive | EntryKind::Document | EntryKind::File => {
                self.file.as_ref()
            }
        }
    }

    fn sidebar(&self, icon: SidebarIcon) -> Option<&SvgData> {
        match icon {
            SidebarIcon::Application => self.application.as_ref(),
            SidebarIcon::Folder => self.folder.as_ref(),
            SidebarIcon::File => self.file.as_ref(),
        }
    }
}

fn uses_application_icon(entry: &FileEntry) -> bool {
    entry.kind == EntryKind::Application || entry.path == Path::new("/applications")
}

#[cfg(target_os = "mochios")]
fn load_default_document_icons(entries: &[FileEntry]) -> BTreeMap<String, ImageData> {
    let mut requested_extensions = entries
        .iter()
        .filter(|entry| !entry.is_directory())
        .filter_map(|entry| {
            entry
                .path
                .extension()
                .and_then(|extension| extension.to_str())
                .map(str::to_ascii_lowercase)
        })
        .collect::<Vec<_>>();
    requested_extensions.sort();
    requested_extensions.dedup();

    let applications = installed_application_icons();
    requested_extensions
        .into_iter()
        .filter_map(|extension| {
            let probe = PathBuf::from(format!("document.{extension}"));
            let bundle_id = appcore::document::resolve_default(
                &extension,
                file_association::content_type(&probe),
                appcore::document::AssociationRoles::VIEW,
            )
            .ok()?;
            let icon = applications.get(&bundle_id)?.clone();
            Some((extension, icon))
        })
        .collect()
}

#[cfg(not(target_os = "mochios"))]
fn load_default_document_icons(_entries: &[FileEntry]) -> BTreeMap<String, ImageData> {
    BTreeMap::new()
}

#[cfg(target_os = "mochios")]
fn installed_application_icons() -> BTreeMap<String, ImageData> {
    let mut icons = BTreeMap::new();
    for applications_root in ["/system/applications", "/applications"] {
        let Ok(entries) = std::fs::read_dir(applications_root) else {
            continue;
        };
        for entry in entries.flatten() {
            let root = entry.path();
            let Ok(manifest) = std::fs::read_to_string(root.join("manifest.toml")) else {
                continue;
            };
            let Some(bundle_id) = metadata_string(&manifest, "id") else {
                continue;
            };
            let Some(icon_name) = metadata_string(&manifest, "document_icon")
                .or_else(|| metadata_string(&manifest, "icon"))
            else {
                continue;
            };
            let icon_path = root.join(icon_name);
            let icon = if icon_path.extension().and_then(|value| value.to_str()) == Some("svg") {
                SvgData::from_path(&icon_path)
                    .ok()
                    .and_then(|svg| ImageData::from_svg(&svg, 72, 72).ok())
            } else {
                ImageData::thumbnail_from_path(&icon_path, 72, 72).ok()
            };
            if let Some(icon) = icon {
                icons.entry(bundle_id).or_insert(icon);
            }
        }
    }
    icons
}

#[cfg(target_os = "mochios")]
fn metadata_string(content: &str, key: &str) -> Option<String> {
    content.lines().find_map(|line| {
        let (candidate, value) = line.split_once('=')?;
        if candidate.trim() != key {
            return None;
        }
        value
            .trim()
            .strip_prefix('"')?
            .strip_suffix('"')
            .map(ToOwned::to_owned)
    })
}

fn is_default_sidebar_path(home: &Path, path: &Path) -> bool {
    DEFAULT_SIDEBAR_DIRECTORIES
        .iter()
        .any(|name| home.join(name) == path)
}

fn sidebar_items(home: &Path, bookmarks: &SidebarBookmarks) -> Vec<SidebarItem> {
    let mut items = DEFAULT_SIDEBAR_DIRECTORIES
        .iter()
        .filter(|name| !bookmarks.hides_default(&home.join(name)))
        .map(|name| SidebarItem::new(*name, home.join(name), SidebarIcon::Folder))
        .collect::<Vec<_>>();
    for path in bookmarks.paths() {
        if items.iter().any(|item| item.path == *path) {
            continue;
        }
        let label = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| path.display().to_string());
        let icon = if path.is_dir() {
            if path.file_name().is_some_and(|name| {
                name.to_string_lossy()
                    .to_ascii_lowercase()
                    .ends_with(".app")
            }) {
                SidebarIcon::Application
            } else {
                SidebarIcon::Folder
            }
        } else {
            SidebarIcon::File
        };
        items.push(SidebarItem::new(label, path.clone(), icon));
    }
    items
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum HitTarget {
    Back,
    Forward,
    ListMode,
    GridMode,
    Path,
    Search,
    Sidebar(usize),
    TreeExpand(usize),
    TreeNavigate(usize),
    Entry(usize),
    Content,
}

fn request_hover_redraw(
    layout: &Layout,
    target: Option<&HitTarget>,
    context: &mut EventContext<'_>,
) {
    let Some(target) = target else {
        return;
    };
    let region = match target {
        HitTarget::Sidebar(_) | HitTarget::TreeExpand(_) | HitTarget::TreeNavigate(_) => {
            layout.sidebar
        }
        HitTarget::Entry(_) | HitTarget::Content => layout.content,
        _ => layout.toolbar,
    };
    context.request_redraw_in(region);
}

struct FilesState {
    browser: Browser,
    home_directory: PathBuf,
    sidebar_items: Vec<SidebarItem>,
    sidebar_bookmarks: SidebarBookmarks,
    folder_tree: FolderTree,
    sidebar_scroll: f32,
    icons: FileIcons,
    scroll: f32,
    hover: Option<HitTarget>,
    path_focused: bool,
    path_input: String,
    path_replace_on_input: bool,
    search_focused: bool,
    last_click: Option<(PathBuf, Instant)>,
    next_context_request: u64,
    active_context_request: Option<u64>,
    context_anchor: Point,
    context_sidebar_path: Option<PathBuf>,
    name_edit: Option<NameEdit>,
    pending_delete: Option<PathBuf>,
    operation: Option<Operation>,
    open_with_path: Option<PathBuf>,
    open_with_handlers: Vec<AssociationHandler>,
    file_acceptance: Option<Rc<dyn Fn(&Path) -> bool>>,
    file_activation: Option<Rc<dyn Fn(&Path) -> Result<(), String>>>,
}

struct NameEdit {
    path: PathBuf,
    value: String,
    replace_on_input: bool,
}

fn refresh_document_icons(state: &mut FilesState) {
    let entries = state
        .browser
        .entries()
        .into_iter()
        .cloned()
        .collect::<Vec<_>>();
    state.icons.refresh_documents(entries);
}

#[derive(Clone)]
pub(crate) struct FilesView {
    state: Rc<RefCell<FilesState>>,
    operation_action: Rc<Cell<Option<OperationAction>>>,
    operation_buttons: Rc<Vec<Button>>,
}

#[derive(Clone, Copy)]
enum OperationAction {
    Cancel,
    Replace,
    KeepBoth,
    Skip,
}

impl FilesView {
    pub(crate) fn new() -> Self {
        let initial_directory = std::env::var_os("MOCHIOS_PREVIEW_ROOT")
            .or_else(|| std::env::var_os("HOME"))
            .map(PathBuf::from)
            .filter(|path| path.is_dir())
            .unwrap_or_else(|| PathBuf::from("/"));
        Self::new_with_activation(initial_directory, None, None)
    }

    pub(crate) fn new_picker(
        initial_directory: impl Into<PathBuf>,
        file_acceptance: impl Fn(&Path) -> bool + 'static,
        file_activation: impl Fn(&Path) -> Result<(), String> + 'static,
    ) -> Self {
        Self::new_with_activation(
            initial_directory.into(),
            Some(Rc::new(file_acceptance)),
            Some(Rc::new(file_activation)),
        )
    }

    fn new_with_activation(
        initial_directory: PathBuf,
        file_acceptance: Option<Rc<dyn Fn(&Path) -> bool>>,
        file_activation: Option<Rc<dyn Fn(&Path) -> Result<(), String>>>,
    ) -> Self {
        let browser = Browser::new(initial_directory);
        let home = std::env::var_os("MOCHIOS_PREVIEW_ROOT")
            .or_else(|| std::env::var_os("HOME"))
            .map(PathBuf::from)
            .filter(|path| path.is_dir())
            .unwrap_or_else(|| browser.current_dir().to_path_buf());
        let mut icons = FileIcons::new();
        icons.refresh_documents(browser.entries().into_iter().cloned().collect());
        let sidebar_bookmarks = SidebarBookmarks::load();
        let sidebar_items = sidebar_items(&home, &sidebar_bookmarks);
        let folder_tree = FolderTree::new(browser.root_directory().to_path_buf());
        let operation_action = Rc::new(Cell::new(None));
        let operation_buttons = [
            ("Cancel", OperationAction::Cancel),
            ("Replace", OperationAction::Replace),
            ("Keep Both", OperationAction::KeepBoth),
            ("Skip", OperationAction::Skip),
        ]
        .into_iter()
        .map(|(label, action)| {
            let pending = operation_action.clone();
            Button::new(label)
                .size(ButtonSize::Small)
                .style(ButtonStyle::Standard)
                .on_click(move || pending.set(Some(action)))
        })
        .collect();
        Self {
            operation_action,
            operation_buttons: Rc::new(operation_buttons),
            state: Rc::new(RefCell::new(FilesState {
                browser,
                home_directory: home,
                sidebar_items,
                sidebar_bookmarks,
                folder_tree,
                sidebar_scroll: 0.0,
                icons,
                scroll: 0.0,
                hover: None,
                path_focused: false,
                path_input: String::new(),
                path_replace_on_input: false,
                search_focused: false,
                last_click: None,
                next_context_request: 0,
                active_context_request: None,
                context_anchor: Point::new(0.0, 0.0),
                context_sidebar_path: None,
                name_edit: None,
                pending_delete: None,
                operation: None,
                open_with_path: None,
                open_with_handlers: Vec::new(),
                file_acceptance,
                file_activation,
            })),
        }
    }

    pub(crate) fn current_directory(&self) -> PathBuf {
        self.state.borrow().browser.current_dir().to_path_buf()
    }

    pub(crate) fn report_error(&self, message: impl Into<String>) {
        self.state.borrow_mut().browser.report_error(message);
    }

    pub(crate) fn activate_selected(&self) -> bool {
        Self::open_selected(&mut self.state.borrow_mut())
    }

    fn navigate(state: &mut FilesState, path: impl Into<PathBuf>) -> bool {
        if state.browser.navigate(path) {
            refresh_document_icons(state);
            state.scroll = 0.0;
            state.last_click = None;
            return true;
        }
        false
    }

    fn activate_entry(state: &mut FilesState, index: usize) -> bool {
        let Some(entry) = state.browser.entries().get(index).cloned().cloned() else {
            return false;
        };

        state.browser.select(entry.path.clone());
        let now = Instant::now();
        let is_double_click = state.last_click.as_ref().is_some_and(|(path, clicked_at)| {
            path == &entry.path && now.saturating_duration_since(*clicked_at) <= DOUBLE_CLICK
        });
        state.last_click = Some((entry.path.clone(), now));
        if is_double_click {
            return Self::open_path(state, &entry);
        }
        true
    }

    fn open_path(state: &mut FilesState, entry: &FileEntry) -> bool {
        if entry.is_directory() {
            return Self::navigate(state, entry.path.clone());
        }
        if state
            .file_acceptance
            .as_ref()
            .is_some_and(|accepts| !accepts(&entry.path))
        {
            state.browser.report_error("This file type is not allowed");
            return true;
        }
        if let Some(activate) = state.file_activation.clone() {
            match activate(&entry.path) {
                Ok(()) => state.browser.clear_error(),
                Err(error) => state.browser.report_error(error),
            }
            return true;
        }
        match file_association::open(&entry.path, None) {
            Ok(()) => {
                state.browser.clear_error();
                true
            }
            Err(error) => {
                state.browser.report_error(error);
                true
            }
        }
    }

    fn open_selected(state: &mut FilesState) -> bool {
        let Some(path) = state.browser.selected().map(Path::to_path_buf) else {
            return false;
        };
        let Some(entry) = state
            .browser
            .entries()
            .into_iter()
            .find(|entry| entry.path == path)
            .cloned()
        else {
            return false;
        };
        Self::open_path(state, &entry)
    }

    fn activate_sidebar_item(state: &mut FilesState, index: usize) -> bool {
        let Some(path) = state.sidebar_items.get(index).map(|item| item.path.clone()) else {
            return false;
        };
        if path.is_dir() {
            return Self::navigate(state, path);
        }
        let Some(parent) = path.parent().map(Path::to_path_buf) else {
            return false;
        };
        if !Self::navigate(state, parent) {
            return false;
        }
        state.browser.select(path);
        true
    }

    fn add_selected_to_sidebar(state: &mut FilesState) -> bool {
        let Some(path) = state.browser.selected().map(Path::to_path_buf) else {
            return false;
        };
        if state.sidebar_items.iter().any(|item| item.path == path) {
            return false;
        }
        let mut updated = state.sidebar_bookmarks.clone();
        let restored_default =
            is_default_sidebar_path(&state.home_directory, &path) && updated.restore_default(&path);
        if !restored_default && !updated.add(path) {
            return false;
        }
        if let Err(error) = updated.save() {
            eprintln!("Files: failed to save sidebar bookmarks: {error}");
            state
                .browser
                .report_error(format!("Cannot save sidebar: {error}"));
            return false;
        }
        state.sidebar_bookmarks = updated;
        state.sidebar_items = sidebar_items(&state.home_directory, &state.sidebar_bookmarks);
        state.browser.clear_error();
        true
    }

    fn remove_context_item_from_sidebar(state: &mut FilesState) -> bool {
        let Some(path) = state.context_sidebar_path.take() else {
            return false;
        };
        let mut updated = state.sidebar_bookmarks.clone();
        let removed = if is_default_sidebar_path(&state.home_directory, &path) {
            updated.hide_default(path.clone())
        } else {
            updated.remove(&path)
        };
        if !removed {
            return false;
        }
        if let Err(error) = updated.save() {
            eprintln!("Files: failed to save sidebar bookmarks: {error}");
            state
                .browser
                .report_error(format!("Cannot save sidebar: {error}"));
            return false;
        }
        state.sidebar_bookmarks = updated;
        state.sidebar_items = sidebar_items(&state.home_directory, &state.sidebar_bookmarks);
        state.browser.clear_error();
        true
    }

    fn begin_rename(state: &mut FilesState, path: PathBuf, name: String) {
        state.browser.select(path.clone());
        state.name_edit = Some(NameEdit {
            path,
            value: name,
            replace_on_input: true,
        });
        state.path_focused = false;
        state.search_focused = false;
    }

    fn commit_name_edit(state: &mut FilesState) -> bool {
        let Some(edit) = state.name_edit.take() else {
            return true;
        };
        state.browser.select(edit.path.clone());
        if state.browser.rename_selected(&edit.value) {
            true
        } else {
            state.name_edit = Some(edit);
            false
        }
    }

    fn clipboard_selection(state: &mut FilesState, kind: Kind) -> bool {
        if let Some(path) = state.browser.selected() {
            if let Err(error) = file_clipboard::set(kind, path) {
                state.browser.report_error(error);
            }
        }
        true
    }

    fn start_operation(state: &mut FilesState, kind: Kind, source: PathBuf) -> bool {
        if state.operation.is_some() || !Self::commit_name_edit(state) {
            return false;
        }
        // The preview must never modify files outside its isolated home.
        let root = state.browser.root_directory();
        if !source
            .parent()
            .and_then(|p| std::fs::canonicalize(p).ok())
            .is_some_and(|p| p.starts_with(root))
            || source.file_name().is_none()
        {
            state
                .browser
                .report_error("The item is outside the available filesystem");
            return true;
        }
        match Operation::new(kind, source, state.browser.current_dir()) {
            Ok(operation) => state.operation = Some(operation),
            Err(error) => state.browser.report_error(error.to_string()),
        }
        true
    }

    fn paste(state: &mut FilesState) -> bool {
        if let Some((kind, source)) = file_clipboard::get() {
            Self::start_operation(state, kind, source)
        } else {
            false
        }
    }

    fn operation_frame(bounds: Rect) -> Rect {
        let content = Layout::new(bounds).content;
        Rect::new(
            content.origin.x,
            (bounds.origin.y + bounds.size.height - 88.0).max(content.origin.y),
            content.size.width,
            88.0_f32.min(content.size.height),
        )
    }

    fn operation_button(frame: Rect, index: usize) -> Rect {
        let width = ((frame.size.width - 40.0) / 4.0).clamp(0.0, 84.0);
        Rect::new(
            frame.origin.x + frame.size.width - 16.0 - width - index as f32 * (width + 8.0),
            frame.origin.y + 45.0,
            width,
            28.0,
        )
    }

    fn advance_operation(&self) {
        let mut state = self.state.borrow_mut();
        let Some(operation) = state.operation.as_mut() else {
            return;
        };
        operation.tick();
        let result = operation.take_result();
        if let Some(result) = result {
            let kind = operation.kind;
            let source = operation.source.clone();
            state.operation = None;
            state.browser.reload();
            state.folder_tree = FolderTree::new(state.browser.root_directory().to_path_buf());
            refresh_document_icons(&mut state);
            match result {
                Ok(Outcome::Completed(destination)) => {
                    if kind == Kind::Move {
                        file_clipboard::clear_move(&source);
                    }
                    if let Some(path) = destination {
                        if path.parent() == Some(state.browser.current_dir()) {
                            state.browser.select(path);
                        }
                    }
                }
                Err(error) => state.browser.report_error(error),
                _ => {}
            }
        }
    }

    fn show_context_menu(
        state: &mut FilesState,
        position: Point,
        items: Vec<ContextMenuItem>,
        context: &mut EventContext<'_>,
    ) {
        state.next_context_request = state.next_context_request.wrapping_add(1).max(1);
        let request_id = state.next_context_request;
        state.active_context_request = Some(request_id);
        state.context_anchor = position;
        context.show_context_menu(ContextMenuRequest {
            request_id,
            position,
            items,
        });
    }
}

impl View for FilesView {
    fn measure(&self, constraints: Constraints, _context: &mut MeasureContext<'_>) -> Size {
        constraints.constrain(constraints.maximum)
    }

    fn paint(&self, bounds: Rect, context: &mut PaintContext<'_>) {
        self.advance_operation();
        let layout = Layout::new(bounds);
        let state = self.state.borrow();

        Rectangle::new()
            .color(RectangleColor::Custom(colors().window_background))
            .paint(bounds, context);
        paint_toolbar(&layout, &state, context);
        paint_sidebar(&layout, &state, context);
        paint_content(&layout, &state, context);
        if let Some(operation) = &state.operation {
            let frame = Self::operation_frame(bounds);
            Rectangle::new()
                .color(RectangleColor::Custom(context.theme.colors.surface_subtle))
                .paint(frame, context);
            let message = if let Some(target) = operation.conflict() {
                format!(
                    "“{}” already exists",
                    target.file_name().unwrap_or_default().to_string_lossy()
                )
            } else {
                let label = operation.label();
                let name = operation
                    .source
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy();
                if operation.kind == Kind::Delete {
                    format!("{label} “{name}” — {} items (permanent)", operation.items)
                } else {
                    format!(
                        "{label} “{name}” — {} items · {}",
                        operation.items,
                        crate::browser::format_size(operation.bytes)
                    )
                }
            };
            Text::label(message).paint(
                Rect::new(
                    frame.origin.x + 16.0,
                    frame.origin.y + 12.0,
                    (frame.size.width - 32.0).max(0.0),
                    22.0,
                ),
                context,
            );
            for (index, button) in self.operation_buttons.iter().enumerate() {
                if (index == 0 && operation.cancellable())
                    || (index > 0 && operation.conflict().is_some())
                {
                    button.paint(Self::operation_button(frame, index), context);
                }
            }
            if operation.conflict().is_none() {
                context.request_redraw_at(Instant::now() + Duration::from_millis(16));
            }
        }
    }

    fn handle_event(
        &self,
        bounds: Rect,
        event: &ViewEvent,
        context: &mut EventContext<'_>,
    ) -> EventResult {
        let layout = Layout::new(bounds);
        if self.state.borrow().operation.is_some() {
            let frame = Self::operation_frame(bounds);
            let (cancellable, conflict) = {
                let state = self.state.borrow();
                let operation = state.operation.as_ref().unwrap();
                (operation.cancellable(), operation.conflict().is_some())
            };
            for (index, button) in self.operation_buttons.iter().enumerate() {
                if (index == 0 && cancellable) || (index > 0 && conflict) {
                    button.handle_event(Self::operation_button(frame, index), event, context);
                }
            }
            if matches!(
                event,
                ViewEvent::KeyPressed {
                    key: Key::Escape,
                    ..
                }
            ) && cancellable
            {
                self.operation_action.set(Some(OperationAction::Cancel));
            }
            if let Some(action) = self.operation_action.take() {
                let mut state = self.state.borrow_mut();
                if let Some(operation) = state.operation.as_mut() {
                    let result = match action {
                        OperationAction::Cancel => {
                            operation.cancel();
                            Ok(())
                        }
                        OperationAction::Replace => operation.resolve(Collision::Replace),
                        OperationAction::KeepBoth => operation.resolve(Collision::KeepBoth),
                        OperationAction::Skip => operation.resolve(Collision::Skip),
                    };
                    if let Err(error) = result {
                        state.browser.report_error(error.to_string());
                    }
                }
                context.request_redraw_in(bounds);
                return EventResult::Consumed;
            }
            if event.position().is_some_and(|p| frame.contains(p)) {
                return EventResult::Consumed;
            }
        }
        match event {
            ViewEvent::PointerMoved { position } => {
                let mut state = self.state.borrow_mut();
                let target = hit_test(&layout, *position, &state);
                if state.hover != target {
                    request_hover_redraw(&layout, state.hover.as_ref(), context);
                    request_hover_redraw(&layout, target.as_ref(), context);
                    state.hover = target.clone();
                }
                if matches!(target, Some(HitTarget::Path | HitTarget::Search)) {
                    context.set_cursor(CursorIcon::Text);
                } else if target.is_some() {
                    context.set_cursor(CursorIcon::Pointer);
                }
                EventResult::Consumed
            }
            ViewEvent::PointerLeft => {
                let mut state = self.state.borrow_mut();
                if let Some(target) = state.hover.take() {
                    request_hover_redraw(&layout, Some(&target), context);
                }
                EventResult::Consumed
            }
            ViewEvent::PointerPressed {
                position,
                button: PointerButton::Primary,
            } => {
                let mut state = self.state.borrow_mut();
                let target = hit_test(&layout, *position, &state);
                let clicked_path = match target {
                    Some(HitTarget::Entry(index)) => state
                        .browser
                        .entries()
                        .get(index)
                        .map(|entry| entry.path.clone()),
                    _ => None,
                };
                if let Some(edit) = state.name_edit.as_ref() {
                    if clicked_path.as_ref() == Some(&edit.path) {
                        return EventResult::Consumed;
                    }
                    if !Self::commit_name_edit(&mut state) {
                        context.request_redraw_in(layout.content);
                        return EventResult::Consumed;
                    }
                }
                let path_clicked = matches!(target, Some(HitTarget::Path));
                if path_clicked && !state.path_focused {
                    state.path_input = state.browser.current_dir().display().to_string();
                    state.path_replace_on_input = true;
                }
                state.path_focused = path_clicked;
                state.search_focused = matches!(target, Some(HitTarget::Search));
                let changed = match target {
                    Some(HitTarget::Back) => {
                        let changed = state.browser.go_back();
                        state.scroll = 0.0;
                        changed
                    }
                    Some(HitTarget::Forward) => {
                        let changed = state.browser.go_forward();
                        state.scroll = 0.0;
                        changed
                    }
                    Some(HitTarget::ListMode) => {
                        state.browser.set_view_mode(ViewMode::List);
                        state.scroll = 0.0;
                        true
                    }
                    Some(HitTarget::GridMode) => {
                        state.browser.set_view_mode(ViewMode::Grid);
                        state.scroll = 0.0;
                        true
                    }
                    Some(HitTarget::Sidebar(index)) => {
                        Self::activate_sidebar_item(&mut state, index)
                    }
                    Some(HitTarget::TreeExpand(index)) => {
                        if let Some(row) = state.folder_tree.rows().get(index) {
                            let path = row.path.clone();
                            if let Err(error) = state.folder_tree.toggle(&path) {
                                state
                                    .browser
                                    .report_error(format!("Cannot expand folder: {error}"));
                            }
                            state.sidebar_scroll = state
                                .sidebar_scroll
                                .min(maximum_sidebar_scroll(&layout, &state));
                        }
                        true
                    }
                    Some(HitTarget::TreeNavigate(index)) => state
                        .folder_tree
                        .rows()
                        .get(index)
                        .map(|row| row.path.clone())
                        .is_some_and(|path| Self::navigate(&mut state, path)),
                    Some(HitTarget::Entry(index)) => Self::activate_entry(&mut state, index),
                    Some(HitTarget::Content) => {
                        state.browser.clear_selection();
                        state.last_click = None;
                        true
                    }
                    Some(HitTarget::Path | HitTarget::Search) | None => true,
                };
                if changed {
                    context.request_redraw_in(bounds);
                }
                EventResult::Consumed
            }
            ViewEvent::PointerPressed {
                position,
                button: PointerButton::Secondary,
            } => {
                let mut state = self.state.borrow_mut();
                if !Self::commit_name_edit(&mut state) {
                    context.request_redraw_in(layout.content);
                    return EventResult::Consumed;
                }
                let target = hit_test(&layout, *position, &state);
                let sidebar_path = match target.as_ref() {
                    Some(HitTarget::Sidebar(index)) => state
                        .sidebar_items
                        .get(*index)
                        .map(|item| item.path.clone()),
                    _ => None,
                };
                state.context_sidebar_path = sidebar_path.clone();
                let selected_entry = if let Some(HitTarget::Entry(index)) = target.as_ref() {
                    let entry = state.browser.entries().get(*index).cloned().cloned();
                    if let Some(entry) = entry {
                        state.browser.select(entry.path.clone());
                        Some(entry)
                    } else {
                        None
                    }
                } else {
                    state.browser.clear_selection();
                    None
                };
                state.open_with_path = None;
                state.open_with_handlers.clear();
                let is_sidebar = sidebar_path.is_some();
                let mut items = if let Some(_path) = sidebar_path {
                    vec![ContextMenuItem {
                        command_id: CONTEXT_COMMAND_REMOVE_FROM_SIDEBAR,
                        label: String::from("Remove from Sidebar"),
                        enabled: true,
                        checked: false,
                        destructive: false,
                        separator: false,
                    }]
                } else if let Some(entry) = selected_entry {
                    let sidebar_contains = state
                        .sidebar_items
                        .iter()
                        .any(|item| item.path == entry.path);
                    state.open_with_path = (!entry.is_directory()).then(|| entry.path.clone());
                    state.open_with_handlers = if entry.is_directory() {
                        Vec::new()
                    } else {
                        match file_association::handlers(&entry.path) {
                            Ok(handlers) => handlers,
                            Err(error) => {
                                state.browser.report_error(error);
                                Vec::new()
                            }
                        }
                    };
                    let mut items = vec![ContextMenuItem {
                        command_id: CONTEXT_COMMAND_OPEN,
                        label: String::from("Open"),
                        enabled: true,
                        checked: false,
                        destructive: false,
                        separator: false,
                    }];
                    if !entry.is_directory() {
                        items.push(ContextMenuItem {
                            command_id: CONTEXT_COMMAND_OPEN_WITH,
                            label: String::from("Open With…"),
                            enabled: !state.open_with_handlers.is_empty(),
                            checked: false,
                            destructive: false,
                            separator: false,
                        });
                        items.push(ContextMenuItem {
                            command_id: CONTEXT_COMMAND_SET_DEFAULT,
                            label: String::from("Set Default Application…"),
                            enabled: !state.open_with_handlers.is_empty(),
                            checked: false,
                            destructive: false,
                            separator: false,
                        });
                    }
                    items.extend([
                        ContextMenuItem {
                            command_id: CONTEXT_COMMAND_ADD_TO_SIDEBAR,
                            label: String::from("Add to Sidebar"),
                            enabled: !sidebar_contains,
                            checked: false,
                            destructive: false,
                            separator: false,
                        },
                        ContextMenuItem {
                            command_id: 0,
                            label: String::new(),
                            enabled: false,
                            checked: false,
                            destructive: false,
                            separator: true,
                        },
                        ContextMenuItem {
                            command_id: CONTEXT_COMMAND_RENAME,
                            label: String::from("Rename"),
                            enabled: true,
                            checked: false,
                            destructive: false,
                            separator: false,
                        },
                        ContextMenuItem {
                            command_id: CONTEXT_COMMAND_DELETE,
                            label: String::from("Delete"),
                            enabled: true,
                            checked: false,
                            destructive: true,
                            separator: false,
                        },
                        ContextMenuItem {
                            command_id: 0,
                            label: String::new(),
                            enabled: false,
                            checked: false,
                            destructive: false,
                            separator: true,
                        },
                        ContextMenuItem {
                            command_id: CONTEXT_COMMAND_NEW_FOLDER,
                            label: String::from("New Folder"),
                            enabled: true,
                            checked: false,
                            destructive: false,
                            separator: false,
                        },
                        ContextMenuItem {
                            command_id: CONTEXT_COMMAND_RELOAD,
                            label: String::from("Reload"),
                            enabled: true,
                            checked: false,
                            destructive: false,
                            separator: false,
                        },
                    ]);
                    items
                } else {
                    vec![
                        ContextMenuItem {
                            command_id: CONTEXT_COMMAND_NEW_FOLDER,
                            label: String::from("New Folder"),
                            enabled: true,
                            checked: false,
                            destructive: false,
                            separator: false,
                        },
                        ContextMenuItem {
                            command_id: 0,
                            label: String::new(),
                            enabled: false,
                            checked: false,
                            destructive: false,
                            separator: true,
                        },
                        ContextMenuItem {
                            command_id: CONTEXT_COMMAND_RELOAD,
                            label: String::from("Reload"),
                            enabled: true,
                            checked: false,
                            destructive: false,
                            separator: false,
                        },
                    ]
                };
                if !is_sidebar {
                    let idle = state.operation.is_none();
                    let selected = state.browser.selected().is_some();
                    for (id, label, enabled) in [
                        (CONTEXT_COMMAND_COPY, "Copy", selected),
                        (CONTEXT_COMMAND_CUT, "Cut", selected && idle),
                        (
                            CONTEXT_COMMAND_PASTE,
                            "Paste",
                            idle && file_clipboard::get().is_some(),
                        ),
                    ] {
                        items.push(ContextMenuItem {
                            command_id: id,
                            label: label.into(),
                            enabled,
                            checked: false,
                            destructive: false,
                            separator: false,
                        });
                    }
                    if !idle {
                        for item in &mut items {
                            if matches!(
                                item.command_id,
                                CONTEXT_COMMAND_NEW_FOLDER
                                    | CONTEXT_COMMAND_RENAME
                                    | CONTEXT_COMMAND_DELETE
                            ) {
                                item.enabled = false;
                            }
                        }
                    }
                }
                Self::show_context_menu(&mut state, *position, items, context);
                context.request_redraw_in(layout.content);
                EventResult::Consumed
            }
            ViewEvent::ContextMenuResult {
                request_id,
                command_id,
            } => {
                let mut state = self.state.borrow_mut();
                if state.active_context_request != Some(*request_id) {
                    return EventResult::Ignored;
                }
                state.active_context_request = None;
                let sidebar_command = matches!(
                    *command_id,
                    Some(CONTEXT_COMMAND_ADD_TO_SIDEBAR | CONTEXT_COMMAND_REMOVE_FROM_SIDEBAR)
                );
                let changed = match command_id {
                    Some(CONTEXT_COMMAND_COPY) => Self::clipboard_selection(&mut state, Kind::Copy),
                    Some(CONTEXT_COMMAND_CUT) => Self::clipboard_selection(&mut state, Kind::Move),
                    Some(CONTEXT_COMMAND_PASTE) => Self::paste(&mut state),
                    Some(CONTEXT_COMMAND_OPEN) => Self::open_selected(&mut state),
                    Some(CONTEXT_COMMAND_OPEN_WITH) => {
                        let items = state
                            .open_with_handlers
                            .iter()
                            .enumerate()
                            .filter_map(|(index, handler)| {
                                let command_id = CONTEXT_COMMAND_OPEN_WITH_FIRST_HANDLER
                                    .checked_add(u32::try_from(index).ok()?)?;
                                Some(ContextMenuItem {
                                    command_id,
                                    label: handler.name.clone(),
                                    enabled: true,
                                    checked: false,
                                    destructive: false,
                                    separator: false,
                                })
                            })
                            .collect();
                        let anchor = state.context_anchor;
                        Self::show_context_menu(&mut state, anchor, items, context);
                        false
                    }
                    Some(CONTEXT_COMMAND_SET_DEFAULT) => {
                        let items = state
                            .open_with_handlers
                            .iter()
                            .enumerate()
                            .filter_map(|(index, handler)| {
                                let command_id = CONTEXT_COMMAND_SET_DEFAULT_FIRST_HANDLER
                                    .checked_add(u32::try_from(index).ok()?)?;
                                Some(ContextMenuItem {
                                    command_id,
                                    label: format!("Use {} by Default", handler.name),
                                    enabled: true,
                                    checked: false,
                                    destructive: false,
                                    separator: false,
                                })
                            })
                            .collect();
                        let anchor = state.context_anchor;
                        Self::show_context_menu(&mut state, anchor, items, context);
                        false
                    }
                    Some(CONTEXT_COMMAND_NEW_FOLDER) => {
                        let created = state.browser.create_folder();
                        if let Some(path) = created {
                            let name = path
                                .file_name()
                                .map(|name| name.to_string_lossy().into_owned())
                                .unwrap_or_default();
                            Self::begin_rename(&mut state, path, name);
                        }
                        true
                    }
                    Some(CONTEXT_COMMAND_RENAME) => {
                        let selected = state.browser.selected().map(Path::to_path_buf);
                        if let Some(path) = selected {
                            let name = path
                                .file_name()
                                .map(|name| name.to_string_lossy().into_owned())
                                .unwrap_or_default();
                            Self::begin_rename(&mut state, path, name);
                        }
                        true
                    }
                    Some(CONTEXT_COMMAND_DELETE) => {
                        state.pending_delete = state.browser.selected().map(Path::to_path_buf);
                        if let Some(path) = state.pending_delete.as_ref() {
                            let name = path
                                .file_name()
                                .map(|name| name.to_string_lossy().into_owned())
                                .unwrap_or_else(|| path.display().to_string());
                            let items = vec![
                                ContextMenuItem {
                                    command_id: CONTEXT_COMMAND_CONFIRM_DELETE,
                                    label: format!("Permanently Delete \"{name}\""),
                                    enabled: true,
                                    checked: false,
                                    destructive: true,
                                    separator: false,
                                },
                                ContextMenuItem {
                                    command_id: CONTEXT_COMMAND_CANCEL_DELETE,
                                    label: String::from("Cancel"),
                                    enabled: true,
                                    checked: false,
                                    destructive: false,
                                    separator: false,
                                },
                            ];
                            let anchor = state.context_anchor;
                            Self::show_context_menu(&mut state, anchor, items, context);
                        }
                        false
                    }
                    Some(CONTEXT_COMMAND_CONFIRM_DELETE) => {
                        if let Some(path) = state.pending_delete.take() {
                            Self::start_operation(&mut state, Kind::Delete, path)
                        } else {
                            false
                        }
                    }
                    Some(CONTEXT_COMMAND_CANCEL_DELETE) | None => {
                        state.pending_delete = None;
                        false
                    }
                    Some(CONTEXT_COMMAND_RELOAD) => {
                        state.browser.reload();
                        refresh_document_icons(&mut state);
                        true
                    }
                    Some(CONTEXT_COMMAND_ADD_TO_SIDEBAR) => {
                        Self::add_selected_to_sidebar(&mut state)
                    }
                    Some(CONTEXT_COMMAND_REMOVE_FROM_SIDEBAR) => {
                        Self::remove_context_item_from_sidebar(&mut state)
                    }
                    Some(command_id)
                        if *command_id >= CONTEXT_COMMAND_SET_DEFAULT_FIRST_HANDLER =>
                    {
                        let index =
                            (*command_id - CONTEXT_COMMAND_SET_DEFAULT_FIRST_HANDLER) as usize;
                        let path = state.open_with_path.clone();
                        let bundle_id = state
                            .open_with_handlers
                            .get(index)
                            .map(|handler| handler.bundle_id.clone());
                        match (path, bundle_id) {
                            (Some(path), Some(bundle_id)) => {
                                match file_association::set_default(&path, &bundle_id) {
                                    Ok(()) => {
                                        state.browser.clear_error();
                                        refresh_document_icons(&mut state);
                                    }
                                    Err(error) => state.browser.report_error(error),
                                }
                                true
                            }
                            _ => false,
                        }
                    }
                    Some(command_id) if *command_id >= CONTEXT_COMMAND_OPEN_WITH_FIRST_HANDLER => {
                        let index =
                            (*command_id - CONTEXT_COMMAND_OPEN_WITH_FIRST_HANDLER) as usize;
                        let path = state.open_with_path.clone();
                        let bundle_id = state
                            .open_with_handlers
                            .get(index)
                            .map(|handler| handler.bundle_id.clone());
                        match (path, bundle_id) {
                            (Some(path), Some(bundle_id)) => {
                                match file_association::open(&path, Some(&bundle_id)) {
                                    Ok(()) => state.browser.clear_error(),
                                    Err(error) => state.browser.report_error(error),
                                }
                                true
                            }
                            _ => false,
                        }
                    }
                    _ => false,
                };
                if changed {
                    state.scroll = 0.0;
                }
                if changed || sidebar_command {
                    context.request_redraw_in(bounds);
                }
                EventResult::Consumed
            }
            ViewEvent::Scroll {
                position, delta_y, ..
            } if layout.sidebar.contains(*position) => {
                let mut state = self.state.borrow_mut();
                let maximum = maximum_sidebar_scroll(&layout, &state);
                state.sidebar_scroll = (state.sidebar_scroll - *delta_y * 36.0).clamp(0.0, maximum);
                context.request_redraw_in(layout.sidebar);
                EventResult::Consumed
            }
            ViewEvent::Scroll {
                position, delta_y, ..
            } if layout.content.contains(*position) => {
                let mut state = self.state.borrow_mut();
                let maximum = maximum_scroll(&layout, &state);
                let previous = state.scroll;
                state.scroll = (state.scroll - *delta_y * 36.0).clamp(0.0, maximum);
                if (state.scroll - previous).abs() > f32::EPSILON {
                    context.request_redraw_in(layout.content);
                }
                EventResult::Consumed
            }
            ViewEvent::TextInput { text } => {
                let mut state = self.state.borrow_mut();
                if let Some(edit) = state.name_edit.as_mut() {
                    if edit.replace_on_input {
                        edit.value.clear();
                        edit.replace_on_input = false;
                    }
                    edit.value.push_str(text);
                    context.request_redraw_in(layout.content);
                    return EventResult::Consumed;
                }
                if state.path_focused {
                    if state.path_replace_on_input {
                        state.path_input.clear();
                        state.path_replace_on_input = false;
                    }
                    state.path_input.push_str(text);
                    context.request_redraw_in(layout.toolbar);
                    return EventResult::Consumed;
                }
                if !state.search_focused {
                    return EventResult::Ignored;
                }
                state.browser.push_search(text);
                state.scroll = 0.0;
                context.request_redraw_in(bounds);
                EventResult::Consumed
            }
            ViewEvent::Backspace => {
                let mut state = self.state.borrow_mut();
                if let Some(edit) = state.name_edit.as_mut() {
                    if edit.replace_on_input {
                        edit.value.clear();
                        edit.replace_on_input = false;
                    } else {
                        edit.value.pop();
                    }
                    context.request_redraw_in(layout.content);
                    return EventResult::Consumed;
                }
                if state.path_focused {
                    if state.path_replace_on_input {
                        state.path_input.clear();
                        state.path_replace_on_input = false;
                    } else {
                        state.path_input.pop();
                    }
                    context.request_redraw_in(layout.toolbar);
                    return EventResult::Consumed;
                }
                if !state.search_focused {
                    return EventResult::Ignored;
                }
                state.browser.pop_search();
                state.scroll = 0.0;
                context.request_redraw_in(bounds);
                EventResult::Consumed
            }
            ViewEvent::KeyPressed { key, modifiers } => {
                let mut state = self.state.borrow_mut();
                if modifiers.shortcut()
                    && !state.path_focused
                    && !state.search_focused
                    && state.name_edit.is_none()
                {
                    let handled = match key {
                        Key::Character('c' | 'C') => {
                            Self::clipboard_selection(&mut state, Kind::Copy)
                        }
                        Key::Character('x' | 'X') if state.operation.is_none() => {
                            Self::clipboard_selection(&mut state, Kind::Move)
                        }
                        Key::Character('v' | 'V') => Self::paste(&mut state),
                        _ => false,
                    };
                    if handled {
                        context.request_redraw_in(bounds);
                        return EventResult::Consumed;
                    }
                }
                let changed = match key {
                    Key::Escape if state.name_edit.is_some() => {
                        state.name_edit = None;
                        true
                    }
                    Key::Enter if state.name_edit.is_some() => Self::commit_name_edit(&mut state),
                    Key::Escape if state.path_focused => {
                        state.path_focused = false;
                        state.path_input.clear();
                        state.path_replace_on_input = false;
                        true
                    }
                    Key::Escape if state.search_focused => {
                        state.browser.clear_search();
                        state.search_focused = false;
                        state.scroll = 0.0;
                        true
                    }
                    Key::ArrowUp if !state.path_focused => {
                        state.browser.select_relative(-1);
                        true
                    }
                    Key::ArrowDown if !state.path_focused => {
                        state.browser.select_relative(1);
                        true
                    }
                    Key::Enter if state.path_focused => {
                        let path = PathBuf::from(&state.path_input);
                        if Self::navigate(&mut state, path) {
                            state.path_focused = false;
                            state.path_input.clear();
                            state.path_replace_on_input = false;
                        }
                        true
                    }
                    Key::Enter => {
                        let opened = Self::open_selected(&mut state);
                        if opened {
                            state.scroll = 0.0;
                        }
                        opened
                    }
                    _ => false,
                };
                if changed {
                    context.request_redraw_in(bounds);
                    EventResult::Consumed
                } else {
                    EventResult::Ignored
                }
            }
            ViewEvent::FocusChanged { focused: false } => {
                let mut state = self.state.borrow_mut();
                let _ = Self::commit_name_edit(&mut state);
                state.path_focused = false;
                state.path_input.clear();
                state.path_replace_on_input = false;
                state.search_focused = false;
                context.request_redraw_in(layout.toolbar);
                EventResult::Consumed
            }
            _ => EventResult::Ignored,
        }
    }
}

struct Layout {
    bounds: Rect,
    toolbar: Rect,
    sidebar: Rect,
    content: Rect,
    status: Rect,
    sidebar_width: f32,
}

impl Layout {
    fn new(bounds: Rect) -> Self {
        let sidebar_width = if bounds.size.width < 760.0 {
            176.0
        } else {
            212.0
        };
        let content_height = (bounds.size.height - Theme::current().layout.top_bar_height).max(0.0);
        Self {
            bounds,
            toolbar: Rect::new(
                bounds.origin.x,
                bounds.origin.y,
                bounds.size.width,
                Theme::current().layout.top_bar_height,
            ),
            sidebar: Rect::new(
                bounds.origin.x,
                bounds.origin.y + Theme::current().layout.top_bar_height,
                sidebar_width,
                content_height,
            ),
            content: Rect::new(
                bounds.origin.x + sidebar_width,
                bounds.origin.y + Theme::current().layout.top_bar_height,
                (bounds.size.width - sidebar_width).max(0.0),
                content_height,
            ),
            status: Rect::new(
                bounds.origin.x,
                bounds.origin.y + bounds.size.height,
                bounds.size.width,
                0.0,
            ),
            sidebar_width,
        }
    }

    fn toolbar_button(&self, index: usize) -> Rect {
        let control = Theme::current().layout.compact_control_height;
        Rect::new(
            self.bounds.origin.x + 13.0 + index as f32 * 35.0,
            self.bounds.origin.y + (Theme::current().layout.top_bar_height - control) / 2.0,
            control,
            control,
        )
    }

    fn mode_button(&self, index: usize) -> Rect {
        let right = self.bounds.origin.x + self.bounds.size.width;
        let control = Theme::current().layout.compact_control_height;
        Rect::new(
            right - 294.0 + index as f32 * control,
            self.bounds.origin.y + (Theme::current().layout.top_bar_height - control) / 2.0,
            control,
            control,
        )
    }

    fn path(&self) -> Rect {
        let control = Theme::current().layout.compact_control_height;
        Rect::new(
            self.bounds.origin.x + 90.0,
            self.bounds.origin.y + (Theme::current().layout.top_bar_height - control) / 2.0,
            (self.bounds.size.width - 394.0).max(80.0),
            control,
        )
    }

    fn search(&self) -> Rect {
        let right = self.bounds.origin.x + self.bounds.size.width;
        let control = Theme::current().layout.compact_control_height;
        Rect::new(
            right - 218.0,
            self.bounds.origin.y + (Theme::current().layout.top_bar_height - control) / 2.0,
            202.0,
            control,
        )
    }
}

fn paint_toolbar(layout: &Layout, state: &FilesState, context: &mut PaintContext<'_>) {
    Rectangle::new()
        .color(RectangleColor::Custom(colors().toolbar_background))
        .paint(layout.toolbar, context);
    stroke_bottom(layout.toolbar, context);

    paint_icon_button(
        layout.toolbar_button(0),
        IconName::ChevronLeft,
        state.browser.can_go_back(),
        state.hover == Some(HitTarget::Back),
        context,
    );
    paint_icon_button(
        layout.toolbar_button(1),
        IconName::ChevronRight,
        state.browser.can_go_forward(),
        state.hover == Some(HitTarget::Forward),
        context,
    );
    let path = layout.path();
    Rectangle::new()
        .color(RectangleColor::Custom(if state.path_focused {
            colors().field_focused
        } else {
            colors().field_background
        }))
        .radius(viewkit::theme::CornerRadius::Small)
        .paint(path, context);
    if state.path_focused {
        context.display_list.push(DrawCommand::StrokeRoundedRect {
            rect: Rect::new(
                path.origin.x + 0.5,
                path.origin.y + 0.5,
                path.size.width - 1.0,
                path.size.height - 1.0,
            ),
            radius: 5.5,
            color: colors().selection,
            width: 1.0,
        });
    }
    paint_text(
        if state.path_focused {
            state.path_input.clone()
        } else {
            state.browser.current_dir().display().to_string()
        },
        Rect::new(
            path.origin.x + 9.0,
            path.origin.y + 4.0,
            path.size.width - 18.0,
            20.0,
        ),
        TextRole::Label,
        None,
        colors().text_primary,
        TextAlignment::Start,
        context,
    );

    let list_selected = state.browser.view_mode() == ViewMode::List;
    paint_mode_button(
        layout.mode_button(0),
        IconName::LayoutList,
        list_selected,
        state.hover == Some(HitTarget::ListMode),
        context,
    );
    paint_mode_button(
        layout.mode_button(1),
        IconName::LayoutGrid,
        !list_selected,
        state.hover == Some(HitTarget::GridMode),
        context,
    );

    let search = layout.search();
    Rectangle::new()
        .color(RectangleColor::Custom(if state.search_focused {
            colors().field_focused
        } else {
            colors().field_background
        }))
        .radius(viewkit::theme::CornerRadius::Small)
        .paint(search, context);
    if state.search_focused {
        context.display_list.push(DrawCommand::StrokeRoundedRect {
            rect: Rect::new(
                search.origin.x + 0.5,
                search.origin.y + 0.5,
                search.size.width - 1.0,
                search.size.height - 1.0,
            ),
            radius: 5.5,
            color: colors().selection,
            width: 1.0,
        });
    }
    Icon::new(IconName::Search)
        .size(14.0)
        .color(colors().text_secondary)
        .paint(
            Rect::new(search.origin.x + 8.0, search.origin.y + 8.0, 14.0, 14.0),
            context,
        );
    let search_text = if state.browser.search().is_empty() {
        "Search".to_owned()
    } else {
        state.browser.search().to_owned()
    };
    paint_text(
        search_text,
        Rect::new(
            search.origin.x + 28.0,
            search.origin.y + 4.0,
            search.size.width - 36.0,
            20.0,
        ),
        TextRole::Label,
        None,
        if state.browser.search().is_empty() {
            colors().text_secondary
        } else {
            colors().text_primary
        },
        TextAlignment::Start,
        context,
    );
}

fn tree_start_y(layout: &Layout, state: &FilesState) -> f32 {
    layout.sidebar.origin.y + 42.0 + state.sidebar_items.len() as f32 * 34.0 + 24.0
        - state.sidebar_scroll
}
fn tree_row_rect(layout: &Layout, state: &FilesState, index: usize) -> Rect {
    Rect::new(
        layout.sidebar.origin.x + 8.0,
        tree_start_y(layout, state) + index as f32 * 32.0,
        layout.sidebar_width - 16.0,
        30.0,
    )
}
fn maximum_sidebar_scroll(layout: &Layout, state: &FilesState) -> f32 {
    (42.0
        + state.sidebar_items.len() as f32 * 34.0
        + 24.0
        + state.folder_tree.rows().len() as f32 * 32.0
        + 12.0
        - layout.sidebar.size.height)
        .max(0.0)
}

fn paint_sidebar(layout: &Layout, state: &FilesState, context: &mut PaintContext<'_>) {
    Rectangle::new()
        .color(RectangleColor::Custom(colors().sidebar_background))
        .paint(layout.sidebar, context);
    context.display_list.push(DrawCommand::StrokeRect {
        rect: Rect::new(
            layout.sidebar.origin.x + layout.sidebar.size.width - 0.5,
            layout.sidebar.origin.y,
            1.0,
            layout.sidebar.size.height,
        ),
        color: colors().border,
        width: 1.0,
    });

    context.display_list.push(DrawCommand::PushClip {
        rect: layout.sidebar,
    });
    paint_text(
        "Favorites",
        Rect::new(
            layout.sidebar.origin.x + 16.0,
            layout.sidebar.origin.y + 16.0 - state.sidebar_scroll,
            layout.sidebar_width - 30.0,
            18.0,
        ),
        TextRole::Caption,
        Some(600),
        colors().text_secondary,
        TextAlignment::Start,
        context,
    );
    for (index, item) in state.sidebar_items.iter().enumerate() {
        paint_sidebar_item(
            layout,
            state,
            index,
            item,
            42.0 + index as f32 * 34.0 - state.sidebar_scroll,
            context,
        );
    }
    let top = tree_start_y(layout, state);
    Rectangle::new()
        .color(RectangleColor::Custom(colors().border))
        .paint(
            Rect::new(
                layout.sidebar.origin.x + 12.0,
                top - 12.0,
                layout.sidebar_width - 24.0,
                1.0,
            ),
            context,
        );
    for (index, row) in state.folder_tree.rows().iter().enumerate() {
        let rect = tree_row_rect(layout, state, index);
        if rect.intersection(layout.sidebar).is_none() {
            continue;
        }
        let indent = (row.depth as f32 * 16.0).min((layout.sidebar_width - 90.0).max(0.0));
        let selected = state.browser.current_dir() == row.path;
        let hovered = matches!(state.hover, Some(HitTarget::TreeExpand(i) | HitTarget::TreeNavigate(i)) if i == index);
        if selected || hovered {
            Rectangle::new()
                .color(RectangleColor::Custom(if selected {
                    colors().sidebar_selection
                } else {
                    colors().sidebar_hover
                }))
                .radius(viewkit::theme::CornerRadius::Small)
                .paint(rect, context);
        }
        let arrow = Rect::new(
            rect.origin.x + indent,
            rect.origin.y,
            20.0,
            rect.size.height,
        );
        if row.expandable {
            Icon::new(if row.expanded {
                IconName::ChevronDown
            } else {
                IconName::ChevronRight
            })
            .size(10.0)
            .color(colors().text_secondary)
            .paint(arrow, context);
        }
        if let Some(icon) = state.icons.sidebar(SidebarIcon::Folder) {
            Svg::new(icon.clone()).paint(
                Rect::new(arrow.origin.x + 22.0, rect.origin.y + 5.0, 20.0, 20.0),
                context,
            );
        }
        let title = if row.depth == 0 {
            String::from("/")
        } else {
            row.path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned()
        };
        paint_text(
            title.clone(),
            Rect::new(
                arrow.origin.x + 48.0,
                rect.origin.y + 4.0,
                (rect.size.width - indent - 50.0).max(0.0),
                22.0,
            ),
            TextRole::Label,
            selected.then_some(600),
            colors().text_primary,
            TextAlignment::Start,
            context,
        );
        let mut node = AccessibilityNode::new(AccessibilityRole::ListItem, rect);
        node.label = Some(title);
        node.value = Some(row.path.display().to_string());
        node.selected = selected;
        context.record_accessibility(node);
    }
    context.display_list.push(DrawCommand::PopClip);
}

fn paint_sidebar_item(
    layout: &Layout,
    state: &FilesState,
    index: usize,
    item: &SidebarItem,
    y_offset: f32,
    context: &mut PaintContext<'_>,
) {
    let bounds = Rect::new(
        layout.sidebar.origin.x + 8.0,
        layout.sidebar.origin.y + y_offset,
        layout.sidebar_width - 16.0,
        29.0,
    );
    let selected = if item.path.is_dir() {
        state.browser.current_dir() == item.path
    } else {
        state.browser.current_dir() == item.path.parent().unwrap_or(Path::new("/"))
            && state.browser.selected() == Some(item.path.as_path())
    };
    let hovered = state.hover == Some(HitTarget::Sidebar(index));
    let mut node = AccessibilityNode::new(AccessibilityRole::ListItem, bounds);
    node.label = Some(item.label.clone());
    node.value = Some(item.path.display().to_string());
    node.selected = selected;
    context.record_accessibility(node);
    if selected || hovered {
        Rectangle::new()
            .color(RectangleColor::Custom(if selected {
                colors().sidebar_selection
            } else {
                colors().sidebar_hover
            }))
            .radius(viewkit::theme::CornerRadius::Small)
            .paint(bounds, context);
    }
    if let Some(icon) = state.icons.sidebar(item.icon) {
        Svg::new(icon.clone()).paint(
            Rect::new(bounds.origin.x + 7.5, bounds.origin.y + 4.5, 20.0, 20.0),
            context,
        );
    }
    paint_text(
        item.label.clone(),
        Rect::new(
            bounds.origin.x + 34.0,
            bounds.origin.y + 4.0,
            bounds.size.width - 40.0,
            21.0,
        ),
        TextRole::Label,
        selected.then_some(600),
        colors().text_primary,
        TextAlignment::Start,
        context,
    );
}

fn paint_content(layout: &Layout, state: &FilesState, context: &mut PaintContext<'_>) {
    Rectangle::new()
        .color(RectangleColor::Custom(colors().content_background))
        .paint(layout.content, context);
    context.display_list.push(DrawCommand::PushClip {
        rect: layout.content,
    });
    match state.browser.view_mode() {
        ViewMode::List => paint_list(layout, state, context),
        ViewMode::Grid => paint_grid(layout, state, context),
    }
    context.display_list.push(DrawCommand::PopClip);
}

fn paint_list(layout: &Layout, state: &FilesState, context: &mut PaintContext<'_>) {
    let header = Rect::new(
        layout.content.origin.x,
        layout.content.origin.y,
        layout.content.size.width,
        Theme::current().layout.compact_control_height,
    );
    Rectangle::new()
        .color(RectangleColor::Custom(colors().list_header))
        .paint(header, context);
    stroke_bottom(header, context);
    let columns = list_columns(layout.content);
    paint_text(
        "Name",
        columns[0],
        TextRole::Caption,
        Some(600),
        colors().text_secondary,
        TextAlignment::Start,
        context,
    );
    paint_text(
        "Date Modified",
        columns[1],
        TextRole::Caption,
        Some(600),
        colors().text_secondary,
        TextAlignment::Start,
        context,
    );
    paint_text(
        "Size",
        columns[2],
        TextRole::Caption,
        Some(600),
        colors().text_secondary,
        TextAlignment::End,
        context,
    );
    paint_text(
        "Kind",
        columns[3],
        TextRole::Caption,
        Some(600),
        colors().text_secondary,
        TextAlignment::Start,
        context,
    );

    let entries = state.browser.entries();
    for (index, entry) in entries.iter().enumerate() {
        let y = layout.content.origin.y
            + Theme::current().layout.compact_control_height
            + index as f32 * Theme::current().layout.control_height
            - state.scroll;
        let row = Rect::new(
            layout.content.origin.x,
            y,
            layout.content.size.width,
            Theme::current().layout.control_height,
        );
        if row.origin.y + row.size.height <= header.origin.y + header.size.height
            || row.origin.y >= layout.status.origin.y
        {
            continue;
        }
        let selected = state.browser.selected() == Some(entry.path.as_path());
        let hovered = state.hover == Some(HitTarget::Entry(index));
        let mut node = AccessibilityNode::new(AccessibilityRole::ListItem, row);
        node.label = Some(entry.name.clone());
        node.value = Some(entry.kind.label().to_owned());
        node.selected = selected;
        context.record_accessibility(node);
        if selected || hovered {
            Rectangle::new()
                .color(RectangleColor::Custom(if selected {
                    colors().selection
                } else {
                    colors().row_hover
                }))
                .paint(row, context);
        }
        let text_color = if selected {
            colors().on_selection
        } else {
            colors().text_primary
        };
        let secondary = if selected {
            colors().on_selection
        } else {
            colors().text_secondary
        };
        let cols = list_columns(row);
        if let Some(icon) = state.icons.document(entry) {
            Image::new(icon.clone())
                .content_mode(ImageContentMode::Fit)
                .paint(
                    Rect::new(cols[0].origin.x + 1.0, cols[0].origin.y + 5.0, 18.0, 18.0),
                    context,
                );
        } else if let Some(icon) = state.icons.entry(entry) {
            Svg::new(icon.clone()).paint(
                Rect::new(cols[0].origin.x + 1.0, cols[0].origin.y + 5.0, 18.0, 18.0),
                context,
            );
        }
        paint_editable_name(
            state,
            entry,
            Rect::new(
                cols[0].origin.x + 24.0,
                cols[0].origin.y,
                cols[0].size.width - 24.0,
                cols[0].size.height,
            ),
            TextRole::Label,
            text_color,
            TextAlignment::Start,
            context,
        );
        paint_text(
            entry.modified.clone(),
            cols[1],
            TextRole::Caption,
            None,
            secondary,
            TextAlignment::Start,
            context,
        );
        paint_text(
            entry.size_label(),
            cols[2],
            TextRole::Caption,
            None,
            secondary,
            TextAlignment::End,
            context,
        );
        paint_text(
            entry.kind.label(),
            cols[3],
            TextRole::Caption,
            None,
            secondary,
            TextAlignment::Start,
            context,
        );
        stroke_bottom(row, context);
    }
}

fn paint_grid(layout: &Layout, state: &FilesState, context: &mut PaintContext<'_>) {
    let directory = state.browser.current_dir();
    let home = std::env::var_os("MOCHIOS_PREVIEW_ROOT")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from);
    let preview_home = std::env::var_os("MOCHIOS_PREVIEW_ROOT").map(PathBuf::from);
    let title = if home.as_deref() == Some(directory)
        || preview_home.as_deref() == Some(directory)
        || directory.parent() == Some(Path::new("/home"))
    {
        String::from("Home")
    } else if directory == Path::new("/") {
        String::from("Computer")
    } else {
        directory
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| String::from("Files"))
    };
    paint_text(
        title,
        Rect::new(
            layout.content.origin.x + GRID_SIDE_INSET,
            layout.content.origin.y + 16.0,
            layout.content.size.width - GRID_SIDE_INSET * 2.0,
            32.0,
        ),
        TextRole::TitleMedium,
        Some(600),
        colors().text_primary,
        TextAlignment::Start,
        context,
    );
    stroke_bottom(
        Rect::new(
            layout.content.origin.x + GRID_SIDE_INSET,
            layout.content.origin.y + GRID_HEADER_HEIGHT - 1.0,
            (layout.content.size.width - GRID_SIDE_INSET * 2.0).max(0.0),
            1.0,
        ),
        context,
    );

    let columns = grid_column_count(layout.content);
    let left = layout.content.origin.x + GRID_SIDE_INSET;
    context.display_list.push(DrawCommand::PushClip {
        rect: Rect::new(
            layout.content.origin.x,
            layout.content.origin.y + GRID_HEADER_HEIGHT,
            layout.content.size.width,
            (layout.content.size.height - GRID_HEADER_HEIGHT).max(0.0),
        ),
    });
    for (index, entry) in state.browser.entries().iter().enumerate() {
        let column = index % columns;
        let row = index / columns;
        let cell = Rect::new(
            left + column as f32 * Theme::current().layout.browser_grid_cell_width,
            layout.content.origin.y
                + GRID_HEADER_HEIGHT
                + 12.0
                + row as f32 * Theme::current().layout.browser_grid_cell_height
                - state.scroll,
            Theme::current().layout.browser_grid_cell_width,
            Theme::current().layout.browser_grid_cell_height,
        );
        if cell.origin.y + cell.size.height <= layout.content.origin.y
            || cell.origin.y >= layout.status.origin.y
        {
            continue;
        }
        let selected = state.browser.selected() == Some(entry.path.as_path());
        let hovered = state.hover == Some(HitTarget::Entry(index));
        let mut node = AccessibilityNode::new(AccessibilityRole::ListItem, cell);
        node.label = Some(entry.name.clone());
        node.value = Some(entry.kind.label().to_owned());
        node.selected = selected;
        context.record_accessibility(node);
        if hovered {
            Rectangle::new()
                .color(RectangleColor::Custom(colors().row_hover))
                .radius(viewkit::theme::CornerRadius::Small)
                .paint(
                    Rect::new(
                        cell.origin.x + 5.0,
                        cell.origin.y + 2.0,
                        cell.size.width - 10.0,
                        cell.size.height - 4.0,
                    ),
                    context,
                );
        }
        if let Some(icon) = state.icons.document(entry) {
            Image::new(icon.clone())
                .content_mode(ImageContentMode::Fit)
                .paint(
                    Rect::new(cell.origin.x + 27.0, cell.origin.y + 8.0, 64.0, 64.0),
                    context,
                );
        } else if let Some(icon) = state.icons.entry(entry) {
            Svg::new(icon.clone()).paint(
                Rect::new(cell.origin.x + 27.0, cell.origin.y + 8.0, 64.0, 64.0),
                context,
            );
        }
        let label = Rect::new(
            cell.origin.x + 6.0,
            cell.origin.y + 78.0,
            cell.size.width - 12.0,
            24.0,
        );
        if selected {
            Rectangle::new()
                .color(RectangleColor::Custom(colors().selection))
                .radius(viewkit::theme::CornerRadius::Small)
                .paint(label, context);
        }
        paint_editable_name(
            state,
            entry,
            label,
            TextRole::Caption,
            if selected {
                colors().on_selection
            } else {
                colors().text_primary
            },
            TextAlignment::Center,
            context,
        );
    }
    context.display_list.push(DrawCommand::PopClip);
}

fn paint_editable_name(
    state: &FilesState,
    entry: &FileEntry,
    bounds: Rect,
    role: TextRole,
    color: Color,
    alignment: TextAlignment,
    context: &mut PaintContext<'_>,
) {
    if let Some(edit) = state
        .name_edit
        .as_ref()
        .filter(|edit| edit.path == entry.path)
    {
        Rectangle::new()
            .color(RectangleColor::Custom(colors().field_focused))
            .radius(viewkit::theme::CornerRadius::Small)
            .paint(bounds, context);
        context.display_list.push(DrawCommand::StrokeRoundedRect {
            rect: Rect::new(
                bounds.origin.x + 0.5,
                bounds.origin.y + 0.5,
                (bounds.size.width - 1.0).max(0.0),
                (bounds.size.height - 1.0).max(0.0),
            ),
            radius: 2.5,
            color: colors().selection,
            width: 1.0,
        });
        paint_text(
            format!("{}|", edit.value),
            bounds,
            role,
            None,
            colors().text_primary,
            alignment,
            context,
        );
    } else {
        paint_text(
            entry.name.clone(),
            bounds,
            role,
            None,
            color,
            alignment,
            context,
        );
    }
}

fn paint_icon_button(
    bounds: Rect,
    icon: IconName,
    enabled: bool,
    hovered: bool,
    context: &mut PaintContext<'_>,
) {
    if hovered && enabled {
        Rectangle::new()
            .color(RectangleColor::Custom(colors().control_hover))
            .radius(viewkit::theme::CornerRadius::Small)
            .paint(bounds, context);
    }
    let icon_size = context.theme.layout.stepper_icon_size;
    Icon::new(icon)
        .size(icon_size)
        .color(if enabled {
            colors().text_primary
        } else {
            colors().disabled
        })
        .paint(
            Rect::new(
                bounds.origin.x + (bounds.size.width - icon_size) / 2.0,
                bounds.origin.y + (bounds.size.height - icon_size) / 2.0,
                icon_size,
                icon_size,
            ),
            context,
        );
}

fn paint_mode_button(
    bounds: Rect,
    icon: IconName,
    selected: bool,
    hovered: bool,
    context: &mut PaintContext<'_>,
) {
    if selected || hovered {
        Rectangle::new()
            .color(RectangleColor::Custom(if selected {
                colors().control_selection
            } else {
                colors().control_selection_hover
            }))
            .radius(viewkit::theme::CornerRadius::Small)
            .paint(bounds, context);
    }
    let icon_size = context.theme.layout.compact_icon_size;
    Icon::new(icon)
        .size(icon_size)
        .color(colors().text_primary)
        .paint(
            Rect::new(
                bounds.origin.x + (bounds.size.width - icon_size) / 2.0,
                bounds.origin.y + (bounds.size.height - icon_size) / 2.0,
                icon_size,
                icon_size,
            ),
            context,
        );
}

fn paint_text(
    value: impl Into<String>,
    bounds: Rect,
    role: TextRole,
    weight: Option<u16>,
    color: Color,
    alignment: TextAlignment,
    context: &mut PaintContext<'_>,
) {
    let style = context.typography.style(role);
    let mut text = Text::styled(value, role)
        .line_height(bounds.size.height.max(style.line_height))
        .color(color)
        .alignment(alignment);
    if let Some(weight) = weight {
        text = text.weight(weight);
    }
    text.paint(bounds, context);
}

fn stroke_bottom(bounds: Rect, context: &mut PaintContext<'_>) {
    context.display_list.push(DrawCommand::StrokeRect {
        rect: Rect::new(
            bounds.origin.x,
            bounds.origin.y + bounds.size.height - 0.5,
            bounds.size.width,
            1.0,
        ),
        color: colors().border,
        width: 1.0,
    });
}

fn list_columns(bounds: Rect) -> [Rect; 4] {
    let theme = Theme::current();
    let spacing = theme.spacing;
    let width = bounds.size.width;
    let name = (width / 2.0)
        .max(theme.layout.navigation_sidebar_width)
        .min(width);
    let modified = (width / 4.0)
        .max(theme.layout.control_min_width)
        .min((width - name).max(0.0));
    let size = theme
        .layout
        .control_min_width
        .min((width - name - modified).max(0.0));
    [
        Rect::new(
            bounds.origin.x + spacing.medium,
            bounds.origin.y + spacing.extra_small,
            (name - spacing.large).max(0.0),
            bounds.size.height - spacing.small,
        ),
        Rect::new(
            bounds.origin.x + name,
            bounds.origin.y + spacing.extra_small,
            (modified - spacing.medium).max(0.0),
            bounds.size.height - spacing.small,
        ),
        Rect::new(
            bounds.origin.x + name + modified,
            bounds.origin.y + spacing.extra_small,
            (size - spacing.large).max(0.0),
            bounds.size.height - spacing.small,
        ),
        Rect::new(
            bounds.origin.x + name + modified + size + spacing.medium,
            bounds.origin.y + spacing.extra_small,
            (width - name - modified - size - spacing.extra_large).max(0.0),
            bounds.size.height - spacing.small,
        ),
    ]
}

fn hit_test(layout: &Layout, point: Point, state: &FilesState) -> Option<HitTarget> {
    for index in 0..2 {
        if layout.toolbar_button(index).contains(point) {
            return Some(match index {
                0 => HitTarget::Back,
                _ => HitTarget::Forward,
            });
        }
    }
    for index in 0..2 {
        if layout.mode_button(index).contains(point) {
            return Some(if index == 0 {
                HitTarget::ListMode
            } else {
                HitTarget::GridMode
            });
        }
    }
    if layout.path().contains(point) {
        return Some(HitTarget::Path);
    }
    if layout.search().contains(point) {
        return Some(HitTarget::Search);
    }
    if layout.sidebar.contains(point) {
        for index in 0..state.sidebar_items.len() {
            let row = Rect::new(
                layout.sidebar.origin.x + 8.0,
                layout.sidebar.origin.y + 42.0 + index as f32 * 34.0 - state.sidebar_scroll,
                layout.sidebar_width - 16.0,
                29.0,
            );
            if row.contains(point) {
                return Some(HitTarget::Sidebar(index));
            }
        }
    }
    if layout.sidebar.contains(point) {
        for (index, row) in state.folder_tree.rows().iter().enumerate() {
            let rect = tree_row_rect(layout, state, index);
            if rect.contains(point) {
                let indent = (row.depth as f32 * 16.0).min((layout.sidebar_width - 90.0).max(0.0));
                return Some(
                    if point.x < rect.origin.x + indent + 20.0 && row.expandable {
                        HitTarget::TreeExpand(index)
                    } else {
                        HitTarget::TreeNavigate(index)
                    },
                );
            }
        }
    }
    if layout.content.contains(point) {
        let count = state.browser.entries().len();
        let index = match state.browser.view_mode() {
            ViewMode::List => {
                let relative = point.y
                    - layout.content.origin.y
                    - Theme::current().layout.compact_control_height
                    + state.scroll;
                (relative >= 0.0)
                    .then_some((relative / Theme::current().layout.control_height) as usize)
            }
            ViewMode::Grid => {
                let columns = grid_column_count(layout.content);
                let left = layout.content.origin.x + GRID_SIDE_INSET;
                let x = point.x - left;
                let y =
                    point.y - layout.content.origin.y - GRID_HEADER_HEIGHT - 12.0 + state.scroll;
                if point.y >= layout.content.origin.y + GRID_HEADER_HEIGHT
                    && x >= 0.0
                    && y >= 0.0
                    && (x / Theme::current().layout.browser_grid_cell_width) < columns as f32
                {
                    Some(
                        (y / Theme::current().layout.browser_grid_cell_height) as usize * columns
                            + (x / Theme::current().layout.browser_grid_cell_width) as usize,
                    )
                } else {
                    None
                }
            }
        };
        return Some(
            index
                .filter(|index| *index < count)
                .map(HitTarget::Entry)
                .unwrap_or(HitTarget::Content),
        );
    }
    None
}

fn maximum_scroll(layout: &Layout, state: &FilesState) -> f32 {
    let count = state.browser.entries().len();
    let content_height = match state.browser.view_mode() {
        ViewMode::List => {
            Theme::current().layout.compact_control_height
                + count as f32 * Theme::current().layout.control_height
        }
        ViewMode::Grid => {
            let columns = grid_column_count(layout.content);
            GRID_HEADER_HEIGHT
                + 20.0
                + count.div_ceil(columns) as f32 * Theme::current().layout.browser_grid_cell_height
        }
    };
    (content_height - layout.content.size.height).max(0.0)
}

fn grid_column_count(content: Rect) -> usize {
    ((content.size.width - GRID_SIDE_INSET * 2.0) / Theme::current().layout.browser_grid_cell_width)
        .floor()
        .max(1.0) as usize
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keyboard_copy_and_paste_runs_job_and_selects_completed_file() {
        let root = std::env::temp_dir().join(format!("files-ui-copy-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("destination")).unwrap();
        std::fs::write(root.join("note.txt"), b"clipboard copy").unwrap();
        let view = FilesView::new_with_activation(root.clone(), None, None);
        view.state
            .borrow_mut()
            .browser
            .select(root.join("note.txt"));
        let bounds = Rect::new(0.0, 0.0, 920.0, 640.0);
        let theme = Theme::current();
        let mut measurer = viewkit::typography::TextMeasurer::new();
        let mut context = EventContext::new(&theme, &theme.typography, &mut measurer);
        let shortcut = |key| ViewEvent::KeyPressed {
            key: Key::Character(key),
            modifiers: viewkit::platform::KeyModifiers::from_bits(
                viewkit::platform::KeyModifiers::CONTROL,
            ),
        };
        assert!(
            view.handle_event(bounds, &shortcut('c'), &mut context)
                .is_consumed()
        );
        view.state
            .borrow_mut()
            .browser
            .navigate(root.join("destination"));
        assert!(
            view.handle_event(bounds, &shortcut('v'), &mut context)
                .is_consumed()
        );
        assert!(view.state.borrow().operation.is_some());
        for _ in 0..100 {
            view.advance_operation();
            if view.state.borrow().operation.is_none() {
                break;
            }
        }
        assert_eq!(
            std::fs::read(root.join("destination/note.txt")).unwrap(),
            b"clipboard copy"
        );
        assert_eq!(
            view.state.borrow().browser.selected(),
            Some(root.join("destination/note.txt").as_path())
        );
        assert!(root.join("note.txt").exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn conflict_keep_both_button_resumes_a_paused_job() {
        let root = std::env::temp_dir().join(format!("files-ui-conflict-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("destination")).unwrap();
        std::fs::write(root.join("note.txt"), b"new").unwrap();
        std::fs::write(root.join("destination/note.txt"), b"old").unwrap();
        let view = FilesView::new_with_activation(root.join("destination"), None, None);
        FilesView::start_operation(
            &mut view.state.borrow_mut(),
            Kind::Copy,
            root.join("note.txt"),
        );
        let bounds = Rect::new(0.0, 0.0, 920.0, 640.0);
        let theme = Theme::current();
        let mut measurer = viewkit::typography::TextMeasurer::new();
        let mut list = viewkit::draw_command::DisplayList::new();
        view.paint(
            bounds,
            &mut PaintContext::new(&mut list, &theme, &theme.typography, &mut measurer),
        );
        let frame = FilesView::operation_button(FilesView::operation_frame(bounds), 2);
        let position = Point::new(frame.origin.x + 20.0, frame.origin.y + 12.0);
        let mut context = EventContext::new(&theme, &theme.typography, &mut measurer);
        view.handle_event(
            bounds,
            &ViewEvent::PointerPressed {
                position,
                button: PointerButton::Primary,
            },
            &mut context,
        );
        view.handle_event(
            bounds,
            &ViewEvent::PointerReleased {
                position,
                button: PointerButton::Primary,
            },
            &mut context,
        );
        for _ in 0..100 {
            view.advance_operation();
            if view.state.borrow().operation.is_none() {
                break;
            }
        }
        assert_eq!(
            std::fs::read(root.join("destination/note.txt")).unwrap(),
            b"old"
        );
        assert_eq!(
            std::fs::read(root.join("destination/note 2.txt")).unwrap(),
            b"new"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn default_sidebar_uses_standard_home_directories_in_order() {
        let home = Path::new("/home/tester");
        let items = sidebar_items(home, &SidebarBookmarks::default());
        assert_eq!(
            items
                .iter()
                .map(|item| item.label.as_str())
                .collect::<Vec<_>>(),
            DEFAULT_SIDEBAR_DIRECTORIES
        );
        assert_eq!(items[0].path, home.join("Desktop"));
        assert_eq!(items[5].path, home.join("Pictures"));
    }

    #[test]
    fn supplied_file_icons_decode() {
        assert!(SvgData::decode(include_bytes!("../appicon.svg")).is_ok());
        assert!(SvgData::decode(APPLICATION_SVG).is_ok());
        assert!(SvgData::decode(FOLDER_SVG).is_ok());
        assert!(SvgData::decode(FILE_SVG).is_ok());
    }

    #[test]
    fn applications_use_application_icon() {
        let applications = FileEntry {
            path: PathBuf::from("/applications"),
            name: "applications".to_owned(),
            kind: EntryKind::Directory,
            size: 0,
            modified: String::new(),
        };
        let app_bundle = FileEntry {
            path: PathBuf::from("/applications/Files.app"),
            name: "Files.app".to_owned(),
            kind: EntryKind::Application,
            size: 0,
            modified: String::new(),
        };

        assert!(uses_application_icon(&applications));
        assert!(uses_application_icon(&app_bundle));
    }
}
