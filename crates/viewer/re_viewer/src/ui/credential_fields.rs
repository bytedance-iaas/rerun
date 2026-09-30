//! The TOS credential input block shared by the "Open from TOS" dialog and the standalone
//! credentials prompt: a choice between a long-term AK/SK pair and STS temporary
//! credentials (AK/SK plus session token), with per-session remembering.

use re_i18n::tr;
use re_ui::UiExt as _;

/// State + widgets for entering TOS credentials of either kind.
#[derive(Default)]
pub struct CredentialFields {
    /// `true`: the fields hold STS temporary credentials (AK/SK/session-token triple).
    use_sts: bool,

    access_key: String,
    secret_key: String,
    session_token: String,

    /// Native only, long-term pairs only: persist to `~/.rerun/config.json` on save.
    /// STS credentials expire on their own schedule — remembering those would only
    /// store a pair that stops working, so the option is not offered.
    #[cfg_attr(target_arch = "wasm32", expect(dead_code))]
    remember: bool,
}

impl CredentialFields {
    /// Everything required for the chosen credential kind is filled in.
    pub fn complete(&self) -> bool {
        !self.access_key.trim().is_empty()
            && !self.secret_key.trim().is_empty()
            && (!self.use_sts || !self.session_token.trim().is_empty())
    }

    pub fn access_key(&self) -> &str {
        self.access_key.trim()
    }

    pub fn secret_key(&self) -> &str {
        self.secret_key.trim()
    }

    /// The session token to sign with: empty for a long-term pair.
    pub fn session_token(&self) -> &str {
        if self.use_sts {
            self.session_token.trim()
        } else {
            ""
        }
    }

    /// One line asking for whatever is still missing, for the dialog's status row.
    pub fn missing_hint(&self) -> &'static str {
        if self.use_sts {
            tr(
                "Enter the access key, secret key and session token.",
                "请输入 access key、secret key 和 session token。",
            )
        } else {
            tr(
                "Enter the access key and secret key.",
                "请输入 access key 和 secret key。",
            )
        }
    }

    /// Put the entered credentials into the session slot (and, when asked, the local config),
    /// then drop the secret values from the widget state — they now live in the slot, and a
    /// dialog should not keep a second copy around.
    ///
    /// Read the values (e.g. via [`Self::access_key`]) before calling this.
    pub fn store_to_session(&mut self) {
        re_data_source::tos::session_credentials::store(
            self.access_key(),
            self.secret_key(),
            self.session_token(),
        );

        #[cfg(not(target_arch = "wasm32"))]
        if !self.use_sts
            && self.remember
            && let Err(err) =
                super::native_config::save_tos_credentials(self.access_key(), self.secret_key())
        {
            re_log::warn!("{err}");
        }

        self.secret_key.clear();
        self.session_token.clear();
    }

    /// The input widgets. `id_salt` keeps the two dialogs' grids distinct;
    /// `focus_first` focuses the AK field (pass `true` on the frame the dialog opened).
    ///
    /// `persistent`: whether these fields feed the session credential slot. The open
    /// dialog passes `false` — credentials entered there apply to that open only — which
    /// hides the "kept for this session" hint and the native remember checkbox.
    pub fn ui(&mut self, ui: &mut egui::Ui, id_salt: &str, focus_first: bool, persistent: bool) {
        ui.horizontal(|ui| {
            ui.label(tr("Credential type:", "凭证类型："));
            ui.radio_value(
                &mut self.use_sts,
                false,
                tr("Long-term AK/SK", "长期 AK/SK"),
            );
            ui.radio_value(
                &mut self.use_sts,
                true,
                tr("STS temporary credentials", "STS 临时凭证"),
            );
        });

        // STS issues its own temporary AK/SK alongside the token — a point worth spelling
        // out, or the fields read like they want the account's long-term pair.
        if self.use_sts {
            ui.label(tr(
                "Fill in the three fields of one STS issue: its temporary access key, \
                 temporary secret key, and session token — not your long-term AK/SK.",
                "填 STS 签发结果里的三个字段：临时 access key、临时 secret key 和 \
                 session token — 不是你账号的长期 AK/SK。",
            ));
        }

        egui::Grid::new(id_salt)
            .num_columns(2)
            .spacing([8.0, 6.0])
            .show(ui, |ui| {
                ui.label("Access key：");
                let ak_edit = egui::TextEdit::singleline(&mut self.access_key)
                    .hint_text("AK…")
                    .desired_width(f32::INFINITY)
                    .show(ui);
                if focus_first {
                    ak_edit.response.request_focus();
                }
                ui.end_row();

                ui.label("Secret key：");
                egui::TextEdit::singleline(&mut self.secret_key)
                    .password(true)
                    .desired_width(f32::INFINITY)
                    .show(ui);
                ui.end_row();

                if self.use_sts {
                    ui.label("Session token：");
                    egui::TextEdit::singleline(&mut self.session_token)
                        .password(true)
                        .desired_width(f32::INFINITY)
                        .show(ui);
                    ui.end_row();
                }
            });

        if persistent {
            ui.label(tr(
                "Entered credentials are kept for this session.",
                "输入的凭证会在本次会话内记住。",
            ));

            #[cfg(not(target_arch = "wasm32"))]
            if !self.use_sts {
                ui.re_checkbox(
                    &mut self.remember,
                    tr(
                        "Remember on this machine (~/.rerun/config.json)",
                        "在本机记住（~/.rerun/config.json）",
                    ),
                );
            }
        }
    }
}
