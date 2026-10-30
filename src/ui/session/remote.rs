//! A SQLite file on an SSH host: sending the local copy back.
//!
//! The copy is edited like any local file (see `db::remote`); this is the
//! one way its changes reach the server, and the one place the user is told
//! the server's file changed since it was downloaded.

use chrono::{DateTime, Local, Utc};
use gpui_kit::component::WindowExt;
use gpui_kit::component::button::ButtonVariant;
use gpui_kit::component::dialog::DialogButtonProps;
use gpui_kit::{App, Context, Window};

use crate::db::{Upload, runtime};

use super::{Session, UploadFile};

impl Session {
    pub(super) fn on_upload_file(
        &mut self,
        _: &UploadFile,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.upload_file(false, window, cx);
    }

    /// Whether the connection's file is on an SSH host and its local copy
    /// holds writes the host does not have yet.
    pub(crate) fn has_unuploaded_changes(&self) -> bool {
        self.connection.has_unuploaded_changes()
    }

    /// Whether an upload is on its way.
    pub(crate) fn is_uploading(&self) -> bool {
        self.uploading
    }

    /// Send the local copy back to the SSH host. Unless `force` is set, a
    /// host file that changed since it was downloaded is not replaced: the
    /// user is asked first, and answering yes comes back here with `force`.
    pub(crate) fn upload_file(&mut self, force: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.uploading {
            return;
        }
        if !self.connection.is_remote_file() {
            crate::ui::notify_info(
                window,
                cx,
                "This file is on this computer, so there is nothing to upload.",
            );
            return;
        }
        if !force && !self.connection.has_unuploaded_changes() {
            crate::ui::notify_info(window, cx, "No changes to upload.");
            return;
        }

        self.uploading = true;
        cx.notify();
        let open = self.open_transactions(cx);
        let connection = self.connection.clone();
        let task = runtime::spawn(async move { connection.upload(force).await });
        cx.spawn_in(window, async move |this, cx| {
            let uploaded = task.await;
            this.update_in(cx, |this, window, cx| {
                this.uploading = false;
                cx.notify();
                let target = this.display_target();
                match uploaded {
                    Ok(Ok(Upload::Uploaded)) => {
                        let mut message = format!("Uploaded {target}.");
                        if open > 0 {
                            message.push_str(
                                " Changes in an open transaction were not included; commit \
                                 them and upload again.",
                            );
                        }
                        crate::ui::notify_info(window, cx, message);
                    }
                    Ok(Ok(Upload::ServerChanged { modified })) => {
                        this.confirm_overwrite(&target, modified, window, cx);
                    }
                    Ok(Err(error)) => {
                        crate::ui::notify_error(window, cx, format!("Error: {error:#}"));
                    }
                    Err(_) => {
                        crate::ui::notify_error(window, cx, "Error: the upload was cancelled");
                    }
                }
            })
            .ok();
        })
        .detach();
    }

    /// Warn that the host's file changed since it was downloaded, and upload
    /// over it only if the user says so.
    fn confirm_overwrite(
        &mut self,
        target: &str,
        modified: Option<DateTime<Utc>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let description = overwrite_warning(target, modified);
        // The upload ran in the background, so another dialog may be up by
        // now; the warning must not be lost behind it.
        if window.has_active_dialog(cx) {
            crate::ui::notify_error(
                window,
                cx,
                format!("Not uploaded: {description} Upload again to choose."),
            );
            return;
        }
        let session = cx.entity().downgrade();
        window.open_alert_dialog(cx, move |alert, _, _| {
            let session = session.clone();
            alert
                .title("File Changed on the Server")
                .description(description.clone())
                .button_props(
                    DialogButtonProps::default()
                        .ok_text("Upload Anyway")
                        .ok_variant(ButtonVariant::Danger)
                        .cancel_text("Cancel")
                        .show_cancel(true),
                )
                .on_ok(move |_, window, cx: &mut App| {
                    if let Some(session) = session.upgrade() {
                        session.update(cx, |session, cx| session.upload_file(true, window, cx));
                    }
                    true
                })
        });
    }
}

/// The warning [`Session::confirm_overwrite`] shows.
pub(crate) fn overwrite_warning(target: &str, modified: Option<DateTime<Utc>>) -> String {
    let when = match modified {
        Some(modified) => format!(
            " (last modified {})",
            modified.with_timezone(&Local).format("%Y-%m-%d %H:%M:%S")
        ),
        None => String::new(),
    };
    format!(
        "{target} has changed on the server since it was downloaded{when}. Uploading \
         replaces those changes with your copy."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_overwrite_warning_names_the_file_and_what_uploading_does() {
        let warning = overwrite_warning("files:/srv/app.db", None);
        assert_eq!(
            warning,
            "files:/srv/app.db has changed on the server since it was downloaded. Uploading \
             replaces those changes with your copy."
        );
        let dated = overwrite_warning("files:/srv/app.db", DateTime::from_timestamp(0, 0));
        assert!(dated.contains("(last modified "), "{dated}");
    }
}
