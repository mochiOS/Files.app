//! The OS-owned default-application confirmation window.
use super::*;
#[derive(Clone)]
struct Configuration {
    endpoint: u64,
    token: [u8; 16],
    description: String,
}
static CONFIGURATION: OnceLock<Configuration> = OnceLock::new();
fn complete(allowed: bool) {
    #[cfg(not(target_os = "mochios"))]
    let _ = allowed;
    #[cfg(target_os = "mochios")]
    if let Some(configuration) = CONFIGURATION.get() {
        let mut payload = [0; 24];
        payload[..16].copy_from_slice(&configuration.token);
        payload[16] = u8::from(allowed);
        let mut request = [0; workspace_protocol::HEADER_LEN + 24];
        if let Ok(length) = workspace_protocol::encode(
            workspace_protocol::OP_ASSOCIATION_CONFIRM,
            1,
            0,
            &payload,
            &mut request,
        ) {
            let mut reply = [0; workspace_protocol::HEADER_LEN + 24];
            let _ = mochi_user_platform::ipc::call(
                configuration.endpoint,
                &request[..length],
                &mut reply,
            );
        }
    }
    appcore::request_exit();
}
struct ConfirmationApp;
impl App for ConfirmationApp {
    type Body = Box<dyn View>;
    fn new() -> Self {
        Self
    }
    fn window(&self) -> WindowOptions {
        WindowOptions::new("Change Default Application")
            .size(560.0, 280.0)
            .resizable(false)
    }
    fn body(&self, _: &ViewContext) -> Self::Body {
        Box::new(
            Dialog::new()
                .accessibility_label("Change default application")
                .on_dismiss(|| complete(false))
                .content(
                    VStack::new()
                        .gap(StackGap::Large)
                        .child(Text::styled(
                            "Change Default Application",
                            TextRole::TitleMedium,
                        ))
                        .child(Text::body(
                            CONFIGURATION
                                .get()
                                .expect("system confirmation configuration")
                                .description
                                .clone(),
                        ))
                        .child(
                            HStack::new()
                                .gap(StackGap::Medium)
                                .child(Button::new("Cancel").on_click(|| complete(false)))
                                .child(
                                    Button::new("Change Default")
                                        .style(ButtonStyle::Accent)
                                        .on_click(|| complete(true)),
                                ),
                        ),
                ),
        )
    }
    fn close_requested(&mut self) -> bool {
        complete(false);
        true
    }
}
pub(super) fn run_if_requested() -> Option<Result<(), ViewKitError>> {
    let argument = std::env::args().find_map(|s| {
        s.strip_prefix("--system-association-confirmation=")
            .map(str::to_owned)
    })?;
    let parsed = (|| {
        let (endpoint, payload) = argument.split_once(':')?;
        let endpoint = endpoint.parse::<u64>().ok().filter(|n| *n != 0)?;
        let payload = super::decode_hex(payload)?;
        let (token, description) =
            workspace_protocol::association_confirmation::decode(&payload).ok()?;
        Some(Configuration {
            endpoint,
            token,
            description: description.into(),
        })
    })();
    let Some(configuration) = parsed else {
        std::process::exit(1);
    };
    let _ = CONFIGURATION.set(configuration);
    Some(run::<ConfirmationApp>())
}
