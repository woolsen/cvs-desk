//! Turning a typed CVSROOT into the text a command runs against, and the password a
//! pserver login needs. A password written into the userinfo is pulled out here so it
//! never reaches the console, the recent-root list, or the settings file.

use cvsdesk::cvs_client::root::{Method, Root};

/// The root to run with, and the password typed into it, if any.
pub(super) fn split_root(raw: &str) -> (String, Option<String>) {
    let raw = raw.trim();
    match Root::parse(raw) {
        Ok(root) => {
            let password = matches!(root.method, Method::Pserver)
                .then_some(root.password)
                .flatten();
            (root.key, password)
        }
        Err(_) => (cvsdesk::entries::redact_root_password(raw), None),
    }
}

/// The password to use for a root: the dialog's field when it has one, otherwise the one
/// typed into the root itself.
pub(super) fn password_for(root: &str, field: &str) -> (String, Option<String>) {
    let (key, embedded) = split_root(root);
    let typed = field.trim();
    let password = if typed.is_empty() {
        embedded
    } else {
        // A password may contain spaces; only the emptiness check trims.
        Some(field.to_string())
    };
    (key, password)
}

/// Whether a root uses the pserver method, for the password field and its hint.
pub(super) fn is_pserver(root: &str) -> bool {
    let root = root.trim();
    match Root::parse(root) {
        Ok(root) => matches!(root.method, Method::Pserver),
        Err(_) => root.starts_with(":pserver:"),
    }
}

/// Whether the root text itself carries a password, even when the rest is malformed.
pub(super) fn has_password(root: &str) -> bool {
    let root = root.trim();
    match Root::parse(root) {
        Ok(root) => root.password.is_some(),
        Err(_) => cvsdesk::entries::redact_root_password(root) != root,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_password_typed_into_the_root_is_pulled_out() {
        let (root, password) = split_root(":pserver:ann:secret@host:/repo");
        assert_eq!(root, ":pserver:ann@host:/repo");
        assert_eq!(password.as_deref(), Some("secret"));
    }

    #[test]
    fn surrounding_space_does_not_hide_the_password() {
        let (root, password) = split_root("  :pserver:ann:secret@host:/repo  ");
        assert_eq!(root, ":pserver:ann@host:/repo");
        assert_eq!(password.as_deref(), Some("secret"));
        assert!(is_pserver("  :pserver:ann@host:/repo  "));
    }

    #[test]
    fn the_password_field_wins_over_the_root() {
        let (root, password) = password_for(":pserver:ann:secret@host:/repo", "typed");
        assert_eq!(root, ":pserver:ann@host:/repo");
        assert_eq!(password.as_deref(), Some("typed"));
    }

    #[test]
    fn a_password_keeps_its_spaces() {
        let (_, password) = password_for(":pserver:ann@host:/repo", " secret ");
        assert_eq!(password.as_deref(), Some(" secret "));
    }

    #[test]
    fn a_non_pserver_root_has_no_password_to_use() {
        let (root, password) = split_root(":ext:ann:secret@host:/repo");
        assert_eq!(root, ":ext:ann@host:/repo");
        assert_eq!(password, None);
    }

    #[test]
    fn a_malformed_root_password_is_redacted_before_a_command() {
        let (root, password) = split_root(":pserver:ann:secret@host");
        assert_eq!(root, ":pserver:ann@host");
        assert_eq!(password, None);
        assert!(has_password(":pserver:ann:secret@host"));
        assert!(!has_password(":pserver:ann@host:/repo"));
    }

    #[test]
    fn an_empty_root_is_not_a_pserver_root() {
        assert!(!is_pserver(""));
        assert!(is_pserver(":pserver:ann@host:/repo"));
    }
}
