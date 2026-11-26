//! The password store CVSNT keeps on Windows.
//!
//! CVSNT ignores `.cvspass` and saves `<root> <scrambled password>` as string values under
//! `HKCU\Software\Cvsnt\cvspass`. The value is already the text the pserver handshake
//! carries. Reading it lets this client use a password the command-line CVSNT has saved,
//! which is what users of the Windows CVS tools expect.

/// The scrambled password CVSNT saved for `root`, if it has one.
#[cfg(windows)]
pub fn scrambled_for(root: &str) -> Option<Vec<u8>> {
    use winreg::enums::HKEY_CURRENT_USER;
    use winreg::RegKey;

    let key = RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey(REGISTRY_KEY)
        .ok()?;
    let mut values = Vec::new();
    for (name, _) in key.enum_values().flatten() {
        if let Ok(value) = key.get_value::<String, _>(&name) {
            values.push((name, value));
        }
    }
    from_values(values, root)
}

#[cfg(windows)]
const REGISTRY_KEY: &str = r"Software\Cvsnt\cvspass";

/// Nothing on platforms without the CVSNT registry.
#[cfg(not(windows))]
pub fn scrambled_for(_root: &str) -> Option<Vec<u8>> {
    None
}

/// Pick the value for `root`. CVSNT stores the scrambled password, so a value that does not
/// start with `A` is not one this client can send and is ignored.
#[cfg(any(windows, test))]
fn from_values(values: Vec<(String, String)>, root: &str) -> Option<Vec<u8>> {
    for (name, value) in values {
        if name != root {
            continue;
        }
        let bytes = value.into_bytes();
        if bytes.first() == Some(&b'A') {
            return Some(bytes);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn values() -> Vec<(String, String)> {
        vec![
            (":pserver:ann@host:/repo".to_string(), "AZdh d,".to_string()),
            (
                ":pserver:bob@host:/repo".to_string(),
                "not-scrambled".to_string(),
            ),
        ]
    }

    #[test]
    fn the_value_saved_for_the_exact_root_is_used() {
        assert_eq!(
            from_values(values(), ":pserver:ann@host:/repo").as_deref(),
            Some(&b"AZdh d,"[..])
        );
    }

    #[test]
    fn a_missing_or_malformed_value_is_ignored() {
        assert_eq!(from_values(values(), ":pserver:cat@host:/repo"), None);
        assert_eq!(from_values(values(), ":pserver:bob@host:/repo"), None);
    }
}
