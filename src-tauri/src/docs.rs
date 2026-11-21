//! Read-only access to a workspace's project folder: lists its Markdown files and reads
//! them back. Nothing here writes to disk. Paths are always relative to the project root,
//! and anything that would leave the root is refused.

use std::path::{Component, Path, PathBuf};

use serde::Serialize;

/// Largest document the board will return, to keep responses small.
pub const MAX_DOC_BYTES: u64 = 1024 * 1024;
const MAX_FILES: usize = 2000;
const MAX_DEPTH: usize = 8;

/// Folders that never hold project documentation worth listing.
const SKIPPED_DIRS: &[&str] = &["node_modules", "target", "dist", "release", "backups", "vendor"];

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct DocEntry {
    /// Relative to the project root, with forward slashes.
    pub path: String,
    /// The first Markdown heading, or the file name if there is none.
    pub title: String,
    pub bytes: u64,
    /// Last change, in milliseconds since the Unix epoch. The page compares it to see what changed.
    pub modified: u64,
}

/// Turns a client-supplied relative path into a safe one. Rejects absolute paths, drive
/// prefixes, `..`, and anything that is not a `.md` file.
pub fn validate_relative(relative: &str) -> Result<PathBuf, String> {
    let path = Path::new(relative);
    if relative.is_empty() || path.is_absolute() {
        return Err("path must be relative to the project folder".into());
    }
    let mut clean = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => clean.push(part),
            Component::CurDir => {}
            _ => return Err("path must stay inside the project folder".into()),
        }
    }
    let is_document = clean
        .extension()
        .and_then(|e| e.to_str())
        .map(is_document_extension)
        .unwrap_or(false);
    if !is_document {
        return Err("only .md, .html, and .htm files can be read".into());
    }
    Ok(clean)
}

/// The project folder must exist and be a directory. Stored as an absolute path.
pub fn validate_root(path: &str) -> Result<String, String> {
    let root = Path::new(path)
        .canonicalize()
        .map_err(|_| format!("folder not found: {path}"))?;
    if !root.is_dir() {
        return Err(format!("not a folder: {path}"));
    }
    // Windows reports canonical paths with a \? prefix. People should see the ordinary form.
    let mut text = root.to_string_lossy().into_owned();
    if let Some(stripped) = text.strip_prefix(r"\\?\") {
        text = stripped.to_string();
    }
    Ok(text)
}

/// The documents the board lists: Markdown, and HTML pages an agent generated.
pub fn is_document_extension(ext: &str) -> bool {
    ["md", "html", "htm"].iter().any(|known| ext.eq_ignore_ascii_case(known))
}

/// A readable title for an HTML page: its <title>, else its first <h1>.
fn html_title(text: &str) -> Option<String> {
    let lower = text.to_ascii_lowercase();
    for (open, close) in [("<title", "</title>"), ("<h1", "</h1>")] {
        let Some(start) = lower.find(open) else { continue };
        let Some(gt) = lower[start..].find('>') else { continue };
        let body_start = start + gt + 1;
        let Some(end_rel) = lower[body_start..].find(close) else { continue };
        let title = strip_tags(&text[body_start..body_start + end_rel]);
        if !title.is_empty() {
            return Some(title);
        }
    }
    None
}

/// Removes markup from a short piece of HTML and collapses its whitespace.
fn strip_tags(fragment: &str) -> String {
    let mut out = String::new();
    let mut in_tag = false;
    for ch in fragment.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(ch),
            _ => {}
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn first_heading(text: &str) -> Option<String> {
    text.lines()
        .map(str::trim)
        .find(|line| line.starts_with("# "))
        .map(|line| line.trim_start_matches('#').trim().to_string())
        .filter(|title| !title.is_empty())
}

/// Lists Markdown files under `root`, sorted by path.
pub fn scan(root: &Path) -> Result<Vec<DocEntry>, String> {
    let mut found = Vec::new();
    let mut stack = vec![(root.to_path_buf(), 0usize)];
    while let Some((dir, depth)) = stack.pop() {
        let entries = std::fs::read_dir(&dir).map_err(|e| format!("could not read {}: {e}", dir.display()))?;
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            let Ok(file_type) = entry.file_type() else { continue };
            // Symbolic links are not followed, so nothing outside the root can be reached.
            if file_type.is_symlink() {
                continue;
            }
            if file_type.is_dir() {
                if depth < MAX_DEPTH && !name.starts_with('.') && !SKIPPED_DIRS.contains(&name.as_str()) {
                    stack.push((path, depth + 1));
                }
                continue;
            }
            let is_document = path
                .extension()
                .and_then(|e| e.to_str())
                .map(is_document_extension)
                .unwrap_or(false);
            if !file_type.is_file() || !is_document {
                continue;
            }
            let metadata = entry.metadata().ok();
            let bytes = metadata.as_ref().map(|m| m.len()).unwrap_or(0);
            let modified = metadata
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0);
            let is_html = path
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| !e.eq_ignore_ascii_case("md"))
                .unwrap_or(false);
            let title = std::fs::read_to_string(&path)
                .ok()
                .and_then(|text| if is_html { html_title(&text) } else { first_heading(&text) })
                .unwrap_or_else(|| name.clone());
            let relative = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join("/");
            found.push(DocEntry { path: relative, title, bytes, modified });
            if found.len() >= MAX_FILES {
                break;
            }
        }
    }
    found.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(found)
}

/// Reads one document. The resolved file must stay inside `root`, even through links.
pub fn read(root: &Path, relative: &str) -> Result<String, String> {
    let clean = validate_relative(relative)?;
    let root = root.canonicalize().map_err(|e| e.to_string())?;
    let full = root.join(&clean).canonicalize().map_err(|_| format!("not found: {relative}"))?;
    if !full.starts_with(&root) {
        return Err("path must stay inside the project folder".into());
    }
    let size = std::fs::metadata(&full).map_err(|e| e.to_string())?.len();
    if size > MAX_DOC_BYTES {
        return Err(format!("{relative} is larger than 1 MB"));
    }
    std::fs::read_to_string(&full).map_err(|e| format!("could not read {relative}: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_root(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("clankban-docs-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("docs")).unwrap();
        std::fs::create_dir_all(dir.join("node_modules/pkg")).unwrap();
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        std::fs::write(dir.join("README.md"), "# Sample project\n\nHello.").unwrap();
        std::fs::write(dir.join("docs/ARCHITECTURE.md"), "Design notes without a heading").unwrap();
        std::fs::write(dir.join("docs/notes.txt"), "not markdown").unwrap();
        std::fs::write(dir.join("node_modules/pkg/readme.md"), "# dependency").unwrap();
        std::fs::write(dir.join(".git/HEAD.md"), "# git").unwrap();
        dir
    }

    #[test]
    fn rejects_paths_that_leave_the_root() {
        assert!(validate_relative("../secret.md").is_err());
        assert!(validate_relative("docs/../../secret.md").is_err());
        assert!(validate_relative("/etc/passwd.md").is_err());
        assert!(validate_relative("").is_err());
    }

    #[test]
    fn only_markdown_and_html_are_readable() {
        assert!(validate_relative("docs/TASKS.md").is_ok());
        assert!(validate_relative("reports/summary.html").is_ok());
        assert!(validate_relative("reports/summary.HTM").is_ok());
        assert!(validate_relative("docs/notes.txt").is_err());
        assert!(validate_relative("main.rs").is_err());
        assert!(validate_relative("page.js").is_err());
    }

    #[test]
    fn html_pages_are_listed_with_their_title() {
        let root = sample_root("html");
        std::fs::write(
            root.join("docs/report.html"),
            "<!doctype html><html><head><title>  Weekly   <em>report</em></title></head><body><h1>Heading</h1></body></html>",
        )
        .unwrap();
        std::fs::write(root.join("docs/plain.html"), "<h1>Only a heading</h1>").unwrap();
        std::fs::write(root.join("docs/bare.htm"), "<p>No title here</p>").unwrap();
        let docs = scan(&root).unwrap();
        let find = |p: &str| docs.iter().find(|d| d.path == p).map(|d| d.title.clone());
        assert_eq!(find("docs/report.html").as_deref(), Some("Weekly report"));
        assert_eq!(find("docs/plain.html").as_deref(), Some("Only a heading"));
        assert_eq!(find("docs/bare.htm").as_deref(), Some("bare.htm"));
        assert!(read(&root, "docs/report.html").unwrap().contains("<h1>Heading</h1>"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn scan_lists_markdown_and_skips_build_and_hidden_folders() {
        let root = sample_root("scan");
        let docs = scan(&root).unwrap();
        let paths: Vec<&str> = docs.iter().map(|d| d.path.as_str()).collect();
        assert_eq!(paths, ["README.md", "docs/ARCHITECTURE.md"]);
        assert_eq!(docs[0].title, "Sample project");
        assert_eq!(docs[1].title, "ARCHITECTURE.md");
        assert!(docs.iter().all(|d| d.modified > 0), "every entry needs a modified time");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn read_returns_the_file_and_refuses_escapes() {
        let root = sample_root("read");
        assert!(read(&root, "README.md").unwrap().contains("Hello"));
        assert!(read(&root, "../outside.md").is_err());
        assert!(read(&root, "docs/missing.md").is_err());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn root_must_be_an_existing_folder() {
        let root = sample_root("root");
        let saved = validate_root(root.to_str().unwrap()).unwrap();
        assert!(!saved.starts_with(r"\\?\"), "stored path should not carry the verbatim prefix: {saved}");
        assert!(validate_root(root.join("README.md").to_str().unwrap()).is_err());
        assert!(validate_root(root.join("nope").to_str().unwrap()).is_err());
        let _ = std::fs::remove_dir_all(&root);
    }
}
