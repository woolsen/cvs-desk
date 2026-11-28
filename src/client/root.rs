//! CVSROOT strings, and finding the root a working copy belongs to.
//!
//! The shape CVS uses is `:method:[user@]host[:port]:/path`. On Windows a bare path such as
//! `C:\repo` is also a root: it means `:local:`. CVSNT adds `proxy=HOST:PORT:` after
//! `:pserver:` when a proxy is configured (see the desktop program's `build_cvsroot`).
//!
//! The text the user typed is kept as it was typed — including whether a port was written
//! — because messages show it. [`Root::key`] is that text with any password removed, never
//! rebuilt from the parsed pieces; `passfile` normalizes it for the `.cvspass` key.

use std::fmt;
use std::path::{Path, PathBuf};

use crate::client::error::{Error, Result};

/// The port a pserver root uses when it names none.
pub const DEFAULT_PSERVER_PORT: u16 = 2401;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Method {
    Pserver,
    Ext,
    Local,
    Server,
    Kserver,
    Gserver,
    Ntserver,
    Sspi,
    Unknown(String),
}

impl Method {
    pub fn parse(text: &str) -> Method {
        match text.to_ascii_lowercase().as_str() {
            "pserver" => Method::Pserver,
            "ext" => Method::Ext,
            "local" => Method::Local,
            "server" => Method::Server,
            "kserver" => Method::Kserver,
            "gserver" => Method::Gserver,
            "ntserver" => Method::Ntserver,
            "sspi" => Method::Sspi,
            other => Method::Unknown(other.to_string()),
        }
    }

    pub fn token(&self) -> &str {
        match self {
            Method::Pserver => "pserver",
            Method::Ext => "ext",
            Method::Local => "local",
            Method::Server => "server",
            Method::Kserver => "kserver",
            Method::Gserver => "gserver",
            Method::Ntserver => "ntserver",
            Method::Sspi => "sspi",
            Method::Unknown(name) => name,
        }
    }

    pub fn is_remote(&self) -> bool {
        !matches!(self, Method::Local)
    }

    /// The methods this client can actually open a connection with today.
    pub fn is_supported(&self) -> bool {
        matches!(self, Method::Pserver | Method::Ext)
    }
}

/// Run-time settings that travel with a root but are not part of its text.
///
/// They live here because every command already receives the root, and the protocol needs
/// them before the first request (`Global_option`) while the command needs them for its
/// local writes (`-n`). [`Root::parse`] leaves them at their defaults.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WireOptions {
    /// The `-z` level for connections to this root.
    pub compression: u32,
    /// `-n`: report what would happen, change nothing.
    pub no_execute: bool,
    /// How quiet: 0 normal, 1 `-q`, 2 `-Q`.
    pub quiet: u32,
    /// `-t`: ask the server to trace as well.
    pub trace: bool,
    /// `-r`: new working files are read-only.
    pub readonly: bool,
    /// `-e editor`: the editor for log messages, `None` for the environment/default.
    pub editor: Option<String>,
}

#[derive(Clone, PartialEq, Eq)]
pub struct Root {
    /// The root exactly as it was written, minus any password. `passfile` normalizes it
    /// before using it as a `.cvspass` key.
    pub key: String,
    pub method: Method,
    pub user: String,
    /// The password written into the root as `user:password@host`, if any. `login` uses it
    /// without asking (login.c:544-547); it is never part of [`Root::key`] or a message.
    pub password: Option<String>,
    pub host: String,
    /// Zero when the method has no port (ext) or the root has none (local).
    pub port: u16,
    pub proxy: Option<(String, u16)>,
    /// The repository path. For a remote root it is the path on the server.
    pub path: String,
    /// Run-time settings that are not part of the root text.
    pub options: WireOptions,
}

impl fmt::Debug for Root {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Root")
            .field("key", &self.key)
            .field("method", &self.method)
            .field("user", &self.user)
            .field("password", &self.password.as_ref().map(|_| "<redacted>"))
            .field("host", &self.host)
            .field("port", &self.port)
            .field("proxy", &self.proxy)
            .field("path", &self.path)
            .field("options", &self.options)
            .finish()
    }
}

impl Root {
    /// Parse one root string. A bare path is a local root, as CVS treats it.
    pub fn parse(raw: &str) -> Result<Root> {
        let raw = raw.trim_end_matches(['\r', '\n']);
        if raw.trim().is_empty() {
            return Err(Error::new("empty CVSROOT"));
        }

        let Some(rest) = raw.strip_prefix(':') else {
            return Ok(Root {
                key: raw.to_string(),
                method: Method::Local,
                user: String::new(),
                password: None,
                host: String::new(),
                port: 0,
                proxy: None,
                path: raw.to_string(),
                options: WireOptions::default(),
            });
        };

        let Some((method_text, mut rest)) = rest.split_once(':') else {
            return Err(Error::new(format!(
                "CVSROOT {} is not a valid root",
                scrub(raw)
            )));
        };
        let method = Method::parse(method_text);

        if method == Method::Local {
            if rest.is_empty() {
                return Err(Error::new(format!(
                    "CVSROOT {} has no repository path",
                    scrub(raw)
                )));
            }
            return Ok(Root {
                key: raw.to_string(),
                method,
                user: String::new(),
                password: None,
                host: String::new(),
                port: 0,
                proxy: None,
                path: rest.to_string(),
                options: WireOptions::default(),
            });
        }

        let mut proxy = None;
        if let Some(after_proxy) = rest.strip_prefix("proxy=") {
            let (proxy_host, after_host) = after_proxy.split_once(':').ok_or_else(|| {
                Error::new(format!("CVSROOT {} has a malformed proxy", scrub(raw)))
            })?;
            let (proxy_port, after_port) = after_host.split_once(':').ok_or_else(|| {
                Error::new(format!("CVSROOT {} has a malformed proxy", scrub(raw)))
            })?;
            let port: u16 = proxy_port.parse().map_err(|_| {
                Error::new(format!(
                    "CVSROOT {} has a bad proxy port '{}'",
                    scrub(raw),
                    proxy_port
                ))
            })?;
            proxy = Some((proxy_host.to_string(), port));
            rest = after_port;
        }

        // The userinfo ends at the first `/`, the first `@` and then the first `:`, as
        // `parse_cvsroot` reads it (root.c:492-520). Bounding it by the path keeps an `@`
        // in the repository name from looking like a password separator.
        let base = raw.len() - rest.len();
        let path_start = rest.find('/').unwrap_or(rest.len());
        let mut host_start = base;
        let mut password_colon = None;
        let at = rest[..path_start]
            .find('@')
            .filter(|&at| !at_is_path(rest, at));
        let (user, password, host_part) = match at {
            Some(at) => {
                let info = &rest[..at];
                host_start = base + at;
                match info.find(':') {
                    Some(colon) => {
                        password_colon = Some(base + colon);
                        (
                            info[..colon].to_string(),
                            Some(info[colon + 1..].to_string()),
                            &rest[at + 1..],
                        )
                    }
                    None => (info.to_string(), None, &rest[at + 1..]),
                }
            }
            None => (String::new(), None, rest),
        };

        let Some((host, port_text, path)) = split_host_path(host_part) else {
            return Err(Error::new(format!(
                "CVSROOT {} has no repository path",
                scrub(raw)
            )));
        };
        if host.is_empty() {
            return Err(Error::new(format!(
                "CVSROOT {} has no host name",
                scrub(raw)
            )));
        }
        if path.is_empty() {
            return Err(Error::new(format!(
                "CVSROOT {} has no repository path",
                scrub(raw)
            )));
        }
        let port = match port_text {
            Some(text) => text.parse::<u16>().map_err(|_| {
                Error::new(format!("CVSROOT {} has a bad port '{}'", scrub(raw), text))
            })?,
            None => default_port(&method),
        };

        let user = if user.trim().is_empty() {
            current_user()
        } else {
            user
        };

        // Keep the typed root as the passfile key, but take the password out of it.
        let key = match password_colon {
            Some(colon) => format!("{}{}", &raw[..colon], &raw[host_start..]),
            None => raw.to_string(),
        };

        Ok(Root {
            key,
            method,
            user,
            password,
            host: host.to_string(),
            port,
            proxy,
            path: path.to_string(),
            options: WireOptions::default(),
        })
    }

    /// The root as it is shown in messages: no password, ever.
    pub fn display(&self) -> &str {
        &self.key
    }

    /// What the protocol's `Root` request carries: the repository path, not the method.
    pub fn protocol_root(&self) -> &str {
        &self.path
    }

    /// The port to connect to, if this method uses one.
    pub fn connect_port(&self) -> u16 {
        self.port
    }
}

/// A root removed from a password, for an error message.
pub fn scrub(raw: &str) -> String {
    // The method prefix comes first, then the userinfo; the path cannot hold a password.
    let Some(method_end) = raw
        .strip_prefix(':')
        .and_then(|s| s.find(':'))
        .map(|i| i + 2)
    else {
        return raw.to_string();
    };
    let tail = &raw[method_end..];
    let path_start = tail.find('/').unwrap_or(tail.len());
    let Some(at) = tail[..path_start].rfind('@') else {
        return raw.to_string();
    };
    let Some(colon) = tail[..at].rfind(':').map(|i| method_end + i) else {
        return raw.to_string();
    };
    format!("{}:***{}", &raw[..colon], &raw[method_end + at..])
}

fn default_port(method: &Method) -> u16 {
    match method {
        Method::Ext | Method::Local => 0,
        _ => DEFAULT_PSERVER_PORT,
    }
}

/// Whether the `@` at `at` is inside a Windows path rather than the userinfo.
pub(crate) fn at_is_path(rest: &str, at: usize) -> bool {
    // If the text after the @ is `host[:port]:path`, the @ is the userinfo separator.
    // This keeps a password such as `p:\x` from looking like a path.
    if split_host_path(&rest[at + 1..]).is_some() {
        return false;
    }
    let Some(colon) = rest[..at].find(':') else {
        return false;
    };
    let between = rest[colon + 1..at].as_bytes();
    // A Windows path names a drive after a colon: `C:\...` or `C:/...`. A password may
    // contain a backslash too, so a bare backslash is not enough.
    between
        .windows(3)
        .any(|w| w[0].is_ascii_alphabetic() && w[1] == b':' && matches!(w[2], b'\\' | b'/'))
}

/// Split `host[:port]:path` at the colon before the path.
///
/// The path may itself contain a colon (a Windows drive), so a colon followed by digits and
/// then another colon is read as a port; any other colon starts the path.
pub(crate) fn split_host_path(text: &str) -> Option<(&str, Option<&str>, &str)> {
    let mut i = if text.starts_with('[') {
        text.find(']')? + 1
    } else {
        0
    };
    let bytes = text.as_bytes();
    while i < bytes.len() {
        if bytes[i] == b':' {
            let after = &text[i + 1..];
            let digits = after.bytes().take_while(|b| b.is_ascii_digit()).count();
            if digits > 0 && after.as_bytes().get(digits) == Some(&b':') {
                return Some((&text[..i], Some(&after[..digits]), &after[digits + 1..]));
            }
            return Some((&text[..i], None, after));
        }
        i += 1;
    }
    None
}

/// The account name CVS falls back to when a root names none.
pub fn current_user() -> String {
    for name in ["USERNAME", "USER", "LOGNAME"] {
        if let Ok(value) = std::env::var(name) {
            if !value.trim().is_empty() {
                return value;
            }
        }
    }
    "unknown".to_string()
}

/// Decode a control file: UTF-8 first, then GBK, the way the desktop program reads them.
pub fn decode_control(bytes: &[u8]) -> String {
    match std::str::from_utf8(bytes) {
        Ok(text) => text.to_string(),
        Err(_) => {
            let (text, _, _) = encoding_rs::GBK.decode(bytes);
            text.into_owned()
        }
    }
}

/// Walk up from `start` until a `CVS/Root` is found. Returns the folder holding `CVS/`.
pub fn find_working_root(start: &Path) -> Option<(PathBuf, Root)> {
    let mut dir = start.to_path_buf();
    loop {
        let root_file = dir.join("CVS").join("Root");
        if root_file.is_file() {
            if let Ok(bytes) = std::fs::read(&root_file) {
                let text = decode_control(&bytes);
                if let Some(line) = text.lines().next() {
                    let line = line.trim();
                    if !line.is_empty() {
                        if let Ok(root) = Root::parse(line) {
                            return Some((dir, root));
                        }
                    }
                }
            }
        }
        if !dir.pop() {
            return None;
        }
    }
}

/// Whichever root applies: `-d`, then `$CVSROOT`, then the working copy above the folder.
///
/// The second value is the folder whose `CVS/Root` supplied the root, when one did.
pub fn resolve(
    explicit: Option<&str>,
    cwd: &Path,
    command: &str,
) -> Result<(Root, Option<PathBuf>)> {
    if let Some(text) = explicit {
        return Ok((Root::parse(text)?, None));
    }
    if let Ok(text) = std::env::var("CVSROOT") {
        if !text.trim().is_empty() {
            return Ok((Root::parse(&text)?, None));
        }
    }
    if let Some((dir, root)) = find_working_root(cwd) {
        return Ok((root, Some(dir)));
    }
    Err(Error::aborted(
        command,
        "No CVSROOT specified!  Please use the `-d' option",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pserver_roots_lose_no_information() {
        let root = Root::parse(":pserver:ann@cvshost:/repo").unwrap();
        assert_eq!(root.method, Method::Pserver);
        assert_eq!(root.user, "ann");
        assert_eq!(root.host, "cvshost");
        assert_eq!(root.port, DEFAULT_PSERVER_PORT);
        assert_eq!(root.path, "/repo");
        assert_eq!(root.key, ":pserver:ann@cvshost:/repo");

        let root = Root::parse(":pserver:ann@cvshost:2402:/repo").unwrap();
        assert_eq!(root.port, 2402);
        assert_eq!(root.path, "/repo");
        // The typed root, port included, is what .cvspass is keyed by.
        assert_eq!(root.key, ":pserver:ann@cvshost:2402:/repo");
    }

    #[test]
    fn a_password_is_taken_out_of_the_key_and_messages() {
        let root = Root::parse(":pserver:ann:secret@cvshost:/repo").unwrap();
        assert_eq!(root.user, "ann");
        assert_eq!(root.password.as_deref(), Some("secret"));
        assert_eq!(root.key, ":pserver:ann@cvshost:/repo");
        assert_eq!(root.display(), ":pserver:ann@cvshost:/repo");
        assert!(!format!("{}", scrub(":pserver:ann:secret@cvshost:/repo")).contains("secret"));
    }

    #[test]
    fn a_backslash_path_may_contain_an_at_sign_without_a_user() {
        let root = Root::parse(":pserver:host:C:\\repos\\a@b").unwrap();
        assert_eq!(root.user, current_user());
        assert_eq!(root.host, "host");
        assert_eq!(root.path, "C:\\repos\\a@b");
        assert!(root.password.is_none());

        let root = Root::parse(":pserver:ann:secret@host:C:\\repos\\a@b").unwrap();
        assert_eq!(root.user, "ann");
        assert_eq!(root.password.as_deref(), Some("secret"));
        assert_eq!(root.host, "host");
        assert_eq!(root.path, "C:\\repos\\a@b");
    }

    #[test]
    fn a_password_may_contain_a_backslash() {
        let root = Root::parse(":pserver:ann:se\\cret@host:/repo").unwrap();
        assert_eq!(root.user, "ann");
        assert_eq!(root.password.as_deref(), Some("se\\cret"));
        assert_eq!(root.host, "host");
        assert_eq!(root.path, "/repo");
    }

    #[test]
    fn a_drive_like_password_is_not_read_as_a_path() {
        // `p:\x` looks like a drive path, but the text after the @ is a valid
        // host and path, so the @ really is the userinfo separator.
        let root = Root::parse(":pserver:ann:p:\\x@host:/repo").unwrap();
        assert_eq!(root.user, "ann");
        assert_eq!(root.password.as_deref(), Some("p:\\x"));
        assert_eq!(root.host, "host");
        assert_eq!(root.path, "/repo");
        assert_eq!(root.key, ":pserver:ann@host:/repo");
    }

    #[test]
    fn the_debug_form_hides_the_password() {
        let root = Root::parse(":pserver:ann:secret@cvshost:/repo").unwrap();
        let text = format!("{:?}", root);
        assert!(!text.contains("secret"), "{}", text);
        assert!(text.contains("<redacted>"), "{}", text);
    }

    #[test]
    fn a_password_is_found_before_the_first_slash_and_at_sign() {
        let root = Root::parse(":pserver:ann:secret@cvshost:/repo@dir").unwrap();
        assert_eq!(root.user, "ann");
        assert_eq!(root.password.as_deref(), Some("secret"));
        assert_eq!(root.host, "cvshost");
        assert_eq!(root.path, "/repo@dir");
        assert_eq!(root.key, ":pserver:ann@cvshost:/repo@dir");

        let root = Root::parse(":pserver:ann@cvshost:/repo@dir").unwrap();
        assert_eq!(root.user, "ann");
        assert_eq!(root.password, None);
        assert_eq!(root.path, "/repo@dir");
        assert_eq!(root.key, ":pserver:ann@cvshost:/repo@dir");
    }

    #[test]
    fn a_proxy_prefix_is_read_before_the_user() {
        let root = Root::parse(":pserver:proxy=proxyhost:8080:ann@cvshost:/repo").unwrap();
        assert_eq!(root.proxy, Some(("proxyhost".to_string(), 8080)));
        assert_eq!(root.user, "ann");
        assert_eq!(root.host, "cvshost");
        assert_eq!(root.path, "/repo");
    }

    #[test]
    fn a_windows_path_after_ext_is_a_path_not_a_port() {
        let root = Root::parse(r":ext:fakehost:C:\work\repo").unwrap();
        assert_eq!(root.method, Method::Ext);
        assert_eq!(root.host, "fakehost");
        assert_eq!(root.path, r"C:\work\repo");
        assert_eq!(root.port, 0);
    }

    #[test]
    fn bare_paths_are_local_roots() {
        let root = Root::parse(r"C:\work\repo").unwrap();
        assert_eq!(root.method, Method::Local);
        assert_eq!(root.path, r"C:\work\repo");
        assert!(!root.method.is_supported());
    }

    #[test]
    fn a_root_with_no_path_is_refused() {
        assert!(Root::parse(":pserver:ann@cvshost").is_err());
        assert!(Root::parse(":pserver:ann@cvshost:").is_err());
        assert!(Root::parse(":pserver::/repo").is_err());
    }

    #[test]
    fn the_root_request_is_the_path_only() {
        let root = Root::parse(":pserver:ann@cvshost:/repo").unwrap();
        assert_eq!(root.protocol_root(), "/repo");
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "cvsdesk-root-{}-{}-{}",
            std::process::id(),
            name,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_working_copy_is_found_from_a_sub_folder() {
        let top = scratch("find");
        std::fs::create_dir_all(top.join("CVS")).unwrap();
        std::fs::write(top.join("CVS").join("Root"), ":pserver:ann@cvshost:/repo").unwrap();
        let sub = top.join("a").join("b");
        std::fs::create_dir_all(&sub).unwrap();
        let (dir, root) = find_working_root(&sub).unwrap();
        assert_eq!(dir, top);
        assert_eq!(root.host, "cvshost");
        std::fs::remove_dir_all(&top).ok();
    }

    #[test]
    fn a_gbk_root_file_is_decoded() {
        let text = ":pserver:ann@cvshost:/repo";
        assert_eq!(decode_control(text.as_bytes()), text);
        // The same text with a GBK name still parses; the name is not part of the root.
        let mut bytes = b":pserver:ann@cvshost:/".to_vec();
        bytes.extend_from_slice(&[0xd6, 0xd0, 0xce, 0xc4]); // "中文" in GBK
        let decoded = decode_control(&bytes);
        assert!(decoded.ends_with("中文"));
    }
}
