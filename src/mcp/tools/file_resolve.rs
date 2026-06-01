//! File resolution helpers for attachment tools.
//!
//! The user often pastes a screenshot into the CLI which lands in a
//! session-managed attachments dir, or refers to a file by basename. The
//! tool gets a path that may not literally exist (assistant guessed
//! `~/Desktop/<basename>`). To keep UX smooth without ever silently picking
//! the wrong file, this module:
//!
//!   1. Expands `~` (Unix `~/`, Windows `~\` and `~/`) and `$VAR`/`%VAR%`
//!      style env refs.
//!   2. If the resolved path exists, reads it.
//!   3. If not, searches a curated, cross-platform list of likely
//!      directories (via the `dirs` crate so we honor XDG / Windows
//!      known-folder GUIDs / macOS conventions) for files matching the
//!      basename (exact or fuzzy on stem).
//!
//! We never silently substitute a different file — wrong attachments on a
//! support ticket are a privacy hazard.

use std::path::{Path, PathBuf};

use crate::error::{AppError, AppResult};

/// Read a file from `path`, with `~` / env expansion + fuzzy search on miss.
/// Returns the bytes on success. On failure, returns an `AppError::Validation`
/// whose message lists searched directories and any near-match candidates.
pub fn read_user_file(path: &str) -> AppResult<Vec<u8>> {
    let expanded = expand(path);
    if expanded.exists() {
        return std::fs::read(&expanded).map_err(|e| {
            AppError::Validation(format!("cannot read `{}`: {e}", expanded.display()))
        });
    }

    let basename = Path::new(path)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(path)
        .to_string();
    let (searched, hits) = scan_candidates(&basename);

    let mut msg = format!("cannot read `{}`: file not found.", expanded.display());
    if !hits.is_empty() {
        msg.push_str(" Found possible matches by basename:\n");
        for h in hits.iter().take(8) {
            msg.push_str(&format!("  - {}\n", h.display()));
        }
        msg.push_str("Re-call with one of these explicit paths (or the user's actual file).");
    } else {
        msg.push_str(&format!(
            " Searched: {}. No files matching `{}` were found. Ask the user for the actual path (drag-drop into the prompt, or `ls`).",
            searched
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join(", "),
            basename
        ));
    }
    Err(AppError::Validation(msg))
}

/// Cross-platform tilde + env expansion.
///
/// Handles:
///  - `~/foo`  (Unix + Windows)
///  - `~\foo`  (Windows)
///  - `$VAR`, `${VAR}` (Unix-style; works on any platform)
///  - `%VAR%`  (Windows-style; works on any platform)
fn expand(p: &str) -> PathBuf {
    let with_env = expand_env(p);
    let s: &str = with_env.as_ref();

    // Tilde expansion — accept both separators so Windows users typing
    // `~/Desktop/foo.png` or `~\Desktop\foo.png` both work.
    if s == "~" {
        if let Some(home) = dirs::home_dir() {
            return home;
        }
    } else if let Some(rest) = s.strip_prefix("~/").or_else(|| s.strip_prefix("~\\")) {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest);
        }
    }
    PathBuf::from(s)
}

/// Substitute `$VAR`, `${VAR}`, and `%VAR%` references. Missing vars become "".
/// `%VAR%` is supported on all platforms so the same tool args work whether
/// the assistant generated a Unix-style or Windows-style argument.
fn expand_env(input: &str) -> std::borrow::Cow<'_, str> {
    if !input.contains('$') && !input.contains('%') {
        return std::borrow::Cow::Borrowed(input);
    }
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '$' => match chars.peek() {
                Some('{') => {
                    chars.next();
                    let mut name = String::new();
                    for nc in chars.by_ref() {
                        if nc == '}' {
                            break;
                        }
                        name.push(nc);
                    }
                    out.push_str(&std::env::var(&name).unwrap_or_default());
                }
                Some(c) if c.is_ascii_alphabetic() || *c == '_' => {
                    let mut name = String::new();
                    while let Some(c) = chars.peek() {
                        if c.is_ascii_alphanumeric() || *c == '_' {
                            name.push(*c);
                            chars.next();
                        } else {
                            break;
                        }
                    }
                    out.push_str(&std::env::var(&name).unwrap_or_default());
                }
                _ => out.push('$'),
            },
            '%' => {
                // `%VAR%` — only consume if we find a closing `%` with only
                // VAR-name chars in between, otherwise emit literal `%`.
                let snapshot: String = chars.clone().collect();
                if let Some(end) = snapshot.find('%') {
                    let name = &snapshot[..end];
                    if !name.is_empty()
                        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                    {
                        for _ in 0..=end {
                            chars.next();
                        }
                        out.push_str(&std::env::var(name).unwrap_or_default());
                        continue;
                    }
                }
                out.push('%');
            }
            _ => out.push(c),
        }
    }
    std::borrow::Cow::Owned(out)
}

/// Returns (searched_dirs, candidate_paths). Candidates are files whose
/// basename equals `basename` (case-insensitive) OR contains the stem
/// (e.g. user typed `Screenshot 8.33.10` matches
/// `Screenshot 2026-05-28 at 8.33.10 PM.png`).
///
/// Searches platform-appropriate user dirs via the `dirs` crate so this
/// works on macOS, Linux (XDG), and Windows (Known Folders).
fn scan_candidates(basename: &str) -> (Vec<PathBuf>, Vec<PathBuf>) {
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Some(d) = dirs::desktop_dir() {
        dirs.push(d);
    }
    if let Some(d) = dirs::download_dir() {
        dirs.push(d);
    }
    if let Some(d) = dirs::picture_dir() {
        // Common screenshot subdir on multiple platforms.
        dirs.push(d.join("Screenshots"));
        dirs.push(d);
    }
    if let Some(d) = dirs::document_dir() {
        dirs.push(d);
    }
    if let Ok(cwd) = std::env::current_dir() {
        dirs.push(cwd);
    }
    // Copilot CLI / generic agent attachment dirs, if discoverable from env.
    for v in &[
        "COPILOT_SESSION_DIR",
        "COPILOT_SESSION_STATE",
        "COPILOT_ATTACHMENTS_DIR",
    ] {
        if let Some(p) = std::env::var_os(v) {
            let p = PathBuf::from(p);
            // Some hosts point at the session root rather than the
            // attachments subdir — try both.
            dirs.push(p.join("attachments"));
            dirs.push(p.join("files"));
            dirs.push(p);
        }
    }
    // System temp dir is where pasted clipboard images often land too.
    dirs.push(std::env::temp_dir());

    // Filter to actual existing directories; preserve order, dedupe.
    let mut seen = std::collections::HashSet::new();
    dirs.retain(|d| d.is_dir() && seen.insert(d.clone()));

    let lower = basename.to_lowercase();
    let stem_lower = Path::new(basename)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(basename)
        .to_lowercase();
    // Token-AND match: split on whitespace + common separators and require
    // every non-trivial token to appear (case-insensitive) in the candidate's
    // name. So "Screenshot 8.33.10" matches "Screenshot 2026-05-28 at 8.33.10 PM.png".
    let tokens: Vec<String> = stem_lower
        .split(|c: char| c.is_whitespace() || c == '_' || c == '-')
        .filter(|t| t.len() >= 2)
        .map(|t| t.to_string())
        .collect();

    let mut hits: Vec<PathBuf> = Vec::new();
    for d in &dirs {
        let Ok(read) = std::fs::read_dir(d) else {
            continue;
        };
        for entry in read.flatten() {
            let p = entry.path();
            if !p.is_file() {
                continue;
            }
            let Some(name) = p.file_name().and_then(|s| s.to_str()) else {
                continue;
            };
            let nl = name.to_lowercase();
            let matches = nl == lower
                || (!stem_lower.is_empty() && nl.contains(&stem_lower))
                || (!tokens.is_empty() && tokens.iter().all(|t| nl.contains(t)));
            if matches {
                hits.push(p);
            }
        }
    }
    hits.sort();
    hits.dedup();
    (dirs, hits)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn expands_tilde_forward_slash() {
        let p = expand("~/foo/bar.txt");
        assert!(p.is_absolute(), "expected absolute, got {p:?}");
        assert!(p.ends_with("foo/bar.txt") || p.ends_with("foo\\bar.txt"));
    }

    #[test]
    fn expands_tilde_back_slash() {
        // Windows-style separator — should still expand on any platform.
        let p = expand("~\\foo\\bar.txt");
        assert!(p.is_absolute(), "expected absolute, got {p:?}");
    }

    #[test]
    fn expands_dollar_var() {
        std::env::set_var("MCP_TEST_DIR_VAR", "/abc");
        let p = expand("$MCP_TEST_DIR_VAR/x");
        assert_eq!(p, PathBuf::from("/abc/x"));
        let p2 = expand("${MCP_TEST_DIR_VAR}/y");
        assert_eq!(p2, PathBuf::from("/abc/y"));
    }

    #[test]
    fn expands_percent_var_cross_platform() {
        std::env::set_var("MCP_TEST_PERCENT_VAR", "/abc");
        let p = expand("%MCP_TEST_PERCENT_VAR%/x");
        assert_eq!(p, PathBuf::from("/abc/x"));
    }

    #[test]
    fn lone_percent_signs_are_preserved() {
        let p = expand("50%-discount/file.png");
        assert_eq!(p, PathBuf::from("50%-discount/file.png"));
    }

    #[test]
    fn missing_file_returns_helpful_error_with_candidates() {
        let tmp = tempfile::tempdir().unwrap();
        let f = tmp.path().join("Screenshot 2026-05-28 at 8.33.10 PM.png");
        let mut w = std::fs::File::create(&f).unwrap();
        w.write_all(b"x").unwrap();
        std::env::set_var("COPILOT_ATTACHMENTS_DIR", tmp.path());

        let err = read_user_file("/nope/Screenshot 8.33.10.png").unwrap_err();
        let s = err.to_string();
        assert!(
            s.contains("Screenshot 2026-05-28 at 8.33.10 PM.png"),
            "expected candidate match in error, got: {s}"
        );
    }

    #[test]
    fn reads_existing_file_directly() {
        let tmp = tempfile::tempdir().unwrap();
        let f = tmp.path().join("hello.bin");
        std::fs::write(&f, b"hello").unwrap();
        let bytes = read_user_file(f.to_str().unwrap()).unwrap();
        assert_eq!(bytes, b"hello");
    }
}
