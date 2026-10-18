//! Keeps the Markdown documentation honest.
//!
//! Inline Rust blocks are doctests (see `src/lib.rs`). Blocks that show code from a real file are
//! marked with `<!-- snippet: path#anchor -->` on the line before the fence; the region between
//! `// ANCHOR: anchor` and `// ANCHOR_END: anchor` in `path` (relative to `docs/`) must match the
//! block exactly. Run with `HORNERO_UPDATE_SNIPPETS=1` to rewrite the blocks from their sources.
//!
//! A `rust,ignore` block is compiled by nothing, so outside `api/` (whose bare signatures are
//! fine) it must either be a snippet or carry a `<!-- ignore: reason -->` marker on the line
//! before it that says why it cannot be compiled.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

fn docs_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn markdown_files() -> Vec<PathBuf> {
    let root = docs_dir();
    let mut files = vec![root.join("README.md"), root.join("../README.md")];
    for dir in ["guide", "api", "adr"] {
        let mut entries: Vec<PathBuf> = fs::read_dir(root.join(dir))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "md"))
            .collect();
        entries.sort();
        files.extend(entries);
    }
    files.push(root.join("design.md"));
    files
}

fn display(path: &Path) -> String {
    path.strip_prefix(docs_dir().join(".."))
        .unwrap_or(path)
        .display()
        .to_string()
}

/// A fenced code block: the fence's info string, the line range of its body, and the
/// `<!-- snippet: ... -->` or `<!-- ignore: ... -->` marker before it, if any.
struct Block {
    info: String,
    open: usize,
    close: usize,
    snippet: Option<String>,
    ignore_reason: Option<String>,
}

/// The text of `<!-- {kind}: text -->` on the nearest non-blank line before `lines[index]`.
fn marker_before(lines: &[&str], index: usize, kind: &str) -> Option<String> {
    lines[..index]
        .iter()
        .rev()
        .find(|line| !line.trim().is_empty())
        .and_then(|line| line.trim().strip_prefix("<!--"))
        .and_then(|rest| rest.strip_suffix("-->"))
        .and_then(|rest| rest.trim().strip_prefix(kind))
        .and_then(|rest| rest.strip_prefix(':'))
        .map(|text| text.trim().to_owned())
}

fn blocks(markdown: &str) -> Vec<Block> {
    let lines: Vec<&str> = markdown.lines().collect();
    let mut blocks = Vec::new();
    let mut index = 0;
    while index < lines.len() {
        let line = lines[index].trim_start();
        if let Some(info) = line.strip_prefix("```") {
            let snippet = marker_before(&lines, index, "snippet");
            let ignore_reason = marker_before(&lines, index, "ignore");
            let close = (index + 1..lines.len())
                .find(|&i| lines[i].trim_start().starts_with("```"))
                .unwrap_or_else(|| panic!("unclosed code fence at line {}", index + 1));
            blocks.push(Block {
                info: info.trim().to_owned(),
                open: index,
                close,
                snippet,
                ignore_reason,
            });
            index = close + 1;
        } else {
            index += 1;
        }
    }
    blocks
}

fn is_anchor_line(line: &str) -> bool {
    line.contains("ANCHOR:") || line.contains("ANCHOR_END:")
}

/// The text a `<!-- snippet: path#anchor -->` marker refers to, dedented.
fn extract(spec: &str) -> String {
    let (path, anchor) = match spec.split_once('#') {
        Some((path, anchor)) => (path, Some(anchor)),
        None => (spec, None),
    };
    let source = fs::read_to_string(docs_dir().join(path))
        .unwrap_or_else(|err| panic!("snippet source `{path}` cannot be read: {err}"));
    let lines: Vec<&str> = source.lines().collect();
    let region: Vec<&str> = match anchor {
        None => lines
            .into_iter()
            .filter(|line| !is_anchor_line(line))
            .collect(),
        Some(anchor) => {
            let start = lines
                .iter()
                .position(|line| line.trim() == format!("// ANCHOR: {anchor}"))
                .unwrap_or_else(|| panic!("`{path}` has no `// ANCHOR: {anchor}`"));
            let end = lines
                .iter()
                .position(|line| line.trim() == format!("// ANCHOR_END: {anchor}"))
                .unwrap_or_else(|| panic!("`{path}` has no `// ANCHOR_END: {anchor}`"));
            lines[start + 1..end]
                .iter()
                .copied()
                .filter(|line| !is_anchor_line(line))
                .collect()
        }
    };
    let indent = region
        .iter()
        .filter(|line| !line.trim().is_empty())
        .map(|line| line.len() - line.trim_start().len())
        .min()
        .unwrap_or(0);
    let mut out: Vec<&str> = region
        .iter()
        .map(|line| {
            if line.len() >= indent {
                &line[indent..]
            } else {
                line.trim_start()
            }
        })
        .collect();
    while out.first().is_some_and(|line| line.trim().is_empty()) {
        out.remove(0);
    }
    while out.last().is_some_and(|line| line.trim().is_empty()) {
        out.pop();
    }
    out.join("\n")
}

#[test]
fn snippets_match_their_sources() {
    let update = std::env::var_os("HORNERO_UPDATE_SNIPPETS").is_some();
    let mut stale = Vec::new();
    for file in markdown_files() {
        let markdown = fs::read_to_string(&file).unwrap();
        let mut lines: Vec<String> = markdown.lines().map(str::to_owned).collect();
        // Replace from the bottom up so earlier line numbers stay valid.
        for block in blocks(&markdown).into_iter().rev() {
            let Some(spec) = &block.snippet else { continue };
            let expected = extract(spec);
            let actual = lines[block.open + 1..block.close].join("\n");
            if actual != expected {
                stale.push(format!(
                    "{}:{} is out of date with `{spec}`; expected:\n{expected}\n",
                    display(&file),
                    block.open + 1
                ));
                lines.splice(
                    block.open + 1..block.close,
                    expected.lines().map(str::to_owned),
                );
            }
        }
        if update {
            let mut text = lines.join("\n");
            text.push('\n');
            if text != markdown {
                fs::write(&file, text).unwrap();
            }
        }
    }
    if !update {
        assert!(
            stale.is_empty(),
            "{}\nrun `HORNERO_UPDATE_SNIPPETS=1 cargo test -p hornero-docs --test markdown` to update",
            stale.join("\n")
        );
    }
}

#[test]
fn snippets_are_not_doctested_twice() {
    for file in markdown_files() {
        let markdown = fs::read_to_string(&file).unwrap();
        for block in blocks(&markdown) {
            if block.snippet.is_some() {
                assert!(
                    block.info.contains("ignore"),
                    "{}:{}: a snippet block is verified by its source file; mark it `rust,ignore`",
                    display(&file),
                    block.open + 1
                );
            }
        }
    }
}

#[test]
fn ignored_blocks_outside_the_api_reference_are_justified() {
    // `rust,ignore` compiles nothing and cannot drift-check itself. Outside `api/`, where bare
    // signatures are the point, such a block must be a snippet (checked against a real file) or
    // say in `<!-- ignore: reason -->` why it is a fragment that cannot be compiled.
    let api = docs_dir().join("api");
    for file in markdown_files() {
        if file.starts_with(&api) {
            continue;
        }
        let markdown = fs::read_to_string(&file).unwrap();
        for block in blocks(&markdown) {
            if !block.info.split(',').any(|word| word.trim() == "ignore") {
                continue;
            }
            if block.snippet.is_some() {
                continue;
            }
            let justified = block
                .ignore_reason
                .as_deref()
                .is_some_and(|reason| !reason.is_empty());
            assert!(
                justified,
                "{}:{}: an `ignore` block must be a `<!-- snippet: ... -->` block or have \
                 `<!-- ignore: reason -->` on the line before it (or be compiled: drop `ignore`)",
                display(&file),
                block.open + 1
            );
        }
    }
}

#[test]
fn ignore_markers_belong_to_ignored_blocks() {
    // A stale `<!-- ignore: ... -->` left on a block that is now compiled would be misleading.
    for file in markdown_files() {
        let markdown = fs::read_to_string(&file).unwrap();
        for block in blocks(&markdown) {
            if block.ignore_reason.is_some() {
                assert!(
                    block.info.split(',').any(|word| word.trim() == "ignore"),
                    "{}:{}: `<!-- ignore: ... -->` on a block that is not `ignore`; remove the marker",
                    display(&file),
                    block.open + 1
                );
            }
        }
    }
}

#[test]
fn code_blocks_declare_a_language() {
    // rustdoc treats a fence without an info string as a Rust doctest.
    for file in markdown_files() {
        let markdown = fs::read_to_string(&file).unwrap();
        for block in blocks(&markdown) {
            assert!(
                !block.info.is_empty(),
                "{}:{}: code fence without a language (use `text`, `sh`, `toml`, `rust`, ...)",
                display(&file),
                block.open + 1
            );
        }
    }
}

#[test]
fn doctests_with_a_main_have_no_statements_outside_it() {
    // rustdoc wraps a doctest that has top-level statements in its own `fn main`; a `main` written
    // in the doctest (often a hidden `# async fn main`) then never runs. rustdoc only warns.
    const STATEMENTS: &[&str] = &["let ", "assert", "println!", "Hornero::", "app."];
    for file in markdown_files() {
        let markdown = fs::read_to_string(&file).unwrap();
        let lines: Vec<&str> = markdown.lines().collect();
        for block in blocks(&markdown) {
            if !block.info.starts_with("rust") || block.info.contains("ignore") {
                continue;
            }
            let body = &lines[block.open + 1..block.close];
            let Some(main) = body.iter().position(|line| line.contains("fn main(")) else {
                continue;
            };
            for (offset, line) in body[..main].iter().enumerate() {
                let code = line.strip_prefix("# ").unwrap_or(line);
                assert!(
                    !STATEMENTS
                        .iter()
                        .any(|statement| code.starts_with(statement)),
                    "{}:{}: statement before `fn main` makes rustdoc skip the doctest's main",
                    display(&file),
                    block.open + 2 + offset
                );
            }
        }
    }
}

/// GitHub's heading anchors: lowercase, punctuation dropped, spaces turned into hyphens,
/// `-1`, `-2`, ... appended to repeated headings.
fn anchors(markdown: &str) -> Vec<String> {
    let fenced: Vec<(usize, usize)> = blocks(markdown)
        .iter()
        .map(|block| (block.open, block.close))
        .collect();
    let mut anchors: Vec<String> = Vec::new();
    let mut seen: HashMap<String, usize> = HashMap::new();
    for (number, line) in markdown.lines().enumerate() {
        if fenced
            .iter()
            .any(|&(open, close)| number >= open && number <= close)
        {
            continue;
        }
        let Some(text) = line.strip_prefix('#') else {
            continue;
        };
        let text = text.trim_start_matches('#');
        if !text.starts_with(' ') {
            continue;
        }
        // Keep the text of `[text](url)` links, drop inline markup.
        let mut plain = String::new();
        let mut rest = text.trim();
        while let Some(start) = rest.find('[') {
            plain.push_str(&rest[..start]);
            rest = &rest[start + 1..];
            if let Some(end) = rest.find("](") {
                plain.push_str(&rest[..end]);
                rest = rest[end..].split_once(')').map_or("", |(_, after)| after);
            }
        }
        plain.push_str(rest);
        let slug: String = plain
            .to_lowercase()
            .chars()
            .filter(|c| c.is_alphanumeric() || matches!(c, '-' | '_' | ' '))
            .map(|c| if c == ' ' { '-' } else { c })
            .collect();
        // github-slugger: suffix `-1`, `-2`, ... until the slug is unused.
        let occurrences = seen.entry(slug.clone()).or_insert(0);
        let mut anchor = slug.clone();
        while anchors.contains(&anchor) {
            *occurrences += 1;
            anchor = format!("{slug}-{occurrences}");
        }
        anchors.push(anchor);
    }
    anchors
}

#[test]
fn relative_links_point_to_existing_files_and_headings() {
    let mut broken = Vec::new();
    for file in markdown_files() {
        let markdown = fs::read_to_string(&file).unwrap();
        let base = file.parent().unwrap();
        let fenced: Vec<(usize, usize)> = blocks(&markdown)
            .iter()
            .map(|block| (block.open, block.close))
            .collect();
        for (number, line) in markdown.lines().enumerate() {
            if fenced
                .iter()
                .any(|&(open, close)| number >= open && number <= close)
            {
                continue;
            }
            let mut rest = line;
            while let Some(start) = rest.find("](") {
                rest = &rest[start + 2..];
                let Some(end) = rest.find(')') else { break };
                let target = &rest[..end];
                rest = &rest[end..];
                if target.contains("://") || target.starts_with("mailto:") {
                    continue;
                }
                let (path, fragment) = match target.split_once('#') {
                    Some((path, fragment)) => (path, Some(fragment)),
                    None => (target, None),
                };
                let resolved = if path.is_empty() {
                    file.clone()
                } else {
                    base.join(path)
                };
                if !resolved.exists() {
                    broken.push(format!(
                        "{}:{}: `{target}` does not exist",
                        display(&file),
                        number + 1
                    ));
                    continue;
                }
                if let Some(fragment) = fragment {
                    let target_markdown = fs::read_to_string(&resolved).unwrap_or_default();
                    if !anchors(&target_markdown)
                        .iter()
                        .any(|anchor| anchor == fragment)
                    {
                        broken.push(format!(
                            "{}:{}: `{target}` has no heading with that anchor",
                            display(&file),
                            number + 1
                        ));
                    }
                }
            }
        }
    }
    assert!(broken.is_empty(), "broken links:\n{}", broken.join("\n"));
}

/// Public items of a crate, found by scanning its sources: `pub` functions, types, traits,
/// fields and enum variants, minus `#[doc(hidden)]` items and the `__private.rs` module that
/// generated code uses.
fn public_items(crate_dir: &str, hidden: &[&str]) -> Vec<String> {
    let mut sources = Vec::new();
    let mut stack = vec![docs_dir().join("../crates").join(crate_dir).join("src")];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|ext| ext == "rs")
                && path.file_name().is_some_and(|name| name != "__private.rs")
            {
                sources.push(fs::read_to_string(path).unwrap());
            }
        }
    }

    const KEYWORDS: &[&str] = &[
        "pub fn ",
        "pub async fn ",
        "pub const fn ",
        "pub struct ",
        "pub enum ",
        "pub trait ",
        "pub type ",
    ];
    let mut items = Vec::new();
    for source in &sources {
        let lines: Vec<&str> = source.lines().collect();
        let mut enum_depth: Option<usize> = None;
        for (index, line) in lines.iter().enumerate() {
            let trimmed = line.trim();
            let attributes = lines[..index]
                .iter()
                .rev()
                .map(|line| line.trim())
                .take_while(|line| line.starts_with('#') || line.starts_with("///"));
            let is_hidden = attributes.clone().any(|line| line == "#[doc(hidden)]");

            if let Some(depth) = enum_depth {
                let indent = line.len() - line.trim_start().len();
                if trimmed == "}" && indent + 4 == depth {
                    enum_depth = None;
                } else if indent == depth && trimmed.starts_with(|c: char| c.is_ascii_uppercase()) {
                    let name: String = trimmed
                        .chars()
                        .take_while(|c| c.is_alphanumeric() || *c == '_')
                        .collect();
                    items.push(name);
                }
                continue;
            }

            if trimmed.starts_with("#[macro_export]") {
                if let Some(next) = lines.get(index + 1)
                    && let Some(name) = next.trim().strip_prefix("macro_rules! ")
                {
                    items.push(name.trim_end_matches(" {").to_owned() + "!");
                }
                continue;
            }

            if is_hidden || trimmed.starts_with("pub(") {
                continue;
            }
            if let Some(keyword) = KEYWORDS
                .iter()
                .find(|keyword| trimmed.starts_with(*keyword))
            {
                let name: String = trimmed[keyword.len()..]
                    .chars()
                    .take_while(|c| c.is_alphanumeric() || *c == '_')
                    .collect();
                if *keyword == "pub enum " && trimmed.ends_with('{') {
                    enum_depth = Some(line.len() - line.trim_start().len() + 4);
                }
                items.push(name);
            } else if let Some(field) = trimmed.strip_prefix("pub ")
                && let Some((name, _)) = field.split_once(':')
                && name.chars().all(|c| c.is_alphanumeric() || c == '_')
            {
                items.push(name.to_owned());
            }
        }
    }
    items.retain(|item| !item.is_empty() && !hidden.contains(&item.as_str()));
    items.sort();
    items.dedup();
    items
}

fn mentions(text: &str, name: &str) -> bool {
    let is_ident = |c: char| c.is_alphanumeric() || c == '_';
    text.match_indices(name).any(|(start, _)| {
        let before = text[..start].chars().next_back();
        let after = text[start + name.len()..].chars().next();
        !before.is_some_and(is_ident) && !after.is_some_and(is_ident)
    })
}

fn assert_documented(reference: &str, items: &[String]) {
    let text = fs::read_to_string(docs_dir().join("api").join(reference)).unwrap();
    let missing: Vec<&str> = items
        .iter()
        .map(String::as_str)
        .filter(|item| !mentions(&text, item))
        .collect();
    assert!(
        missing.is_empty(),
        "api/{reference} does not mention these public items: {missing:?}"
    );
}

#[test]
fn api_reference_covers_hornero_core() {
    // `Handle::resolve_in` belongs to a `#[doc(hidden)]` type used by generated code.
    let items = public_items("hornero-core", &["resolve_in"]);
    assert!(
        items.len() > 100,
        "the scanner found suspiciously few items: {items:?}"
    );
    assert_documented("hornero-core.md", &items);
}

#[test]
fn api_reference_covers_the_facade_and_macros() {
    let mut items = public_items("hornero", &[]);
    items.extend(
        [
            "Injectable",
            "injectable",
            "module",
            "controller",
            "routes",
            "inject",
            "constructor",
            "get",
            "post",
            "put",
            "patch",
            "delete",
            "head",
            "options",
            "route",
            "guards",
            "interceptors",
            "filters",
            "meta",
            "imports",
            "providers",
            "controllers",
            "exports",
            "middlewares",
            "global",
            "value",
            "factory",
            "provider",
            "scope",
            "hooks",
            "default",
            "with",
            "prelude",
            "api_response",
            "api_body",
            "api_tags",
            "api_exclude",
            "openapi",
            "swagger-ui",
        ]
        .map(str::to_owned),
    );
    assert_documented("hornero.md", &items);
}

#[test]
fn api_reference_covers_the_integration_crates() {
    assert_documented("hornero-testing.md", &public_items("hornero-testing", &[]));
    assert_documented("hornero-config.md", &public_items("hornero-config", &[]));
    assert_documented(
        "hornero-validation.md",
        &public_items("hornero-validation", &[]),
    );
    assert_documented("hornero-sea-orm.md", &public_items("hornero-sea-orm", &[]));
    assert_documented("hornero-openapi.md", &public_items("hornero-openapi", &[]));
}

#[test]
fn anchors_follow_github_rules() {
    let markdown = "# 1. 快速入門\n\
                    ## `Shutdown`：背景工作與主動關閉\n\
                    ## L0：掛進既有的 axum Router\n\
                    ## Guard\n\
                    ## Guard\n\
                    ## hornero-core API\n\
                    ## `Hornero`\n\
                    ## Guard-1\n\
                    ```text\n\
                    # not a heading\n\
                    ```\n\
                    #not-a-heading-either\n";
    assert_eq!(
        anchors(markdown),
        [
            "1-快速入門",
            "shutdown背景工作與主動關閉",
            "l0掛進既有的-axum-router",
            "guard",
            "guard-1",
            "hornero-core-api",
            "hornero",
            "guard-1-1",
        ]
    );
}
