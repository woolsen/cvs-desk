//! Choosing the CVSROOT an operation acts on: the folder's own root, roots used
//! before, or one built from its parts.

use cvsdesk::entries::{self, ConnKind};

use crate::app::ui::typography::small_monospace;
use crate::app::ui::wrap;

/// Draw the root field, the roots used before and the part-by-part builder.
///
/// The field is what the dialog finally uses. The builder only fills it in, so a person
/// can paste a root, reuse one, or type four fields - whichever is less work.
pub(super) fn show(ui: &mut egui::Ui, root: &mut String, recent: &[String]) {
    ui.horizontal(|ui| {
        ui.label("Repository:");
        ui.add(
            egui::TextEdit::singleline(root)
                .hint_text(":pserver:user@host:/path")
                .desired_width(360.0),
        );
    });
    let typed = root.trim();
    if typed.is_empty() {
        ui.label(
            egui::RichText::new(
                "A folder's own commands read its CVS/Root; this is only needed to \
                 check out, import or log in.",
            )
            .small()
            .weak(),
        );
    } else {
        let (kind, _user, host, _port, path) = entries::parse_cvsroot(typed);
        wrap::label_in(
            ui,
            &format!(
                "{}  server={}  path={}",
                kind.label(),
                if host.is_empty() { "-" } else { &host },
                if path.is_empty() { "-" } else { &path },
            ),
            small_monospace(),
            false,
        );
    }

    if !recent.is_empty() {
        ui.horizontal_wrapped(|ui| {
            ui.label("Recent:");
            for candidate in recent {
                if ui
                    .small_button(short_root(candidate))
                    .on_hover_text(candidate)
                    .clicked()
                {
                    *root = candidate.clone();
                }
            }
        });
    }

    builder(ui, root);
}

/// A button label that keeps the method, account and the end of the path.
fn short_root(root: &str) -> String {
    const MAX: usize = 48;
    let (kind, user, host, port, path) = entries::parse_cvsroot(root);
    let mut text = kind.token().to_string();
    text.push(':');
    if kind.remote() {
        if !user.is_empty() {
            text.push_str(&user);
            text.push('@');
        }
        text.push_str(&host);
        if !port.is_empty() {
            text.push(':');
            text.push_str(&port);
        }
        text.push(':');
    }
    text.push_str(&path);
    let count = text.chars().count();
    if count > MAX {
        let tail: String = text.chars().skip(count - (MAX - 1)).collect();
        format!("…{tail}")
    } else {
        text
    }
}

/// The four fields a person fills in when they do not have a root string handy.
#[derive(Clone)]
struct Parts {
    kind: ConnKind,
    user: String,
    host: String,
    port: String,
    path: String,
}

impl Default for Parts {
    fn default() -> Self {
        Self {
            kind: ConnKind::Pserver,
            user: String::new(),
            host: String::new(),
            port: String::new(),
            path: String::new(),
        }
    }
}

impl Parts {
    /// Start from the root the dialog opened with, so its fields already hold it.
    fn from_root(root: &str) -> Self {
        let (kind, user, host, port, path) = entries::parse_cvsroot(root);
        Self {
            kind,
            user,
            host,
            port,
            path,
        }
    }

    fn root(&self) -> String {
        entries::build_cvsroot(
            self.kind, &self.user, &self.host, &self.port, &self.path, None,
        )
    }
}

fn builder(ui: &mut egui::Ui, root: &mut String) {
    // egui holds the typed parts between frames. Without that, every frame would reset
    // the fields and nothing could be typed.
    let id = ui.make_persistent_id("cvsdesk-connection-parts");
    let mut parts = ui
        .data(|d| d.get_temp::<Parts>(id))
        .unwrap_or_else(|| Parts::from_root(root));

    ui.collapsing("Build a new connection…", |ui| {
        ui.horizontal(|ui| {
            ui.label("Connection:");
            egui::ComboBox::from_id_salt(ui.scope_id().with("kind"))
                .selected_text(parts.kind.label())
                .show_ui(ui, |ui| {
                    for kind in ConnKind::ALL {
                        ui.selectable_value(&mut parts.kind, kind, kind.label());
                    }
                });
        });
        ui.horizontal(|ui| {
            ui.label("User:");
            ui.add(egui::TextEdit::singleline(&mut parts.user).desired_width(120.0));
            ui.label("Server:");
            ui.add(egui::TextEdit::singleline(&mut parts.host).desired_width(180.0));
        });
        ui.horizontal(|ui| {
            ui.label("Port:");
            ui.add(egui::TextEdit::singleline(&mut parts.port).desired_width(70.0));
            ui.label("Repository path:");
            ui.add(egui::TextEdit::singleline(&mut parts.path).desired_width(240.0));
        });
        wrap::label_in(ui, &parts.root(), small_monospace(), false);
        if ui.button("Use this root").clicked() {
            *root = parts.root();
        }
    });

    ui.data_mut(|d| d.insert_temp(id, parts));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_recent_root_is_shortened_without_losing_its_tail() {
        assert_eq!(
            short_root(":pserver:ann@host:/repo"),
            "pserver:ann@host:/repo"
        );
        assert_eq!(short_root(":local:/repo"), "local:/repo");
    }

    #[test]
    fn a_long_root_keeps_its_recognisable_end() {
        let long = format!(":pserver:ann@host:/{}", "very-long-folder/".repeat(10));
        let short = short_root(&long);
        assert!(short.chars().count() <= 48, "{short}");
        assert!(short.starts_with('…'));
        assert!(short.ends_with("very-long-folder/"), "{short}");
    }

    #[test]
    fn the_builder_puts_the_parts_back_together() {
        let parts = Parts {
            kind: ConnKind::Kserver,
            user: "ann".into(),
            host: "cvshost".into(),
            port: "2402".into(),
            path: "/repo".into(),
        };
        assert_eq!(parts.root(), ":kserver:ann@cvshost:2402:/repo");
    }
}
