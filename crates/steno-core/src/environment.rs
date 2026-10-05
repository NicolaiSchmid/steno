//! The process environment as Unicode text. `std::env::vars()` panics on
//! the first name or value that is not valid Unicode, which would stop the
//! app and the CLI from starting; every variable Steno reads (`HOME`,
//! `XDG_DATA_HOME`, `APPDATA`, `CODEX_HOME`, the `STENO_<KEY>` secret
//! overrides) is text, so [`process_environment`] skips the others.

use std::ffi::OsString;

/// Every variable of the process environment whose name and value are
/// valid Unicode, collected into a map of the caller's choice.
///
/// ```
/// use std::collections::HashMap;
///
/// let environment: HashMap<String, String> = steno_core::environment::process_environment();
/// assert_eq!(
///     environment.get("PATH").map(String::as_str),
///     std::env::var("PATH").ok().as_deref()
/// );
/// ```
#[must_use]
pub fn process_environment<C: FromIterator<(String, String)>>() -> C {
    unicode_pairs(std::env::vars_os()).collect()
}

/// The pairs of `variables` whose name and value are both valid Unicode.
fn unicode_pairs(
    variables: impl IntoIterator<Item = (OsString, OsString)>,
) -> impl Iterator<Item = (String, String)> {
    variables
        .into_iter()
        .filter_map(|(name, value)| Some((name.into_string().ok()?, value.into_string().ok()?)))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::ffi::OsString;

    use super::unicode_pairs;

    /// Text no platform reads as Unicode: a lone continuation byte on
    /// Unix, an unpaired surrogate on Windows.
    fn not_unicode() -> OsString {
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStringExt;
            OsString::from_vec(vec![b'a', 0x80])
        }
        #[cfg(windows)]
        {
            use std::os::windows::ffi::OsStringExt;
            OsString::from_wide(&[u16::from(b'a'), 0xD800])
        }
    }

    #[test]
    fn a_name_or_value_that_is_not_unicode_is_skipped() {
        let variables = vec![
            (OsString::from("HOME"), OsString::from("/home/ada")),
            (not_unicode(), OsString::from("a value")),
            (OsString::from("STENO_LLM_API_KEY"), not_unicode()),
            (OsString::from("CODEX_HOME"), OsString::from("/codex")),
        ];
        let kept: BTreeMap<String, String> = unicode_pairs(variables).collect();
        assert_eq!(
            kept,
            BTreeMap::from([
                ("CODEX_HOME".to_owned(), "/codex".to_owned()),
                ("HOME".to_owned(), "/home/ada".to_owned()),
            ])
        );
    }
}
