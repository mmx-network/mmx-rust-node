//! Copying to the system clipboard. Sensitive text (recovery words) is
//! removed again after a minute, or when the app quits, provided the
//! clipboard still holds it. Only a SHA-256 digest of sensitive text is kept
//! for that check, never the text itself.
use mmxwallet::service::{Error, Result};
use sha2::{Digest, Sha256};
use std::{
    sync::{Mutex, MutexGuard},
    time::Duration,
};
use zeroize::Zeroizing;

/// How long sensitive text stays on the clipboard.
pub const CLEAR_AFTER: Duration = Duration::from_secs(60);
/// Longest text the app copies.
pub const MAX_TEXT_BYTES: usize = 16 * 1024;

type Digest32 = [u8; 32];

fn digest(text: &str) -> Digest32 {
    Sha256::digest(text.as_bytes()).into()
}

/// The system clipboard.
pub trait Clipboard {
    /// Writes text; sensitive text is marked so clipboard history managers
    /// skip it.
    fn write(&self, text: &str, sensitive: bool) -> Result<()>;
    /// The current text, if any can be read.
    fn read(&self) -> Option<Zeroizing<String>>;
    fn clear(&self);
}

#[derive(Default)]
struct State {
    /// Incremented by every copy made by the app.
    generation: u64,
    /// Digest of the last copy if it was sensitive and is not cleared yet.
    sensitive: Option<Digest32>,
}

/// Tracks the app's copies so a sensitive one can be cleared later.
#[derive(Default)]
pub struct Guard {
    state: Mutex<State>,
}

/// Identifies a sensitive copy for [`Guard::expire`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ticket(u64);

impl Guard {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Copies `text`. A sensitive copy returns the ticket to pass to
    /// [`Guard::expire`] after [`CLEAR_AFTER`].
    pub fn copy(
        &self,
        clipboard: &impl Clipboard,
        text: &str,
        sensitive: bool,
    ) -> Result<Option<Ticket>> {
        if text.len() > MAX_TEXT_BYTES {
            return Err(Error::new("invalid_argument", "text too long"));
        }
        let mut state = self.lock();
        // A failed write leaves whatever the clipboard held before; an
        // earlier sensitive copy may still be on it, so keep tracking it.
        clipboard
            .write(text, sensitive)
            .map_err(|_| Error::new("internal", "clipboard unavailable"))?;
        state.generation += 1;
        state.sensitive = sensitive.then(|| digest(text));
        Ok(sensitive.then_some(Ticket(state.generation)))
    }

    /// Clears the clipboard if the copy of `ticket` is the app's latest copy
    /// and the clipboard still holds that text.
    pub fn expire(&self, clipboard: &impl Clipboard, ticket: Ticket) {
        let expected = {
            let state = self.lock();
            match state.sensitive {
                Some(d) if state.generation == ticket.0 => d,
                _ => return,
            }
        };
        // Read without holding the lock: reading may wait for another app.
        let holds = clipboard
            .read()
            .is_some_and(|current| digest(&current) == expected);
        let mut state = self.lock();
        if state.generation != ticket.0 || state.sensitive != Some(expected) {
            return;
        }
        state.sensitive = None;
        if holds {
            clipboard.clear();
        }
    }

    /// Clears a sensitive copy that has not expired yet when the app quits.
    /// With `check_contents` the clipboard is cleared only if it still holds
    /// the text; without it (where reading could wait on the closing app
    /// itself) it is cleared unconditionally.
    pub fn clear_on_exit(&self, clipboard: &impl Clipboard, check_contents: bool) {
        let mut state = self.lock();
        let Some(expected) = state.sensitive.take() else {
            return;
        };
        if !check_contents
            || clipboard
                .read()
                .is_some_and(|current| digest(&current) == expected)
        {
            clipboard.clear();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    #[derive(Default)]
    struct Fake {
        text: RefCell<String>,
        fail_writes: bool,
        clears: RefCell<u32>,
        secret: RefCell<bool>,
    }

    impl Clipboard for Fake {
        fn write(&self, text: &str, sensitive: bool) -> Result<()> {
            if self.fail_writes {
                return Err(Error::new("internal", "x"));
            }
            *self.text.borrow_mut() = text.into();
            *self.secret.borrow_mut() = sensitive;
            Ok(())
        }
        fn read(&self) -> Option<Zeroizing<String>> {
            Some(Zeroizing::new(self.text.borrow().clone()))
        }
        fn clear(&self) {
            self.text.borrow_mut().clear();
            *self.clears.borrow_mut() += 1;
        }
    }

    #[test]
    fn plain_copies_are_never_cleared() {
        let (guard, clip) = (Guard::default(), Fake::default());
        assert_eq!(guard.copy(&clip, "mmx1abc", false).unwrap(), None);
        assert!(!*clip.secret.borrow());
        guard.clear_on_exit(&clip, false);
        assert_eq!(*clip.text.borrow(), "mmx1abc");
        assert_eq!(*clip.clears.borrow(), 0);
    }

    #[test]
    fn sensitive_copy_is_cleared_after_the_delay() {
        let (guard, clip) = (Guard::default(), Fake::default());
        let ticket = guard.copy(&clip, "words", true).unwrap().unwrap();
        assert!(*clip.secret.borrow());
        guard.expire(&clip, ticket);
        assert_eq!(*clip.text.borrow(), "");
        assert_eq!(*clip.clears.borrow(), 1);
        // Expiring again or quitting does nothing more.
        guard.expire(&clip, ticket);
        guard.clear_on_exit(&clip, false);
        assert_eq!(*clip.clears.borrow(), 1);
    }

    #[test]
    fn text_copied_elsewhere_is_kept() {
        let (guard, clip) = (Guard::default(), Fake::default());
        let ticket = guard.copy(&clip, "words", true).unwrap().unwrap();
        *clip.text.borrow_mut() = "copied by the user".into();
        guard.expire(&clip, ticket);
        assert_eq!(*clip.text.borrow(), "copied by the user");
        assert_eq!(*clip.clears.borrow(), 0);
    }

    #[test]
    fn a_later_copy_replaces_the_pending_one() {
        let (guard, clip) = (Guard::default(), Fake::default());
        let first = guard.copy(&clip, "words", true).unwrap().unwrap();
        guard.copy(&clip, "mmx1abc", false).unwrap();
        guard.expire(&clip, first);
        assert_eq!(*clip.text.borrow(), "mmx1abc");

        // Copying the same words again restarts the delay.
        let first = guard.copy(&clip, "words", true).unwrap().unwrap();
        let second = guard.copy(&clip, "words", true).unwrap().unwrap();
        assert_ne!(first, second);
        guard.expire(&clip, first);
        assert_eq!(*clip.text.borrow(), "words");
        guard.expire(&clip, second);
        assert_eq!(*clip.text.borrow(), "");
    }

    #[test]
    fn quitting_clears_a_pending_copy() {
        let (guard, clip) = (Guard::default(), Fake::default());
        guard.copy(&clip, "words", true).unwrap();
        guard.clear_on_exit(&clip, true);
        assert_eq!(*clip.text.borrow(), "");

        guard.copy(&clip, "words", true).unwrap();
        *clip.text.borrow_mut() = "other".into();
        guard.clear_on_exit(&clip, true);
        assert_eq!(*clip.text.borrow(), "other");

        guard.copy(&clip, "words", true).unwrap();
        *clip.text.borrow_mut() = "other".into();
        guard.clear_on_exit(&clip, false);
        assert_eq!(*clip.text.borrow(), "");
    }

    #[test]
    fn failed_or_oversized_copies() {
        let guard = Guard::default();
        let ok = Fake::default();
        let ticket = guard.copy(&ok, "words", true).unwrap().unwrap();
        let broken = Fake {
            fail_writes: true,
            ..Fake::default()
        };
        assert_eq!(
            guard.copy(&broken, "x", false).unwrap_err().code,
            "internal"
        );
        // The earlier sensitive copy is still tracked.
        guard.expire(&ok, ticket);
        assert_eq!(*ok.text.borrow(), "");

        let long = "a".repeat(MAX_TEXT_BYTES + 1);
        assert_eq!(
            guard.copy(&ok, &long, false).unwrap_err().code,
            "invalid_argument"
        );
    }
}
