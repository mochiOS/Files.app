//! Files' Quick Look UI. Apps generate content; Files owns browsing and presentation.
use appcore::{
    document::{DocumentIconResolver, preview::PreviewContent},
    task::{TaskError, TaskScope, TaskSlot},
};
use std::{
    cell::{Cell, RefCell},
    fs,
    io::Read,
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    rc::Rc,
};
use viewkit::{
    draw_command::DrawCommand,
    event::{EventContext, ViewEvent},
    prelude::*,
    view::{PaintContext, View},
};

#[derive(Clone)]
enum Phase {
    Loading,
    Image(ImageData),
    Text(TextEditorInteractionState, bool),
    Message(String),
}
struct Session {
    paths: Vec<PathBuf>,
    index: usize,
    phase: Phase,
}
#[derive(Clone, Copy)]
enum Action {
    Close,
    Previous,
    Next,
    Open,
}
pub(crate) enum Outcome {
    None,
    Closed,
    Selected(PathBuf),
    Open(PathBuf),
}
pub(crate) struct QuickLook {
    session: Rc<RefCell<Option<Session>>>,
    tasks: Option<TaskScope>,
    slot: TaskSlot,
    pending: Rc<Cell<Option<Action>>>,
    buttons: [ButtonInteractionState; 4],
}
impl QuickLook {
    pub(crate) fn new() -> Self {
        Self {
            session: Rc::new(RefCell::new(None)),
            tasks: None,
            slot: TaskSlot::new(),
            pending: Rc::new(Cell::new(None)),
            buttons: std::array::from_fn(|_| ButtonInteractionState::new()),
        }
    }
    pub(crate) fn is_open(&self) -> bool {
        self.session.borrow().is_some()
    }
    pub(crate) fn show(&mut self, paths: Vec<PathBuf>, selected: &Path) -> bool {
        let Some(index) = paths.iter().position(|path| path == selected) else {
            return false;
        };
        *self.session.borrow_mut() = Some(Session {
            paths,
            index,
            phase: Phase::Loading,
        });
        self.pending.set(None);
        self.load();
        true
    }
    fn close(&mut self) {
        self.slot = TaskSlot::new();
        self.session.borrow_mut().take();
        self.pending.set(None);
    }
    fn load(&mut self) {
        self.slot = TaskSlot::new();
        let path = {
            let mut current = self.session.borrow_mut();
            let Some(session) = current.as_mut() else {
                return;
            };
            session.phase = Phase::Loading;
            session.paths[session.index].clone()
        };
        if self.tasks.is_none() {
            self.tasks = TaskScope::new().ok();
        }
        let Some(tasks) = &self.tasks else {
            if let Some(session) = self.session.borrow_mut().as_mut() {
                session.phase = Phase::Message("The preview could not be started.".into());
            }
            return;
        };
        let session = Rc::downgrade(&self.session);
        let result = self.slot.spawn(
            tasks,
            move |task| load(&path, || task.is_cancel_requested()).map_err(TaskError::Operation),
            move |result| {
                let Some(current) = session.upgrade() else {
                    return;
                };
                let mut current = current.borrow_mut();
                let Some(session) = current.as_mut() else {
                    return;
                };
                session.phase = match result {
                    Ok(PreviewContent::Image(image)) => ImageData::from_premultiplied_rgba8(
                        image.width(),
                        image.height(),
                        image.pixels().to_vec(),
                    )
                    .map(Phase::Image)
                    .unwrap_or_else(|_| Phase::Message("The image could not be displayed.".into())),
                    Ok(PreviewContent::Text { text, truncated }) => {
                        let editor = TextEditorInteractionState::new();
                        editor.set_value(text);
                        Phase::Text(editor, truncated)
                    }
                    Err(TaskError::Operation(message)) => Phase::Message(message),
                    Err(_) => Phase::Message("The preview could not be loaded.".into()),
                };
            },
        );
        if result.is_err() {
            if let Some(session) = self.session.borrow_mut().as_mut() {
                session.phase = Phase::Message("The preview could not be started.".into());
            }
        }
    }
    fn apply(&mut self, action: Action) -> Outcome {
        match action {
            Action::Close => {
                self.close();
                Outcome::Closed
            }
            Action::Open => {
                let path = self
                    .session
                    .borrow()
                    .as_ref()
                    .map(|session| session.paths[session.index].clone());
                self.close();
                path.map(Outcome::Open).unwrap_or(Outcome::Closed)
            }
            Action::Previous | Action::Next => {
                let path = {
                    let mut current = self.session.borrow_mut();
                    let Some(session) = current.as_mut() else {
                        return Outcome::None;
                    };
                    let next = if matches!(action, Action::Previous) {
                        session.index.checked_sub(1)
                    } else {
                        session
                            .index
                            .checked_add(1)
                            .filter(|index| *index < session.paths.len())
                    };
                    let Some(next) = next else {
                        return Outcome::None;
                    };
                    session.index = next;
                    session.paths[next].clone()
                };
                self.load();
                Outcome::Selected(path)
            }
        }
    }
    fn button(&self, index: usize, enabled: bool) -> Button {
        let action = [Action::Close, Action::Previous, Action::Next, Action::Open][index];
        let label = ["Close Preview", "Previous File", "Next File", "Open"][index];
        let pending = self.pending.clone();
        let button = Button::with_interaction_and_label(
            self.buttons[index].clone(),
            if index == 3 { "Open" } else { "" },
        )
        .size(ButtonSize::Small)
        .enabled(enabled)
        .accessibility_label(label)
        .on_click(move || pending.set(Some(action)));
        if index == 3 {
            button.style(ButtonStyle::Standard)
        } else {
            button
                .leading_symbol(
                    [
                        SymbolName::X,
                        SymbolName::ChevronLeft,
                        SymbolName::ChevronRight,
                    ][index],
                )
                .toolbar()
                .style(ButtonStyle::Ghost)
        }
    }
    pub(crate) fn paint(&self, bounds: Rect, context: &mut PaintContext<'_>) {
        let current = self.session.borrow();
        let Some(session) = current.as_ref() else {
            return;
        };
        let layout = Layout::new(bounds, context.theme);
        context.display_list.push(DrawCommand::FillRect {
            rect: bounds,
            color: context.theme.dialog.backdrop,
        });
        let name = session.paths[session.index]
            .file_name()
            .unwrap_or_default()
            .to_string_lossy();
        Dialog::new()
            .accessibility_label(format!("Quick Look: {name}"))
            .paint(layout.panel, context);
        self.button(0, true).paint(layout.buttons[0], context);
        self.button(1, session.index > 0)
            .paint(layout.buttons[1], context);
        self.button(2, session.index + 1 < session.paths.len())
            .paint(layout.buttons[2], context);
        self.button(3, true).paint(layout.buttons[3], context);
        Text::label(name)
            .single_line(true)
            .paint(layout.title, context);
        let mut status = format!("{} of {}", session.index + 1, session.paths.len());
        match &session.phase {
            Phase::Loading => Text::body("Loading preview…")
                .alignment(TextAlignment::Center)
                .paint(layout.message(), context),
            Phase::Image(image) => Image::new(image.clone())
                .content_mode(ImageContentMode::Fit)
                .paint(layout.content, context),
            Phase::Text(editor, truncated) => {
                text_view(editor).paint(layout.content, context);
                if *truncated {
                    status.push_str(" · Preview truncated");
                }
            }
            Phase::Message(message) => Text::body(message)
                .alignment(TextAlignment::Center)
                .paint(layout.message(), context),
        }
        Text::caption(status)
            .single_line(true)
            .paint(layout.status, context);
    }
    pub(crate) fn handle(
        &mut self,
        bounds: Rect,
        event: &ViewEvent,
        context: &mut EventContext<'_>,
    ) -> Outcome {
        let (index, count, phase) = {
            let current = self.session.borrow();
            let Some(session) = current.as_ref() else {
                return Outcome::None;
            };
            (session.index, session.paths.len(), session.phase.clone())
        };
        if let ViewEvent::KeyPressed { key, modifiers } = event {
            if !modifiers.shortcut() && !modifiers.alt() && !modifiers.shift() {
                let action = match key {
                    Key::Space | Key::Escape => Some(Action::Close),
                    Key::ArrowLeft => Some(Action::Previous),
                    Key::ArrowRight => Some(Action::Next),
                    _ => None,
                };
                if let Some(action) = action {
                    context.request_redraw();
                    return self.apply(action);
                }
            }
        }
        let layout = Layout::new(bounds, context.theme());
        for (index, enabled) in [true, index > 0, index + 1 < count, true]
            .into_iter()
            .enumerate()
        {
            self.button(index, enabled)
                .handle_event(layout.buttons[index], event, context);
        }
        if let Some(action) = self.pending.take() {
            context.request_redraw();
            return self.apply(action);
        }
        if let Phase::Text(editor, _) = phase {
            let view = text_view(&editor);
            if matches!(
                event,
                ViewEvent::KeyPressed { .. } | ViewEvent::Command { .. } | ViewEvent::SelectAll
            ) && !editor.is_focused()
            {
                context.request_keyboard_focus(layout.content);
                view.handle_event(
                    layout.content,
                    &ViewEvent::KeyboardFocusRequested {
                        bounds: Some(layout.content),
                    },
                    context,
                );
            }
            view.handle_event(layout.content, event, context);
        }
        // The caller consumes the event while this panel is open; destructive
        // commands and pointer input never fall through to the file browser.
        Outcome::None
    }
}
fn text_view(editor: &TextEditorInteractionState) -> TextEditor {
    TextEditor::with_interaction(editor.clone())
        .read_only(true)
        .monospaced(true)
        .line_wrap(false)
}
fn load(path: &Path, cancelled: impl Fn() -> bool) -> Result<PreviewContent, String> {
    if cancelled() {
        return Err("The preview was cancelled.".into());
    }
    let file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(path)
        .map_err(|_| "This file could not be read.".to_owned())?;
    let before = file
        .metadata()
        .map_err(|_| "This file could not be read.".to_owned())?;
    if !before.is_file() {
        return Err("A preview isn’t available for this item.".into());
    }
    if before.len() > appcore::document::thumbnail::MAX_INPUT_BYTES as u64 {
        return Err("This file is too large for a preview.".into());
    }
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let mut bytes = Vec::new();
    file.take(appcore::document::thumbnail::MAX_INPUT_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "This file could not be read.".to_owned())?;
    if bytes.len() > appcore::document::thumbnail::MAX_INPUT_BYTES {
        return Err("This file is too large for a preview.".into());
    }
    let declared = crate::file_association::declared_content_type(&extension);
    let content_type = if declared == "application/octet-stream"
        && !bytes.contains(&0)
        && std::str::from_utf8(&bytes).is_ok()
    {
        "text/plain"
    } else {
        declared
    };
    let resolver = DocumentIconResolver::new()
        .map_err(|_| "A preview isn’t available for this file.".to_owned())?;
    let provider = resolver
        .preview_provider(&extension, content_type)
        .ok()
        .flatten()
        .ok_or_else(|| "A preview isn’t available for this file.".to_owned())?;
    let content = provider
        .generate(&bytes, &extension, content_type, 1024, &cancelled)
        .map_err(|_| "The preview could not be loaded.".to_owned())?;
    let after = fs::metadata(path).map_err(|_| "This file is no longer available.".to_owned())?;
    if before.len() != after.len() || before.modified().ok() != after.modified().ok() {
        return Err("The file changed. Reopen the preview to try again.".into());
    }
    Ok(content)
}
struct Layout {
    panel: Rect,
    title: Rect,
    content: Rect,
    status: Rect,
    buttons: [Rect; 4],
}
impl Layout {
    fn new(bounds: Rect, theme: &Theme) -> Self {
        let width = (bounds.size.width - theme.dialog.padding * 2.).clamp(0., 760.);
        let height = (bounds.size.height - theme.dialog.padding * 2.).clamp(0., 560.);
        let panel = Rect::new(
            bounds.origin.x + (bounds.size.width - width) / 2.,
            bounds.origin.y + (bounds.size.height - height) / 2.,
            width,
            height,
        );
        let pad = theme.dialog.padding;
        let x = panel.origin.x + pad;
        let y = panel.origin.y + pad;
        let w = (width - pad * 2.).max(0.);
        let h = (height - pad * 2.).max(0.);
        let side = theme.layout.icon_button_size;
        let gap = theme.spacing.small;
        let footer = theme.layout.compact_control_height;
        Self {
            panel,
            title: Rect::new(x + side + gap, y, (w - side * 3. - gap * 3.).max(0.), side),
            content: Rect::new(x, y + side + gap, w, (h - side - footer - gap * 2.).max(0.)),
            status: Rect::new(x, y + h - footer, (w - 80.).max(0.), footer),
            buttons: [
                Rect::new(x, y, side, side),
                Rect::new(x + w - side * 2. - gap, y, side, side),
                Rect::new(x + w - side, y, side, side),
                Rect::new(x + (w - 72.).max(0.), y + h - footer, 72., footer),
            ],
        }
    }
    fn message(&self) -> Rect {
        Rect::new(
            self.content.origin.x,
            self.content.origin.y + (self.content.size.height - 48.).max(0.) / 2.,
            self.content.size.width,
            48.,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn browsing_stays_in_range_and_close_cancels_pending_loads() {
        let mut preview = QuickLook::new();
        // No provider is launched for nonexistent test files.
        assert!(preview.show(
            vec!["/missing/a.txt".into(), "/missing/b.txt".into()],
            Path::new("/missing/a.txt")
        ));
        assert!(matches!(preview.apply(Action::Previous), Outcome::None));
        assert!(
            matches!(preview.apply(Action::Next), Outcome::Selected(path) if path == Path::new("/missing/b.txt"))
        );
        assert!(matches!(preview.apply(Action::Next), Outcome::None));
        assert!(
            matches!(preview.apply(Action::Previous), Outcome::Selected(path) if path == Path::new("/missing/a.txt"))
        );
        assert!(matches!(preview.apply(Action::Close), Outcome::Closed));
        assert!(!preview.is_open());
        assert!(!preview.show(vec![], Path::new("/missing/a.txt")));
    }
    #[test]
    fn layout_fits_normal_and_small_windows() {
        for (w, h) in [(920., 640.), (640., 480.), (320., 240.)] {
            let layout = Layout::new(Rect::new(0., 0., w, h), &Theme::LIGHT);
            assert!(layout.panel.origin.x >= 0. && layout.panel.origin.y >= 0.);
            assert!(layout.panel.origin.x + layout.panel.size.width <= w);
            assert!(layout.content.size.width >= 0. && layout.content.size.height >= 0.);
            assert!(layout.content.origin.y + layout.content.size.height <= layout.status.origin.y);
        }
    }
    #[test]
    fn rejects_nonfiles_before_resolving_a_provider() {
        assert!(load(Path::new("/"), || false).is_err());
        assert!(load(Path::new("/missing/a.txt"), || true).is_err());
    }
}
