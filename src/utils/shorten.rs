use std::collections::HashMap;

use crate::parser::types::{Dependency, DependencyTree, SymbolTree};

/// Normalize path separators to forward slashes for cross-platform consistency.
/// Without this, Windows produces `\`-separated keys while Mac/Linux produce
/// `/`-separated keys, causing different sort orders in parse_circular and
/// therefore non-deterministic cycle counts across platforms.
fn normalize_sep(s: &str) -> String {
    let mut normalized = s.to_string();
    if let Some(stripped) = normalized.strip_prefix(r"\\?\") {
        normalized = stripped.to_string();
    }
    normalized.replace('\\', "/")
}

fn shorten_with_context(path: &str, context: &str) -> String {
    let normalized_path = normalize_sep(path);
    let normalized_context = normalize_sep(context).trim_end_matches('/').to_string();

    if let Some(rest) = normalized_path.strip_prefix(&(normalized_context.clone() + "/")) {
        rest.to_string()
    } else {
        normalized_path
    }
}

pub fn shorten_tree(context: &String, tree: &DependencyTree) -> DependencyTree {
    let mut output: DependencyTree = HashMap::new();
    for (key, dependencies) in tree.iter() {
        if key.contains("node_modules") {
            continue;
        }

        let short_key = shorten_with_context(key, context);
        output.insert(
            short_key.clone(),
            <std::option::Option<Vec<Dependency>> as Clone>::clone(&dependencies.as_ref())
                .map(|deps| {
                    deps.iter()
                        .map(|item| Dependency {
                            issuer: short_key.clone(),
                            request: item.request.clone(),
                            kind: item.kind.clone(),
                            id: item.id.as_ref().map(|id| shorten_with_context(id, context)),
                        })
                        .collect::<Vec<Dependency>>()
                })
                .into(),
        );
    }
    output
}

pub fn shorten_path(path: &String, context: &String) -> String {
    shorten_with_context(path, context)
}

pub fn shorten_symbol_tree(context: &String, tree: &SymbolTree) -> SymbolTree {
    let mut output: SymbolTree = HashMap::new();
    for (key, symbol_node) in tree.iter() {
        if key.contains("node_modules") {
            continue;
        }

        let short_key = shorten_with_context(key, context);
        output.insert(short_key.clone(), symbol_node.clone());
    }
    output
}
