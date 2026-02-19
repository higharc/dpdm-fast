use glob::glob;
use serde_json::Value;
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use crate::utils::json::strip_jsonc_comments;
use crate::utils::path::join_paths;

/// Default include patterns per TypeScript spec
const DEFAULT_INCLUDE: &[&str] = &["**/*"];

/// Default exclude patterns per TypeScript spec
const DEFAULT_EXCLUDE: &[&str] = &["node_modules", "bower_components", "jspm_packages"];

/// TypeScript file extensions to match
const TS_EXTENSIONS: &[&str] = &[".ts", ".tsx", ".mts", ".cts"];

/// Result of loading a tsconfig
pub struct TsConfigResult {
    pub config: Value,
    pub config_dir: PathBuf,
}

/// Load a tsconfig.json file and resolve its `extends` chain.
/// Returns the merged configuration with all inherited settings.
pub fn load_tsconfig_with_extends(
    tsconfig_path: &Path,
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
        .unwrap_or_else(|| PathBuf::from("."));

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

        if let Some(parent_result) = load_tsconfig_with_extends(&parent_path, visited) {
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
fn resolve_extends_path(extends: &str, config_dir: &PathBuf) -> PathBuf {
    if extends.starts_with('.') {
        // Relative path
        join_paths(&[config_dir, &PathBuf::from(extends)])
    } else if extends.starts_with('/') {
        // Absolute path
        PathBuf::from(extends)
    } else {
        // Package reference (e.g., "@tsconfig/node16")
        let mut search_dir = config_dir.clone();
        loop {
            let candidate = search_dir.join("node_modules").join(extends);
            let candidate_with_json = if extends.ends_with(".json") {
                candidate.clone()
            } else {
                let package_tsconfig = candidate.join("tsconfig.json");
                if package_tsconfig.exists() {
                    return package_tsconfig;
                }
                PathBuf::from(format!("{}.json", candidate.display()))
            };

            if candidate_with_json.exists() {
                return candidate_with_json;
            }

            if candidate.exists() {
                return candidate;
            }

            match search_dir.parent() {
                Some(parent) => search_dir = parent.to_path_buf(),
                None => break,
            }
        }

        join_paths(&[config_dir, &PathBuf::from(extends)])
    }
}

/// Merge two tsconfig objects. Child values override parent values.
fn merge_tsconfig(parent: Value, child: Value) -> Value {
    match (parent, child) {
        (Value::Object(mut parent_obj), Value::Object(child_obj)) => {
            for (key, child_value) in child_obj {
                if key == "compilerOptions" {
                    if let Some(parent_co) = parent_obj.get("compilerOptions") {
                        let merged_co = merge_compiler_options(parent_co.clone(), child_value);
                        parent_obj.insert(key, merged_co);
                    } else {
                        parent_obj.insert(key, child_value);
                    }
                } else {
                    // For other fields (include, exclude, etc.), child completely overrides parent
                    parent_obj.insert(key, child_value);
                }
            }
            Value::Object(parent_obj)
        }
        (_, child) => child,
    }
}

/// Merge compilerOptions objects.
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

/// Get all files from a tsconfig and its references.
/// Respects include/exclude patterns and follows references recursively.
pub fn get_files_from_tsconfig(
    tsconfig_path: &Path,
    visited: &mut HashSet<PathBuf>,
) -> Vec<PathBuf> {
    // Prevent circular references
    let canonical = match fs::canonicalize(tsconfig_path) {
        Ok(p) => p,
        Err(_) => {
            eprintln!("Failed to canonicalize tsconfig path: {}", tsconfig_path.display());
            return vec![];
        }
    };

    if visited.contains(&canonical) {
        return vec![];
    }
    visited.insert(canonical.clone());

    // Load merged tsconfig (include/exclude inherited via extends)
    let mut extends_visited = HashSet::new();
    let result = match load_tsconfig_with_extends(tsconfig_path, &mut extends_visited) {
        Some(r) => r,
        None => {
            eprintln!("Failed to load tsconfig: {}", tsconfig_path.display());
            return vec![];
        }
    };

    let config = result.config;
    let config_dir = result.config_dir;

    let mut all_files: Vec<PathBuf> = Vec::new();

    // Get explicit files array (NOT inherited, only if specified)
    if let Some(files_array) = config.get("files").and_then(|f| f.as_array()) {
        for file_val in files_array {
            if let Some(file_str) = file_val.as_str() {
                let file_path = config_dir.join(file_str);
                if file_path.exists() {
                    all_files.push(file_path);
                }
            }
        }
    }

    // Get include patterns (from merged config or defaults)
    let include: Vec<String> = config
        .get("include")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_else(|| DEFAULT_INCLUDE.iter().map(|s| s.to_string()).collect());

    // Get exclude patterns (from merged config or defaults)
    let mut exclude: Vec<String> = config
        .get("exclude")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_else(|| DEFAULT_EXCLUDE.iter().map(|s| s.to_string()).collect());

    // Add outDir to exclude if specified
    if let Some(out_dir) = config
        .get("compilerOptions")
        .and_then(|co| co.get("outDir"))
        .and_then(|od| od.as_str())
    {
        exclude.push(out_dir.to_string());
    }

    // Expand include globs and filter by exclude
    let included_files = expand_and_filter(&include, &exclude, &config_dir);
    all_files.extend(included_files);

    // Recursively process references
    if let Some(refs) = config.get("references").and_then(|r| r.as_array()) {
        for ref_obj in refs {
            if let Some(ref_path) = ref_obj.get("path").and_then(|p| p.as_str()) {
                let resolved_ref = resolve_reference_path(ref_path, &config_dir);
                let ref_files = get_files_from_tsconfig(&resolved_ref, visited);
                all_files.extend(ref_files);
            }
        }
    }

    all_files
}

/// Resolve a reference path to a tsconfig.json file
fn resolve_reference_path(ref_path: &str, config_dir: &PathBuf) -> PathBuf {
    let full_path = config_dir.join(ref_path);

    // If it's a directory, look for tsconfig.json inside
    if full_path.is_dir() {
        full_path.join("tsconfig.json")
    } else if full_path.extension().is_some() {
        // Already has an extension, use as-is
        full_path
    } else {
        // Try adding .json extension
        let with_json = PathBuf::from(format!("{}.json", full_path.display()));
        if with_json.exists() {
            with_json
        } else {
            full_path.join("tsconfig.json")
        }
    }
}

/// Expand include glob patterns and filter out excluded files
fn expand_and_filter(include: &[String], exclude: &[String], config_dir: &PathBuf) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = Vec::new();

    // Normalize config_dir for glob pattern construction
    let config_dir_str = config_dir.to_string_lossy().to_string();
    // Remove UNC prefix on Windows for glob compatibility
    let config_dir_normalized = if config_dir_str.starts_with("\\\\?\\") {
        config_dir_str.trim_start_matches("\\\\?\\").to_string()
    } else {
        config_dir_str
    };

    for pattern in include {
        let full_pattern = format!("{}/{}", config_dir_normalized, pattern);

        match glob(&full_pattern) {
            Ok(paths) => {
                for entry in paths.filter_map(Result::ok) {
                    // Check if file has a TypeScript extension
                    if let Some(ext) = entry.extension() {
                        let ext_str = format!(".{}", ext.to_string_lossy());
                        if !TS_EXTENSIONS.contains(&ext_str.as_str()) {
                            continue;
                        }
                    } else {
                        continue;
                    }

                    // Check if file matches any exclude pattern
                    if !is_excluded(&entry, exclude, config_dir) {
                        files.push(entry);
                    }
                }
            }
            Err(e) => {
                eprintln!("Error expanding glob pattern {}: {:?}", full_pattern, e);
            }
        }
    }

    files
}

/// Check if a file path matches any exclude pattern
fn is_excluded(file_path: &Path, exclude: &[String], config_dir: &PathBuf) -> bool {
    // Get relative path from config_dir
    let relative_path = file_path
        .strip_prefix(config_dir)
        .unwrap_or(file_path)
        .to_string_lossy()
        .to_string();

    // Normalize path separators for matching
    let normalized_path = relative_path.replace('\\', "/");

    for pattern in exclude {
        // Simple pattern matching:
        // - If pattern contains *, use glob-style matching
        // - Otherwise, check if path starts with or contains the pattern
        if pattern.contains('*') {
            // Use glob matching
            if let Ok(glob_pattern) = glob::Pattern::new(pattern) {
                if glob_pattern.matches(&normalized_path) {
                    return true;
                }
            }
        } else {
            // Simple containment check
            let normalized_pattern = pattern.replace('\\', "/");
            if normalized_path.starts_with(&normalized_pattern)
                || normalized_path.contains(&format!("/{}/", normalized_pattern))
                || normalized_path.starts_with(&format!("{}/", normalized_pattern))
            {
                return true;
            }
        }
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::Write;
    use tempfile::TempDir;

    fn create_test_file(dir: &Path, name: &str, content: &str) {
        let path = dir.join(name);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        let mut file = fs::File::create(path).unwrap();
        file.write_all(content.as_bytes()).unwrap();
    }

    #[test]
    fn test_get_files_from_simple_tsconfig() {
        let temp = TempDir::new().unwrap();
        let root = temp.path();

        // Create tsconfig
        create_test_file(
            root,
            "tsconfig.json",
            r#"{
                "include": ["src/**/*"],
                "exclude": ["node_modules"]
            }"#,
        );

        // Create source files
        create_test_file(root, "src/index.ts", "export const x = 1;");
        create_test_file(root, "src/utils/helper.ts", "export const y = 2;");
        create_test_file(root, "node_modules/pkg/index.ts", "// excluded");

        let mut visited = HashSet::new();
        let files = get_files_from_tsconfig(&root.join("tsconfig.json"), &mut visited);

        assert_eq!(files.len(), 2);
        assert!(files.iter().any(|f| f.ends_with("index.ts")));
        assert!(files.iter().any(|f| f.ends_with("helper.ts")));
    }

    #[test]
    fn test_get_files_with_extends() {
        let temp = TempDir::new().unwrap();
        let root = temp.path();

        // Create base tsconfig with include
        create_test_file(
            root,
            "tsconfig.base.json",
            r#"{
                "include": ["src/**/*"],
                "exclude": ["**/*.test.ts"]
            }"#,
        );

        // Create child tsconfig that extends base (no include/exclude)
        create_test_file(
            root,
            "tsconfig.json",
            r#"{
                "extends": "./tsconfig.base.json",
                "compilerOptions": {
                    "strict": true
                }
            }"#,
        );

        // Create source files
        create_test_file(root, "src/index.ts", "export const x = 1;");
        create_test_file(root, "src/index.test.ts", "// test file");

        let mut visited = HashSet::new();
        let files = get_files_from_tsconfig(&root.join("tsconfig.json"), &mut visited);

        // Should include index.ts but exclude index.test.ts
        assert_eq!(files.len(), 1);
        assert!(files.iter().any(|f| f.ends_with("index.ts")));
    }

    #[test]
    fn test_is_excluded() {
        let config_dir = PathBuf::from("/project");

        // Test node_modules exclusion
        assert!(is_excluded(
            &PathBuf::from("/project/node_modules/pkg/index.ts"),
            &["node_modules".to_string()],
            &config_dir
        ));

        // Test pattern exclusion
        assert!(is_excluded(
            &PathBuf::from("/project/src/file.test.ts"),
            &["**/*.test.ts".to_string()],
            &config_dir
        ));

        // Test non-excluded file
        assert!(!is_excluded(
            &PathBuf::from("/project/src/index.ts"),
            &["node_modules".to_string()],
            &config_dir
        ));
    }
}
