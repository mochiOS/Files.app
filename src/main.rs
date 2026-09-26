mod browser;
mod file_association;
mod files_view;
mod sidebar;

use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::atomic::AtomicBool;

use appkit::prelude::ContentType;
use files_view::FilesView;
use mochios_workspace_protocol as workspace_protocol;
use viewkit::event::{EventContext, EventResult, ViewEvent};
use viewkit::prelude::*;
use viewkit::view::{Constraints, MeasureContext, PaintContext};

struct FilesApp;

impl App for FilesApp {
    type Body = FilesView;

    fn new() -> Self {
        Self
    }

    fn window(&self) -> WindowOptions {
        WindowOptions::new("Files")
            .size(920.0, 640.0)
            .resizable(true)
    }

    fn body(&self, _context: &ViewContext) -> Self::Body {
        FilesView::new()
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

struct SystemPanelBody {
    files: FilesView,
    name_field: Option<TextField>,
    cancel: Button,
    accept: Button,
}

impl SystemPanelBody {
    fn new(configuration: SystemPanelConfiguration) -> Self {
        let initial_directory = panel_initial_directory(&configuration);
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
                move |path| {
                    let _ = complete_system_panel(&activated_configuration, 0, path);
                },
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
                cancel,
                accept,
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
                },
            );
            let submit_configuration = configuration.clone();
            let submit_files = files.clone();
            let submit_name = name.clone();
            let name_field = TextField::with_interaction(name.clone())
                .size(TextFieldSize::Small)
                .placeholder("File name")
                .on_submit(move || {
                    complete_save_selection(
                        &submit_configuration,
                        &submit_files,
                        &submit_name,
                    );
                });
            let accept_configuration = configuration.clone();
            let accept_files = files.clone();
            let accept_name = name;
            let accept = Button::new("Save")
                .size(ButtonSize::Small)
                .style(ButtonStyle::Accent)
                .on_click(move || {
                    complete_save_selection(
                        &accept_configuration,
                        &accept_files,
                        &accept_name,
                    );
                });
            Self {
                files,
                name_field: Some(name_field),
                cancel,
                accept,
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
        std::env::var_os("HOME")
            .map(PathBuf::from)
            .filter(|path| path.is_dir())
            .unwrap_or_else(|| PathBuf::from("/"))
    }
}

fn complete_save_selection(
    configuration: &SystemPanelConfiguration,
    files: &FilesView,
    name: &TextFieldInteractionState,
) {
    let value = name.value();
    if value.is_empty() || value == "." || value == ".." || value.contains(['/', '\0']) {
        files.report_error("Enter a valid file name");
        name.set_focused(true);
        return;
    }
    let destination = files.current_directory().join(value);
    if !path_matches_content_types(&destination, &configuration.allowed_content_types) {
        files.report_error("This file type is not allowed");
        name.set_focused(true);
        return;
    }
    if let Err(error) = complete_system_panel(configuration, 0, &destination) {
        files.report_error(error);
    }
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
        Rectangle::new()
            .color(RectangleColor::Custom(context.theme.colors.surface_subtle))
            .paint(footer, context);
        if let (Some(field), Some(name)) = (self.name_field.as_ref(), name) {
            Text::label("Save As:").paint(
                Rect::new(name.origin.x - 76.0, name.origin.y + 5.0, 68.0, 20.0),
                context,
            );
            field.paint(name, context);
        }
        self.cancel.paint(cancel, context);
        self.accept.paint(accept, context);
    }

    fn handle_event(
        &self,
        bounds: Rect,
        event: &ViewEvent,
        context: &mut EventContext<'_>,
    ) -> EventResult {
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

struct SystemPanelApp;

impl App for SystemPanelApp {
    type Body = SystemPanelBody;

    fn new() -> Self {
        Self
    }

    fn window(&self) -> WindowOptions {
        let configuration = SYSTEM_PANEL.get().expect("system panel configuration");
        WindowOptions::new(&configuration.title)
            .size(920.0, 640.0)
            .resizable(true)
    }

    fn body(&self, _context: &ViewContext) -> Self::Body {
        let configuration = SYSTEM_PANEL
            .get()
            .expect("system panel configuration")
            .clone();
        SystemPanelBody::new(configuration)
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
        let mut reply = [0u8; workspace_protocol::HEADER_LEN + 24];
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
        let accepted = reply
            .get(..reply_length)
            .and_then(|bytes| workspace_protocol::decode(bytes).ok())
            .and_then(|message| workspace_protocol::decode_status(message).ok())
            .is_some_and(|(status, _, _)| status == 0);
        if !accepted {
            SYSTEM_PANEL_COMPLETED.store(false, std::sync::atomic::Ordering::Release);
            return Err(String::from("The workspace did not accept the selection."));
        }
        appkit::request_exit();
        Ok(())
    }
    #[cfg(not(target_os = "mochios"))]
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
        // Prevent AppKit from recursively asking workspace.service for the
        // panel it is currently implementing.
        unsafe { std::env::set_var("MOCHIOS_SYSTEM_FILE_PANEL", "1") };
        return run::<SystemPanelApp>();
    }
    run::<FilesApp>()
}
