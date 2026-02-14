use anyhow::Result;
use tree_sitter::{Parser, Query, QueryCursor, StreamingIterator};

use super::languages::Lang;

/// A symbol extracted from source code.
#[derive(Debug, Clone)]
pub struct ExtractedSymbol {
    pub name: String,
    pub kind: String,
    pub start_line: usize,
    pub end_line: usize,
    pub signature: Option<String>,
    pub doc_comment: Option<String>,
    pub parent_name: Option<String>,
}

/// Extract symbols from source code using tree-sitter.
pub fn extract_symbols(lang: Lang, source: &[u8]) -> Result<Vec<ExtractedSymbol>> {
    let grammar = lang.grammar();

    let mut parser = Parser::new();
    parser
        .set_language(&grammar)
        .map_err(|e| anyhow::anyhow!("Failed to set language: {}", e))?;

    let tree = parser
        .parse(source, None)
        .ok_or_else(|| anyhow::anyhow!("Failed to parse source"))?;

    let query_str = lang.symbol_query();
    let query = Query::new(&grammar, query_str)
        .map_err(|e| anyhow::anyhow!("Failed to compile query: {}", e))?;

    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&query, tree.root_node(), source);

    let capture_names = query.capture_names();
    let mut symbols = Vec::new();

    while let Some(m) = matches.next() {
        let pattern_idx = m.pattern_index;
        let mut name_text = String::new();
        let mut def_node = None;

        for capture in m.captures {
            let cap_name = &capture_names[capture.index as usize];
            match *cap_name {
                "name" => {
                    name_text = capture
                        .node
                        .utf8_text(source)
                        .unwrap_or("")
                        .to_string();
                }
                _ => {
                    // The outer capture (function, struct, etc.) — this is the def node
                    if def_node.is_none() {
                        def_node = Some(capture.node);
                    }
                }
            }
        }

        if name_text.is_empty() {
            continue;
        }

        let node = match def_node {
            Some(n) => n,
            None => continue,
        };

        // Determine symbol kind from the pattern
        let kind = kind_from_pattern(query_str, pattern_idx);

        // Extract signature (first line of the definition)
        let signature = extract_signature(node, source);

        // Extract doc comment (comment node immediately preceding)
        let doc_comment = extract_doc_comment(node, source);

        // Determine parent (if nested inside a class/struct/impl)
        let parent_name = find_parent_symbol(node, source);

        symbols.push(ExtractedSymbol {
            name: name_text,
            kind,
            start_line: node.start_position().row + 1, // 1-indexed
            end_line: node.end_position().row + 1,
            signature,
            doc_comment,
            parent_name,
        });
    }

    Ok(symbols)
}

/// Determine the kind of symbol from the query pattern index.
/// We use the capture name of the outer (non-@name) capture.
fn kind_from_pattern(query_str: &str, pattern_idx: usize) -> String {
    // Parse the query to find the Nth pattern's outer capture name.
    // The outer capture is the one that's NOT @name.
    // Each pattern in the query ends with `) @<kind>`.
    let mut current_pattern = 0;
    for line in query_str.lines() {
        let trimmed = line.trim();
        // Look for closing pattern captures like `) @function`
        if trimmed.starts_with(") @") {
            if current_pattern == pattern_idx {
                return trimmed
                    .trim_start_matches(") @")
                    .trim()
                    .to_string();
            }
            current_pattern += 1;
        }
    }
    "unknown".to_string()
}

/// Extract a signature from the definition node (first line, up to the body).
fn extract_signature(node: tree_sitter::Node, source: &[u8]) -> Option<String> {
    let text = node.utf8_text(source).ok()?;

    // Take everything up to the first `{` or the first line
    let sig = if let Some(brace_pos) = text.find('{') {
        text[..brace_pos].trim()
    } else if let Some(newline_pos) = text.find('\n') {
        text[..newline_pos].trim()
    } else {
        text.trim()
    };

    if sig.is_empty() {
        None
    } else {
        // Limit length
        Some(if sig.len() > 200 {
            format!("{}...", &sig[..200])
        } else {
            sig.to_string()
        })
    }
}

/// Look for a doc comment immediately before the node.
fn extract_doc_comment(node: tree_sitter::Node, source: &[u8]) -> Option<String> {
    let mut prev = node.prev_sibling();

    let mut doc_lines = Vec::new();

    // Walk backwards collecting consecutive comment nodes
    while let Some(p) = prev {
        let kind = p.kind();
        if kind == "line_comment" || kind == "comment" || kind == "block_comment" {
            if let Ok(text) = p.utf8_text(source) {
                doc_lines.push(text.trim().to_string());
            }
            prev = p.prev_sibling();
        } else {
            break;
        }
    }

    if doc_lines.is_empty() {
        return None;
    }

    doc_lines.reverse();
    let doc = doc_lines.join("\n");

    // Limit length
    Some(if doc.len() > 500 {
        format!("{}...", &doc[..500])
    } else {
        doc
    })
}

/// Find the parent symbol name if this node is nested (e.g., method inside impl/class).
fn find_parent_symbol(node: tree_sitter::Node, source: &[u8]) -> Option<String> {
    let mut parent = node.parent();
    while let Some(p) = parent {
        let kind = p.kind();
        // Check if parent is a "container" node
        let is_container = matches!(
            kind,
            "impl_item"
                | "trait_item"
                | "class_definition"
                | "class_declaration"
                | "class_specifier"
                | "struct_item"
                | "module"
                | "namespace_definition"
                | "interface_declaration"
        );
        if is_container {
            // Try to get the name of this container
            if let Some(name_node) = p
                .child_by_field_name("name")
                .or_else(|| p.child_by_field_name("type"))
            {
                return name_node.utf8_text(source).ok().map(|s| s.to_string());
            }
        }
        parent = p.parent();
    }
    None
}
