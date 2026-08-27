//! Session-level "hidden" flags for recordings (dataset episodes), driving the recording
//! panel's collapsed "Hidden episodes" group.
//!
//! Purely visual — a hidden episode keeps its data and any running download; closing (the ×
//! button) is what frees memory. The flags are not persisted: episodes get fresh recording
//! ids on every dataset open, so there is nothing stable to persist against.

use std::sync::LazyLock;

use parking_lot::Mutex;
use re_log_types::StoreId;

static HIDDEN: LazyLock<Mutex<ahash::HashSet<StoreId>>> = LazyLock::new(Default::default);

/// Hide a recording: the panel moves it into the "Hidden episodes" group.
pub fn hide(store_id: StoreId) {
    HIDDEN.lock().insert(store_id);
}

/// Un-hide a recording: the panel moves it back into the regular list.
pub fn unhide(store_id: &StoreId) {
    HIDDEN.lock().remove(store_id);
}

pub fn is_hidden(store_id: &StoreId) -> bool {
    HIDDEN.lock().contains(store_id)
}
