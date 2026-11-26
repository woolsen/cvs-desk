//! CVSROOT strings, and finding the root a working copy belongs to.
//!
//! The shape CVS uses is `:method:[user@]host[:port]:/path`. On Windows a bare path such as
//! `C:\repo` is also a root: it means `:local:`. CVSNT adds `proxy=HOST:PORT:` after
//! `:pserver:` when a proxy is configured (see the desktop program's `build_cvsroot`).
//!
//! A root is also the key `.cvspass` is stored under, so the text the user typed has to be
//! kept as it was typed — including whether a port was written. [`Root::key`] is that text
//! with any password removed, never rebuilt from the parsed pieces.

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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Root {
    /// The root exactly as it was written, minus any password. `.cvspass` uses this as its key.
    pub key: String,
    pub method: Method,
    pub user: String,
    pub host: String,
    /// Zero when the method has no port (ext) or the root has none (local).
    pub port: u16,
    pub proxy: Option<(String, u16)>,
    /// The repository path. For a remote root it is the path on the server.
    pub path: String,
    /// The `-z` level for connections to this root. It is a connection setting, not part of
    /// the root text, so [`Root::parse`] always leaves it at zero.
    pub compression: u32,
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
                host: String::new(),
                port: 0,
                proxy: None,
                path: raw.to_string(),
                compression: 0,
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
                host: String::new(),
                port: 0,
                proxy: None,
                path: rest.to_string(),
                compression: 0,
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

        let (user, password, host_part) = match rest.rfind('@') {
            Some(at) => {
                let info = &rest[..at];
                match info.split_once(':') {
                    Some((user, password)) => (user.to_string(), Some(password), &rest[at + 1..]),
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
        let key = match password {
            Some(_) => {
                let at = raw.rfind('@').unwrap();
                let colon = raw[..at].rfind(':').unwrap();
                format!("{}{}", &raw[..colon], &raw[at..])
            }
            None => raw.to_string(),
        };

        Ok(Root {
            key,
            method,
            user,
            host: host.to_string(),
            port,
            proxy,
            path: path.to_string(),
            compression: 0,
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
    let Some(at) = raw.rfind('@') else {
        return raw.to_string();
    };
    let Some(colon) = raw[..at].rfind(':') else {
        return raw.to_string();
    };
    // Only scrub when the colon is inside the userinfo, not the one after the method.
    let method_end = raw
        .strip_prefix(':')
        .and_then(|s| s.find(':'))
        .map(|i| i + 2);
    if method_end.is_some_and(|start| colon < start) {
        return raw.to_string();
    }
    format!("{}:***{}", &raw[..colon], &raw[at..])
}

fn default_port(method: &Method) -> u16 {
    match method {
        Method::Ext | Method::Local => 0,
        _ => DEFAULT_PSERVER_PORT,
    }
}

/// Split `host[:port]:path` at the colon before the path.
///
/// The path may itself contain a colon (a Windows drive), so a colon followed by digits and
/// then another colon is read as a port; any other colon starts the path.
fn split_host_path(text: &str) -> Option<(&str, Option<&str>, &str)> {
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
        assert_eq!(root.key, ":pserver:ann@cvshost:/repo");
        assert_eq!(root.display(), ":pserver:ann@cvshost:/repo");
        assert!(!format!("{}", scrub(":pserver:ann:secret@cvshost:/repo")).contains("secret"));
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
