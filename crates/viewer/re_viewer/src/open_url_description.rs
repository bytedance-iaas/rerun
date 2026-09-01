use re_viewer_context::open_url::ViewerOpenUrl;

/// A description of what happens when opening a [`ViewerOpenUrl`].
pub struct ViewerOpenUrlDescription {
    /// The general category of this URL.
    pub category: &'static str,

    /// The specific target of this URL if known.
    ///
    /// This is always shorter than the original URL.
    pub target_short: Option<String>,
}

impl std::fmt::Display for ViewerOpenUrlDescription {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if let Some(target) = &self.target_short {
            write!(f, "{}：{target}", self.category)
        } else {
            write!(f, "{}", self.category)
        }
    }
}

impl ViewerOpenUrlDescription {
    pub fn from_url(open_url: &ViewerOpenUrl) -> Self {
        match open_url {
            ViewerOpenUrl::IntraRecordingSelection(item) => Self {
                category: "选区",
                target_short: item.entity_path().map(|p| p.to_string()),
            },

            ViewerOpenUrl::HttpUrl(url) => {
                let path = url.path();
                let rrd_file_name = path.split('/').next_back().map(|s| s.to_owned());

                Self {
                    category: "HTTP 链接",
                    target_short: rrd_file_name,
                }
            }

            #[cfg(not(target_arch = "wasm32"))]
            ViewerOpenUrl::FilePath(path) => Self {
                category: "文件",
                target_short: path.file_name().map(|s| s.display().to_string()),
            },

            ViewerOpenUrl::RedapDatasetSegment(uri) => Self {
                category: "数据段",
                target_short: Some(uri.segment_id.to_string()),
            },

            ViewerOpenUrl::RedapProxy(_) => Self {
                category: "gRPC 代理",
                target_short: None,
            },

            ViewerOpenUrl::TosDataset { location, .. } => Self {
                category: "TOS 数据集",
                target_short: location
                    .to_string()
                    .trim_end_matches('/')
                    .rsplit('/')
                    .next()
                    .filter(|segment| !segment.is_empty())
                    .map(|segment| segment.to_owned()),
            },

            ViewerOpenUrl::RedapCatalog(uri) => Self {
                category: "目录",
                target_short: Some(uri.origin.host.to_string()),
            },

            ViewerOpenUrl::RedapEntry(uri) => Self {
                category: "Redap 条目",
                target_short: Some(uri.entry_id.to_string()),
            },

            ViewerOpenUrl::RedapFolder(uri) => Self {
                category: "文件夹",
                target_short: Some(uri.path.clone()),
            },

            ViewerOpenUrl::WebEventListener => Self {
                category: "Web 事件监听器",
                target_short: None,
            },

            ViewerOpenUrl::WebViewerUrl { url_parameters, .. } => {
                if url_parameters.len() == 1 {
                    Self::from_url(url_parameters.first())
                } else {
                    Self {
                        category: "多个 URL",
                        target_short: Some(format!("{} 个 URL", url_parameters.len())),
                    }
                }
            }

            ViewerOpenUrl::Settings => Self {
                category: "设置",
                target_short: None,
            },

            ViewerOpenUrl::ChunkStoreBrowser { .. } => Self {
                category: "Chunk store 浏览器",
                target_short: None,
            },
        }
    }
}
