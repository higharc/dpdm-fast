use super::parse_tree_recursive::parse_tree_recursive;
use super::types::{Alias, ParseOptions};
use crate::parser::types::{DependencyTree, SymbolTree};
use crate::utils::options::normalize_options;
use crate::utils::path::join_paths;
use crate::utils::shorten::{shorten_symbol_tree, shorten_tree};
use crate::utils::tsconfig_files::load_tsconfig_with_extends;
use crate::utils::workspace::detect_workspaces;
use glob::glob;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use swc_core::common::{sync::Lrc, SourceMap};

/// Calculate the specificity of a path pattern for sorting.
/// More specific patterns (longer, fewer wildcards) should be tried first.
fn pattern_specificity(pattern: &str) -> (usize, usize) {
    // Primary: length of the pattern (longer = more specific)
    // Secondary: number of non-wildcard characters (more = more specific)
    let non_wildcard_len = pattern.chars().filter(|&c| c != '*').count();
    (non_wildcard_len, pattern.len())
}

/// Extract paths from tsconfig JSON and create an Alias struct.
/// Paths are sorted by specificity (longer/more specific patterns first).
fn extract_alias_from_tsconfig(tsconfig_json: &Value, root: PathBuf) -> Option<Alias> {
    let paths = tsconfig_json
        .get("compilerOptions")
        .and_then(|co| co.get("paths"))?;

    if paths.is_null() {
        return None;
    }

    // Collect paths into a Vec
    let mut paths_vec: Vec<(String, Vec<String>)> = paths
        .as_object()?
        .iter()
        .filter_map(|(k, v)| {
            let values: Vec<String> = v
                .as_array()?
                .iter()
                .filter_map(|val| val.as_str().map(String::from))
                .collect();
            Some((k.clone(), values))
        })
        .collect();

    if paths_vec.is_empty() {
        return None;
    }

    // Sort by specificity: longer/more specific patterns first
    // This ensures "@/components/*" is tried before "@/*"
    paths_vec.sort_by(|a, b| {
        let spec_a = pattern_specificity(&a.0);
        let spec_b = pattern_specificity(&b.0);
        // Reverse order: higher specificity first
        spec_b.cmp(&spec_a)
    });

    Some(Alias {
        root,
        paths: paths_vec,
    })
}

pub async fn parse_dependency_tree(
    entries: &Vec<String>,
    base_options: &ParseOptions,
) -> (DependencyTree, SymbolTree) {
    let options: ParseOptions = normalize_options(Some((*base_options).clone()));

    let current_directory = fs::canonicalize(PathBuf::from(".")).unwrap();

    // Parse tsconfig with extends resolution
    let (tsconfig_json, tsconfig_dir) = match options.tsconfig.as_ref() {
        Some(tsconfig) => {
            let tsconfig_path = PathBuf::from(tsconfig);
            let mut visited = HashSet::new();

            match load_tsconfig_with_extends(&tsconfig_path, &mut visited) {
                Some(result) => (result.config, result.config_dir),
                None => {
                    eprintln!("Failed to load tsconfig: {}", tsconfig_path.display());
                    return (HashMap::new(), HashMap::new());
                }
            }
        }
        None => (serde_json::json!({}), current_directory.clone()),
    };

    // Resolve baseUrl relative to tsconfig.json location (not CWD)
    // This matches TypeScript's behavior
    let root = match tsconfig_json
        .get("compilerOptions")
        .and_then(|co| co.get("baseUrl"))
        .and_then(|bu| bu.as_str())
    {
        Some(base_url) => {
            let base_url: PathBuf = PathBuf::from(base_url);
            join_paths(&[&tsconfig_dir, &base_url])
        }
        None => tsconfig_dir.clone(),
    };

    // Extract alias configuration from the merged tsconfig
    let alias = extract_alias_from_tsconfig(&tsconfig_json, root);

    // Detect workspace packages (monorepo support)
    let workspace_map = detect_workspaces(&tsconfig_dir);

    let cm = Lrc::new(SourceMap::default());
    let output: Arc<Mutex<DependencyTree>> = Arc::new(Mutex::new(HashMap::new()));
    let symbol_output: Arc<Mutex<SymbolTree>> = Arc::new(Mutex::new(HashMap::new()));

    // Build a deterministic entry list
    let mut entry_files: Vec<PathBuf> = Vec::new();
    for entry in entries {
        for entry_path in glob(&entry).expect("Failed to read glob pattern") {
            match entry_path {
                Ok(filename) => {
                    let path = current_directory.join(filename);
                    let canonical = fs::canonicalize(&path).unwrap_or(path);
                    entry_files.push(canonical);
                }
                Err(e) => eprintln!("{:?}", e),
            }
        }
    }

    entry_files.sort();
    entry_files.dedup();

    for path in entry_files {
        parse_tree_recursive(
            current_directory.clone(),
            path,
            Arc::clone(&output),
            Arc::clone(&symbol_output),
            Arc::new(cm.clone()),
            Arc::new(options.clone()),
            alias.as_ref().map(|a| Arc::new(a.clone())),
            workspace_map.as_ref().map(|m| Arc::new(m.clone())),
        )
        .await;
    }

    let output_lock = output.lock().unwrap();
    let symbol_lock = symbol_output.lock().unwrap();
    let deps_tree = shorten_tree(
        &current_directory.to_string_lossy().to_string(),
        &output_lock,
    );
    let symbol_tree = shorten_symbol_tree(
        &current_directory.to_string_lossy().to_string(),
        &symbol_lock,
    );
    (deps_tree, symbol_tree)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::path::PathBuf;

    #[test]
    fn test_pattern_specificity_ordering() {
        // More specific (longer) patterns should have higher specificity
        let spec_short = pattern_specificity("@/*");
        let spec_long = pattern_specificity("@/components/*");
        let spec_exact = pattern_specificity("config");

        // @/components/* should be more specific than @/*
        assert!(spec_long > spec_short, "longer patterns should have higher specificity");

        // Exact matches (no wildcards) should be most specific for their length
        assert!(spec_exact.0 == 6, "exact pattern should count all characters");
    }

    #[test]
    fn test_extract_alias_sorts_by_specificity() {
        let tsconfig = json!({
            "compilerOptions": {
                "baseUrl": ".",
                "paths": {
                    "@/*": ["./src/*"],
                    "@/components/*": ["./src/components/*"],
                    "@/utils/*": ["./src/utils/*"],
                    "config": ["./src/config.ts"]
                }
            }
        });

        let alias = extract_alias_from_tsconfig(&tsconfig, PathBuf::from("/root")).unwrap();

        // Paths should be sorted by specificity (longer first)
        let patterns: Vec<&str> = alias.paths.iter().map(|(p, _)| p.as_str()).collect();

        // @/components/* and @/utils/* should come before @/*
        let short_idx = patterns.iter().position(|&p| p == "@/*").unwrap();
        let long_idx = patterns.iter().position(|&p| p == "@/components/*").unwrap();

        assert!(
            long_idx < short_idx,
            "more specific patterns should come first: {:?}",
            patterns
        );
    }

    #[test]
    fn test_extract_alias_handles_empty_paths() {
        let tsconfig = json!({
            "compilerOptions": {
                "baseUrl": "."
            }
        });

        let alias = extract_alias_from_tsconfig(&tsconfig, PathBuf::from("/root"));
        assert!(alias.is_none(), "should return None when paths is missing");
    }
}
