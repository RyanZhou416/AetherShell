//! Render ordinary Windows disk paths without a verbatim prefix when Win32 preserves their meaning.
//! Filesystem operations keep their canonical paths; this helper only changes returned path text.

use std::path::Path;

/// Render a path for callers, retaining verbatim syntax for long or Win32-special names.
pub fn display_path(path: &Path) -> String {
    let text = path.to_string_lossy();
    #[cfg(windows)]
    {
        normal_path(&text).unwrap_or_else(|| text.into_owned())
    }
    #[cfg(not(windows))]
    {
        text.into_owned()
    }
}

#[cfg(any(windows, test))]
fn normal_path(text: &str) -> Option<String> {
    let rest = text.strip_prefix(r"\\?\")?;
    let ordinary = if let Some(unc) = rest.strip_prefix(r"UNC\") {
        let mut parts = unc.split('\\');
        if [parts.next()?, parts.next()?]
            .into_iter()
            .any(|name| name.is_empty() || win32_special(name))
        {
            return None;
        }
        format!(r"\\{unc}")
    } else {
        let bytes = rest.as_bytes();
        if bytes.len() < 3
            || !bytes[0].is_ascii_alphabetic()
            || bytes[1] != b':'
            || bytes[2] != b'\\'
        {
            return None;
        }
        rest.to_owned()
    };
    // MAX_PATH includes a NUL. Keep extra headroom for directory APIs that append a child name.
    if ordinary.encode_utf16().count() >= 248 {
        return None;
    }
    let names = if ordinary.starts_with(r"\\") {
        ordinary
            .trim_start_matches('\\')
            .split('\\')
            .skip(2)
            .collect::<Vec<_>>()
    } else {
        ordinary[3..].split('\\').collect::<Vec<_>>()
    };
    if names.into_iter().any(win32_special) {
        return None;
    }
    Some(ordinary)
}

#[cfg(any(windows, test))]
fn win32_special(name: &str) -> bool {
    if name == "."
        || name == ".."
        || name.ends_with(['.', ' '])
        || name.contains(['/', ':'])
        || name
            .chars()
            .any(|c| c < '\u{20}' || matches!(c, '<' | '>' | '"' | '|' | '?' | '*'))
    {
        return true;
    }
    let stem = name
        .split('.')
        .next()
        .unwrap_or("")
        .trim_end_matches(' ')
        .to_ascii_uppercase();
    matches!(
        stem.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
    ) || stem
        .strip_prefix("COM")
        .or_else(|| stem.strip_prefix("LPT"))
        .is_some_and(|suffix| {
            matches!(
                suffix,
                "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordinary_disk_and_unc_paths_are_readable() {
        assert_eq!(
            normal_path(r"\\?\C:\work\中文.txt").as_deref(),
            Some(r"C:\work\中文.txt")
        );
        assert_eq!(
            normal_path(r"\\?\UNC\server\share\folder\file.txt").as_deref(),
            Some(r"\\server\share\folder\file.txt")
        );
    }

    #[test]
    fn special_names_devices_and_long_paths_keep_their_spelling() {
        for path in [
            r"\\?\C:\x.\file",
            r"\\?\C:\x \file",
            r"\\?\C:\NUL.txt",
            r"\\?\C:\COM¹.txt",
            r"\\?\C:\a\..\b",
            r"\\?\Volume{01234567}\file",
            r"\\.\pipe\service",
            r"\\?\C:\x:stream",
            r"\\?\UNC\server",
        ] {
            assert_eq!(display_path(Path::new(path)), path);
        }
        let path = format!(r"\\?\C:\{}", "x".repeat(260));
        assert_eq!(display_path(Path::new(&path)), path);
    }

    #[test]
    fn ordinary_and_non_windows_paths_are_unchanged() {
        for path in [
            r"C:\work\file",
            r"\\server\share\file",
            "/tmp/中文",
            "relative/file",
        ] {
            assert_eq!(display_path(Path::new(path)), path);
        }
    }
}
