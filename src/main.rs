mod browser;
mod file_association;
mod file_clipboard;
mod files_view;
mod folder_tree;
mod operations;
mod sidebar;

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::OnceLock;
use std::sync::atomic::AtomicBool;

use appcore::prelude::ContentType;
use files_view::FilesView;
use mochios_workspace_protocol as workspace_protocol;
use viewkit::event::{EventContext, EventResult, ViewEvent};
use viewkit::prelude::*;
use viewkit::view::{Constraints, MeasureContext, PaintContext};

struct FilesApp {
    files: FilesView,
}

impl App for FilesApp {
    type Body = FilesView;

    fn new() -> Self {
        Self {
            files: FilesView::new(),
        }
    }

    fn window(&self) -> WindowOptions {
        WindowOptions::new("Files")
            .size(920.0, 640.0)
            .resizable(true)
    }

    fn body(&self, _context: &ViewContext) -> Self::Body {
        self.files.clone()
    }
}

#[derive(Clone)]
struct SystemPanelConfiguration {
    workspace_endpoint: u64,
    token: [u8; workspace_protocol::FILE_PANEL_TOKEN_LEN],
    mode: u16,
    title: String,
    initial_directory: String,
    suggested_name: String,
    allowed_content_types: Vec<ContentType>,
}

static SYSTEM_PANEL: OnceLock<SystemPanelConfiguration> = OnceLock::new();
static SYSTEM_PANEL_COMPLETED: AtomicBool = AtomicBool::new(false);

#[derive(Clone)]
struct SystemPanelBody {
    files: FilesView,
    name_field: Option<Rc<TextField>>,
    cancel: Rc<Button>,
    accept: Rc<Button>,
    overwrite: Rc<RefCell<Option<Rc<dyn View>>>>,
}

impl SystemPanelBody {
    fn new(configuration: SystemPanelConfiguration) -> Self {
        let initial_directory = panel_initial_directory(&configuration);
        let overwrite = Rc::new(RefCell::new(None));
        let cancel_configuration = configuration.clone();
        let cancel = Button::new("Cancel")
            .size(ButtonSize::Small)
            .style(ButtonStyle::Standard)
            .on_click(move || {
                let _ = complete_system_panel(&cancel_configuration, 1, Path::new(""));
            });

        if configuration.mode == workspace_protocol::FILE_PANEL_MODE_OPEN {
            let activated_configuration = configuration.clone();
            let allowed_content_types = configuration.allowed_content_types.clone();
            let files = FilesView::new_picker(
                initial_directory,
                move |path| path_matches_content_types(path, &allowed_content_types),
                move |path| complete_system_panel(&activated_configuration, 0, path),
            );
            let accept_files = files.clone();
            let accept = Button::new("Open")
                .size(ButtonSize::Small)
                .style(ButtonStyle::Accent)
                .on_click(move || {
                    if !accept_files.activate_selected() {
                        accept_files.report_error("Select a file to open");
                    }
                });
            Self {
                files,
                name_field: None,
                cancel: Rc::new(cancel),
                accept: Rc::new(accept),
                overwrite,
            }
        } else {
            let name = TextFieldInteractionState::new();
            name.set_value(configuration.suggested_name.clone());
            name.set_focused(true);

            let activated_name = name.clone();
            let files = FilesView::new_picker(
                initial_directory,
                |_| true,
                move |path| {
                    if let Some(file_name) = path.file_name().and_then(|value| value.to_str()) {
                        activated_name.set_value(file_name);
                        activated_name.set_focused(true);
                    }
                    Ok(())
                },
            );
            let submit_configuration = configuration.clone();
            let submit_files = files.clone();
            let submit_name = name.clone();
            let submit_overwrite = overwrite.clone();
            let name_field = TextField::with_interaction(name.clone())
                .size(TextFieldSize::Small)
                .placeholder("File name")
                .on_submit(move || {
                    complete_save_selection(
                        &submit_configuration,
                        &submit_files,
                        &submit_name,
                        &submit_overwrite,
                    );
                });
            let accept_configuration = configuration.clone();
            let accept_files = files.clone();
            let accept_name = name;
            let accept_overwrite = overwrite.clone();
            let accept = Button::new("Save")
                .size(ButtonSize::Small)
                .style(ButtonStyle::Accent)
                .on_click(move || {
                    complete_save_selection(
                        &accept_configuration,
                        &accept_files,
                        &accept_name,
                        &accept_overwrite,
                    );
                });
            Self {
                files,
                name_field: Some(Rc::new(name_field)),
                cancel: Rc::new(cancel),
                accept: Rc::new(accept),
                overwrite,
            }
        }
    }

    fn layout(bounds: Rect) -> (Rect, Rect, Rect, Option<Rect>) {
        let footer_height = 58.0_f32.min(bounds.size.height);
        let footer = Rect::new(
            bounds.origin.x,
            bounds.origin.y + bounds.size.height - footer_height,
            bounds.size.width,
            footer_height,
        );
        let content = Rect::new(
            bounds.origin.x,
            bounds.origin.y,
            bounds.size.width,
            (bounds.size.height - footer_height).max(0.0),
        );
        let button_y = footer.origin.y + (footer.size.height - 30.0) / 2.0;
        let accept = Rect::new(
            footer.origin.x + footer.size.width - 100.0,
            button_y,
            84.0,
            30.0,
        );
        let cancel = Rect::new(accept.origin.x - 96.0, button_y, 84.0, 30.0);
        let name = Rect::new(
            footer.origin.x + 92.0,
            button_y,
            (cancel.origin.x - footer.origin.x - 108.0).max(120.0),
            30.0,
        );
        (content, cancel, accept, Some(name))
    }
}

fn panel_initial_directory(configuration: &SystemPanelConfiguration) -> PathBuf {
    let requested = PathBuf::from(&configuration.initial_directory);
    if requested.is_dir() {
        requested
    } else {
        std::env::var_os("MOCHIOS_PREVIEW_ROOT")
            .or_else(|| std::env::var_os("HOME"))
            .map(PathBuf::from)
            .filter(|path| path.is_dir())
            .unwrap_or_else(|| PathBuf::from("/"))
    }
}

#[derive(Debug, PartialEq, Eq)]
enum SaveDestination {
    New(PathBuf),
    Existing(PathBuf),
}

fn save_destination(
    directory: &Path,
    value: &str,
    allowed: &[ContentType],
) -> Result<SaveDestination, String> {
    if value.is_empty() || value == "." || value == ".." || value.contains(['/', '\0']) {
        return Err("Enter a valid file name".into());
    }
    let destination = directory.join(value);
    if !path_matches_content_types(&destination, allowed) {
        return Err("This file type is not allowed".into());
    }
    match std::fs::symlink_metadata(&destination) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            Err("Choose a regular file rather than a symbolic link".into())
        }
        Ok(metadata) if !metadata.is_file() => {
            Err("A folder or special file already uses this name".into())
        }
        Ok(_) => Ok(SaveDestination::Existing(destination)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Ok(SaveDestination::New(destination))
        }
        Err(error) => Err(format!("Cannot inspect the destination: {error}")),
    }
}

fn complete_save_selection(
    configuration: &SystemPanelConfiguration,
    files: &FilesView,
    name: &TextFieldInteractionState,
    overwrite: &Rc<RefCell<Option<Rc<dyn View>>>>,
) {
    if overwrite.borrow().is_some() {
        return;
    }
    match save_destination(
        &files.current_directory(),
        &name.value(),
        &configuration.allowed_content_types,
    ) {
        Err(error) => {
            files.report_error(error);
            name.set_focused(true);
        }
        Ok(SaveDestination::New(destination)) => {
            if let Err(error) = complete_system_panel(configuration, 0, &destination) {
                files.report_error(error);
            }
        }
        Ok(SaveDestination::Existing(destination)) => {
            // Reuse ViewKit's omochi dialog, typography, spacing and buttons.
            // Weak references avoid a prompt -> callback -> prompt ownership cycle.
            let cancel = Rc::downgrade(overwrite);
            let dismiss = cancel.clone();
            let replace = cancel.clone();
            let configuration = configuration.clone();
            let files = files.clone();
            let title = format!(
                "Replace “{}”?",
                destination
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
            );
            let cancel_button =
                Button::new("Cancel")
                    .style(ButtonStyle::Standard)
                    .on_click(move || {
                        if let Some(prompt) = cancel.upgrade() {
                            prompt.borrow_mut().take();
                        }
                    });
            let replace_button =
                Button::new("Replace")
                    .style(ButtonStyle::Primary)
                    .on_click(move || {
                        if let Some(prompt) = replace.upgrade() {
                            prompt.borrow_mut().take();
                        }
                        // Recheck type after confirmation before passing the path to the writer.
                        let file_name = destination
                            .file_name()
                            .and_then(|n| n.to_str())
                            .unwrap_or_default();
                        match save_destination(
                            destination.parent().unwrap(),
                            file_name,
                            &configuration.allowed_content_types,
                        ) {
                            Ok(_) => {
                                if let Err(error) =
                                    complete_system_panel(&configuration, 0, &destination)
                                {
                                    files.report_error(error);
                                }
                            }
                            Err(error) => files.report_error(error),
                        }
                    });
            let content = VStack::new()
                .gap(StackGap::Medium)
                .alignment(StackAlignment::Stretch)
                .child(Text::styled(title.clone(), TextRole::TitleSmall))
                .child(Text::body("A file with this name already exists. Replacing it will overwrite its contents."))
                .child(Spacer::new())
                .child(HStack::new()
                    .gap(StackGap::Small)
                    .distribution(StackDistribution::End)
                    .child(cancel_button)
                    .child(replace_button));
            let dialog = Dialog::new()
                .accessibility_label(title)
                .on_dismiss(move || {
                    if let Some(prompt) = dismiss.upgrade() {
                        prompt.borrow_mut().take();
                    }
                })
                .content(content);
            *overwrite.borrow_mut() = Some(Rc::new(dialog));
        }
    }
}

fn overwrite_bounds(bounds: Rect, theme: &Theme) -> Rect {
    let width = theme
        .layout
        .dialog_width
        .min((bounds.size.width - 32.0).max(0.0));
    let height = theme
        .layout
        .dialog_height
        .max(220.0)
        .min((bounds.size.height - 32.0).max(0.0));
    Rect::new(
        bounds.origin.x + (bounds.size.width - width) / 2.0,
        bounds.origin.y + (bounds.size.height - height) / 2.0,
        width,
        height,
    )
}

fn path_matches_content_types(path: &Path, allowed: &[ContentType]) -> bool {
    allowed.is_empty()
        || allowed
            .iter()
            .any(|content_type| ContentType::for_path(path).conforms_to(content_type))
}

impl View for SystemPanelBody {
    fn measure(&self, constraints: Constraints, _context: &mut MeasureContext<'_>) -> Size {
        constraints.constrain(constraints.maximum)
    }

    fn paint(&self, bounds: Rect, context: &mut PaintContext<'_>) {
        let (content, cancel, accept, name) = Self::layout(bounds);
        self.files.paint(content, context);
        let footer = Rect::new(
            bounds.origin.x,
            content.origin.y + content.size.height,
            bounds.size.width,
            bounds.size.height - content.size.height,
        );
        Surface::pane().paint(footer, context);
        if let (Some(field), Some(name)) = (self.name_field.as_ref(), name) {
            Text::label("Save As:").paint(
                Rect::new(name.origin.x - 76.0, name.origin.y + 5.0, 68.0, 20.0),
                context,
            );
            field.paint(name, context);
        }
        self.cancel.paint(cancel, context);
        self.accept.paint(accept, context);
        if let Some(prompt) = self.overwrite.borrow().as_ref() {
            prompt.paint(overwrite_bounds(bounds, context.theme), context);
        }
    }

    fn handle_event(
        &self,
        bounds: Rect,
        event: &ViewEvent,
        context: &mut EventContext<'_>,
    ) -> EventResult {
        let prompt = self.overwrite.borrow().clone();
        if let Some(prompt) = prompt {
            prompt.handle_event(overwrite_bounds(bounds, context.theme()), event, context);
            context.request_redraw_in(bounds);
            return EventResult::Consumed;
        }
        let (content, cancel, accept, name) = Self::layout(bounds);
        let mut result = self
            .cancel
            .handle_event(cancel, event, context)
            .merge(self.accept.handle_event(accept, event, context));
        if let (Some(field), Some(name)) = (self.name_field.as_ref(), name) {
            result = result.merge(field.handle_event(name, event, context));
        }
        if result.is_consumed() {
            return result;
        }
        result.merge(self.files.handle_event(content, event, context))
    }
}

struct SystemPanelApp {
    body: SystemPanelBody,
}

impl App for SystemPanelApp {
    type Body = SystemPanelBody;

    fn new() -> Self {
        Self {
            body: SystemPanelBody::new(
                SYSTEM_PANEL
                    .get()
                    .expect("system panel configuration")
                    .clone(),
            ),
        }
    }

    fn window(&self) -> WindowOptions {
        let configuration = SYSTEM_PANEL.get().expect("system panel configuration");
        WindowOptions::new(&configuration.title)
            .size(920.0, 640.0)
            .resizable(true)
    }

    fn body(&self, _context: &ViewContext) -> Self::Body {
        self.body.clone()
    }

    fn close_requested(&mut self) -> bool {
        if let Some(configuration) = SYSTEM_PANEL.get() {
            let _ = complete_system_panel(configuration, 1, Path::new(""));
        }
        true
    }
}

fn complete_system_panel(
    configuration: &SystemPanelConfiguration,
    status: i32,
    path: &Path,
) -> Result<(), String> {
    #[cfg(target_os = "mochios")]
    {
        let path = path
            .to_str()
            .ok_or_else(|| String::from("The path is not valid UTF-8."))?;
        let result = workspace_protocol::FilePanelResult {
            status,
            token: configuration.token,
            path,
        };
        let mut payload = vec![0u8; workspace_protocol::FILE_PANEL_RESULT_PREFIX_LEN + path.len()];
        let payload_length = workspace_protocol::encode_file_panel_result(result, &mut payload)
            .map_err(|_| String::from("The selection could not be encoded."))?;
        let mut request = vec![0u8; workspace_protocol::HEADER_LEN + payload_length];
        let request_length = workspace_protocol::encode(
            workspace_protocol::OP_FILE_PANEL_COMPLETE,
            1,
            0,
            &payload[..payload_length],
            &mut request,
        )
        .map_err(|_| String::from("The selection could not be encoded."))?;
        if SYSTEM_PANEL_COMPLETED.swap(true, std::sync::atomic::Ordering::AcqRel) {
            return Ok(());
        }
        let mut reply = vec![0u8; workspace_protocol::MAX_MESSAGE_LEN];
        let message = match mochi_user_platform::ipc::call(
            configuration.workspace_endpoint,
            &request[..request_length],
            &mut reply,
        ) {
            Ok(message) => message,
            Err(_) => {
                SYSTEM_PANEL_COMPLETED.store(false, std::sync::atomic::Ordering::Release);
                return Err(String::from("The workspace did not accept the selection."));
            }
        };
        let reply_length = (message & 0xffff_ffff) as usize;
        let response = reply
            .get(..reply_length)
            .and_then(|bytes| workspace_protocol::decode(bytes).ok());
        if let Some(message) = response {
            if message.opcode == workspace_protocol::OP_FILE_PANEL_OPERATION_ERROR {
                SYSTEM_PANEL_COMPLETED.store(false, std::sync::atomic::Ordering::Release);
                let error = workspace_protocol::decode_file_panel_finish(message.payload)
                    .ok()
                    .filter(|finish| finish.status == 1 && finish.token == configuration.token)
                    .map(|finish| finish.error.to_owned())
                    .filter(|error| !error.is_empty())
                    .unwrap_or_else(|| String::from("The application could not use this file."));
                return Err(error);
            }
        }
        let accepted = response
            .and_then(|message| workspace_protocol::decode_status(message).ok())
            .is_some_and(|(status, _, _)| status == 0);
        if !accepted {
            SYSTEM_PANEL_COMPLETED.store(false, std::sync::atomic::Ordering::Release);
            return Err(String::from("The workspace did not accept the selection."));
        }
        appcore::request_exit();
        Ok(())
    }
    #[cfg(target_os = "linux")]
    {
        use viewkit::platform::linux::preview::{Connection, Packet};
        if SYSTEM_PANEL_COMPLETED.load(std::sync::atomic::Ordering::Acquire) {
            return Ok(());
        }
        let _ = configuration;
        let socket = std::env::var_os("MOCHIOS_PREVIEW_FILE_PANEL_REPLY")
            .ok_or_else(|| "The file panel has no application connection.".to_owned())?;
        let stream =
            std::os::unix::net::UnixStream::connect(socket).map_err(|error| error.to_string())?;
        let mut connection = Connection::new(stream).map_err(|error| error.to_string())?;
        connection
            .send(Packet::FilePanelSelection {
                path: (status == 0).then(|| path.to_path_buf()),
            })
            .map_err(|error| error.to_string())?;
        loop {
            for packet in connection.poll().map_err(|error| error.to_string())? {
                if let Packet::FilePanelOperation { error } = packet {
                    if let Some(error) = error {
                        return Err(error);
                    }
                    SYSTEM_PANEL_COMPLETED.store(true, std::sync::atomic::Ordering::Release);
                    appcore::request_exit();
                    return Ok(());
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(8));
        }
    }
    #[cfg(not(any(target_os = "mochios", target_os = "linux")))]
    {
        let _ = (configuration, status, path);
        Err(String::from("System file panels are available on mochiOS."))
    }
}

fn parse_system_panel_argument(argument: &str) -> Option<SystemPanelConfiguration> {
    let encoded = argument.strip_prefix("--system-file-panel=")?;
    let mut fields = encoded.splitn(3, ':');
    let workspace_endpoint = fields.next()?.parse().ok()?;
    let token = decode_hex(fields.next()?)?;
    let payload = decode_hex(fields.next()?)?;
    let token: [u8; workspace_protocol::FILE_PANEL_TOKEN_LEN] = token.try_into().ok()?;
    let request = workspace_protocol::decode_file_panel_request(&payload).ok()?;
    let allowed_content_types = request
        .allowed_content_types
        .split('\x1f')
        .filter(|value| !value.is_empty())
        .map(ContentType::parse)
        .collect::<Result<Vec<_>, _>>()
        .ok()?;
    Some(SystemPanelConfiguration {
        workspace_endpoint,
        token,
        mode: request.mode,
        title: request.title.to_owned(),
        initial_directory: request.initial_directory.to_owned(),
        suggested_name: request.suggested_name.to_owned(),
        allowed_content_types,
    })
}

fn decode_hex(value: &str) -> Option<Vec<u8>> {
    if value.len() % 2 != 0 {
        return None;
    }
    value
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let high = (pair[0] as char).to_digit(16)?;
            let low = (pair[1] as char).to_digit(16)?;
            Some(((high << 4) | low) as u8)
        })
        .collect()
}

fn main() -> Result<(), ViewKitError> {
    if let Some(configuration) =
        std::env::args().find_map(|argument| parse_system_panel_argument(&argument))
    {
        let _ = SYSTEM_PANEL.set(configuration);
        // Prevent AppCore from recursively asking workspace.service for the
        // panel it is currently implementing.
        unsafe { std::env::set_var("MOCHIOS_SYSTEM_FILE_PANEL", "1") };
        return run::<SystemPanelApp>();
    }
    run::<FilesApp>()
}

#[cfg(test)]
mod save_panel_tests {
    use super::*;
    use std::fs;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use viewkit::platform::PointerButton;
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    struct Directory(PathBuf);
    impl Directory {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "files-save-panel-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Directory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn save_selection_distinguishes_new_existing_and_invalid_targets() {
        let directory = Directory::new();
        fs::write(directory.0.join("existing.txt"), b"original").unwrap();
        fs::create_dir(directory.0.join("folder")).unwrap();
        assert_eq!(
            save_destination(&directory.0, "new.txt", &[]).unwrap(),
            SaveDestination::New(directory.0.join("new.txt"))
        );
        assert_eq!(
            save_destination(&directory.0, "existing.txt", &[]).unwrap(),
            SaveDestination::Existing(directory.0.join("existing.txt"))
        );
        for name in ["", ".", "..", "../outside", "nul\0name", "folder"] {
            assert!(
                save_destination(&directory.0, name, &[]).is_err(),
                "{name:?}"
            );
        }
        let png = ContentType::parse("image/png").unwrap();
        assert!(save_destination(&directory.0, "existing.txt", &[png]).is_err());
        assert_eq!(
            fs::read(directory.0.join("existing.txt")).unwrap(),
            b"original"
        );
        assert!(!directory.0.join("new.txt").exists());
    }

    #[test]
    fn saving_existing_file_requires_confirmation_and_escape_returns_to_picker() {
        let directory = Directory::new();
        fs::write(directory.0.join("existing.txt"), b"original").unwrap();
        let panel = SystemPanelBody::new(SystemPanelConfiguration {
            workspace_endpoint: 0,
            token: [0; workspace_protocol::FILE_PANEL_TOKEN_LEN],
            mode: workspace_protocol::FILE_PANEL_MODE_SAVE,
            title: "Save As".into(),
            initial_directory: directory.0.to_string_lossy().into_owned(),
            suggested_name: "existing.txt".into(),
            allowed_content_types: vec![],
        });
        let bounds = Rect::new(0.0, 0.0, 920.0, 640.0);
        let theme = Theme::current();
        let mut measurer = viewkit::typography::TextMeasurer::new();
        let mut list = viewkit::draw_command::DisplayList::new();
        panel.paint(
            bounds,
            &mut PaintContext::new(&mut list, &theme, &theme.typography, &mut measurer),
        );
        let (_, _, accept, _) = SystemPanelBody::layout(bounds);
        let position = Point::new(accept.origin.x + 20.0, accept.origin.y + 10.0);
        let mut context = EventContext::new(&theme, &theme.typography, &mut measurer);
        panel.handle_event(
            bounds,
            &ViewEvent::PointerPressed {
                position,
                button: PointerButton::Primary,
            },
            &mut context,
        );
        panel.handle_event(
            bounds,
            &ViewEvent::PointerReleased {
                position,
                button: PointerButton::Primary,
            },
            &mut context,
        );
        assert!(panel.overwrite.borrow().is_some());
        assert_eq!(
            fs::read(directory.0.join("existing.txt")).unwrap(),
            b"original"
        );
        panel.handle_event(
            bounds,
            &ViewEvent::KeyPressed {
                key: Key::Escape,
                modifiers: KeyModifiers::default(),
            },
            &mut context,
        );
        assert!(panel.overwrite.borrow().is_none());
        assert_eq!(panel.files.current_directory(), directory.0);
        assert_eq!(
            fs::read(directory.0.join("existing.txt")).unwrap(),
            b"original"
        );
    }

    #[cfg(unix)]
    #[test]
    fn save_rejects_symlinks_even_when_the_target_is_missing() {
        let directory = Directory::new();
        std::os::unix::fs::symlink(
            directory.0.join("missing.txt"),
            directory.0.join("link.txt"),
        )
        .unwrap();
        assert!(save_destination(&directory.0, "link.txt", &[]).is_err());
        assert!(!directory.0.join("missing.txt").exists());
    }
}
