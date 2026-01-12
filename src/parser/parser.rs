use super::parse_tree_recursive::parse_tree_recursive;
use super::types::{Alias, ParseOptions};
use crate::parser::types::{DependencyTree, SymbolTree};
use crate::utils::json::strip_jsonc_comments;
use crate::utils::options::normalize_options;
use crate::utils::path::join_paths;
use crate::utils::shorten::{shorten_symbol_tree, shorten_tree};
use glob::glob;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use swc_core::common::{sync::Lrc, SourceMap};

/// Result of loading a tsconfig, including its directory for baseUrl resolution
struct TsConfigResult {
    config: Value,
    /// The directory containing the tsconfig file (used for baseUrl resolution)
    config_dir: PathBuf,
}

/// Load a tsconfig.json file and resolve its `extends` chain.
/// Returns the merged configuration with all inherited settings.
fn load_tsconfig_with_extends(
    tsconfig_path: &PathBuf,
    current_directory: &PathBuf,
    visited: &mut HashSet<PathBuf>,
) -> Option<TsConfigResult> {
    // Resolve the tsconfig path to an absolute path
    let canonical_path = fs::canonicalize(tsconfig_path).ok()?;

    // Detect circular extends
    if visited.contains(&canonical_path) {
        eprintln!(
            "Circular extends detected in tsconfig: {}",
            canonical_path.display()
        );
        return None;
    }
    visited.insert(canonical_path.clone());

    // Get the directory containing this tsconfig
    let config_dir = canonical_path
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| current_directory.clone());

    // Read and parse the tsconfig file
    let content = match fs::read_to_string(&canonical_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!(
                "Failed to read tsconfig {}: {:?}",
                canonical_path.display(),
                e
            );
            return None;
        }
    };

    let cleaned_content = strip_jsonc_comments(&content, true);
    let mut config: Value = match serde_json::from_str(&cleaned_content) {
        Ok(json) => json,
        Err(e) => {
            eprintln!(
                "Failed to parse tsconfig {}: {:?}",
                canonical_path.display(),
                e
            );
            return None;
        }
    };

    // Check for extends and process parent config
    if let Some(extends) = config.get("extends").and_then(|e| e.as_str()) {
        let parent_path = resolve_extends_path(extends, &config_dir);

        if let Some(parent_result) =
            load_tsconfig_with_extends(&parent_path, current_directory, visited)
        {
            // Merge parent into child (child overrides parent)
            config = merge_tsconfig(parent_result.config, config);
        }
    }

    // Remove the extends field from the result (it's been processed)
    if let Some(obj) = config.as_object_mut() {
        obj.remove("extends");
    }

    Some(TsConfigResult { config, config_dir })
}

/// Resolve the path in an `extends` field to an absolute path.
/// Handles both relative paths (./base.json) and package references (@tsconfig/node16).
fn resolve_extends_path(extends: &str, config_dir: &PathBuf) -> PathBuf {
    if extends.starts_with('.') {
        // Relative path
        join_paths(&[config_dir, &PathBuf::from(extends)])
    } else if extends.starts_with('/') {
        // Absolute path (rare but possible)
        PathBuf::from(extends)
    } else {
        // Package reference (e.g., "@tsconfig/node16")
        // Try to resolve from node_modules
        let mut search_dir = config_dir.clone();
        loop {
            let candidate = search_dir.join("node_modules").join(extends);
            // Try with .json extension if not present
            let candidate_with_json = if extends.ends_with(".json") {
                candidate.clone()
            } else {
                // Try tsconfig.json inside the package
                let package_tsconfig = candidate.join("tsconfig.json");
                if package_tsconfig.exists() {
                    return package_tsconfig;
                }
                // Try with .json extension
                PathBuf::from(format!("{}.json", candidate.display()))
            };

            if candidate_with_json.exists() {
                return candidate_with_json;
            }

            if candidate.exists() {
                return candidate;
            }

            // Move up one directory
            match search_dir.parent() {
                Some(parent) => search_dir = parent.to_path_buf(),
                None => break,
            }
        }

        // Fallback: return the path as-is relative to config_dir
        join_paths(&[config_dir, &PathBuf::from(extends)])
    }
}

/// Merge two tsconfig objects. Child values override parent values.
/// For compilerOptions, we do a shallow merge (child fields override parent fields).
fn merge_tsconfig(parent: Value, child: Value) -> Value {
    match (parent, child) {
        (Value::Object(mut parent_obj), Value::Object(child_obj)) => {
            for (key, child_value) in child_obj {
                if key == "compilerOptions" {
                    // Special handling for compilerOptions - shallow merge
                    if let Some(parent_co) = parent_obj.get("compilerOptions") {
                        let merged_co = merge_compiler_options(parent_co.clone(), child_value);
                        parent_obj.insert(key, merged_co);
                    } else {
                        parent_obj.insert(key, child_value);
                    }
                } else {
                    // For other fields, child completely overrides parent
                    parent_obj.insert(key, child_value);
                }
            }
            Value::Object(parent_obj)
        }
        (_, child) => child, // If parent isn't an object, just use child
    }
}

/// Merge compilerOptions objects. Child fields override parent fields.
/// This is a shallow merge - if child has "paths", it completely replaces parent's "paths".
fn merge_compiler_options(parent: Value, child: Value) -> Value {
    match (parent, child) {
        (Value::Object(mut parent_obj), Value::Object(child_obj)) => {
            for (key, child_value) in child_obj {
                parent_obj.insert(key, child_value);
            }
            Value::Object(parent_obj)
        }
        (_, child) => child,
    }
}

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

            match load_tsconfig_with_extends(&tsconfig_path, &current_directory, &mut visited) {
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

    let cm = Lrc::new(SourceMap::default());
    let output: Arc<Mutex<DependencyTree>> = Arc::new(Mutex::new(HashMap::new()));
    let symbol_output: Arc<Mutex<SymbolTree>> = Arc::new(Mutex::new(HashMap::new()));

    // 获取文件列表
    let mut tasks = vec![];
    for entry in entries {
        for entry_path in glob(&entry).expect("Failed to read glob pattern") {
            match entry_path {
                Ok(filename) => {
                    let path: PathBuf = current_directory.join(filename);
                    let output_clone = Arc::clone(&output);
                    let symbol_output_clone = Arc::clone(&symbol_output);
                    let alias_arc = alias.as_ref().map(|a| Arc::new(a.clone()));
                    let task = parse_tree_recursive(
                        current_directory.clone(),
                        path,
                        output_clone,
                        symbol_output_clone,
                        Arc::new(cm.clone()),
                        Arc::new(options.clone()),
                        alias_arc,
                    );
                    tasks.push(task);
                }
                Err(e) => eprintln!("{:?}", e),
            }
        }
    }

    futures::future::join_all(tasks).await;

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

    #[test]
    fn test_merge_tsconfig_child_overrides_parent() {
        let parent = json!({
            "compilerOptions": {
                "strict": true,
                "baseUrl": ".",
                "paths": {
                    "@/*": ["./src/*"]
                }
            }
        });

        let child = json!({
            "compilerOptions": {
                "strict": false,
                "paths": {
                    "@/*": ["./lib/*"]
                }
            }
        });

        let merged = merge_tsconfig(parent, child);

        // Child should override parent values
        assert_eq!(
            merged["compilerOptions"]["strict"],
            json!(false),
            "child strict should override parent"
        );

        // Child paths should completely replace parent paths
        assert_eq!(
            merged["compilerOptions"]["paths"]["@/*"],
            json!(["./lib/*"]),
            "child paths should override parent paths"
        );

        // Parent-only values should be preserved
        assert_eq!(
            merged["compilerOptions"]["baseUrl"],
            json!("."),
            "parent baseUrl should be preserved"
        );
    }

    #[test]
    fn test_merge_compiler_options() {
        let parent = json!({
            "strict": true,
            "target": "es5"
        });

        let child = json!({
            "strict": false,
            "module": "commonjs"
        });

        let merged = merge_compiler_options(parent, child);

        assert_eq!(merged["strict"], json!(false));
        assert_eq!(merged["target"], json!("es5"));
        assert_eq!(merged["module"], json!("commonjs"));
    }
}
