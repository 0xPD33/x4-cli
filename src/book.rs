use std::path::Path;

use epub::doc::EpubDoc;

pub const FORMATS: &[&str] = &["epub", "txt", "xtc", "xtch", "bmp"];

pub fn is_book(path: &Path) -> bool {
    path.extension()
        .is_some_and(|e| FORMATS.contains(&e.to_string_lossy().to_lowercase().as_str()))
}

/// A local folder name made safe for the card, with the same space rule as books.
pub fn folder_name(name: &str, no_spaces: bool) -> String {
    clean(name, no_spaces)
}

fn clean(s: &str, no_spaces: bool) -> String {
    let s = sanitize(s);
    if no_spaces {
        s.replace(" - ", "-").replace(' ', "_")
    } else {
        s
    }
}

/// Name to store on the card: `Author - Title.epub` from the OPF, else the local name.
/// Always sanitized: the CrossPoint upload protocol splits on ':' and dotfiles are hidden.
/// `no_spaces` writes `Frank_Herbert-Dune.epub` instead of `Frank Herbert - Dune.epub`.
pub fn target_name(path: &Path, raw: bool, no_spaces: bool) -> String {
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().into_owned())
        .unwrap_or_default();
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let base = match ext.eq_ignore_ascii_case("epub") && !raw {
        true => epub_name(path).unwrap_or(stem),
        false => stem,
    };
    format!("{}.{ext}", clean(&base, no_spaces))
}

fn epub_name(path: &Path) -> Option<String> {
    let doc = EpubDoc::new(path).ok()?;
    let title = doc.mdata("title")?.value.trim().to_string();
    if title.is_empty() {
        return None;
    }
    Some(
        match doc
            .mdata("creator")
            .map(|c| c.value.trim())
            .filter(|a| !a.is_empty())
        {
            Some(author) => format!("{author} - {title}"),
            None => title,
        },
    )
}

/// FAT-safe stem: drops `\/:*?"<>|` and control chars, collapses spaces, caps at 100 chars.
pub fn sanitize(s: &str) -> String {
    let cleaned: String = s
        .chars()
        .map(|c| {
            if c.is_control() || "\\/:*?\"<>|".contains(c) {
                ' '
            } else {
                c
            }
        })
        .collect();
    let words = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    let capped: String = words.chars().take(100).collect();
    let trimmed = capped.trim_matches(|c| c == '.' || c == ' ');
    if trimmed.is_empty() {
        "book".into()
    } else {
        trimmed.into()
    }
}
