use re_i18n::tr;
//! Remembers which remote datasets (TOS / Hugging Face) were opened, across viewer restarts.
//!
//! Converted data lives only in RAM, so restarting the viewer starts blank. This list — persisted
//! with the rest of the app state (`localStorage` on the web, the state file natively) — lets the
//! welcome screen show what was opened before and re-open it with one click. Only non-secret
//! metadata is stored: never access keys or tokens.

/// Which remote backend a recent dataset lives on.
#[derive(Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum RecentKind {
    Tos,
    Hf,
}

/// One remembered remote dataset. No credentials, ever.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct RecentDataset {
    /// `tos://bucket/prefix/` or `hf://org/name`; doubles as the identity for deduplication.
    pub url: String,

    pub kind: RecentKind,

    /// TOS only: the bucket's region, so re-opening lands on the right endpoint.
    /// (`default` so entries persisted before this field existed still load.)
    #[serde(default)]
    pub region: String,

    /// Episodes/files in the dataset, once a stream reported it.
    pub item_count: Option<usize>,

    /// Unix seconds of the last time this dataset was opened.
    pub last_opened_unix: i64,

    /// Was this dataset still open when the viewer last saved its state?
    ///
    /// Stamped on every state save; drives session restore on the next launch
    /// (a dataset the user closed before quitting must not come back).
    #[serde(default)]
    pub open_at_exit: bool,
}

impl RecentDataset {
    /// Short display name: the last meaningful path segment of the URL.
    pub fn display_name(&self) -> &str {
        self.url
            .trim_end_matches('/')
            .rsplit('/')
            .next()
            .filter(|segment| !segment.is_empty())
            .unwrap_or(&self.url)
    }
}

/// Cap: enough to be useful, small enough to stay glanceable.
const MAX_RECENT: usize = 10;

/// Insert or refresh an entry (dedup by URL), most recent first.
pub fn remember(list: &mut Vec<RecentDataset>, mut entry: RecentDataset) {
    if let Some(existing_idx) = list.iter().position(|recent| recent.url == entry.url) {
        // Keep previously learned metadata unless the new entry knows better.
        let existing = list.remove(existing_idx);
        entry.item_count = entry.item_count.or(existing.item_count);
    }
    list.insert(0, entry);
    list.truncate(MAX_RECENT);
}

/// "just now" / "5 min ago" / "3 h ago" / "2 days ago" — for the welcome screen row.
pub fn relative_time_label(last_opened_unix: i64, now_unix: i64) -> String {
    let secs = now_unix.saturating_sub(last_opened_unix);
    if secs < 60 {
        tr("just now", "刚刚").to_owned()
    } else if secs < 60 * 60 {
        format!("{} 分钟前", secs / 60)
    } else if secs < 24 * 60 * 60 {
        format!("{} 小时前", secs / (60 * 60))
    } else if secs < 2 * 24 * 60 * 60 {
        tr("yesterday", "昨天").to_owned()
    } else {
        format!("{} 天前", secs / (24 * 60 * 60))
    }
}

/// Current unix time in seconds (works on native and wasm).
pub fn now_unix() -> i64 {
    re_log_types::Timestamp::now().nanos_since_epoch() / 1_000_000_000
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(url: &str, when: i64) -> RecentDataset {
        RecentDataset {
            url: url.to_owned(),
            kind: RecentKind::Tos,
            region: String::new(),
            item_count: None,
            last_opened_unix: when,
            open_at_exit: false,
        }
    }

    #[test]
    fn remember_dedups_and_keeps_metadata() {
        let mut list = Vec::new();
        remember(&mut list, entry("tos://b/a/", 1));
        let mut with_count = entry("tos://b/a/", 2);
        with_count.item_count = Some(47);
        remember(&mut list, with_count);
        // Re-open without a count: the learned count survives.
        remember(&mut list, entry("tos://b/a/", 3));

        assert_eq!(list.len(), 1);
        assert_eq!(list[0].last_opened_unix, 3);
        assert_eq!(list[0].item_count, Some(47));
    }

    #[test]
    fn remember_caps_and_orders_most_recent_first() {
        let mut list = Vec::new();
        for i in 0..20 {
            remember(&mut list, entry(&format!("tos://b/{i}/"), i));
        }
        assert_eq!(list.len(), MAX_RECENT);
        assert_eq!(list[0].url, "tos://b/19/");
    }

    #[test]
    fn display_name_is_last_segment() {
        assert_eq!(entry("tos://bucket/data/set-1/", 0).display_name(), "set-1");
        assert_eq!(entry("hf://org/name", 0).display_name(), "name");
    }

    #[test]
    fn relative_time_labels() {
        assert_eq!(relative_time_label(100, 130), "刚刚");
        assert_eq!(relative_time_label(0, 5 * 60), "5 分钟前");
        assert_eq!(relative_time_label(0, 3 * 60 * 60), "3 小时前");
        assert_eq!(relative_time_label(0, 30 * 60 * 60), "昨天");
        assert_eq!(relative_time_label(0, 3 * 24 * 60 * 60), "3 天前");
        // A clock that went backwards must not underflow.
        assert_eq!(relative_time_label(100, 50), "刚刚");
    }
}
