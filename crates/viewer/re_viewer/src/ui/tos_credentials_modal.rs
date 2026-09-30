use re_i18n::{tr, trf};
use re_ui::UiExt as _;
use re_ui::modal::{ModalHandler, ModalWrapper};

/// What the user did with the credentials prompt, for whoever is waiting on it.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum CredentialsOutcome {
    Saved,
    Cancelled,
}

/// Prompt for TOS credentials, shown the first time an operation needs them and none are
/// configured anywhere — deployments are allowed to ship without credentials entirely.
///
/// The user chooses between a long-term AK/SK pair and STS temporary credentials
/// (see [`super::credential_fields::CredentialFields`]). What they enter goes into the
/// session credential slot ([`re_data_source::tos::session_credentials`]), so they are
/// asked at most once per session.
#[derive(Default)]
pub struct TosCredentialsModal {
    modal: ModalHandler,
    just_opened: bool,

    credentials: super::credential_fields::CredentialFields,

    /// The latest outcome, until the waiting flow consumes it via [`Self::take_outcome`].
    outcome: Option<CredentialsOutcome>,

    /// Whether the modal was open on the previous frame — a transition to closed without an
    /// explicit outcome means it was dismissed (Escape, click outside), which counts as cancel.
    was_open: bool,
}

impl TosCredentialsModal {
    pub fn open(&mut self) {
        self.modal.open();
        self.just_opened = true;
        self.outcome = None;
    }

    pub fn is_open(&self) -> bool {
        self.modal.is_open()
    }

    /// The pending outcome, consumed — `None` while the dialog is still open (or never shown).
    pub fn take_outcome(&mut self) -> Option<CredentialsOutcome> {
        self.outcome.take()
    }

    pub fn ui(&mut self, ui: &egui::Ui) {
        let mut outcome = None;

        self.modal.ui(
            ui.ctx(),
            || ModalWrapper::new(tr("Volcengine credentials", "火山凭证")),
            |ui| {
                ui.strong(tr(
                    "The Volcengine credentials this session works with.",
                    "本次会话使用的火山凭证。",
                ));
                ui.label(tr(
                    "Enter a long-term AK/SK pair, or STS temporary credentials — they are \
                     kept for this session, and can be updated here (menu → Configure Volcengine Credential…) \
                     at any time.",
                    "请输入长期 AK/SK，或 STS 临时凭证 — 本次会话内会一直使用，\
                     随时可以从菜单（配置火山凭证…）更新。",
                ));

                // Where credentials currently come from, so "update" has visible context.
                if let Some(session) = re_data_source::tos::session_credentials::get() {
                    ui.label(trf!(
                        "Current session credentials: {}…",
                        "当前会话凭证：{}…",
                        session.access_key.chars().take(8).collect::<String>()
                    ));
                } else if crate::viewer_config::get().is_some_and(|config| {
                    !config.tos_access_key.is_empty() && !config.tos_secret_key.is_empty()
                }) {
                    ui.label(tr(
                        "Currently using the deployment's configured credentials; anything \
                         entered here takes precedence.",
                        "当前使用部署配置的默认凭证；在这里输入的凭证优先于它。",
                    ));
                } else {
                    ui.label(tr(
                        "No credentials are configured yet.",
                        "当前没有配置任何凭证。",
                    ));
                }
                ui.add_space(4.0);

                self.credentials
                    .ui(ui, "tos_credentials_prompt_fields", self.just_opened, true);

                let can_save = self.credentials.complete();

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let button_width = ui.tokens().modal_button_width;

                    let save_response = ui.add_enabled(
                        can_save,
                        egui::Button::new(tr("Save", "保存"))
                            .min_size(egui::vec2(button_width, 0.0)),
                    );
                    if save_response.clicked()
                        || can_save && ui.input(|i| i.key_pressed(egui::Key::Enter))
                    {
                        self.credentials.store_to_session();
                        outcome = Some(CredentialsOutcome::Saved);
                        ui.close();
                    }

                    let cancel_response = ui.add(
                        egui::Button::new(tr("Cancel", "取消"))
                            .min_size(egui::vec2(button_width, 0.0)),
                    );
                    if cancel_response.clicked() {
                        outcome = Some(CredentialsOutcome::Cancelled);
                        ui.close();
                    }
                });
            },
        );

        // Dismissing the modal any other way (Escape, clicking outside) is a cancel too —
        // the waiting flow must not hang forever on an outcome that will never come.
        let is_open_now = self.modal.is_open();
        if self.was_open && !is_open_now && outcome.is_none() {
            outcome = Some(CredentialsOutcome::Cancelled);
        }
        self.was_open = is_open_now;
        self.just_opened = false;

        if let Some(outcome_value) = outcome {
            self.outcome = Some(outcome_value);
        }
    }
}
