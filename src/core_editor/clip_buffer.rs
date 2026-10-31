use crate::Granularity;
use std::collections::VecDeque;

/// `DEFAULT_MAX_KILLS` (GNU `kill.c:58-60`): the number of slots the kill
/// ring keeps. Pushing past it frees the oldest slot and shifts the rest
/// down, exactly like GNU's slot-0 eviction.
const MAX_KILLS: usize = 10;

/// Defines an interface to interact with a Clipboard for cut and paste.
///
/// Mutable reference requirements are stricter than always necessary, but the currently used system clipboard API demands them for exclusive access.
pub trait Clipboard: Send {
    fn set(&mut self, content: &str, mode: Granularity);

    fn get(&mut self) -> (String, Granularity);

    #[allow(dead_code)]
    fn clear(&mut self) {
        self.set("", Granularity::CharWise);
    }

    #[allow(dead_code)]
    fn len(&mut self) -> usize {
        self.get().0.len()
    }
}

/// Kill-ring buffer usable within the application/library — GNU readline's
/// `rl_kill_ring` (kill.c:63-72) with a per-slot [`Granularity`] so the vi
/// `p`/`P` machinery can keep its charwise/linewise paste semantics.
///
/// Slots are oldest-first; the newest kill always lands on the back. `set`
/// is the plain register write (one fresh slot per call); [`push_kill`] is
/// `_rl_copy_to_kill_ring`, which merges consecutive kills into the newest
/// slot instead. [`yank_entry`]/[`rotate_kill_index`] model
/// `rl_kill_ring[rl_kill_index]` for `yank`/`yank-pop`.
///
/// [`push_kill`]: Self::push_kill
/// [`yank_entry`]: Self::yank_entry
/// [`rotate_kill_index`]: Self::rotate_kill_index
#[derive(Default)]
pub struct LocalClipboard {
    /// `rl_kill_ring`: the slots themselves.
    ring: VecDeque<(String, Granularity)>,
    /// `rl_kill_index` (kill.c:67): the slot `yank`/`yank-pop` reads. Every
    /// kill or plain `set` leaves it on the newest slot; each `yank-pop`
    /// walks it down with wraparound.
    index: usize,
}

impl LocalClipboard {
    #[allow(dead_code)]
    pub fn new() -> Self {
        Self::default()
    }

    /// `_rl_copy_to_kill_ring` (kill.c:90-155): add `text` to the ring.
    ///
    /// With `merge` (`_rl_last_command_was_kill`) and a non-empty ring, the
    /// newest slot is extended instead of taking a fresh slot: a forward
    /// kill (`append`, GNU's `from < to`) concatenates after the old text, a
    /// backward kill prepends. Under `vi` mode GNU suppresses the merge
    /// (kill.c:131) and the newest slot is *replaced* outright.
    pub fn push_kill(
        &mut self,
        text: &str,
        mode: Granularity,
        append: bool,
        merge: bool,
        vi: bool,
    ) {
        if merge && !self.ring.is_empty() {
            let slot = self.ring.len() - 1;
            let (old, _) = self.ring[slot].clone();
            let merged = if vi || append {
                // vi: replace; emacs forward: old ++ new
                if vi {
                    text.to_owned()
                } else {
                    old + text
                }
            } else {
                // emacs backward: new ++ old
                let mut merged = text.to_owned();
                merged.push_str(&old);
                merged
            };
            self.ring[slot] = (merged, mode);
            self.index = slot;
        } else {
            self.push_slot(text, mode);
        }
    }

    /// Push a fresh slot, evicting the oldest once `rl_max_kills` is reached
    /// (kill.c:105-118), and point `rl_kill_index` at it.
    fn push_slot(&mut self, text: &str, mode: Granularity) {
        if self.ring.len() == MAX_KILLS {
            self.ring.pop_front();
        }
        self.ring.push_back((text.to_owned(), mode));
        self.index = self.ring.len() - 1;
    }

    /// `rl_yank`'s read of `rl_kill_ring[rl_kill_index]` (kill.c:520): the
    /// newest slot after a kill, walked backward by each `yank-pop`.
    pub fn yank_entry(&self) -> Option<(String, Granularity)> {
        self.ring.get(self.index).cloned()
    }

    /// `rl_yank_pop`'s index step: `--rl_kill_index`, wrapping to
    /// `rl_kill_ring_length - 1` (kill.c:546-548).
    pub fn rotate_kill_index(&mut self) {
        if !self.ring.is_empty() {
            self.index = self.index.checked_sub(1).unwrap_or(self.ring.len() - 1);
        }
    }

    /// `rl_kill_ring_length`: live slots, for tests and diagnostics.
    #[allow(dead_code)]
    pub(crate) fn ring_len(&self) -> usize {
        self.ring.len()
    }

    /// Oldest-first slot access, for tests.
    #[allow(dead_code)]
    pub(crate) fn ring_slot(&self, slot: usize) -> Option<&(String, Granularity)> {
        self.ring.get(slot)
    }
}

impl Clipboard for LocalClipboard {
    fn set(&mut self, content: &str, mode: Granularity) {
        self.push_slot(content, mode);
    }

    fn get(&mut self) -> (String, Granularity) {
        self.ring
            .get(self.index)
            .cloned()
            .unwrap_or_else(|| (String::new(), Granularity::CharWise))
    }
}

/// Creates a local clipboard
#[allow(dead_code)]
pub fn get_local_clipboard() -> Box<dyn Clipboard> {
    Box::new(LocalClipboard::new())
}

#[cfg(feature = "system_clipboard")]
pub use system_clipboard::SystemClipboard;

/// Creates a handle for the OS clipboard
#[cfg(feature = "system_clipboard")]
pub fn get_system_clipboard() -> Box<dyn Clipboard> {
    SystemClipboard::new().map_or_else(
        |_e| Box::new(LocalClipboard::new()) as Box<dyn Clipboard>,
        |cb| Box::new(cb),
    )
}

#[cfg(feature = "system_clipboard")]
mod system_clipboard {
    use super::*;
    use arboard::Clipboard as Arboard;

    /// Wrapper around [`arboard`](https://docs.rs/arboard) crate
    ///
    /// Requires that the feature `system_clipboard` is enabled
    pub struct SystemClipboard {
        cb: Arboard,
        local_copy: String,
        mode: Granularity,
    }

    impl SystemClipboard {
        pub fn new() -> Result<Self, arboard::Error> {
            Ok(SystemClipboard {
                cb: Arboard::new()?,
                local_copy: String::new(),
                mode: Granularity::CharWise,
            })
        }
    }

    impl Clipboard for SystemClipboard {
        fn set(&mut self, content: &str, mode: Granularity) {
            self.local_copy = content.to_owned();
            let _ = self.cb.set_text(content);
            self.mode = mode;
        }

        fn get(&mut self) -> (String, Granularity) {
            let system_content = self.cb.get_text().unwrap_or_default();
            if system_content == self.local_copy {
                // We assume the content was yanked inside the line editor and the last yank determined the mode.
                (system_content, self.mode)
            } else {
                // Content has changed, default to direct insertion.
                (system_content, Granularity::CharWise)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::get_local_clipboard;
    #[cfg(feature = "system_clipboard")]
    use super::get_system_clipboard;
    use super::{Clipboard, LocalClipboard};
    use crate::Granularity;
    #[test]
    fn reads_back_local() {
        let mut cb = get_local_clipboard();
        // If the system clipboard is used we want to persist it for the user
        let previous_state = cb.get().0;

        // Actual test
        cb.set("test", Granularity::CharWise);
        assert_eq!(cb.len(), 4);
        assert_eq!(cb.get().0, "test".to_owned());
        cb.clear();
        assert_eq!(cb.get().0, String::new());

        // Restore!

        cb.set(&previous_state, Granularity::CharWise);
    }

    #[test]
    fn ring_pushes_newest_last_and_caps_at_ten() {
        // kill.c:105-118: once `rl_max_kills` slots are used, slot 0 is freed
        // and the rest shift down — so the newest ten kills are retained.
        let mut ring = LocalClipboard::new();
        for i in 0..12 {
            ring.push_kill(&i.to_string(), Granularity::CharWise, true, false, false);
        }
        assert_eq!(ring.ring_len(), 10);
        assert_eq!(ring.get().0, "11");
        assert_eq!(ring.ring_slot(0).unwrap().0, "2");
    }

    #[test]
    fn ring_merge_appends_and_prepends_into_newest_slot() {
        // kill.c:131-150: `_rl_last_command_was_kill` merges into the newest
        // slot — forward kills append, backward kills prepend.
        let mut ring = LocalClipboard::new();
        ring.push_kill("world", Granularity::CharWise, true, false, false);
        ring.push_kill("!", Granularity::CharWise, true, true, false);
        assert_eq!(ring.get().0, "world!");
        ring.push_kill("hello ", Granularity::CharWise, false, true, false);
        assert_eq!(ring.get().0, "hello world!");
        assert_eq!(ring.ring_len(), 1);
    }

    #[test]
    fn ring_vi_merge_replaces_instead_of_merging() {
        // kill.c:131: `rl_editing_mode == vi_mode` suppresses the merge — a
        // consecutive vi kill overwrites the newest slot outright.
        let mut ring = LocalClipboard::new();
        ring.push_kill("first", Granularity::CharWise, true, false, false);
        ring.push_kill("second", Granularity::CharWise, true, true, true);
        assert_eq!(ring.get().0, "second");
        assert_eq!(ring.ring_len(), 1);
    }

    #[test]
    fn yank_entry_walks_back_with_wrap() {
        // kill.c:546-548: `rl_yank_pop` decrements `rl_kill_index`, wrapping
        // to `rl_kill_ring_length - 1` past the oldest slot.
        let mut ring = LocalClipboard::new();
        for s in ["one", "two", "three"] {
            ring.push_kill(s, Granularity::CharWise, true, false, false);
        }
        assert_eq!(ring.yank_entry().unwrap().0, "three");
        ring.rotate_kill_index();
        assert_eq!(ring.yank_entry().unwrap().0, "two");
        ring.rotate_kill_index();
        assert_eq!(ring.yank_entry().unwrap().0, "one");
        ring.rotate_kill_index();
        assert_eq!(ring.yank_entry().unwrap().0, "three");
    }

    #[cfg(feature = "system_clipboard")]
    #[test]
    fn reads_back_system() {
        let mut cb = get_system_clipboard();
        // If the system clipboard is used we want to persist it for the user
        let previous_state = cb.get().0;

        // Actual test
        cb.set("test", Granularity::CharWise);
        assert_eq!(cb.len(), 4);
        assert_eq!(cb.get().0, "test".to_owned());
        cb.clear();
        assert_eq!(cb.get().0, String::new());

        // Restore!

        cb.set(&previous_state, Granularity::CharWise);
    }
}
