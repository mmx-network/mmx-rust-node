//! Save and open dialogs for files chosen by the user (contact lists).
//!
//! Each dialog belongs to the wallet window: it stays above the window and
//! the window takes no input while it is open, so it cannot get lost behind
//! the window. On Linux the dialog is a GTK file chooser created on the
//! window's own thread (the dialog plugin's GTK dialogs are not attached to
//! the window); elsewhere the dialog plugin attaches it.
//!
//! Requests are answered one at a time: a request made while a dialog is
//! open shows its dialog after that one closes. `None` means the user
//! cancelled.
use mmxwallet::service::Result;
use std::{
    path::PathBuf,
    sync::{Mutex, PoisonError},
};
use tauri::Window;

const FILTER_NAME: &str = "JSON";
const EXTENSION: &str = "json";

/// What the dialog asks for.
enum Choose {
    /// A file to save to, with a suggested name.
    Save(String),
    /// An existing file to read.
    Open,
}

#[derive(Default)]
pub struct Dialogs {
    open: Mutex<()>,
}

impl Dialogs {
    /// Asks where to save a file. Blocks until the dialog closes.
    pub fn save(&self, window: &Window, suggested_name: String) -> Result<Option<PathBuf>> {
        self.show(window, Choose::Save(suggested_name))
    }

    /// Asks for a file to read. Blocks until the dialog closes.
    pub fn open(&self, window: &Window) -> Result<Option<PathBuf>> {
        self.show(window, Choose::Open)
    }

    fn show(&self, window: &Window, choose: Choose) -> Result<Option<PathBuf>> {
        let _open = self.open.lock().unwrap_or_else(PoisonError::into_inner);
        platform::show(window, choose)
    }
}

#[cfg(target_os = "linux")]
mod platform {
    use super::{Choose, EXTENSION, FILTER_NAME};
    use gtk::prelude::*;
    use mmxwallet::service::{Error, Result};
    use std::{cell::RefCell, path::PathBuf, sync::mpsc};
    use tauri::Window;

    fn unavailable() -> Error {
        Error::new("internal", "the file dialog is not available")
    }

    pub(super) fn show(window: &Window, choose: Choose) -> Result<Option<PathBuf>> {
        let (answer, answered) = mpsc::channel();
        let parent = window.clone();
        window
            .run_on_main_thread(move || {
                // Without the window there is no dialog; the dropped sender
                // reports that.
                let Ok(parent) = parent.gtk_window() else {
                    return;
                };
                let (title, action) = match choose {
                    Choose::Save(_) => ("Save File", gtk::FileChooserAction::Save),
                    Choose::Open => ("Open File", gtk::FileChooserAction::Open),
                };
                let dialog =
                    gtk::FileChooserNative::new(Some(title), Some(&parent), action, None, None);
                dialog.set_modal(true);
                let filter = gtk::FileFilter::new();
                filter.set_name(Some(FILTER_NAME));
                filter.add_pattern(&format!("*.{EXTENSION}"));
                filter.add_pattern(&format!("*.{}", EXTENSION.to_uppercase()));
                dialog.add_filter(filter);
                if let Choose::Save(name) = &choose {
                    dialog.set_do_overwrite_confirmation(true);
                    dialog.set_current_name(name);
                }
                // The dialog keeps itself alive until it is answered.
                let this = RefCell::new(Some(dialog.clone()));
                dialog.connect_response(move |dialog, response| {
                    let chosen = if response == gtk::ResponseType::Accept {
                        dialog.filename()
                    } else {
                        None
                    };
                    let _ = answer.send(chosen);
                    dialog.destroy();
                    this.borrow_mut().take();
                });
                dialog.show();
            })
            .map_err(|_| unavailable())?;
        answered.recv().map_err(|_| unavailable())
    }
}

#[cfg(not(target_os = "linux"))]
mod platform {
    use super::{Choose, EXTENSION, FILTER_NAME};
    use mmxwallet::service::{Error, Result};
    use std::path::PathBuf;
    use tauri::Window;
    use tauri_plugin_dialog::DialogExt;

    pub(super) fn show(window: &Window, choose: Choose) -> Result<Option<PathBuf>> {
        let dialog = window
            .dialog()
            .file()
            .add_filter(FILTER_NAME, &[EXTENSION])
            .set_parent(window);
        let chosen = match choose {
            Choose::Save(name) => dialog.set_file_name(name).blocking_save_file(),
            Choose::Open => dialog.blocking_pick_file(),
        };
        let Some(chosen) = chosen else {
            return Ok(None);
        };
        chosen
            .into_path()
            .map(Some)
            .map_err(|_| Error::new("io_error", "the chosen file is not available"))
    }
}
