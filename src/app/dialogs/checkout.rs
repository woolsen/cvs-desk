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
    /// The target of a failed checkout, with the request it belonged to.
    pub retry: Option<RetryTarget>,
    /// The folder a relative `folder` is resolved against. `None` means the folder the
    /// window shows; a failed checkout pins it so a navigation during the command cannot
    /// move the retry elsewhere.
    pub base: Option<PathBuf>,
    /// The root of the working copy at `base`, kept with a pinned target.
    pub base_root: Option<String>,
    /// Whether `base` was a working copy when a failed checkout pinned it.
    pub base_has_cvs: Option<bool>,
}

/// A failed checkout's target. The module and root are part of the key: a retry may only
/// reuse a partially written folder for the same request.
#[derive(Clone, Debug)]
pub(crate) struct RetryTarget {
    path: PathBuf,
    root: String,
    module: String,
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
            base: None,
            base_root: None,
            base_has_cvs: None,
        }
    }

    /// A confirmation belongs to the folder and the request that showed it. Editing the
    /// root or module asks again, while a retry of the same request keeps its target.
    fn forget_confirmation_if_request_changed(&mut self, old_root: &str, old_module: &str) {
        if self.root != old_root || self.module != old_module {
            self.confirmed = None;
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
        let tag = dialog.tag.trim();
        Self {
            root: dialog.root.clone(),
            module: dialog.module.clone(),
            folder: dialog.folder.clone(),
            tag: if tag.eq_ignore_ascii_case("HEAD") {
                String::new()
            } else {
                dialog.tag.clone()
            },
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
    base: PathBuf,
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
    PendingCheckout {
        dialog,
        target,
        base: base.to_path_buf(),
    }
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
    /// The target is a `CVS` metadata folder.
    RefuseMetadata,
}

pub(super) fn show(
    ctx: &egui::Context,
    state: &mut CheckoutDialog,
    recent: &[String],
    base: &Path,
    has_cvs: bool,
    busy: bool,
) -> Option<DialogAction> {
    let base = state.base.clone().unwrap_or_else(|| base.to_path_buf());
    let has_cvs = state.base_has_cvs.unwrap_or(has_cvs);
    let old_root = state.root.clone();
    let old_module = state.module.clone();
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
                if connection::has_password(&state.root) {
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
                ui.add(
                    egui::TextEdit::singleline(&mut state.tag)
                        .hint_text("Empty: HEAD")
                        .desired_width(220.0),
                );
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
            match checkout_target(&base, &state.module, &state.folder) {
                Ok(target) => {
                    let mut line = format!("Will write: {}", target.dir.display());
                    match destination_check(
                        &target.dir,
                        state.retry.as_ref(),
                        state.confirmed.as_deref(),
                        &state.root,
                        &state.module,
                    ) {
                        DestinationCheck::RefuseWorkingCopy => {
                            line.push_str(" — already a CVS working copy");
                        }
                        DestinationCheck::RefuseFile => line.push_str(" — a file is in the way"),
                        DestinationCheck::RefuseMetadata => {
                            line.push_str(" — a CVS metadata entry");
                        }
                        DestinationCheck::Confirm => line.push_str(" — not empty"),
                        DestinationCheck::Ready => {}
                    }
                    if has_cvs && target.dir.starts_with(&base) && target.dir != base {
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
                    checkout_target(&base, &state.module, &state.folder)
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
                let cancel = ui.button(if busy { "Close" } else { "Cancel" });
                let cancel = if busy {
                    cancel.on_hover_text("Close this window; the running command continues")
                } else {
                    cancel
                };
                if cancel.clicked() {
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
    state.forget_confirmation_if_request_changed(&old_root, &old_module);
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
    let tag = tag.trim();
    req.tag = if tag.is_empty() || tag.eq_ignore_ascii_case("HEAD") {
        // HEAD is the default. Sending `-r HEAD` would write a sticky tag into the new
        // working copy and make every later commit fail.
        String::new()
    } else {
        tag.to_string()
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
    retry: Option<&RetryTarget>,
    confirmed: Option<&Path>,
    root: &str,
    module: &str,
) -> DestinationCheck {
    if target.is_file() {
        return DestinationCheck::RefuseFile;
    }
    if contains_cvs_metadata(target) {
        return DestinationCheck::RefuseMetadata;
    }
    let cvs = target.join("CVS");
    if cvs.exists() && !cvs.is_dir() {
        return DestinationCheck::RefuseMetadata;
    }
    // A folder that already holds CVS belongs to another working copy. The one exception
    // is the folder this same request just created before it failed: retrying it is fine.
    let retrying = retry
        .is_some_and(|retry| retry.path == target && retry.root == root && retry.module == module);
    if cvs.is_dir() && !retrying {
        return DestinationCheck::RefuseWorkingCopy;
    }
    if target.is_dir() && !is_empty_dir(target) && confirmed != Some(target) {
        return DestinationCheck::Confirm;
    }
    DestinationCheck::Ready
}

/// A folder named CVS is only special when it is a working copy's metadata folder.
fn contains_cvs_metadata(target: &Path) -> bool {
    let mut path = PathBuf::new();
    for part in target.components() {
        path.push(part);
        if part
            .as_os_str()
            .to_string_lossy()
            .eq_ignore_ascii_case("CVS")
            && is_cvs_metadata(&path)
        {
            return true;
        }
    }
    false
}

fn is_cvs_metadata(path: &Path) -> bool {
    path.join("Entries").is_file()
        || path.join("Root").is_file()
        || path.join("Repository").is_file()
}

fn is_empty_dir(path: &Path) -> bool {
    std::fs::read_dir(path)
        .map(|mut entries| entries.next().is_none())
        .unwrap_or(false)
}

/// The root to run with: the typed one, or the current working copy's when the field is
/// empty. An explicit root keeps a checkout into another folder on the same repository.
fn effective_root(app: &App, state: &CheckoutDialog) -> String {
    let typed = state.root.trim();
    if !typed.is_empty() {
        return typed.to_string();
    }
    state
        .base_root
        .clone()
        .unwrap_or_else(|| cvsdesk::entries::clean_root(&app.root))
        .trim()
        .to_string()
}

/// Validate the fields, then start a login (when a password was given) or the checkout.
/// A window is returned only when it must stay open, with a warning for the user.
pub(crate) fn apply(app: &mut App, d: Dialog) -> Option<Dialog> {
    let Dialog::Checkout(state) = d else {
        return None;
    };
    let mut state = *state;
    let base = state.base.clone().unwrap_or_else(|| app.dir.clone());
    let base_has_cvs = state.base_has_cvs.unwrap_or(app.has_cvs);
    let base_root = state
        .base_root
        .clone()
        .unwrap_or_else(|| cvsdesk::entries::clean_root(&app.root));
    if app.runner.running {
        state.warning = "cvs is already running; wait for it to finish.".into();
        return Some(Dialog::Checkout(Box::new(state)));
    }
    let root_text = effective_root(app, &state);
    if root_text.trim().is_empty() {
        state.warning = if base_has_cvs {
            "This working copy has no CVS/Root. Enter a repository.".into()
        } else {
            "Enter a repository, or open a CVS working copy.".into()
        };
        return Some(Dialog::Checkout(Box::new(state)));
    }
    let target = match checkout_target(&base, &state.module, &state.folder) {
        Ok(target) => target,
        Err(error) => {
            state.warning = error.message().into();
            return Some(Dialog::Checkout(Box::new(state)));
        }
    };
    match destination_check(
        &target.dir,
        state.retry.as_ref(),
        state.confirmed.as_deref(),
        &state.root,
        &state.module,
    ) {
        DestinationCheck::RefuseFile => {
            state.warning = format!(
                "{} is a file, not a folder. Choose another target.",
                target.dir.display()
            );
            return Some(Dialog::Checkout(Box::new(state)));
        }
        DestinationCheck::RefuseMetadata => {
            state.warning = format!(
                "{} is, or contains, a CVS metadata entry. Choose another folder.",
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
            let nested = base_has_cvs && target.dir.starts_with(&base) && target.dir != base;
            state.warning = if nested {
                format!(
                    "{} is not empty and sits inside the working copy on screen. Click Checkout again to use it.",
                    target.dir.display()
                )
            } else {
                format!(
                    "{} is not empty. Click Checkout again to write into it.",
                    target.dir.display()
                )
            };
            return Some(Dialog::Checkout(Box::new(state)));
        }
        DestinationCheck::Ready => {}
    }
    let nested = base_has_cvs
        && target.dir.starts_with(&base)
        && target.dir != base
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
    let (root, embedded) = connection::password_for(&root_text, &state.password);
    let password = if connection::is_pserver(&root) {
        embedded
    } else {
        None
    };
    state.root = root.clone();
    state.password.clear();
    state.base = Some(base.clone());
    state.base_has_cvs = Some(base_has_cvs);
    state.base_root = Some(base_root);
    app.prefs.remember_root(&root);
    let pending = PendingCheckout {
        dialog: state,
        target,
        base,
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
    dialog.password.clear();
    dialog.warning = warning;
    dialog.base = Some(pending.base.clone());
    dialog.confirmed = Some(pending.target.dir.clone());
    dialog.retry = Some(RetryTarget {
        path: pending.target.dir.clone(),
        root: dialog.root.clone(),
        module: dialog.module.clone(),
    });
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
    let root_text = effective_root(app, state);
    let base_has_cvs = state.base_has_cvs.unwrap_or(app.has_cvs);
    if root_text.trim().is_empty() {
        warn_dialog(
            app,
            if base_has_cvs {
                "This working copy has no CVS/Root. Enter a repository."
            } else {
                "Enter a repository, or open a CVS working copy."
            },
        );
        return;
    }
    let (root, password) = connection::password_for(&root_text, &state.password);
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

    const ROOT: &str = ":pserver:ann@host:/repo";
    const MODULE: &str = "proj";

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
        assert!(
            req.tag.is_empty(),
            "HEAD is the default and is not sent as a sticky tag"
        );
    }

    #[test]
    fn a_default_head_tag_sends_no_revision() {
        let req = request("HEAD", false, "proj", "", ":pserver:ann@host:/repo");
        assert_eq!(
            req.to_args(":pserver:ann@host:/repo", &[]),
            vec!["-d", ":pserver:ann@host:/repo", "checkout", "proj"]
        );
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
        assert_eq!(req.to_args("", &[]), vec!["checkout", "-d", "work", "proj"]);
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
            destination_check(&target, None, None, ROOT, MODULE),
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
            destination_check(&target, None, None, ROOT, MODULE),
            DestinationCheck::Confirm
        );
        assert_eq!(
            destination_check(&target, None, Some(&target), ROOT, MODULE),
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
        let retry = RetryTarget {
            path: target.clone(),
            root: ROOT.to_string(),
            module: MODULE.to_string(),
        };

        assert_eq!(
            destination_check(&target, None, None, ROOT, MODULE),
            DestinationCheck::RefuseWorkingCopy
        );
        assert_eq!(
            destination_check(&target, Some(&retry), Some(&target), ROOT, MODULE),
            DestinationCheck::Ready,
            "a retry may finish the folder the failed checkout already made"
        );
        assert_eq!(
            destination_check(
                &target,
                Some(&retry),
                Some(&target),
                ":pserver:ann@other:/repo",
                MODULE
            ),
            DestinationCheck::RefuseWorkingCopy,
            "another root must not write into the failed folder"
        );
        assert_eq!(
            destination_check(&target, Some(&retry), Some(&target), ROOT, "another"),
            DestinationCheck::RefuseWorkingCopy,
            "another module must not write into the failed folder"
        );
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn a_pinned_base_keeps_its_working_copy_context() {
        let base = temp_dir("checkout-pinned");
        let app_base = temp_dir("checkout-pinned-app");
        let mut app = crate::app::tests::app_with_rows(Vec::new());
        app.dir = app_base.clone();
        app.has_cvs = false;
        app.root.clear();

        let mut state = CheckoutDialog::new("", false, ROOT.into());
        state.module = MODULE.into();
        state.folder = "sub".into();
        state.base = Some(base.clone());
        state.base_has_cvs = Some(true);
        state.base_root = Some(ROOT.into());

        let returned = apply(&mut app, Dialog::Checkout(Box::new(state)));

        let Some(Dialog::Checkout(state)) = returned else {
            panic!("the window must stay open for the nested confirmation");
        };
        assert!(
            state.warning.contains("inside the working copy"),
            "{}",
            state.warning
        );
        assert!(
            !app.runner.running,
            "nothing may run before the user confirms"
        );

        std::fs::remove_dir_all(&base).ok();
        std::fs::remove_dir_all(&app_base).ok();
    }

    #[test]
    fn a_file_in_the_target_path_is_refused() {
        let base = temp_dir("checkout-file");
        let target = base.join("proj");
        std::fs::write(&target, b"x").unwrap();
        assert_eq!(
            destination_check(&target, None, None, ROOT, MODULE),
            DestinationCheck::RefuseFile
        );
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn a_cvs_metadata_folder_is_refused() {
        let base = temp_dir("checkout-metadata");
        let target = base.join("CVS");
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(target.join("Root"), b"x").unwrap();
        assert_eq!(
            destination_check(&target, None, None, ROOT, MODULE),
            DestinationCheck::RefuseMetadata
        );
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn a_target_inside_a_cvs_folder_is_refused() {
        let base = temp_dir("checkout-metadata-inside");
        let target = base.join("CVS").join("proj");
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(base.join("CVS").join("Root"), b"x").unwrap();
        assert_eq!(
            destination_check(&target, None, None, ROOT, MODULE),
            DestinationCheck::RefuseMetadata
        );
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn a_plain_folder_named_cvs_is_not_metadata() {
        let base = temp_dir("checkout-plain-cvs");
        let target = base.join("cvs").join("proj");
        std::fs::create_dir_all(&target).unwrap();
        assert_eq!(
            destination_check(&target, None, None, ROOT, MODULE),
            DestinationCheck::Ready,
            "a workspace folder named cvs is not a metadata folder"
        );
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn a_cvs_file_in_the_target_is_refused() {
        let base = temp_dir("checkout-metadata-file");
        let target = base.join("proj");
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(target.join("CVS"), b"x").unwrap();
        assert_eq!(
            destination_check(&target, None, None, ROOT, MODULE),
            DestinationCheck::RefuseMetadata
        );
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn editing_the_request_forgets_a_confirmation_but_not_the_retry() {
        let mut state = CheckoutDialog::new("", false, ROOT.into());
        state.module = MODULE.into();
        state.confirmed = Some(PathBuf::from("C:\\work\\proj"));
        state.retry = Some(RetryTarget {
            path: PathBuf::from("C:\\work\\proj"),
            root: ROOT.into(),
            module: MODULE.into(),
        });

        let root = state.root.clone();
        state.forget_confirmation_if_request_changed(&root, MODULE);
        assert!(state.confirmed.is_some(), "an unchanged request keeps it");

        state.module = "another".into();
        state.forget_confirmation_if_request_changed(&root, MODULE);
        assert!(state.confirmed.is_none(), "another module asks again");
    }

    #[test]
    fn an_empty_root_uses_the_current_working_copy_root() {
        let mut app = crate::app::tests::app_with_rows(Vec::new());
        app.has_cvs = true;
        app.root = ROOT.into();
        let mut state = CheckoutDialog::new("", false, String::new());
        assert_eq!(effective_root(&app, &state), ROOT);

        state.root = ":pserver:bob@host:/repo".into();
        assert_eq!(effective_root(&app, &state), ":pserver:bob@host:/repo");
    }

    #[test]
    fn a_pinned_base_keeps_its_own_root() {
        let mut app = crate::app::tests::app_with_rows(Vec::new());
        app.has_cvs = false;
        app.root.clear();
        let mut state = CheckoutDialog::new("", false, String::new());
        state.base_root = Some(":pserver:base@host:/repo".into());
        assert_eq!(
            effective_root(&app, &state),
            ":pserver:base@host:/repo",
            "a retry after navigation still uses the working copy it came from"
        );
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
