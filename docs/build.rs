//! Copies the Markdown into `OUT_DIR` with its relative links rewritten for rustdoc.
//!
//! The Markdown links to other files (`08-errors.md#domain-error`) so that it reads well on its
//! own. In `cargo doc` every file is a module, so those links become intra-doc links
//! (`crate::guide::errors#domain-error`), which rustdoc resolves and checks. Links to files outside
//! the documentation, such as `../examples/todo/src/lib.rs`, keep their text and lose the link.

use std::path::{Component, Path, PathBuf};
use std::{env, fs};

const DIRS: &[&str] = &["guide", "api", "adr"];

fn main() {
    let docs = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let out = PathBuf::from(env::var("OUT_DIR").unwrap());

    let mut files = vec![PathBuf::from("README.md"), PathBuf::from("design.md")];
    for dir in DIRS {
        println!("cargo::rerun-if-changed={dir}");
        let mut names: Vec<_> = fs::read_dir(docs.join(dir))
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .filter(|name| name.ends_with(".md"))
            .collect();
        names.sort();
        files.extend(names.into_iter().map(|name| Path::new(dir).join(name)));
    }

    for file in &files {
        println!("cargo::rerun-if-changed={}", file.display());
        let markdown = fs::read_to_string(docs.join(file)).unwrap();
        let target = out.join(file);
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::write(target, rewrite(&markdown, file, &files)).unwrap();
    }
}

/// The module that renders `file`: `guide/08-errors.md` → `crate::guide::errors`.
fn module_path(file: &Path) -> String {
    let mut path = String::from("crate");
    for component in file.parent().unwrap().components() {
        path.push_str("::");
        path.push_str(component.as_os_str().to_str().unwrap());
    }
    let stem = file.file_stem().unwrap().to_str().unwrap();
    if stem != "README" {
        let name = stem
            .trim_start_matches(|c: char| c.is_ascii_digit())
            .trim_start_matches('-');
        path.push_str("::");
        path.push_str(&name.replace('-', "_"));
    }
    path
}

/// `link` resolved against the directory of `from`, without `..` or `.`.
fn resolve(from: &Path, link: &str) -> PathBuf {
    let mut path = PathBuf::new();
    for component in from.parent().unwrap().join(link).components() {
        match component {
            Component::ParentDir => {
                if !path.pop() {
                    path.push("..");
                }
            }
            Component::CurDir => {}
            other => path.push(other),
        }
    }
    path
}

fn rewrite(markdown: &str, file: &Path, files: &[PathBuf]) -> String {
    let mut out = String::with_capacity(markdown.len());
    let mut in_code = false;
    for line in markdown.split_inclusive('\n') {
        if line.trim_start().starts_with("```") {
            in_code = !in_code;
        }
        if in_code {
            out.push_str(line);
        } else {
            out.push_str(&rewrite_line(line, file, files));
        }
    }
    out
}

fn rewrite_line(line: &str, file: &Path, files: &[PathBuf]) -> String {
    let mut out = String::new();
    let mut rest = line;
    while let Some(at) = rest.find("](") {
        let Some(len) = rest[at + 2..].find(')') else {
            break;
        };
        let target = &rest[at + 2..at + 2 + len];
        let (before, after) = (&rest[..at], &rest[at + 2 + len + 1..]);
        rest = after;

        // Same-page anchors, URLs and links that already are intra-doc links stay as they are.
        let keep = target.starts_with('#') || target.contains(':');
        let (path, fragment) = target
            .split_once('#')
            .map_or((target, None), |(p, f)| (p, Some(f)));
        if keep {
            out.push_str(&format!("{before}]({target})"));
            continue;
        }
        let resolved = resolve(file, path);
        if files.contains(&resolved) {
            let fragment = fragment.map(|f| format!("#{f}")).unwrap_or_default();
            out.push_str(&format!("{before}]({}{fragment})", module_path(&resolved)));
        } else {
            // Outside the docs: keep `[text]` as plain text.
            let open = before.rfind('[').expect("link text starts with `[`");
            out.push_str(&before[..open]);
            out.push_str(&before[open + 1..]);
        }
    }
    out.push_str(rest);
    out
}
