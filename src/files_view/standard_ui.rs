//! Files composes standard controls here; filesystem operations stay in FilesView.
use super::*;
use viewkit::prelude::{
    ButtonInteractionState, HStack, ListRow, Scroll, ScrollState, SidebarItem as NavigationItem,
    Surface, TextField, TextFieldInteractionState, TextFieldSize, TextTone, Toolbar, VStack,
    ViewExt,
};

pub(super) struct Controls {
    path: TextFieldInteractionState,
    search: TextFieldInteractionState,
    buttons: RefCell<BTreeMap<String, ButtonInteractionState>>,
    pending: Rc<RefCell<Vec<Action>>>,
    content_scroll: ScrollState,
    sidebar_scroll: ScrollState,
}

enum Action {
    Target(HitTarget),
    SubmitPath,
    SubmitName,
}

impl Controls {
    pub(super) fn new() -> Self {
        Self {
            path: TextFieldInteractionState::new(),
            search: TextFieldInteractionState::new(),
            buttons: RefCell::new(BTreeMap::new()),
            pending: Rc::new(RefCell::new(Vec::new())),
            content_scroll: ScrollState::new(),
            sidebar_scroll: ScrollState::new(),
        }
    }
    fn button(&self, key: impl Into<String>) -> ButtonInteractionState {
        let key = key.into();
        let mut buttons = self.buttons.borrow_mut();
        if buttons.len() >= 1024 && !buttons.contains_key(&key) {
            buttons.clear();
        }
        buttons.entry(key).or_default().clone()
    }
    fn select(&self, target: HitTarget) -> impl FnMut() + 'static {
        let pending = self.pending.clone();
        move || pending.borrow_mut().push(Action::Target(target.clone()))
    }
    fn submit(&self, path: bool) -> impl FnMut() + 'static {
        let pending = self.pending.clone();
        move || {
            pending.borrow_mut().push(if path {
                Action::SubmitPath
            } else {
                Action::SubmitName
            })
        }
    }
}

struct Placed {
    bounds: Rect,
    view: Box<dyn View>,
}
impl Placed {
    fn new(bounds: Rect, view: impl View + 'static) -> Self {
        Self {
            bounds,
            view: Box::new(view),
        }
    }
    fn paint(&self, context: &mut PaintContext<'_>) {
        self.view.paint(self.bounds, context);
    }
}

fn synchronize(state: &FilesState) {
    let path = if state.path_focused {
        state.path_input.clone()
    } else {
        state.browser.current_dir().display().to_string()
    };
    if state.controls.path.value() != path {
        state.controls.path.set_value(path);
    }
    state.controls.path.set_focused(state.path_focused);
    if state.controls.search.value() != state.browser.search() {
        state.controls.search.set_value(state.browser.search());
    }
    state.controls.search.set_focused(state.search_focused);
    state.controls.content_scroll.set_offset(0., state.scroll);
    state
        .controls
        .sidebar_scroll
        .set_offset(0., state.sidebar_scroll);
}

fn path_field(state: &FilesState) -> TextField {
    TextField::with_interaction(state.controls.path.clone())
        .size(TextFieldSize::Small)
        .placeholder("Location")
        .on_submit(state.controls.submit(true))
}
fn search_field(state: &FilesState) -> TextField {
    TextField::with_interaction(state.controls.search.clone())
        .size(TextFieldSize::Small)
        .placeholder("Search")
        .leading_symbol(IconName::Search)
}
fn name_field(state: &FilesState, edit: &NameEdit) -> TextField {
    TextField::with_interaction(edit.input.clone())
        .size(TextFieldSize::Small)
        .on_submit(state.controls.submit(false))
}

fn toolbar_button(
    state: &FilesState,
    target: HitTarget,
    label: &'static str,
    symbol: IconName,
    enabled: bool,
    selected: bool,
) -> Button {
    Button::with_interaction_and_label(state.controls.button(label), "")
        .leading_symbol(symbol)
        .size(ButtonSize::Small)
        .enabled(enabled)
        .style(if selected {
            ButtonStyle::Standard
        } else {
            ButtonStyle::Ghost
        })
        .accessibility_label(label)
        .accessibility_selected(selected)
        .on_click(state.controls.select(target))
}

fn toolbar(state: &FilesState) -> impl View {
    let side = Theme::current().layout.compact_control_height;
    Toolbar::new(
        HStack::new()
            .gap(viewkit::prelude::StackGap::Small)
            .child(
                toolbar_button(
                    state,
                    HitTarget::Back,
                    "Back",
                    IconName::ChevronLeft,
                    state.browser.can_go_back(),
                    false,
                )
                .frame(side, side)
                .flex_shrink(0.),
            )
            .child(
                toolbar_button(
                    state,
                    HitTarget::Forward,
                    "Forward",
                    IconName::ChevronRight,
                    state.browser.can_go_forward(),
                    false,
                )
                .frame(side, side)
                .flex_shrink(0.),
            )
            .child(path_field(state).layout().flex_grow(1.).flex_shrink(1.))
            .child(
                toolbar_button(
                    state,
                    HitTarget::ListMode,
                    "List view",
                    IconName::LayoutList,
                    true,
                    state.browser.view_mode() == ViewMode::List,
                )
                .frame(side, side)
                .flex_shrink(0.),
            )
            .child(
                toolbar_button(
                    state,
                    HitTarget::GridMode,
                    "Icon view",
                    IconName::LayoutGrid,
                    true,
                    state.browser.view_mode() == ViewMode::Grid,
                )
                .frame(side, side)
                .flex_shrink(0.),
            )
            .child(search_field(state).layout().width(202.).flex_shrink(1.)),
    )
}

pub(super) fn paint_toolbar(layout: &Layout, state: &FilesState, context: &mut PaintContext<'_>) {
    synchronize(state);
    toolbar(state).paint(layout.toolbar, context);
}

struct Pane<'a> {
    layout: &'a Layout,
    state: &'a FilesState,
    sidebar: bool,
}
impl View for Pane<'_> {
    fn paint(&self, _: Rect, context: &mut PaintContext<'_>) {
        if self.sidebar {
            paint_sidebar(self.layout, self.state, context);
        } else {
            paint_content(self.layout, self.state, context);
        }
    }
}
pub(super) fn paint_split(layout: &Layout, state: &FilesState, context: &mut PaintContext<'_>) {
    let body = Rect::new(
        layout.sidebar.origin.x,
        layout.sidebar.origin.y,
        layout.bounds.size.width,
        layout.sidebar.size.height,
    );
    viewkit::prelude::NavigationSplitView::new(
        Pane {
            layout,
            state,
            sidebar: true,
        },
        Pane {
            layout,
            state,
            sidebar: false,
        },
    )
    .sidebar_insets(viewkit::edge_insets::EdgeInsets::ZERO)
    .paint(body, context);
}

#[derive(Clone)]
enum FileImage {
    Raster(ImageData),
    Vector(SvgData),
    Missing,
}
impl View for FileImage {
    fn measure(&self, constraints: Constraints, _: &mut MeasureContext<'_>) -> Size {
        constraints.constrain(Size::new(20., 20.))
    }
    fn paint(&self, bounds: Rect, context: &mut PaintContext<'_>) {
        match self {
            Self::Raster(image) => Image::new(image.clone())
                .content_mode(ImageContentMode::Fit)
                .paint(bounds, context),
            Self::Vector(svg) => Svg::new(svg.clone()).paint(bounds, context),
            Self::Missing => Icon::new(IconName::File).paint(bounds, context),
        }
    }
}
fn file_image(state: &FilesState, entry: &FileEntry) -> FileImage {
    state
        .icons
        .document(entry)
        .cloned()
        .map(FileImage::Raster)
        .or_else(|| state.icons.entry(entry).cloned().map(FileImage::Vector))
        .unwrap_or(FileImage::Missing)
}
fn sidebar_image(state: &FilesState, icon: SidebarIcon) -> impl View + 'static {
    HStack::new().child(
        state
            .icons
            .sidebar(icon)
            .cloned()
            .map(FileImage::Vector)
            .unwrap_or(FileImage::Missing)
            .frame(20., 20.),
    )
}

fn sidebar_rows(layout: &Layout, state: &FilesState) -> Vec<Placed> {
    let mut items = Vec::new();
    for (index, item) in state.sidebar_items.iter().enumerate() {
        let bounds = Rect::new(
            layout.sidebar.origin.x + 8.,
            layout.sidebar.origin.y + 42. + index as f32 * 34. - state.sidebar_scroll,
            layout.sidebar_width - 16.,
            29.,
        );
        if bounds.intersection(layout.sidebar).is_none() {
            continue;
        }
        let selected = if item.path.is_dir() {
            state.browser.current_dir() == item.path
        } else {
            state.browser.selected() == Some(item.path.as_path())
        };
        items.push(Placed::new(
            bounds,
            NavigationItem::with_interaction(
                state
                    .controls
                    .button(format!("favorite:{}", item.path.display())),
                &item.label,
            )
            .leading(sidebar_image(state, item.icon))
            .selected(selected)
            .on_select(state.controls.select(HitTarget::Sidebar(index))),
        ));
    }
    for (index, row) in state.folder_tree.rows().iter().enumerate() {
        let bounds = tree_row_rect(layout, state, index);
        if bounds.intersection(layout.sidebar).is_none() {
            continue;
        }
        let indent = (row.depth as f32 * 16.).min((layout.sidebar_width - 90.).max(0.));
        let title = if row.depth == 0 {
            "/".into()
        } else {
            row.path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned()
        };
        let item = NavigationItem::with_interaction(
            state
                .controls
                .button(format!("tree:{}", row.path.display())),
            title,
        )
        .leading(sidebar_image(state, SidebarIcon::Folder))
        .indent(indent + 20.)
        .selected(state.browser.current_dir() == row.path)
        .on_select(state.controls.select(HitTarget::TreeNavigate(index)));
        let disclosure = row.expandable.then(|| {
            Button::with_interaction_and_label(
                state
                    .controls
                    .button(format!("expand:{}", row.path.display())),
                "",
            )
            .leading_symbol(if row.expanded {
                IconName::ChevronDown
            } else {
                IconName::ChevronRight
            })
            .size(ButtonSize::Small)
            .style(ButtonStyle::Ghost)
            .accessibility_label(if row.expanded {
                "Collapse folder"
            } else {
                "Expand folder"
            })
            .on_click(state.controls.select(HitTarget::TreeExpand(index)))
        });
        items.push(Placed::new(
            bounds,
            TreeItem {
                item,
                disclosure,
                indent,
            },
        ));
    }
    items
}

struct TreeItem {
    item: NavigationItem,
    disclosure: Option<Button>,
    indent: f32,
}
impl TreeItem {
    fn arrow(&self, bounds: Rect) -> Rect {
        Rect::new(
            bounds.origin.x + self.indent,
            bounds.origin.y,
            20.,
            bounds.size.height,
        )
    }
}
impl View for TreeItem {
    fn paint(&self, bounds: Rect, context: &mut PaintContext<'_>) {
        self.item.paint(bounds, context);
        if let Some(button) = &self.disclosure {
            button.paint(self.arrow(bounds), context);
        }
    }
    fn handle_event(
        &self,
        bounds: Rect,
        event: &ViewEvent,
        context: &mut EventContext<'_>,
    ) -> EventResult {
        if let Some(button) = &self.disclosure {
            let result = button.handle_event(self.arrow(bounds), event, context);
            if result == EventResult::Consumed && !matches!(event, ViewEvent::PointerMoved { .. }) {
                return result;
            }
        }
        self.item.handle_event(bounds, event, context)
    }
}

pub(super) fn paint_sidebar(layout: &Layout, state: &FilesState, context: &mut PaintContext<'_>) {
    context.display_list.push(DrawCommand::PushClip {
        rect: layout.sidebar,
    });
    Text::caption("Favorites").tone(TextTone::Secondary).paint(
        Rect::new(
            layout.sidebar.origin.x + 16.,
            layout.sidebar.origin.y + 16. - state.sidebar_scroll,
            layout.sidebar_width - 30.,
            18.,
        ),
        context,
    );
    VStack::new().child(viewkit::prelude::Divider::new()).paint(
        Rect::new(
            layout.sidebar.origin.x + 12.,
            tree_start_y(layout, state) - 12.,
            layout.sidebar_width - 24.,
            1.,
        ),
        context,
    );
    for row in sidebar_rows(layout, state) {
        row.paint(context);
    }
    context.display_list.push(DrawCommand::PopClip);
    scroller(layout, state, true).paint(layout.sidebar, context);
}

fn entry_rect(layout: &Layout, state: &FilesState, index: usize) -> Rect {
    let theme = Theme::current();
    match state.browser.view_mode() {
        ViewMode::List => Rect::new(
            layout.content.origin.x,
            layout.content.origin.y
                + theme.layout.compact_control_height
                + index as f32 * theme.layout.control_height
                - state.scroll,
            layout.content.size.width,
            theme.layout.control_height,
        ),
        ViewMode::Grid => {
            let columns = grid_column_count(layout.content);
            Rect::new(
                layout.content.origin.x
                    + GRID_SIDE_INSET
                    + (index % columns) as f32 * theme.layout.browser_grid_cell_width,
                layout.content.origin.y
                    + GRID_HEADER_HEIGHT
                    + 12.
                    + (index / columns) as f32 * theme.layout.browser_grid_cell_height
                    - state.scroll,
                theme.layout.browser_grid_cell_width,
                theme.layout.browser_grid_cell_height,
            )
        }
    }
}
fn name_rect(bounds: Rect, mode: ViewMode) -> Rect {
    if mode == ViewMode::List {
        let column = list_columns(bounds)[0];
        Rect::new(
            column.origin.x + 24.,
            bounds.origin.y,
            (column.size.width - 24.).max(0.),
            bounds.size.height,
        )
    } else {
        Rect::new(
            bounds.origin.x + 6.,
            bounds.origin.y + 78.,
            bounds.size.width - 12.,
            28.,
        )
    }
}
fn header_height(mode: ViewMode, theme: &Theme) -> f32 {
    if mode == ViewMode::List {
        theme.layout.compact_control_height
    } else {
        GRID_HEADER_HEIGHT
    }
}

struct EntryContent {
    entry: FileEntry,
    image: FileImage,
    mode: ViewMode,
    editing: bool,
    selected: bool,
}
impl View for EntryContent {
    fn paint(&self, bounds: Rect, context: &mut PaintContext<'_>) {
        let name = name_rect(bounds, self.mode);
        let image = if self.mode == ViewMode::List {
            let col = list_columns(bounds)[0];
            Rect::new(
                col.origin.x,
                bounds.origin.y + (bounds.size.height - 18.) * 0.5,
                18.,
                18.,
            )
        } else {
            Rect::new(
                bounds.origin.x + (bounds.size.width - 64.) * 0.5,
                bounds.origin.y + 8.,
                64.,
                64.,
            )
        };
        self.image.paint(image, context);
        if !self.editing {
            Text::styled(
                &self.entry.name,
                if self.mode == ViewMode::List {
                    TextRole::Label
                } else {
                    TextRole::Caption
                },
            )
            .center_visible_ink(true)
            .alignment(if self.mode == ViewMode::List {
                TextAlignment::Start
            } else {
                TextAlignment::Center
            })
            .paint(name, context);
        }
        if self.mode == ViewMode::List {
            let columns = list_columns(bounds);
            for (index, text) in [
                self.entry.modified.clone(),
                self.entry.size_label(),
                self.entry.kind.label().into(),
            ]
            .into_iter()
            .enumerate()
            {
                Text::caption(text)
                    .center_visible_ink(true)
                    .tone(if self.selected {
                        TextTone::Primary
                    } else {
                        TextTone::Secondary
                    })
                    .alignment(if index == 1 {
                        TextAlignment::End
                    } else {
                        TextAlignment::Start
                    })
                    .paint(columns[index + 1], context);
            }
        }
    }
}

fn content_rows(layout: &Layout, state: &FilesState) -> Vec<Placed> {
    let entries = state.browser.entries();
    let top = layout.content.origin.y + header_height(state.browser.view_mode(), &Theme::current());
    entries
        .iter()
        .enumerate()
        .filter_map(|(index, entry)| {
            let bounds = entry_rect(layout, state, index);
            if bounds.origin.y + bounds.size.height <= top
                || bounds.origin.y >= layout.status.origin.y
            {
                return None;
            }
            let selected = state.browser.selected() == Some(entry.path.as_path());
            Some(Placed::new(
                bounds,
                ListRow::with_interaction(
                    state
                        .controls
                        .button(format!("entry:{}", entry.path.display())),
                    &entry.name,
                )
                .selected(selected)
                .adjacent_rows(
                    state.browser.view_mode() == ViewMode::List && index > 0,
                    state.browser.view_mode() == ViewMode::List && index + 1 < entries.len(),
                )
                .content(EntryContent {
                    entry: (*entry).clone(),
                    image: file_image(state, entry),
                    mode: state.browser.view_mode(),
                    editing: state
                        .name_edit
                        .as_ref()
                        .is_some_and(|edit| edit.path == entry.path),
                    selected,
                })
                .on_select(state.controls.select(HitTarget::Entry(index))),
            ))
        })
        .collect()
}
fn rename_field(layout: &Layout, state: &FilesState) -> Option<Placed> {
    let edit = state.name_edit.as_ref()?;
    let index = state
        .browser
        .entries()
        .iter()
        .position(|entry| entry.path == edit.path)?;
    Some(Placed::new(
        name_rect(entry_rect(layout, state, index), state.browser.view_mode()),
        name_field(state, edit),
    ))
}

pub(super) fn focus_selected(
    layout: &Layout,
    state: &mut FilesState,
    context: &mut EventContext<'_>,
) {
    let index = state
        .browser
        .entries()
        .iter()
        .position(|entry| Some(entry.path.as_path()) == state.browser.selected());
    let Some(index) = index else {
        return;
    };
    let row = entry_rect(layout, state, index);
    let top = layout.content.origin.y + header_height(state.browser.view_mode(), context.theme());
    let bottom = layout.content.origin.y + layout.content.size.height;
    if row.origin.y < top {
        state.scroll -= top - row.origin.y;
    } else if row.origin.y + row.size.height > bottom {
        state.scroll += row.origin.y + row.size.height - bottom;
    }
    state.scroll = state.scroll.clamp(0., maximum_scroll(layout, state));
    state.controls.content_scroll.set_offset(0., state.scroll);
    state.path_focused = false;
    state.search_focused = false;
    let bounds = entry_rect(layout, state, index);
    let focus = ViewEvent::KeyboardFocusRequested {
        bounds: Some(bounds),
    };
    toolbar(state).handle_event(layout.toolbar, &focus, context);
    for row in content_rows(layout, state) {
        row.view.handle_event(row.bounds, &focus, context);
    }
    context.request_keyboard_focus(bounds);
}

pub(super) fn paint_content(layout: &Layout, state: &FilesState, context: &mut PaintContext<'_>) {
    let height = header_height(state.browser.view_mode(), context.theme);
    if state.browser.view_mode() == ViewMode::List {
        let header = Rect::new(
            layout.content.origin.x,
            layout.content.origin.y,
            layout.content.size.width,
            height,
        );
        Surface::pane().paint(header, context);
        for (index, title) in ["Name", "Date Modified", "Size", "Kind"]
            .into_iter()
            .enumerate()
        {
            Text::caption(title)
                .tone(TextTone::Secondary)
                .center_visible_ink(true)
                .paint(list_columns(header)[index], context);
        }
    } else {
        let title = state
            .browser
            .current_dir()
            .file_name()
            .unwrap_or_default()
            .to_string_lossy();
        Text::styled(
            if state.browser.current_dir() == state.home_directory {
                "Home"
            } else if title.is_empty() {
                "Computer"
            } else {
                &title
            },
            TextRole::TitleSmall,
        )
        .paint(
            Rect::new(
                layout.content.origin.x + GRID_SIDE_INSET,
                layout.content.origin.y + 16.,
                layout.content.size.width - GRID_SIDE_INSET * 2.,
                32.,
            ),
            context,
        );
    }
    let viewport = Rect::new(
        layout.content.origin.x,
        layout.content.origin.y + height,
        layout.content.size.width,
        (layout.content.size.height - height).max(0.),
    );
    context
        .display_list
        .push(DrawCommand::PushClip { rect: viewport });
    for row in content_rows(layout, state) {
        row.paint(context);
    }
    if let Some(field) = rename_field(layout, state) {
        field.paint(context);
    }
    if state.browser.entries().is_empty() {
        Text::body(if state.browser.search().is_empty() {
            "This folder is empty"
        } else {
            "No matching files"
        })
        .tone(TextTone::Secondary)
        .alignment(TextAlignment::Center)
        .paint(
            Rect::new(
                viewport.origin.x,
                viewport.origin.y + 24.,
                viewport.size.width,
                28.,
            ),
            context,
        );
    }
    context.display_list.push(DrawCommand::PopClip);
    scroller(layout, state, false).paint(layout.content, context);
}

// Files virtualizes rows; standard Scroll owns wheel, dragging and scrollbar
// presentation. The extent and offsets are shared with the virtualized layout.
struct Extent(Size);
impl View for Extent {
    fn measure(&self, constraints: Constraints, _: &mut MeasureContext<'_>) -> Size {
        constraints.constrain(self.0)
    }
    fn paint(&self, _: Rect, _: &mut PaintContext<'_>) {}
}
fn scroller(layout: &Layout, state: &FilesState, sidebar: bool) -> Scroll {
    let (bounds, maximum, scroll) = if sidebar {
        (
            layout.sidebar,
            maximum_sidebar_scroll(layout, state),
            state.controls.sidebar_scroll.clone(),
        )
    } else {
        (
            layout.content,
            maximum_scroll(layout, state),
            state.controls.content_scroll.clone(),
        )
    };
    Scroll::new(scroll).content(Extent(Size::new(
        bounds.size.width,
        bounds.size.height + maximum,
    )))
}

pub(super) fn handle(
    files: &FilesView,
    layout: &Layout,
    event: &ViewEvent,
    context: &mut EventContext<'_>,
) -> bool {
    let mut consumed = false;
    {
        let state = files.state.borrow();
        synchronize(&state);
        // Commit an edit before rebuilding the collection for another target.
        if !matches!(
            event,
            ViewEvent::KeyPressed {
                key: Key::Escape,
                ..
            }
        ) {
            if let Some(field) = rename_field(layout, &state) {
                let top = layout.content.origin.y
                    + header_height(state.browser.view_mode(), context.theme());
                let outside = event
                    .position()
                    .is_some_and(|point| !layout.content.contains(point) || point.y < top);
                if outside {
                    if matches!(event, ViewEvent::PointerPressed { .. }) {
                        if let Some(edit) = &state.name_edit {
                            edit.input.set_focused(false);
                        }
                    }
                    field
                        .view
                        .handle_event(field.bounds, &ViewEvent::PointerLeft, context);
                } else {
                    consumed |= field.view.handle_event(field.bounds, event, context)
                        == EventResult::Consumed;
                }
            }
            // Route keyboard input to the focused field before unrelated buttons.
            if consumed && event.position().is_none() {
                // The rename field owns this event.
            } else {
                consumed |= toolbar(&state).handle_event(layout.toolbar, event, context)
                    == EventResult::Consumed;
            }
        }
    }
    {
        let mut state = files.state.borrow_mut();
        state.path_focused = state.controls.path.is_focused();
        state.search_focused = state.controls.search.is_focused();
        state.path_input = state.controls.path.value();
        if let Some(edit) = &mut state.name_edit {
            edit.value = edit.input.value();
        }
        let search = state.controls.search.value();
        if search != state.browser.search() {
            state.browser.clear_search();
            state.browser.push_search(&search);
            state.scroll = 0.;
            state.controls.content_scroll.reset();
            context.request_redraw();
        }
    }
    let broadcast = matches!(
        event,
        ViewEvent::PointerMoved { .. } | ViewEvent::PointerLeft | ViewEvent::FocusChanged { .. }
    );
    if !consumed || broadcast {
        let state = files.state.borrow();
        for sidebar in [true, false] {
            let bounds = if sidebar {
                layout.sidebar
            } else {
                layout.content
            };
            consumed |= scroller(layout, &state, sidebar).handle_event(bounds, event, context)
                == EventResult::Consumed;
        }
        if !consumed || broadcast {
            let header = header_height(state.browser.view_mode(), context.theme());
            let body = Rect::new(
                layout.content.origin.x,
                layout.content.origin.y + header,
                layout.content.size.width,
                (layout.content.size.height - header).max(0.),
            );
            for (viewport, controls) in [
                (layout.sidebar, sidebar_rows(layout, &state)),
                (body, content_rows(layout, &state)),
            ] {
                for control in controls {
                    let outside = event
                        .position()
                        .is_some_and(|point| !viewport.contains(point));
                    let routed = if outside {
                        &ViewEvent::PointerLeft
                    } else {
                        event
                    };
                    let result = control.view.handle_event(control.bounds, routed, context);
                    if !outside {
                        consumed |= result == EventResult::Consumed;
                    }
                }
            }
        }
    }
    let mut state = files.state.borrow_mut();
    state.scroll = state.controls.content_scroll.offset_y();
    state.sidebar_scroll = state.controls.sidebar_scroll.offset_y();
    let pending = std::mem::take(&mut *state.controls.pending.borrow_mut());
    for action in pending {
        match action {
            Action::Target(target) => {
                if FilesView::commit_name_edit(&mut state) {
                    if let (
                        HitTarget::Entry(index),
                        ViewEvent::KeyPressed {
                            key: Key::Enter, ..
                        },
                    ) = (&target, event)
                    {
                        let entry = state.browser.entries().get(*index).cloned().cloned();
                        if let Some(entry) = entry {
                            FilesView::open_path(&mut state, &entry);
                        }
                    } else {
                        FilesView::apply_target(&mut state, layout, Some(target));
                    }
                }
            }
            Action::SubmitName => {
                FilesView::commit_name_edit(&mut state);
            }
            Action::SubmitPath => {
                let path = PathBuf::from(&state.path_input);
                if FilesView::navigate(&mut state, path) {
                    state.path_focused = false;
                }
            }
        }
        consumed = true;
        context.request_redraw();
    }
    consumed
        || matches!(event, ViewEvent::PointerMoved { position } if layout.toolbar.contains(*position))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};
    use viewkit::platform::KeyModifiers;

    fn fixture() -> (PathBuf, FilesView) {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let root = std::env::temp_dir().join(format!(
            "files-native-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(root.join("child")).unwrap();
        std::fs::write(root.join("note.txt"), "hello").unwrap();
        (
            root.clone(),
            FilesView::new_with_activation(root, None, None),
        )
    }
    fn send(view: &FilesView, event: ViewEvent) {
        let theme = Theme::current();
        let mut text = viewkit::typography::TextMeasurer::new();
        view.handle_event(
            Rect::new(0., 0., 920., 640.),
            &event,
            &mut EventContext::new(&theme, &theme.typography, &mut text),
        );
    }
    fn key(key: Key) -> ViewEvent {
        ViewEvent::KeyPressed {
            key,
            modifiers: KeyModifiers::default(),
        }
    }

    #[test]
    fn native_location_field_replaces_selection_and_navigates_on_submit() {
        let (root, view) = fixture();
        {
            let mut state = view.state.borrow_mut();
            state.path_focused = true;
            state.path_input = root.display().to_string();
        }
        send(&view, ViewEvent::SelectAll);
        send(
            &view,
            ViewEvent::TextInput {
                text: root.join("child").display().to_string(),
            },
        );
        assert_eq!(
            view.state.borrow().path_input,
            root.join("child").display().to_string()
        );
        send(&view, key(Key::Enter));
        assert_eq!(view.current_directory(), root.join("child"));
        assert!(!view.state.borrow().path_focused);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn native_rename_replaces_selected_name_and_escape_cancels() {
        let (root, view) = fixture();
        FilesView::begin_rename(
            &mut view.state.borrow_mut(),
            root.join("note.txt"),
            "note.txt".into(),
        );
        send(
            &view,
            ViewEvent::TextInput {
                text: "renamed.txt".into(),
            },
        );
        send(&view, key(Key::Enter));
        assert_eq!(
            std::fs::read_to_string(root.join("renamed.txt")).unwrap(),
            "hello"
        );
        assert!(!root.join("note.txt").exists());
        FilesView::begin_rename(
            &mut view.state.borrow_mut(),
            root.join("renamed.txt"),
            "renamed.txt".into(),
        );
        send(
            &view,
            ViewEvent::TextInput {
                text: "cancelled.txt".into(),
            },
        );
        send(&view, key(Key::Escape));
        assert!(root.join("renamed.txt").exists());
        assert!(!root.join("cancelled.txt").exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn native_rows_activate_on_release_and_keep_directory_double_click() {
        let (root, view) = fixture();
        let layout = Layout::new(Rect::new(0., 0., 920., 640.));
        let rect = entry_rect(&layout, &view.state.borrow(), 0);
        let position = Point::new(rect.origin.x + 40., rect.origin.y + 30.);
        send(
            &view,
            ViewEvent::PointerPressed {
                position,
                button: PointerButton::Primary,
            },
        );
        assert_eq!(view.state.borrow().browser.selected(), None);
        send(
            &view,
            ViewEvent::PointerReleased {
                position,
                button: PointerButton::Primary,
            },
        );
        assert_eq!(
            view.state.borrow().browser.selected(),
            Some(root.join("child").as_path())
        );
        assert_eq!(view.current_directory(), root);
        send(
            &view,
            ViewEvent::PointerPressed {
                position,
                button: PointerButton::Primary,
            },
        );
        send(
            &view,
            ViewEvent::PointerReleased {
                position,
                button: PointerButton::Primary,
            },
        );
        assert_eq!(view.current_directory(), root.join("child"));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn focused_native_row_opens_with_enter_without_a_double_click() {
        let (root, view) = fixture();
        let layout = Layout::new(Rect::new(0., 0., 920., 640.));
        let bounds = entry_rect(&layout, &view.state.borrow(), 0);
        send(
            &view,
            ViewEvent::KeyboardFocusRequested {
                bounds: Some(bounds),
            },
        );
        send(&view, key(Key::Enter));
        assert_eq!(view.current_directory(), root.join("child"));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn arrow_navigation_moves_native_focus_before_enter() {
        let (root, view) = fixture();
        std::fs::create_dir(root.join("child2")).unwrap();
        {
            let mut state = view.state.borrow_mut();
            state.browser.reload();
            state.browser.select(root.join("child"));
        }
        let layout = Layout::new(Rect::new(0., 0., 920., 640.));
        let bounds = entry_rect(&layout, &view.state.borrow(), 0);
        send(
            &view,
            ViewEvent::KeyboardFocusRequested {
                bounds: Some(bounds),
            },
        );
        send(&view, key(Key::ArrowDown));
        send(&view, key(Key::Enter));
        assert_eq!(view.current_directory(), root.join("child2"));
        std::fs::remove_dir_all(root).unwrap();
    }
}
