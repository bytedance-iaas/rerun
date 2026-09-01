use eframe::epaint::Margin;
use egui::{Button, Frame, RichText, TextStyle, Theme, Ui};
use re_i18n::{tr, trf};
use re_ui::egui_ext::card_layout::{CardLayout, CardLayoutItem};
use re_ui::{ReButtonExt as _, UICommand, UICommandSender as _, UiExt as _, design_tokens_of};
use re_uri::Origin;
use re_viewer_context::{
    AppContext, EditRedapServerModalCommand, Item, SystemCommand, SystemCommandSender as _,
};

pub enum LoginState {
    NoAuth,
    Auth { email: Option<String> },
}

pub struct CloudState {
    pub has_server: Option<Origin>,
    pub login: LoginState,
}

#[cfg(feature = "analytics")] // these are currently only used when analytics is enabled
impl CloudState {
    fn is_logged_in(&self) -> bool {
        matches!(self.login, LoginState::Auth { .. })
    }

    fn has_server(&self) -> bool {
        self.has_server.is_some()
    }
}

pub enum IntroItem {
    DocItem {
        title: &'static str,
        url: &'static str,
        body: &'static str,
    },
    /// A link into this deployment (curation console, SDK downloads) — same-domain
    /// paths, so web only: `daft_link` returns `None` natively and the card is not built.
    DeploymentItem {
        title: &'static str,
        link_label: &'static str,
        url: String,
        body: &'static str,
    },
    /// Our user-guide links — the guide is embedded and rendered in-app
    /// (`crate::ui::user_guide`), so this card shows on every viewer alike.
    GuideItem {
        title: &'static str,
    },
    CloudLoginItem,
}

/// The user guides for this fork's features: label and in-app guide page index.
fn guide_pages() -> [(&'static str, usize); 2] {
    [
        (
            tr(
                "Viewer — open, visualize & explore datasets",
                "Viewer 篇 — 浏览和探索数据集",
            ),
            0,
        ),
        (
            tr(
                "Catalog server — query & train on TOS datasets",
                "Catalog server 篇 — 查询 TOS 数据集并用于训练",
            ),
            1,
        ),
    ]
}

impl IntroItem {
    fn items(login_enabled: bool) -> Vec<Self> {
        let mut items = Vec::new();
        if let Some(url) = re_viewer_context::daft_link::base_url() {
            items.push(Self::DeploymentItem {
                title: tr("Curate data", "数据质检"),
                link_label: tr("Open", "打开"),
                url,
                body: tr(
                    "Run quality checks on your Volcengine TOS datasets in the curation console.",
                    "在质检台里对火山引擎 TOS 数据集做质量检查。",
                ),
            });
        }
        if let Some(url) = re_viewer_context::daft_link::downloads_url() {
            items.push(Self::DeploymentItem {
                title: tr("Get the SDK", "下载 SDK"),
                link_label: tr("Download", "下载"),
                url,
                body: tr("Volcengine-enhanced Python SDK — wheels for every platform with the viewer built in. Install with pip.", "火山引擎增强版 Python SDK — 各平台 wheel 均内置 Viewer，用 pip 安装即可。"),
            });
        }
        items.push(Self::GuideItem {
            title: tr("User guide", "用户指南"),
        });
        items.extend([
            Self::DocItem {
                title: tr("Send data in", "写入数据"),
                url: "https://rerun.io/docs/getting-started/data-in",
                body: tr("Ingest multi-rate, multimodal data from robot logs, sensors, simulation, or video.", "从机器人日志、传感器、仿真或视频接入多频率、多模态数据。"),
            },
            Self::DocItem {
                title: tr("Explore data", "探索数据"),
                url: "https://rerun.io/docs/getting-started/configure-the-viewer",
                body: tr("Visualize and explore multi-rate, multimodal data across every stage of the pipeline.", "可视化并探索管线各环节的多频率、多模态数据。"),
            },
            Self::DocItem {
                title: tr("Query data out", "查询数据"),
                url: "https://rerun.io/docs/getting-started/data-out",
                body: tr("Query raw, intermediate, and derived data with dataframes or SQL, and stream to training.", "用 dataframe 或 SQL 查询原始、中间和派生数据，并流式接入训练。"),
            },
        ]);
        if login_enabled {
            items.push(Self::CloudLoginItem);
        }
        items
    }

    fn frame(&self, ui: &Ui) -> Frame {
        let opposite_theme = match ui.theme() {
            Theme::Dark => Theme::Light,
            Theme::Light => Theme::Dark,
        };
        let opposite_tokens = design_tokens_of(opposite_theme);
        let tokens = ui.tokens();
        let frame = Frame::new()
            .inner_margin(Margin::same(16))
            .corner_radius(8)
            .stroke(tokens.native_frame_stroke);
        match self {
            Self::DocItem { .. } | Self::DeploymentItem { .. } | Self::GuideItem { .. } => frame,
            Self::CloudLoginItem => frame.fill(opposite_tokens.panel_bg_color),
        }
    }

    fn card_item(&self, ui: &Ui) -> CardLayoutItem {
        let min_width = match &self {
            Self::DocItem { .. } | Self::DeploymentItem { .. } | Self::GuideItem { .. } => 200.0,
            Self::CloudLoginItem => 400.0,
        };
        CardLayoutItem {
            frame: Some(self.frame(ui)),
            min_width,
        }
    }

    fn show(&self, ui: &mut Ui, ctx: &AppContext<'_>, cloud_state: &CloudState) {
        let label_size = 13.0;
        ui.vertical(|ui| match self {
            Self::DocItem { title, url, body } => {
                egui::Sides::new().shrink_left().show(ui, |ui| {
                    ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Wrap);

                    ui.heading(RichText::new(*title).strong());
                }, |ui| {
                    let _response = ui.re_hyperlink(tr("Docs", "文档"), *url, true);
                    #[cfg(feature = "analytics")]
                    if _response.clicked() || _response.clicked_with_open_in_background() {
                        re_analytics::record(|| re_analytics::event::WelcomeScreenNavigation {
                            card_type: "docs".to_owned(),
                            destination: (*url).to_owned(),
                            cta_cloud: false,
                            is_logged_in: cloud_state.is_logged_in(),
                            has_server: cloud_state.has_server(),
                        });
                    }
                });
                ui.label(RichText::new(*body).size(label_size));
            }
            Self::DeploymentItem {
                title,
                link_label,
                url,
                body,
            } => {
                egui::Sides::new().shrink_left().show(
                    ui,
                    |ui| {
                        ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Wrap);
                        ui.heading(RichText::new(*title).strong());
                    },
                    |ui| {
                        let _response = ui.re_hyperlink(*link_label, url.as_str(), true);
                        #[cfg(feature = "analytics")]
                        if _response.clicked() || _response.clicked_with_open_in_background() {
                            re_analytics::record(|| {
                                re_analytics::event::WelcomeScreenNavigation {
                                    card_type: "deployment".to_owned(),
                                    destination: url.clone(),
                                    cta_cloud: false,
                                    is_logged_in: cloud_state.is_logged_in(),
                                    has_server: cloud_state.has_server(),
                                }
                            });
                        }
                    },
                );
                ui.label(RichText::new(*body).size(label_size));
            }
            Self::GuideItem { title } => {
                ui.heading(RichText::new(*title).strong());
                ui.add_space(2.0);
                for (label, page) in guide_pages() {
                    ui.style_mut()
                        .text_styles
                        .get_mut(&TextStyle::Body)
                        .expect("Should always have body text style")
                        .size = label_size;
                    if ui.link(label).clicked() {
                        crate::ui::user_guide::request_open(ui.ctx(), page);
                        #[cfg(feature = "analytics")]
                        re_analytics::record(|| re_analytics::event::WelcomeScreenNavigation {
                            card_type: "guide".to_owned(),
                            destination: format!("user-guide:{page}"),
                            cta_cloud: false,
                            is_logged_in: cloud_state.is_logged_in(),
                            has_server: cloud_state.has_server(),
                        });
                    }
                }
            }
            Self::CloudLoginItem => {
                let opposite_theme = match ui.theme() {
                    Theme::Dark => Theme::Light,
                    Theme::Light => Theme::Dark,
                };
                ui.set_style(ui.style_of(opposite_theme));

                ui.heading(RichText::new("Rerun Hub").strong());

                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing.x = 0.0;

                    let link = |ui: &mut Ui, label: &str, url: &str| {
                        let _response = ui.hyperlink_to(label, url);
                        #[cfg(feature = "analytics")]
                        if _response.clicked() || _response.clicked_with_open_in_background() {
                            re_analytics::record(|| re_analytics::event::WelcomeScreenNavigation {
                                card_type: "redap".to_owned(),
                                destination: url.to_owned(),
                                cta_cloud: false,
                                is_logged_in: cloud_state.is_logged_in(),
                                has_server: cloud_state.has_server(),
                            });
                        }
                    };

                    ui.style_mut().text_styles.get_mut(&TextStyle::Body).expect("Should always have body text style").size = label_size;
                    ui.label(
                        tr("The production backend for the Rerun data layer — turn your object stores into a queryable, streamable foundation. ", "Rerun 数据层的生产级后端 — 把你的对象存储变成可查询、可流式读取的数据底座。")
                    );
                    link(ui, tr("Learn more", "了解更多"), "https://rerun.io/#rerun-data-platform");
                    ui.label(tr(" or ", " 或 "));
                    link(ui, tr("book a demo", "预约演示"), "https://calendly.com/d/ctht-4kp-qnt/rerun-demo-meeting");
                    ui.label("。");
                });

                let analytics = || {
                    #[cfg(feature = "analytics")]
                    re_analytics::record(|| re_analytics::event::WelcomeScreenNavigation {
                        card_type: "redap".to_owned(),
                        destination: String::new(),
                        cta_cloud: true,
                        is_logged_in: cloud_state.is_logged_in(),
                        has_server: cloud_state.has_server(),
                    });
                };

                match cloud_state {
                    CloudState { has_server: None, login: LoginState::NoAuth } => {
                        if ui.primary_button(tr("Add server and login", "添加服务器并登录")).clicked() {
                            analytics();
                            ctx.command_sender.send_ui(UICommand::AddRedapServer);
                        }
                    }
                    CloudState { has_server: None, login } => {
                        ui.horizontal_wrapped(|ui| {
                            if ui.primary_button(tr("Add server", "添加服务器")).clicked() {
                                analytics();
                                ctx.command_sender.send_ui(UICommand::AddRedapServer);
                            }
                            if let LoginState::Auth { email: Some(email) } = login {
                                ui.spacing_mut().item_spacing.x = 0.0;
                                ui.weak(tr("logged in as ", "当前登录账号 "));
                                ui.strong(email);
                            }
                        });
                    }
                    CloudState { has_server: Some(origin), login: LoginState::NoAuth } => {
                        ui.horizontal_wrapped(|ui| {
                            if ui.primary_button(tr("Add credentials", "添加凭证")).clicked() {
                                analytics();
                                ctx.command_sender.send_system(SystemCommand::EditRedapServerModal(EditRedapServerModalCommand::new(origin.clone())));
                            }
                            ui.spacing_mut().item_spacing.x = 0.0;
                            ui.weak(tr("for address ", "服务器地址 "));
                            ui.strong(format!("{}", origin.host));
                        });
                    }
                    CloudState { has_server: Some(origin), login: LoginState::Auth { .. } } => {
                        if ui.primary_button(tr("Explore your data", "探索你的数据")).clicked() {
                            analytics();
                            ctx.command_sender.send_system(SystemCommand::set_selection(Item::RedapServer(origin.clone())));
                        }
                    }
                }
            }
        });
    }
}

pub fn intro_section(ui: &mut egui::Ui, ctx: &AppContext<'_>, cloud_state: &CloudState) {
    let items = IntroItem::items(ctx.login_enabled);

    ui.add_space(32.0);

    if let Some(auth) = ctx.auth_context {
        ui.strong(RichText::new(trf!("Hi, {}!", "你好，{}！", auth.email)).size(15.0));

        if ui
            .add(Button::new(tr("Log out", "退出登录")).secondary().small())
            .clicked()
        {
            ctx.command_sender.send_system(SystemCommand::Logout);
        }

        ui.add_space(32.0);
    }

    // Two labeled rows: this deployment's own cards (curation, SDK, user guide) first,
    // the upstream doc cards and the cloud banner below.
    let (ours, upstream): (Vec<_>, Vec<_>) = items.into_iter().partition(|item| {
        matches!(
            item,
            IntroItem::DeploymentItem { .. } | IntroItem::GuideItem { .. }
        )
    });
    for (header, row) in [
        (tr("Volcengine enhancements", "火山引擎增强功能"), ours),
        (tr("About the original Rerun", "关于原版 Rerun"), upstream),
    ] {
        if row.is_empty() {
            continue;
        }
        super::section_heading_ui(ui, header);
        // Cards stretch to fill the row, which looks silly when there are only one or
        // two (natively only the User guide card shows) — cap the row width per card.
        let max_row_width = row.len() as f32 * 450.0;
        ui.scope(|ui| {
            ui.set_max_width(ui.available_width().min(max_row_width));
            CardLayout::new(
                row.iter().map(|item| item.card_item(ui)).collect(),
                Frame::NONE,
            )
            .show(ui, |ui, index, _card_hovered| {
                row[index].show(ui, ctx, cloud_state);
            });
        });
        ui.add_space(24.0);
    }
}
