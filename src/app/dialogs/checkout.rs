//! Checkout: the repository, a module, an optional tag, and whether to check it out
//! read-only.
//!
//! A checkout that fails keeps its fields: the same window comes back with the error, so a
//! missing password or a network drop does not make the user type the module again.

use std::path::{Path, PathBuf};

use cvsdesk::cvs::{Cmd, CommandRequest};

use super::connection;
use super::repository;
use super::{Dialog, DialogAction};
use crate::app::{App, LoginPurpose, OutputKind};

/// The fields the Checkout window keeps between frames.
#[derive(Clone, Debug)]
pub(crate) struct CheckoutDialog {
    pub tag: String,
    pub readonly: bool,
    pub module: String,
    /// The folder cvs writes the working copy into. Empty means the module's name
    /// under the folder the window shows.
    pub folder: String,
    pub root: String,
    /// A pserver password for this checkout. It is not kept after the command starts.
    pub password: String,
    /// A problem to show inside the window.
    pub warning: String,
    /// A target the user has already accepted (a folder that is not empty, or a nested one).
    pub confirmed: Option<PathBuf>,
    /// The target of a failed checkout, whose partially written folder may be reused.
    pub retry: Option<PathBuf>,
}

impl CheckoutDialog {
    pub(crate) fn new(tag: &str, readonly: bool, root: String) -> Self {
        Self {
            tag: tag.to_string(),
            readonly,
            module: String::new(),
            folder: String::new(),
            root,
            password: String::new(),
            warning: String::new(),
            confirmed: None,
            retry: None,
        }
    }
}

/// The fields of one successful checkout, offered again when the same root is used.
#[derive(Clone, Debug)]
pub(crate) struct CheckoutMemory {
    root: String,
    module: String,
    folder: String,
    tag: String,
    readonly: bool,
}

impl CheckoutMemory {
    pub(crate) fn from_dialog(dialog: &CheckoutDialog) -> Self {
        Self {
            root: dialog.root.clone(),
            module: dialog.module.clone(),
            folder: dialog.folder.clone(),
            tag: dialog.tag.clone(),
            readonly: dialog.readonly,
        }
    }

    /// Fill a new window when it is for the same repository. The password is never kept.
    pub(crate) fn restore(&self, dialog: &mut CheckoutDialog) {
        if dialog.root == self.root {
            dialog.module = self.module.clone();
            dialog.folder = self.folder.clone();
            dialog.tag = self.tag.clone();
            dialog.readonly = self.readonly;
        }
    }
}

/// A checkout that passed the window's checks and is about to run or be retried.
#[derive(Clone, Debug)]
pub(crate) struct PendingCheckout {
    dialog: CheckoutDialog,
    target: CheckoutTarget,
}

impl PendingCheckout {
    /// The fields to offer again after this checkout succeeds.
    pub(crate) fn memory(&self) -> CheckoutMemory {
        CheckoutMemory::from_dialog(&self.dialog)
    }
}

#[cfg(test)]
pub(crate) fn pending_for_test(dialog: CheckoutDialog, base: &Path) -> PendingCheckout {
    let target = checkout_target(base, &dialog.module, &dialog.folder).expect("a test target");
    PendingCheckout { dialog, target }
}

/// Where a checkout writes: the folder to run in, the folder cvs creates, and the name
/// handed to `checkout -d`.
#[derive(Clone, Debug, PartialEq, Eq)]
struct CheckoutTarget {
    run_dir: PathBuf,
    dir: PathBuf,
    name: String,
}

/// Why the module and folder fields do not name a folder cvs can write.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CheckoutTargetError {
    NoModule,
    CurrentFolder,
    NoFolderName,
}

impl CheckoutTargetError {
    fn message(self) -> &'static str {
        match self {
            Self::NoModule => "Enter a module to check out.",
            Self::CurrentFolder => "Choose a folder other than the one the window shows.",
            Self::NoFolderName => "Choose a folder with a name under a drive or folder.",
        }
    }
}

/// What the target folder's current contents allow.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DestinationCheck {
    Ready,
    /// The folder has files. The user must click Checkout once to confirm.
    Confirm,
    /// The folder is already a CVS working copy of its own.
    RefuseWorkingCopy,
    /// A file occupies the target path.
    RefuseFile,
}

pub(super) fn show(
    ctx: &egui::Context,
    state: &mut CheckoutDialog,
    recent: &[String],
    base: &Path,
    has_cvs: bool,
    busy: bool,
) -> Option<DialogAction> {
    let before = (
        state.root.clone(),
        state.tag.clone(),
        state.module.clone(),
        state.folder.clone(),
        state.password.clone(),
        state.readonly,
    );
    let mut open = true;
    let mut action = None;
    egui::Window::new("Checkout")
        .open(&mut open)
        .drag_area(egui::WindowDrag::Anywhere)
        .collapsible(false)
        .resizable(false)
        .default_width(600.0)
        .show(ctx, |ui| {
            repository::show(ui, &mut state.root, recent);
            if connection::is_pserver(&state.root) {
                ui.horizontal(|ui| {
                    ui.label("Password:");
                    ui.add(
                        egui::TextEdit::singleline(&mut state.password)
                            .password(true)
                            .hint_text("Leave empty to use .cvspass")
                            .desired_width(280.0),
                    );
                });
                ui.label(
                    egui::RichText::new(
                        "Used once to log in; cvs saves it in .cvspass, not in this window.",
                    )
                    .small()
                    .weak(),
                );
                if connection::split_root(&state.root).1.is_some() {
                    ui.label(
                        egui::RichText::new(
                            "The repository string contains a password; it is moved out of the \
                             command line when the checkout starts.",
                        )
                        .small()
                        .weak(),
                    );
                }
            }
            ui.separator();
            ui.label("Checkout a module into a folder.");
            ui.horizontal(|ui| {
                ui.label("Module:");
                ui.add(egui::TextEdit::singleline(&mut state.module).desired_width(220.0));
                let can_list = !busy && (!state.root.trim().is_empty() || has_cvs);
                let list = ui
                    .add_enabled(can_list, egui::Button::new("List modules"))
                    .on_hover_text(
                        "Ask the server for its modules file; the reply appears in the Console",
                    );
                let list = if busy {
                    list.on_disabled_hover_text("cvs is already running")
                } else if state.root.trim().is_empty() && !has_cvs {
                    list.on_disabled_hover_text("Enter a repository first")
                } else {
                    list
                };
                if list.clicked() {
                    action = Some(DialogAction::ListModules);
                }
            });
            ui.horizontal(|ui| {
                ui.label("Tag / branch:");
                ui.text_edit_singleline(&mut state.tag);
            });
            ui.horizontal(|ui| {
                ui.label("Folder:");
                ui.add(
                    egui::TextEdit::singleline(&mut state.folder)
                        .hint_text("Empty: the module name under this folder")
                        .desired_width(300.0),
                );
                let can_pick = module_leaf(&state.module).is_some();
                let pick = ui.add_enabled(can_pick, egui::Button::new("Choose parent…"));
                let pick = if can_pick {
                    pick
                } else {
                    pick.on_disabled_hover_text("Type the module first")
                };
                if pick
                    .on_hover_text("Pick the parent folder; the module name is added to it")
                    .clicked()
                {
                    if let Some(dir) = rfd::FileDialog::new()
                        .set_title("Parent folder for the checkout")
                        .pick_folder()
                    {
                        let name = module_leaf(&state.module).unwrap_or_default();
                        state.folder = dir.join(name).display().to_string();
                    }
                }
            });
            ui.label(
                egui::RichText::new(
                    "The folder may be absolute or relative to this one, and is created \
                     if it is not there yet.",
                )
                .small()
                .weak(),
            );
            match checkout_target(base, &state.module, &state.folder) {
                Ok(target) => {
                    let mut line = format!("Will write: {}", target.dir.display());
                    match destination_check(
                        &target.dir,
                        state.retry.as_deref(),
                        state.confirmed.as_deref(),
                    ) {
                        DestinationCheck::RefuseWorkingCopy => {
                            line.push_str(" — already a CVS working copy");
                        }
                        DestinationCheck::RefuseFile => line.push_str(" — a file is in the way"),
                        DestinationCheck::Confirm => line.push_str(" — not empty"),
                        DestinationCheck::Ready => {}
                    }
                    if has_cvs && target.dir.starts_with(base) && target.dir != *base {
                        line.push_str(" — inside the working copy on screen");
                    }
                    ui.label(egui::RichText::new(line).small().weak());
                }
                Err(error) => {
                    ui.label(egui::RichText::new(error.message()).small().weak());
                }
            }
            if !state.warning.is_empty() {
                ui.colored_label(
                    egui::Color32::from_rgb(180, 60, 40),
                    egui::RichText::new(&state.warning).small(),
                );
            }
            if busy {
                ui.colored_label(
                    egui::Color32::from_rgb(180, 80, 0),
                    egui::RichText::new("cvs is already running; wait for it to finish.").small(),
                );
            }
            ui.checkbox(&mut state.readonly, "Check out read-only");
            ui.separator();
            ui.horizontal(|ui| {
                let disabled = if busy {
                    Some("cvs is already running")
                } else if module_leaf(&state.module).is_none() {
                    Some("Enter a module")
                } else if state.root.trim().is_empty() && !has_cvs {
                    Some("Enter a repository, or open a CVS working copy")
                } else {
                    checkout_target(base, &state.module, &state.folder)
                        .err()
                        .map(CheckoutTargetError::message)
                };
                let button = ui.add_enabled(disabled.is_none(), egui::Button::new("Checkout"));
                let button = match disabled {
                    Some(reason) => button.on_disabled_hover_text(reason),
                    None => button,
                };
                if button.clicked() {
                    action = Some(DialogAction::Checkout);
                }
                if ui.button("Cancel").clicked() {
                    action = Some(DialogAction::Close);
                }
            });
        });
    if !open {
        action = Some(DialogAction::Close);
    }
    let after = (
        state.root.clone(),
        state.tag.clone(),
        state.module.clone(),
        state.folder.clone(),
        state.password.clone(),
        state.readonly,
    );
    if before != after {
        // A warning belongs to the fields that produced it.
        state.warning.clear();
    }
    action
}

/// The command the window's fields describe. Kept apart from `apply` so the root the
/// dialog names can be tested without starting cvs.
fn request(
    tag: &str,
    readonly: bool,
    module: &str,
    destination: &str,
    root: &str,
) -> CommandRequest {
    let (root, _password) = connection::split_root(root);
    let mut req = CommandRequest::new(Cmd::Checkout);
    req.tag = if tag.trim().is_empty() {
        String::new()
    } else {
        tag.trim().to_string()
    };
    req.readonly = readonly;
    req.destination = destination.trim().to_string();
    if !module.trim().is_empty() {
        req.files = vec![module.trim().to_string()];
    }
    req.root = Some(root);
    req
}

/// The read-only command behind the window's List modules button.
fn modules_request(root: &str) -> CommandRequest {
    let (root, _password) = connection::split_root(root);
    let mut req = CommandRequest::new(Cmd::Checkout);
    req.extra = vec!["-c".to_string()];
    req.root = Some(root);
    req
}

/// Resolve the dialog's module and folder fields. An empty folder means the module's own
/// name under the folder the window shows; a folder may be absolute or relative to it.
/// `checkout -d` takes a name beside the process, so the command runs in the target's
/// parent and names only its last part.
fn checkout_target(
    base: &Path,
    module: &str,
    folder: &str,
) -> Result<CheckoutTarget, CheckoutTargetError> {
    let module = module_leaf(module).ok_or(CheckoutTargetError::NoModule)?;
    let folder = folder.trim();
    let target = if folder.is_empty() {
        base.join(module)
    } else {
        let path = PathBuf::from(folder);
        if path.is_absolute() {
            path
        } else {
            base.join(path)
        }
    };
    let target = crate::app::nav::normalize(&target);
    if target == base {
        return Err(CheckoutTargetError::CurrentFolder);
    }
    let name = target
        .file_name()
        .ok_or(CheckoutTargetError::NoFolderName)?
        .to_string_lossy()
        .into_owned();
    let run_dir = target
        .parent()
        .ok_or(CheckoutTargetError::NoFolderName)?
        .to_path_buf();
    Ok(CheckoutTarget {
        run_dir,
        dir: target,
        name,
    })
}

/// The last part of a module path: `tools/proj` is checked out as `proj`.
fn module_leaf(module: &str) -> Option<&str> {
    module
        .trim()
        .rsplit(['/', '\\'])
        .find(|part| !part.trim().is_empty())
        .map(str::trim)
}

/// Whether a target may be written into, or needs one confirmation first.
fn destination_check(
    target: &Path,
    retry: Option<&Path>,
    confirmed: Option<&Path>,
) -> DestinationCheck {
    if target.is_file() {
        return DestinationCheck::RefuseFile;
    }
    // A folder that already holds CVS belongs to another working copy. The one exception
    // is the folder this same checkout just created before it failed: retrying it is fine.
    if target.join("CVS").is_dir() && retry != Some(target) {
        return DestinationCheck::RefuseWorkingCopy;
    }
    if target.is_dir() && !is_empty_dir(target) && confirmed != Some(target) {
        return DestinationCheck::Confirm;
    }
    DestinationCheck::Ready
}

fn is_empty_dir(path: &Path) -> bool {
    std::fs::read_dir(path)
        .map(|mut entries| entries.next().is_none())
        .unwrap_or(false)
}

/// Validate the fields, then start a login (when a password was given) or the checkout.
/// A window is returned only when it must stay open, with a warning for the user.
pub(crate) fn apply(app: &mut App, d: Dialog) -> Option<Dialog> {
    let Dialog::Checkout(state) = d else {
        return None;
    };
    let mut state = *state;
    if app.runner.running {
        state.warning = "cvs is already running; wait for it to finish.".into();
        return Some(Dialog::Checkout(Box::new(state)));
    }
    if state.root.trim().is_empty() && !app.has_cvs {
        state.warning = "Enter a repository, or open a CVS working copy.".into();
        return Some(Dialog::Checkout(Box::new(state)));
    }
    let target = match checkout_target(&app.dir, &state.module, &state.folder) {
        Ok(target) => target,
        Err(error) => {
            state.warning = error.message().into();
            return Some(Dialog::Checkout(Box::new(state)));
        }
    };
    match destination_check(
        &target.dir,
        state.retry.as_deref(),
        state.confirmed.as_deref(),
    ) {
        DestinationCheck::RefuseFile => {
            state.warning = format!(
                "{} is a file, not a folder. Choose another target.",
                target.dir.display()
            );
            return Some(Dialog::Checkout(Box::new(state)));
        }
        DestinationCheck::RefuseWorkingCopy => {
            state.warning = format!(
                "{} is already a CVS working copy. Choose another folder, or open it instead.",
                target.dir.display()
            );
            return Some(Dialog::Checkout(Box::new(state)));
        }
        DestinationCheck::Confirm => {
            state.confirmed = Some(target.dir.clone());
            state.warning = format!(
                "{} is not empty. Click Checkout again to write into it.",
                target.dir.display()
            );
            return Some(Dialog::Checkout(Box::new(state)));
        }
        DestinationCheck::Ready => {}
    }
    let nested = app.has_cvs
        && target.dir.starts_with(&app.dir)
        && target.dir != app.dir
        && state.confirmed.as_deref() != Some(target.dir.as_path());
    if nested {
        state.confirmed = Some(target.dir.clone());
        state.warning = format!(
            "{} is inside the working copy on screen. Click Checkout again to nest a new working copy here.",
            target.dir.display()
        );
        return Some(Dialog::Checkout(Box::new(state)));
    }
    if let Err(error) = std::fs::create_dir_all(&target.run_dir) {
        state.warning = format!("Could not create {}: {error}", target.run_dir.display());
        return Some(Dialog::Checkout(Box::new(state)));
    }
    let (root, embedded) = connection::password_for(&state.root, &state.password);
    let password = if connection::is_pserver(&root) {
        embedded
    } else {
        None
    };
    state.root = root.clone();
    state.password.clear();
    app.prefs.remember_root(&root);
    let pending = PendingCheckout {
        dialog: state,
        target,
    };
    match password {
        Some(password) => match begin_login(app, pending, password) {
            None => None,
            Some(pending) => {
                let mut dialog = pending.dialog;
                dialog.warning = "Could not start the login. See the Console.".into();
                Some(Dialog::Checkout(Box::new(dialog)))
            }
        },
        None => match start_pending(app, pending) {
            None => None,
            Some(pending) => {
                let mut dialog = pending.dialog;
                dialog.warning = "Could not start the checkout. See the Console.".into();
                Some(Dialog::Checkout(Box::new(dialog)))
            }
        },
    }
}

/// Start the actual checkout for a pending window.
pub(crate) fn start_pending(
    app: &mut App,
    mut pending: PendingCheckout,
) -> Option<PendingCheckout> {
    let req = request(
        &pending.dialog.tag,
        pending.dialog.readonly,
        &pending.dialog.module,
        &pending.target.name,
        &pending.dialog.root,
    );
    if app.start_in(pending.target.run_dir.clone(), req, OutputKind::Console) {
        app.checkout_waiting = Some(pending.target.dir.clone());
        pending.dialog.warning.clear();
        app.pending_checkout = Some(pending);
        app.status = "Checking out…".into();
        None
    } else {
        Some(pending)
    }
}

/// Log in first when the window carried a password, then run the checkout.
fn begin_login(
    app: &mut App,
    pending: PendingCheckout,
    password: String,
) -> Option<PendingCheckout> {
    let root = pending.dialog.root.clone();
    if start_login(app, root, password, LoginPurpose::Checkout) {
        app.pending_checkout = Some(pending);
        None
    } else {
        Some(pending)
    }
}

fn start_login(app: &mut App, root: String, password: String, purpose: LoginPurpose) -> bool {
    let mut req = CommandRequest::new(Cmd::Login);
    req.root = Some(root);
    req.password = Some(password);
    if app.start(req, OutputKind::Console) {
        app.login_purpose = Some(purpose);
        app.status = "Logging in…".into();
        true
    } else {
        false
    }
}

/// Open the Checkout window again after a failed login or checkout, with the same fields.
pub(crate) fn reopen(app: &mut App, pending: PendingCheckout, warning: String) {
    let mut dialog = pending.dialog;
    // Keep the retry on the folder the failed command was writing, even when the reader
    // has moved to another folder in the meantime.
    dialog.folder = pending.target.dir.display().to_string();
    dialog.password.clear();
    dialog.warning = warning;
    dialog.confirmed = Some(pending.target.dir.clone());
    dialog.retry = Some(pending.target.dir.clone());
    app.dialog = Some(Dialog::Checkout(Box::new(dialog)));
}

/// Put a warning in the Checkout window when it is still open.
pub(crate) fn warn_dialog(app: &mut App, warning: impl Into<String>) {
    let warning = warning.into();
    if let Some(Dialog::Checkout(state)) = app.dialog.as_mut() {
        state.warning = warning.clone();
    }
    if !warning.is_empty() {
        app.status = warning;
    }
}

/// The List modules button: ask for the modules file and keep the window open.
pub(super) fn list_modules(app: &mut App) {
    if app.runner.running {
        warn_dialog(app, "Stop the running cvs command first.");
        return;
    }
    let Some(Dialog::Checkout(state)) = app.dialog.as_ref() else {
        return;
    };
    let (root, password) = connection::password_for(&state.root, &state.password);
    if root.trim().is_empty() && !app.has_cvs {
        warn_dialog(app, "Enter a repository, or open a CVS working copy.");
        return;
    }
    let password = if connection::is_pserver(&root) {
        password
    } else {
        None
    };
    if let Some(Dialog::Checkout(state)) = app.dialog.as_mut() {
        state.root = root.clone();
        state.password.clear();
        state.warning.clear();
    }
    let started = match password {
        Some(password) => {
            let purpose_root = root.clone();
            start_login(
                app,
                root,
                password,
                LoginPurpose::Modules { root: purpose_root },
            )
        }
        None => start_module_list(app, root),
    };
    if !started {
        warn_dialog(app, "Could not start the module list. See the Console.");
    }
}

/// The command behind List modules, also used after a login started by that button.
pub(crate) fn start_module_list(app: &mut App, root: String) -> bool {
    let req = modules_request(&root);
    if app.start(req, OutputKind::Console) {
        app.module_list_waiting = true;
        app.status = "Reading the module list — see the Console".into();
        true
    } else {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "cvsdesk-{}-{}-{}",
            tag,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn checkout_runs_against_the_root_in_the_dialog() {
        let req = request("HEAD", false, "proj", "", ":pserver:ann@host:/repo");
        assert_eq!(req.root.as_deref(), Some(":pserver:ann@host:/repo"));
        assert_eq!(req.files, vec!["proj"]);
        assert_eq!(req.tag, "HEAD", "the dialog's default tag is kept");
    }

    #[test]
    fn a_root_password_is_not_put_on_the_checkout_command() {
        let req = request("HEAD", false, "proj", "", ":pserver:ann:secret@host:/repo");
        assert_eq!(req.root.as_deref(), Some(":pserver:ann@host:/repo"));
    }

    #[test]
    fn the_module_list_is_a_read_only_checkout() {
        let req = modules_request(":pserver:ann@host:/repo");
        let root = req.root.clone().unwrap_or_default();
        assert_eq!(
            req.to_args(&root, &[]),
            vec!["-d", ":pserver:ann@host:/repo", "checkout", "-c"]
        );
    }

    #[test]
    fn a_named_tag_and_readonly_survive() {
        let req = request("Release_1_0", true, " proj ", "", ":pserver:ann@host:/repo");
        assert_eq!(req.tag, "Release_1_0");
        assert!(req.readonly);
        assert_eq!(req.files, vec!["proj"], "the module is trimmed");
    }

    #[test]
    fn the_folder_is_the_destination_checkout_is_given() {
        let req = request("HEAD", false, "proj", " work ", ":pserver:ann@host:/repo");
        assert_eq!(req.destination, "work");
        assert_eq!(
            req.to_args("", &[]),
            vec!["checkout", "-r", "HEAD", "-d", "work", "proj"]
        );
    }

    #[test]
    fn an_empty_folder_checks_out_the_module_under_this_folder() {
        let base = Path::new("C:\\work");
        let target = checkout_target(base, "proj", "").unwrap();
        assert_eq!(target.run_dir, base);
        assert_eq!(target.dir, base.join("proj"));
        assert_eq!(target.name, "proj");

        let target = checkout_target(base, "tools/proj", "  ").unwrap();
        assert_eq!(target.run_dir, base);
        assert_eq!(
            target.dir,
            base.join("proj"),
            "cvs names the folder after the module's last part"
        );
    }

    #[test]
    fn a_folder_name_under_this_folder_is_the_destination() {
        let base = Path::new("C:\\work");
        let target = checkout_target(base, "proj", "custom").unwrap();
        assert_eq!(target.run_dir, base);
        assert_eq!(target.dir, base.join("custom"));
        assert_eq!(target.name, "custom");

        let target = checkout_target(base, "proj", "sub/custom").unwrap();
        assert_eq!(target.run_dir, base.join("sub"));
        assert_eq!(target.dir, base.join("sub").join("custom"));
        assert_eq!(target.name, "custom");
    }

    #[cfg(windows)]
    #[test]
    fn an_absolute_folder_runs_in_its_parent() {
        let base = Path::new("C:\\work");
        let target = checkout_target(base, "proj", "D:\\out\\custom").unwrap();
        assert_eq!(target.run_dir, Path::new("D:\\out"));
        assert_eq!(target.dir, Path::new("D:\\out\\custom"));
        assert_eq!(target.name, "custom");
    }

    #[test]
    fn a_missing_module_is_named_as_such() {
        let base = Path::new("C:\\work");
        assert_eq!(
            checkout_target(base, "  ", "").unwrap_err(),
            CheckoutTargetError::NoModule
        );
    }

    #[test]
    fn the_folder_already_on_screen_is_refused() {
        let base = Path::new("C:\\work");
        assert_eq!(
            checkout_target(base, "proj", ".").unwrap_err(),
            CheckoutTargetError::CurrentFolder
        );
    }

    #[test]
    fn an_empty_folder_is_ready_without_asking() {
        let base = temp_dir("checkout-empty");
        let target = base.join("proj");
        assert_eq!(
            destination_check(&target, None, None),
            DestinationCheck::Ready
        );
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn a_folder_with_files_asks_once() {
        let base = temp_dir("checkout-files");
        let target = base.join("proj");
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(target.join("a.txt"), b"x").unwrap();

        assert_eq!(
            destination_check(&target, None, None),
            DestinationCheck::Confirm
        );
        assert_eq!(
            destination_check(&target, None, Some(&target)),
            DestinationCheck::Ready
        );
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn a_working_copy_is_refused_unless_it_is_the_failed_target() {
        let base = temp_dir("checkout-cvs");
        let target = base.join("proj");
        std::fs::create_dir_all(target.join("CVS")).unwrap();
        std::fs::write(target.join("a.txt"), b"x").unwrap();

        assert_eq!(
            destination_check(&target, None, None),
            DestinationCheck::RefuseWorkingCopy
        );
        assert_eq!(
            destination_check(&target, Some(&target), Some(&target)),
            DestinationCheck::Ready,
            "a retry may finish the folder the failed checkout already made"
        );
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn a_file_in_the_target_path_is_refused() {
        let base = temp_dir("checkout-file");
        let target = base.join("proj");
        std::fs::write(&target, b"x").unwrap();
        assert_eq!(
            destination_check(&target, None, None),
            DestinationCheck::RefuseFile
        );
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn a_successful_checkout_is_offered_again_for_the_same_root() {
        let mut state = CheckoutDialog::new("HEAD", false, ":pserver:ann@host:/repo".into());
        state.module = "proj".into();
        state.folder = "work".into();
        state.password = "secret".into();
        let memory = CheckoutMemory::from_dialog(&state);

        let mut next = CheckoutDialog::new("HEAD", false, ":pserver:ann@host:/repo".into());
        memory.restore(&mut next);
        assert_eq!(next.module, "proj");
        assert_eq!(next.folder, "work");
        assert!(next.password.is_empty(), "a password is never remembered");

        let mut other = CheckoutDialog::new("HEAD", false, ":pserver:ann@other:/repo".into());
        memory.restore(&mut other);
        assert!(other.module.is_empty(), "another root starts empty");
    }
}
