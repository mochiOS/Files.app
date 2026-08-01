use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::browser::{Browser, EntryKind, FileEntry, ViewMode};
use viewkit::components::{Icon, IconName, Rectangle, RectangleColor, Svg, Text};
use viewkit::draw_command::DrawCommand;
use viewkit::event::{ContextMenuItem, ContextMenuRequest, EventContext, EventResult, ViewEvent};
use viewkit::geometry::{Point, Rect, Size};
use viewkit::platform::{CursorIcon, Key, PointerButton};
use viewkit::prelude::SvgData;
use viewkit::theme::Color;
use viewkit::typography::TextAlignment;
use viewkit::view::{Constraints, MeasureContext, PaintContext, View};

const FOLDER_SVG: &[u8] = include_bytes!("../resources/icons/folder.svg");
const FILE_SVG: &[u8] = include_bytes!("../resources/icons/file.svg");
const APPLICATION_SVG: &[u8] = include_bytes!("../resources/icons/application.svg");

const TOOLBAR_HEIGHT: f32 = 54.0;
const STATUS_HEIGHT: f32 = 28.0;
const LIST_HEADER_HEIGHT: f32 = 30.0;
const LIST_ROW_HEIGHT: f32 = 29.0;
const GRID_CELL_WIDTH: f32 = 118.0;
const GRID_CELL_HEIGHT: f32 = 112.0;
const DOUBLE_CLICK: Duration = Duration::from_millis(500);
const CONTEXT_COMMAND_OPEN: u32 = 1;
const CONTEXT_COMMAND_RELOAD: u32 = 2;

const WINDOW_BACKGROUND: Color = Color::from_rgb_hex(0xf8f8f8);
const TOOLBAR_BACKGROUND: Color = Color::from_rgb_hex(0xf2f2f2);
const SIDEBAR_BACKGROUND: Color = Color::from_rgb_hex(0xe9e9e9);
const CONTENT_BACKGROUND: Color = Color::WHITE;
const BORDER: Color = Color::from_rgb_hex(0xd0d0d0);
const TEXT_PRIMARY: Color = Color::from_rgb_hex(0x252525);
const TEXT_SECONDARY: Color = Color::from_rgb_hex(0x707070);
const ROW_HOVER: Color = Color::from_rgb_hex(0xf0f5fa);
const SELECTION: Color = Color::from_rgb_hex(0x3478d4);
const SEARCH_BACKGROUND: Color = Color::from_rgb_hex(0xe2e2e2);
const DISABLED: Color = Color::from_rgb_hex(0xa7a7a7);

const SIDEBAR_ITEMS: [SidebarItem; 4] = [
    SidebarItem::new("Applications", "/applications", SidebarIcon::Application),
    SidebarItem::new("System", "/system", SidebarIcon::Folder),
    SidebarItem::new("Libraries", "/libraries", SidebarIcon::Folder),
    SidebarItem::new("Temporary", "/tmp", SidebarIcon::Folder),
];

#[derive(Clone, Copy)]
enum SidebarIcon {
    Application,
    Folder,
}

#[derive(Clone, Copy)]
struct SidebarItem {
    label: &'static str,
    path: &'static str,
    icon: SidebarIcon,
}

impl SidebarItem {
    const fn new(label: &'static str, path: &'static str, icon: SidebarIcon) -> Self {
        Self { label, path, icon }
    }
}

struct FileIcons {
    application: Option<SvgData>,
    folder: Option<SvgData>,
    file: Option<SvgData>,
}

impl FileIcons {
    fn new() -> Self {
        Self {
            application: SvgData::decode(APPLICATION_SVG).ok(),
            folder: SvgData::decode(FOLDER_SVG).ok(),
            file: SvgData::decode(FILE_SVG).ok(),
        }
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
        }
    }
}

fn uses_application_icon(entry: &FileEntry) -> bool {
    entry.kind == EntryKind::Application || entry.path == Path::new("/applications")
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum HitTarget {
    Back,
    Forward,
    Up,
    ListMode,
    GridMode,
    Path,
    Search,
    Sidebar(usize),
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
        HitTarget::Sidebar(_) => layout.sidebar,
        HitTarget::Entry(_) | HitTarget::Content => layout.content,
        _ => layout.toolbar,
    };
    context.request_redraw_in(region);
}

struct FilesState {
    browser: Browser,
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
}

pub(crate) struct FilesView {
    state: RefCell<FilesState>,
}

impl FilesView {
    pub(crate) fn new() -> Self {
        Self {
            state: RefCell::new(FilesState {
                browser: Browser::new("/"),
                icons: FileIcons::new(),
                scroll: 0.0,
                hover: None,
                path_focused: false,
                path_input: String::new(),
                path_replace_on_input: false,
                search_focused: false,
                last_click: None,
                next_context_request: 0,
                active_context_request: None,
            }),
        }
    }

    fn navigate(state: &mut FilesState, path: impl Into<PathBuf>) -> bool {
        if state.browser.navigate(path) {
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
        if is_double_click && entry.is_directory() {
            return Self::navigate(state, entry.path);
        }
        true
    }
}

impl View for FilesView {
    fn measure(&self, constraints: Constraints, _context: &mut MeasureContext<'_>) -> Size {
        constraints.constrain(constraints.maximum)
    }

    fn paint(&self, bounds: Rect, context: &mut PaintContext<'_>) {
        let layout = Layout::new(bounds);
        let state = self.state.borrow();

        Rectangle::new()
            .color(RectangleColor::Custom(WINDOW_BACKGROUND))
            .paint(bounds, context);
        paint_toolbar(&layout, &state, context);
        paint_sidebar(&layout, &state, context);
        paint_content(&layout, &state, context);
        paint_status(&layout, &state, context);
    }

    fn handle_event(
        &self,
        bounds: Rect,
        event: &ViewEvent,
        context: &mut EventContext<'_>,
    ) -> EventResult {
        let layout = Layout::new(bounds);
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
                    Some(HitTarget::Up) => {
                        let changed = state.browser.go_up();
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
                    Some(HitTarget::Sidebar(index)) => SIDEBAR_ITEMS
                        .get(index)
                        .is_some_and(|item| Self::navigate(&mut state, item.path)),
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
                let target = hit_test(&layout, *position, &state);
                let open_enabled = if let Some(HitTarget::Entry(index)) = target {
                    let entry = state.browser.entries().get(index).cloned().cloned();
                    if let Some(entry) = entry {
                        let is_directory = entry.is_directory();
                        state.browser.select(entry.path);
                        is_directory
                    } else {
                        false
                    }
                } else {
                    false
                };
                state.next_context_request = state.next_context_request.wrapping_add(1).max(1);
                let request_id = state.next_context_request;
                state.active_context_request = Some(request_id);
                context.show_context_menu(ContextMenuRequest {
                    request_id,
                    position: *position,
                    items: vec![
                        ContextMenuItem {
                            command_id: CONTEXT_COMMAND_OPEN,
                            label: String::from("Open"),
                            enabled: open_enabled,
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
                    ],
                });
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
                let changed = match command_id {
                    Some(CONTEXT_COMMAND_OPEN) => state.browser.open_selected(),
                    Some(CONTEXT_COMMAND_RELOAD) => {
                        state.browser.reload();
                        true
                    }
                    _ => false,
                };
                if changed {
                    state.scroll = 0.0;
                    context.request_redraw_in(bounds);
                }
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
            ViewEvent::KeyPressed { key, .. } => {
                let mut state = self.state.borrow_mut();
                let changed = match key {
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
                        let opened = state.browser.open_selected();
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
        let content_height = (bounds.size.height - TOOLBAR_HEIGHT - STATUS_HEIGHT).max(0.0);
        Self {
            bounds,
            toolbar: Rect::new(
                bounds.origin.x,
                bounds.origin.y,
                bounds.size.width,
                TOOLBAR_HEIGHT,
            ),
            sidebar: Rect::new(
                bounds.origin.x,
                bounds.origin.y + TOOLBAR_HEIGHT,
                sidebar_width,
                content_height,
            ),
            content: Rect::new(
                bounds.origin.x + sidebar_width,
                bounds.origin.y + TOOLBAR_HEIGHT,
                (bounds.size.width - sidebar_width).max(0.0),
                content_height,
            ),
            status: Rect::new(
                bounds.origin.x,
                bounds.origin.y + bounds.size.height - STATUS_HEIGHT,
                bounds.size.width,
                STATUS_HEIGHT,
            ),
            sidebar_width,
        }
    }

    fn toolbar_button(&self, index: usize) -> Rect {
        Rect::new(
            self.bounds.origin.x + 13.0 + index as f32 * 35.0,
            self.bounds.origin.y + 10.0,
            30.0,
            32.0,
        )
    }

    fn mode_button(&self, index: usize) -> Rect {
        let right = self.bounds.origin.x + self.bounds.size.width;
        Rect::new(
            right - 294.0 + index as f32 * 32.0,
            self.bounds.origin.y + 11.0,
            32.0,
            30.0,
        )
    }

    fn path(&self) -> Rect {
        Rect::new(
            self.bounds.origin.x + 125.0,
            self.bounds.origin.y + 11.0,
            (self.bounds.size.width - 429.0).max(80.0),
            30.0,
        )
    }

    fn search(&self) -> Rect {
        let right = self.bounds.origin.x + self.bounds.size.width;
        Rect::new(right - 218.0, self.bounds.origin.y + 11.0, 202.0, 30.0)
    }
}

fn paint_toolbar(layout: &Layout, state: &FilesState, context: &mut PaintContext<'_>) {
    Rectangle::new()
        .color(RectangleColor::Custom(TOOLBAR_BACKGROUND))
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
    paint_icon_button(
        layout.toolbar_button(2),
        IconName::FolderOpen,
        state.browser.current_dir() != Path::new("/"),
        state.hover == Some(HitTarget::Up),
        context,
    );
    let path = layout.path();
    Rectangle::new()
        .color(RectangleColor::Custom(if state.path_focused {
            Color::WHITE
        } else {
            SEARCH_BACKGROUND
        }))
        .radius(viewkit::theme::CornerRadius::Custom(6.0))
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
            color: SELECTION,
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
            path.origin.y + 5.0,
            path.size.width - 18.0,
            20.0,
        ),
        13.0,
        400,
        TEXT_PRIMARY,
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
            Color::WHITE
        } else {
            SEARCH_BACKGROUND
        }))
        .radius(viewkit::theme::CornerRadius::Custom(6.0))
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
            color: SELECTION,
            width: 1.0,
        });
    }
    Icon::new(IconName::Search)
        .size(14.0)
        .color(TEXT_SECONDARY)
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
            search.origin.y + 6.0,
            search.size.width - 36.0,
            20.0,
        ),
        13.0,
        400,
        if state.browser.search().is_empty() {
            TEXT_SECONDARY
        } else {
            TEXT_PRIMARY
        },
        TextAlignment::Start,
        context,
    );
}

fn paint_sidebar(layout: &Layout, state: &FilesState, context: &mut PaintContext<'_>) {
    Rectangle::new()
        .color(RectangleColor::Custom(SIDEBAR_BACKGROUND))
        .paint(layout.sidebar, context);
    context.display_list.push(DrawCommand::StrokeRect {
        rect: Rect::new(
            layout.sidebar.origin.x + layout.sidebar.size.width - 0.5,
            layout.sidebar.origin.y,
            1.0,
            layout.sidebar.size.height,
        ),
        color: BORDER,
        width: 1.0,
    });

    paint_text(
        "Favorites",
        Rect::new(
            layout.sidebar.origin.x + 16.0,
            layout.sidebar.origin.y + 16.0,
            layout.sidebar_width - 30.0,
            18.0,
        ),
        11.0,
        600,
        TEXT_SECONDARY,
        TextAlignment::Start,
        context,
    );
    for (index, item) in SIDEBAR_ITEMS.iter().enumerate() {
        paint_sidebar_item(
            layout,
            state,
            index,
            item,
            42.0 + index as f32 * 34.0,
            context,
        );
    }
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
    let selected = state.browser.current_dir() == Path::new(item.path);
    let hovered = state.hover == Some(HitTarget::Sidebar(index));
    if selected || hovered {
        Rectangle::new()
            .color(RectangleColor::Custom(if selected {
                Color::from_rgb_hex(0xd0d0d0)
            } else {
                Color::from_rgb_hex(0xdfdfdf)
            }))
            .radius(viewkit::theme::CornerRadius::Custom(5.0))
            .paint(bounds, context);
    }
    if let Some(icon) = state.icons.sidebar(item.icon) {
        Svg::new(icon.clone()).paint(
            Rect::new(bounds.origin.x + 7.5, bounds.origin.y + 4.5, 20.0, 20.0),
            context,
        );
    }
    paint_text(
        item.label,
        Rect::new(
            bounds.origin.x + 34.0,
            bounds.origin.y + 4.0,
            bounds.size.width - 40.0,
            21.0,
        ),
        13.0,
        if selected { 500 } else { 400 },
        TEXT_PRIMARY,
        TextAlignment::Start,
        context,
    );
}

fn paint_content(layout: &Layout, state: &FilesState, context: &mut PaintContext<'_>) {
    Rectangle::new()
        .color(RectangleColor::Custom(CONTENT_BACKGROUND))
        .paint(layout.content, context);
    context.display_list.push(DrawCommand::PushClip {
        rect: layout.content,
    });
    match state.browser.view_mode() {
        ViewMode::List => paint_list(layout, state, context),
        ViewMode::Grid => paint_grid(layout, state, context),
    }
    if let Some(error) = state.browser.error() {
        paint_text(
            error,
            Rect::new(
                layout.content.origin.x + 28.0,
                layout.content.origin.y + 56.0,
                layout.content.size.width - 56.0,
                44.0,
            ),
            14.0,
            400,
            TEXT_SECONDARY,
            TextAlignment::Center,
            context,
        );
    }
    context.display_list.push(DrawCommand::PopClip);
}

fn paint_list(layout: &Layout, state: &FilesState, context: &mut PaintContext<'_>) {
    let header = Rect::new(
        layout.content.origin.x,
        layout.content.origin.y,
        layout.content.size.width,
        LIST_HEADER_HEIGHT,
    );
    Rectangle::new()
        .color(RectangleColor::Custom(Color::from_rgb_hex(0xf7f7f7)))
        .paint(header, context);
    stroke_bottom(header, context);
    let columns = list_columns(layout.content);
    paint_text(
        "Name",
        columns[0],
        11.0,
        600,
        TEXT_SECONDARY,
        TextAlignment::Start,
        context,
    );
    paint_text(
        "Date Modified",
        columns[1],
        11.0,
        600,
        TEXT_SECONDARY,
        TextAlignment::Start,
        context,
    );
    paint_text(
        "Size",
        columns[2],
        11.0,
        600,
        TEXT_SECONDARY,
        TextAlignment::End,
        context,
    );
    paint_text(
        "Kind",
        columns[3],
        11.0,
        600,
        TEXT_SECONDARY,
        TextAlignment::Start,
        context,
    );

    let entries = state.browser.entries();
    for (index, entry) in entries.iter().enumerate() {
        let y = layout.content.origin.y + LIST_HEADER_HEIGHT + index as f32 * LIST_ROW_HEIGHT
            - state.scroll;
        let row = Rect::new(
            layout.content.origin.x,
            y,
            layout.content.size.width,
            LIST_ROW_HEIGHT,
        );
        if row.origin.y + row.size.height <= header.origin.y + header.size.height
            || row.origin.y >= layout.status.origin.y
        {
            continue;
        }
        let selected = state.browser.selected() == Some(entry.path.as_path());
        let hovered = state.hover == Some(HitTarget::Entry(index));
        if selected || hovered {
            Rectangle::new()
                .color(RectangleColor::Custom(if selected {
                    SELECTION
                } else {
                    ROW_HOVER
                }))
                .paint(row, context);
        }
        let text_color = if selected { Color::WHITE } else { TEXT_PRIMARY };
        let secondary = if selected {
            Color::WHITE
        } else {
            TEXT_SECONDARY
        };
        let cols = list_columns(row);
        if let Some(icon) = state.icons.entry(entry) {
            Svg::new(icon.clone()).paint(
                Rect::new(cols[0].origin.x + 1.0, cols[0].origin.y + 5.0, 18.0, 18.0),
                context,
            );
        }
        paint_text(
            entry.name.clone(),
            Rect::new(
                cols[0].origin.x + 24.0,
                cols[0].origin.y,
                cols[0].size.width - 24.0,
                cols[0].size.height,
            ),
            13.0,
            400,
            text_color,
            TextAlignment::Start,
            context,
        );
        paint_text(
            entry.modified.clone(),
            cols[1],
            12.0,
            400,
            secondary,
            TextAlignment::Start,
            context,
        );
        paint_text(
            entry.size_label(),
            cols[2],
            12.0,
            400,
            secondary,
            TextAlignment::End,
            context,
        );
        paint_text(
            entry.kind.label(),
            cols[3],
            12.0,
            400,
            secondary,
            TextAlignment::Start,
            context,
        );
        stroke_bottom(row, context);
    }
}

fn paint_grid(layout: &Layout, state: &FilesState, context: &mut PaintContext<'_>) {
    let columns = grid_column_count(layout.content);
    let content_width = columns as f32 * GRID_CELL_WIDTH;
    let left =
        layout.content.origin.x + ((layout.content.size.width - content_width) / 2.0).max(12.0);
    for (index, entry) in state.browser.entries().iter().enumerate() {
        let column = index % columns;
        let row = index / columns;
        let cell = Rect::new(
            left + column as f32 * GRID_CELL_WIDTH,
            layout.content.origin.y + 16.0 + row as f32 * GRID_CELL_HEIGHT - state.scroll,
            GRID_CELL_WIDTH,
            GRID_CELL_HEIGHT,
        );
        if cell.origin.y + cell.size.height <= layout.content.origin.y
            || cell.origin.y >= layout.status.origin.y
        {
            continue;
        }
        let selected = state.browser.selected() == Some(entry.path.as_path());
        let hovered = state.hover == Some(HitTarget::Entry(index));
        if hovered {
            Rectangle::new()
                .color(RectangleColor::Custom(ROW_HOVER))
                .radius(viewkit::theme::CornerRadius::Custom(6.0))
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
        if let Some(icon) = state.icons.entry(entry) {
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
                .color(RectangleColor::Custom(SELECTION))
                .radius(viewkit::theme::CornerRadius::Custom(4.0))
                .paint(label, context);
        }
        paint_text(
            entry.name.clone(),
            label,
            12.0,
            400,
            if selected { Color::WHITE } else { TEXT_PRIMARY },
            TextAlignment::Center,
            context,
        );
    }
}

fn paint_status(layout: &Layout, state: &FilesState, context: &mut PaintContext<'_>) {
    Rectangle::new()
        .color(RectangleColor::Custom(TOOLBAR_BACKGROUND))
        .paint(layout.status, context);
    context.display_list.push(DrawCommand::StrokeRect {
        rect: Rect::new(
            layout.status.origin.x,
            layout.status.origin.y + 0.5,
            layout.status.size.width,
            1.0,
        ),
        color: BORDER,
        width: 1.0,
    });
    let count = state.browser.entries().len();
    let selection = usize::from(state.browser.selected().is_some());
    let summary = if selection == 0 {
        format!("{count} items")
    } else {
        format!("{count} items, {selection} selected")
    };
    paint_text(
        summary,
        Rect::new(
            layout.status.origin.x + layout.sidebar_width + 12.0,
            layout.status.origin.y + 5.0,
            180.0,
            18.0,
        ),
        11.0,
        400,
        TEXT_SECONDARY,
        TextAlignment::Start,
        context,
    );
    paint_text(
        layout_path(state.browser.current_dir()),
        Rect::new(
            layout.status.origin.x + layout.sidebar_width + 200.0,
            layout.status.origin.y + 5.0,
            layout.status.size.width - layout.sidebar_width - 214.0,
            18.0,
        ),
        11.0,
        400,
        TEXT_SECONDARY,
        TextAlignment::End,
        context,
    );
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
            .color(RectangleColor::Custom(Color::from_rgb_hex(0xe2e2e2)))
            .radius(viewkit::theme::CornerRadius::Custom(5.0))
            .paint(bounds, context);
    }
    Icon::new(icon)
        .size(17.0)
        .color(if enabled { TEXT_PRIMARY } else { DISABLED })
        .paint(
            Rect::new(bounds.origin.x + 6.5, bounds.origin.y + 7.5, 17.0, 17.0),
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
                Color::from_rgb_hex(0xd3d3d3)
            } else {
                Color::from_rgb_hex(0xe5e5e5)
            }))
            .radius(viewkit::theme::CornerRadius::Custom(4.0))
            .paint(bounds, context);
    }
    Icon::new(icon).size(16.0).color(TEXT_PRIMARY).paint(
        Rect::new(bounds.origin.x + 8.0, bounds.origin.y + 7.0, 16.0, 16.0),
        context,
    );
}

fn paint_text(
    value: impl Into<String>,
    bounds: Rect,
    size: f32,
    weight: u16,
    color: Color,
    alignment: TextAlignment,
    context: &mut PaintContext<'_>,
) {
    Text::new(value)
        .font_size(size)
        .line_height(bounds.size.height.max(size))
        .weight(weight)
        .color(color)
        .alignment(alignment)
        .paint(bounds, context);
}

fn stroke_bottom(bounds: Rect, context: &mut PaintContext<'_>) {
    context.display_list.push(DrawCommand::StrokeRect {
        rect: Rect::new(
            bounds.origin.x,
            bounds.origin.y + bounds.size.height - 0.5,
            bounds.size.width,
            1.0,
        ),
        color: BORDER,
        width: 1.0,
    });
}

fn list_columns(bounds: Rect) -> [Rect; 4] {
    let width = bounds.size.width;
    let name = (width * 0.44).max(180.0).min(width);
    let modified = (width * 0.23).max(120.0).min((width - name).max(0.0));
    let size = 100.0_f32.min((width - name - modified).max(0.0));
    [
        Rect::new(
            bounds.origin.x + 10.0,
            bounds.origin.y + 5.0,
            (name - 16.0).max(0.0),
            bounds.size.height - 8.0,
        ),
        Rect::new(
            bounds.origin.x + name,
            bounds.origin.y + 5.0,
            (modified - 10.0).max(0.0),
            bounds.size.height - 8.0,
        ),
        Rect::new(
            bounds.origin.x + name + modified,
            bounds.origin.y + 5.0,
            (size - 14.0).max(0.0),
            bounds.size.height - 8.0,
        ),
        Rect::new(
            bounds.origin.x + name + modified + size + 12.0,
            bounds.origin.y + 5.0,
            (width - name - modified - size - 20.0).max(0.0),
            bounds.size.height - 8.0,
        ),
    ]
}

fn hit_test(layout: &Layout, point: Point, state: &FilesState) -> Option<HitTarget> {
    for index in 0..3 {
        if layout.toolbar_button(index).contains(point) {
            return Some(match index {
                0 => HitTarget::Back,
                1 => HitTarget::Forward,
                _ => HitTarget::Up,
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
        let offsets = [42.0, 76.0, 110.0, 144.0];
        for (index, offset) in offsets.iter().enumerate() {
            let row = Rect::new(
                layout.sidebar.origin.x + 8.0,
                layout.sidebar.origin.y + *offset,
                layout.sidebar_width - 16.0,
                29.0,
            );
            if row.contains(point) {
                return Some(HitTarget::Sidebar(index));
            }
        }
    }
    if layout.content.contains(point) {
        let count = state.browser.entries().len();
        let index = match state.browser.view_mode() {
            ViewMode::List => {
                let relative =
                    point.y - layout.content.origin.y - LIST_HEADER_HEIGHT + state.scroll;
                (relative >= 0.0).then_some((relative / LIST_ROW_HEIGHT) as usize)
            }
            ViewMode::Grid => {
                let columns = grid_column_count(layout.content);
                let content_width = columns as f32 * GRID_CELL_WIDTH;
                let left = layout.content.origin.x
                    + ((layout.content.size.width - content_width) / 2.0).max(12.0);
                let x = point.x - left;
                let y = point.y - layout.content.origin.y - 16.0 + state.scroll;
                if x >= 0.0 && y >= 0.0 {
                    Some((y / GRID_CELL_HEIGHT) as usize * columns + (x / GRID_CELL_WIDTH) as usize)
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
        ViewMode::List => LIST_HEADER_HEIGHT + count as f32 * LIST_ROW_HEIGHT,
        ViewMode::Grid => {
            let columns = grid_column_count(layout.content);
            24.0 + count.div_ceil(columns) as f32 * GRID_CELL_HEIGHT
        }
    };
    (content_height - layout.content.size.height).max(0.0)
}

fn grid_column_count(content: Rect) -> usize {
    (content.size.width / GRID_CELL_WIDTH).floor().max(1.0) as usize
}

fn layout_path(path: &Path) -> String {
    path.display().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

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
