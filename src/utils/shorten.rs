use std::{collections::HashMap, path::Path};

use crate::parser::types::{Dependency, DependencyTree, SymbolTree};

/// Normalize path separators to forward slashes for cross-platform consistency.
/// Without this, Windows produces `\`-separated keys while Mac/Linux produce
/// `/`-separated keys, causing different sort orders in parse_circular and
/// therefore non-deterministic cycle counts across platforms.
fn normalize_sep(s: String) -> String {
    if cfg!(windows) {
        s.replace('\\', "/")
    } else {
        s
    }
}

pub fn shorten_tree(context: &String, tree: &DependencyTree) -> DependencyTree {
    let mut output: DependencyTree = HashMap::new();
    for (key, dependencies) in tree.iter() {
        if key.contains("node_modules") {
            continue;
        }

        let short_key = normalize_sep(
            Path::new(key)
                .strip_prefix(&context)
                .unwrap_or_else(|_| Path::new(key))
                .to_str()
                .unwrap()
                .to_string(),
        );
        output.insert(
            short_key.clone(),
            <std::option::Option<Vec<Dependency>> as Clone>::clone(&dependencies.as_ref())
                .map(|deps| {
                    deps.iter()
                        .map(|item| Dependency {
                            issuer: short_key.clone(),
                            request: item.request.clone(),
                            kind: item.kind.clone(),
                            id: item.id.as_ref().map(|id| {
                                normalize_sep(
                                    Path::new(id)
                                        .strip_prefix(&context)
                                        .unwrap_or_else(|_| Path::new(id))
                                        .to_str()
                                        .unwrap()
                                        .to_string(),
                                )
                            }),
                        })
                        .collect::<Vec<Dependency>>()
                })
                .into(),
        );
    }
    output
}

pub fn shorten_path(path: &String, context: &String) -> String {
    normalize_sep(
        Path::new(path)
            .strip_prefix(&context)
            .unwrap_or_else(|_| Path::new(path))
            .to_str()
            .unwrap()
            .to_string(),
    )
}

pub fn shorten_symbol_tree(context: &String, tree: &SymbolTree) -> SymbolTree {
    let mut output: SymbolTree = HashMap::new();
    for (key, symbol_node) in tree.iter() {
        if key.contains("node_modules") {
            continue;
        }

        let short_key = normalize_sep(
            Path::new(key)
                .strip_prefix(&context)
                .unwrap_or_else(|_| Path::new(key))
                .to_str()
                .unwrap()
                .to_string(),
        );
        output.insert(short_key.clone(), symbol_node.clone());
    }
    output
}
