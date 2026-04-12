use gpui::{HighlightStyle, hsla};
use std::collections::BTreeSet;
use std::ops::Range;
use tree_sitter::{Language, Parser, Query, QueryCursor, StreamingIterator};
use tree_sitter_language::LanguageFn;

/// Get tree-sitter language + highlights query for a language name.
fn get_language(name: &str) -> Option<(LanguageFn, &'static str)> {
    match name {
        "rust" => Some((tree_sitter_rust::LANGUAGE, tree_sitter_rust::HIGHLIGHTS_QUERY)),
        "javascript" => Some((tree_sitter_javascript::LANGUAGE, tree_sitter_javascript::HIGHLIGHT_QUERY)),
        "typescript" => Some((tree_sitter_typescript::LANGUAGE_TYPESCRIPT, tree_sitter_typescript::HIGHLIGHTS_QUERY)),
        "python" => Some((tree_sitter_python::LANGUAGE, tree_sitter_python::HIGHLIGHTS_QUERY)),
        "go" => Some((tree_sitter_go::LANGUAGE, tree_sitter_go::HIGHLIGHTS_QUERY)),
        "java" => Some((tree_sitter_java::LANGUAGE, tree_sitter_java::HIGHLIGHTS_QUERY)),
        "c" => Some((tree_sitter_c::LANGUAGE, tree_sitter_c::HIGHLIGHT_QUERY)),
        "cpp" => Some((tree_sitter_cpp::LANGUAGE, tree_sitter_cpp::HIGHLIGHT_QUERY)),
        "ruby" => Some((tree_sitter_ruby::LANGUAGE, tree_sitter_ruby::HIGHLIGHTS_QUERY)),
        "swift" => Some((tree_sitter_swift::LANGUAGE, tree_sitter_swift::HIGHLIGHTS_QUERY)),
        "scala" => Some((tree_sitter_scala::LANGUAGE, tree_sitter_scala::HIGHLIGHTS_QUERY)),
        "zig" => Some((tree_sitter_zig::LANGUAGE, tree_sitter_zig::HIGHLIGHTS_QUERY)),
        "bash" => Some((tree_sitter_bash::LANGUAGE, tree_sitter_bash::HIGHLIGHT_QUERY)),
        "html" => Some((tree_sitter_html::LANGUAGE, tree_sitter_html::HIGHLIGHTS_QUERY)),
        "css" => Some((tree_sitter_css::LANGUAGE, tree_sitter_css::HIGHLIGHTS_QUERY)),
        "json" => Some((tree_sitter_json::LANGUAGE, tree_sitter_json::HIGHLIGHTS_QUERY)),
        "toml" => Some((tree_sitter_toml_ng::LANGUAGE, tree_sitter_toml_ng::HIGHLIGHTS_QUERY)),
        "yaml" => Some((tree_sitter_yaml::LANGUAGE, tree_sitter_yaml::HIGHLIGHTS_QUERY)),
        "markdown" => Some((tree_sitter_md::LANGUAGE, tree_sitter_md::HIGHLIGHT_QUERY_BLOCK)),
        "sql" => Some((tree_sitter_sequel::LANGUAGE, tree_sitter_sequel::HIGHLIGHTS_QUERY)),
        "elixir" => Some((tree_sitter_elixir::LANGUAGE, tree_sitter_elixir::HIGHLIGHTS_QUERY)),
        _ => None,
    }
}

// ── Dark theme palette ─────────────────────────────────────────

fn style_for_capture(name: &str) -> Option<HighlightStyle> {
    let color = match name {
        "keyword" | "keyword.return" | "keyword.function" | "keyword.operator"
        | "keyword.import" | "keyword.export" | "keyword.conditional"
        | "keyword.repeat" | "keyword.exception" => Some(hsla(0.63, 0.70, 0.68, 1.0)), // blue
        "string" | "string.special" => Some(hsla(0.28, 0.50, 0.60, 1.0)),               // green
        "comment" | "comment.doc" => Some(hsla(0.0, 0.0, 0.45, 1.0)),                   // gray
        "function" | "function.call" | "function.method" | "function.builtin" | "function.macro"
        | "method" => Some(hsla(0.14, 0.65, 0.70, 1.0)),                                // yellow
        "type" | "type.builtin" | "constructor" => Some(hsla(0.48, 0.55, 0.65, 1.0)),   // cyan
        "variable.builtin" | "variable.special" | "variable.parameter"
        | "constant" | "constant.builtin" => Some(hsla(0.55, 0.55, 0.70, 1.0)),         // light blue
        "number" | "boolean" | "float" => Some(hsla(0.08, 0.65, 0.65, 1.0)),            // orange
        "operator" => Some(hsla(0.0, 0.0, 0.75, 1.0)),                                  // light gray
        "punctuation" | "punctuation.bracket" | "punctuation.delimiter"
        | "punctuation.special" => Some(hsla(0.0, 0.0, 0.60, 1.0)),                     // dim gray
        "property" | "field" | "label" => Some(hsla(0.55, 0.40, 0.70, 1.0)),            // periwinkle
        "attribute" | "tag" => Some(hsla(0.0, 0.55, 0.65, 1.0)),                        // red
        "string.escape" | "string.regex" => Some(hsla(0.08, 0.55, 0.60, 1.0)),          // dark orange
        "namespace" | "module" => Some(hsla(0.48, 0.40, 0.60, 1.0)),                    // teal
        "enum" | "variant" => Some(hsla(0.48, 0.55, 0.65, 1.0)),                        // cyan
        "embedded" | "preproc" => Some(hsla(0.83, 0.50, 0.65, 1.0)),                    // magenta
        _ => None,
    }?;
    Some(HighlightStyle {
        color: Some(color),
        ..Default::default()
    })
}

// ── Public API ─────────────────────────────────────────────────

/// Run tree-sitter highlighting on `text` for the given language name.
/// Returns styled ranges suitable for mapping to GPUI TextRuns.
pub fn highlight(language_name: &str, text: &str) -> Vec<(Range<usize>, HighlightStyle)> {
    let Some((lang_fn, query_src)) = get_language(language_name) else {
        return Vec::new();
    };

    let lang: Language = lang_fn.into();
    let mut parser = Parser::new();
    if parser.set_language(&lang).is_err() {
        return Vec::new();
    }
    let Some(tree) = parser.parse(text, None) else {
        return Vec::new();
    };

    // Some highlights.scm files use patterns that their grammar version
    // doesn't recognise (e.g. a node name added in a newer grammar).
    // Ignore those errors — we'll just get fewer highlights.
    let query = match Query::new(&lang, query_src) {
        Ok(q) => q,
        Err(_) => return Vec::new(),
    };

    let capture_names: Vec<&str> = query.capture_names().iter().map(|s| &**s).collect();

    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&query, tree.root_node(), text.as_bytes());
    let mut raw: Vec<(Range<usize>, HighlightStyle)> = Vec::new();

    while let Some(m) = matches.next() {
        for cap in m.captures {
            let name = capture_names[cap.index as usize];
            if let Some(style) = style_for_capture(name) {
                let range = cap.node.byte_range();
                if range.start < range.end {
                    raw.push((range, style));
                }
            }
        }
    }

    if raw.is_empty() {
        return Vec::new();
    }

    merge_styles(&(0..text.len()), raw)
}

/// Merge overlapping highlight ranges. Later (more specific) captures
/// override earlier ones where they overlap.
fn merge_styles(
    total_range: &Range<usize>,
    styles: Vec<(Range<usize>, HighlightStyle)>,
) -> Vec<(Range<usize>, HighlightStyle)> {
    let mut boundaries = BTreeSet::new();
    boundaries.insert(total_range.start);
    boundaries.insert(total_range.end);
    for (range, _) in &styles {
        boundaries.insert(range.start);
        boundaries.insert(range.end);
    }

    let pts: Vec<usize> = boundaries.into_iter().collect();
    let mut result = Vec::with_capacity(pts.len());

    for i in 0..pts.len().saturating_sub(1) {
        let interval = pts[i]..pts[i + 1];
        if interval.start >= interval.end {
            continue;
        }

        // Find the last (most specific) style covering this interval.
        let mut top: Option<HighlightStyle> = None;
        for (range, style) in &styles {
            if range.start <= interval.start && interval.end <= range.end {
                top = Some(*style);
            }
        }

        if let Some(style) = top {
            result.push((interval, style));
        }
    }

    // Merge adjacent ranges with same style.
    let mut merged: Vec<(Range<usize>, HighlightStyle)> = Vec::with_capacity(result.len());
    for (range, style) in result {
        if let Some((last_range, last_style)) = merged.last_mut() {
            if last_range.end == range.start && *last_style == style {
                last_range.end = range.end;
                continue;
            }
        }
        merged.push((range, style));
    }

    merged
}
