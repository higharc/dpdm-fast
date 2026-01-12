use glob::glob;
use serde_json::Value;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use super::path::join_paths;

/// A map from workspace package names to their directory paths
pub type WorkspaceMap = HashMap<String, PathBuf>;

/// Find the root package.json by walking up from the given directory.
/// Returns the path to package.json and its parent directory.
fn find_root_package_json(start_dir: &Path) -> Option<(PathBuf, PathBuf)> {
    let mut current = start_dir.to_path_buf();

    loop {
        let package_json = current.join("package.json");
        if package_json.exists() {
            // Check if this package.json has a workspaces field
            if let Ok(content) = fs::read_to_string(&package_json) {
                if let Ok(json) = serde_json::from_str::<Value>(&content) {
                    if json.get("workspaces").is_some() {
                        return Some((package_json, current));
                    }
                }
            }
        }

        // Move up to parent directory
        match current.parent() {
            Some(parent) => current = parent.to_path_buf(),
            None => break,
        }
    }

    None
}

/// Parse the workspaces field from package.json.
/// Supports both array format and object format (with "packages" key).
fn parse_workspaces_field(json: &Value) -> Vec<String> {
    match json.get("workspaces") {
        Some(Value::Array(arr)) => arr
            .iter()
            .filter_map(|v| v.as_str().map(String::from))
            .collect(),
        Some(Value::Object(obj)) => {
            // Handle { "packages": [...] } format (used by some tools)
            if let Some(Value::Array(arr)) = obj.get("packages") {
                arr.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            } else {
                vec![]
            }
        }
        _ => vec![],
    }
}

/// Expand a workspace pattern (which may contain globs) to actual directories.
fn expand_workspace_pattern(root_dir: &Path, pattern: &str) -> Vec<PathBuf> {
    let full_pattern = root_dir.join(pattern);
    let mut pattern_str = full_pattern.to_string_lossy().to_string();
    
    // Strip Windows extended-length path prefix (\\?\) as glob doesn't handle it
    if pattern_str.starts_with(r"\\?\") {
        pattern_str = pattern_str[4..].to_string();
    }

    // If pattern contains glob characters, expand it
    if pattern.contains('*') {
        match glob(&pattern_str) {
            Ok(paths) => {
                paths
                    .filter_map(Result::ok)
                    .filter(|p| p.is_dir())
                    .collect()
            },
            Err(_) => vec![],
        }
    } else {
        // Direct path, check if it exists
        let path = root_dir.join(pattern);
        if path.is_dir() {
            vec![path]
        } else {
            vec![]
        }
    }
}

/// Read a workspace directory's package.json and extract its name.
fn get_package_name(workspace_dir: &Path) -> Option<String> {
    let package_json = workspace_dir.join("package.json");

    if !package_json.exists() {
        return None;
    }

    let content = fs::read_to_string(&package_json).ok()?;
    let json: Value = serde_json::from_str(&content).ok()?;

    json.get("name")
        .and_then(|n| n.as_str())
        .map(String::from)
}

/// Detect workspace packages starting from the given directory.
/// Returns a map from package names to their directories.
///
/// # Arguments
/// * `start_dir` - Directory to start searching from (usually tsconfig directory)
///
/// # Returns
/// * `Some(WorkspaceMap)` - Map of package names to directories
/// * `None` - If no workspace root was found
pub fn detect_workspaces(start_dir: &Path) -> Option<WorkspaceMap> {
    // Find the root package.json with workspaces
    let (package_json_path, root_dir) = find_root_package_json(start_dir)?;

    // Read and parse package.json
    let content = fs::read_to_string(&package_json_path).ok()?;
    let json: Value = serde_json::from_str(&content).ok()?;

    // Get workspace patterns
    let patterns = parse_workspaces_field(&json);

    if patterns.is_empty() {
        return None;
    }

    let mut workspace_map = WorkspaceMap::new();

    // Process each pattern
    for pattern in &patterns {
        let workspace_dirs = expand_workspace_pattern(&root_dir, pattern);

        for dir in workspace_dirs {
            if let Some(name) = get_package_name(&dir) {
                workspace_map.insert(name, dir);
            }
        }
    }

    if workspace_map.is_empty() {
        None
    } else {
        Some(workspace_map)
    }
}

/// Check if an import path matches a workspace package and return the resolved path.
///
/// # Arguments
/// * `import_path` - The import specifier (e.g., "common/constants/env")
/// * `workspace_map` - Map of package names to directories
///
/// # Returns
/// * `Some(resolved_path)` - The resolved path if this is a workspace import
/// * `None` - If this doesn't match any workspace package
pub fn resolve_workspace_import(import_path: &str, workspace_map: &WorkspaceMap) -> Option<PathBuf> {
    // Skip relative and absolute paths
    if import_path.starts_with('.') || import_path.starts_with('/') {
        return None;
    }

    // Handle scoped packages (@org/package/path)
    let (package_name, subpath) = if import_path.starts_with('@') {
        // Scoped package: @org/package/subpath
        let parts: Vec<&str> = import_path.splitn(3, '/').collect();
        if parts.len() >= 2 {
            let scope_and_name = format!("{}/{}", parts[0], parts[1]);
            let subpath = if parts.len() > 2 { parts[2] } else { "" };
            (scope_and_name, subpath)
        } else {
            return None;
        }
    } else {
        // Regular package: package/subpath
        let parts: Vec<&str> = import_path.splitn(2, '/').collect();
        let package_name = parts[0].to_string();
        let subpath = if parts.len() > 1 { parts[1] } else { "" };
        (package_name, subpath)
    };

    // Check if this package is in our workspace map
    let workspace_dir = workspace_map.get(&package_name)?;

    // Build the full path
    if subpath.is_empty() {
        Some(workspace_dir.clone())
    } else {
        Some(join_paths(&[workspace_dir, &PathBuf::from(subpath)]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_workspaces_array() {
        let json: Value = serde_json::json!({
            "workspaces": ["packages/*", "lib", "apps/*"]
        });
        let result = parse_workspaces_field(&json);
        assert_eq!(result, vec!["packages/*", "lib", "apps/*"]);
    }

    #[test]
    fn test_parse_workspaces_object() {
        let json: Value = serde_json::json!({
            "workspaces": {
                "packages": ["packages/*", "lib"]
            }
        });
        let result = parse_workspaces_field(&json);
        assert_eq!(result, vec!["packages/*", "lib"]);
    }

    #[test]
    fn test_parse_workspaces_missing() {
        let json: Value = serde_json::json!({
            "name": "my-project"
        });
        let result = parse_workspaces_field(&json);
        assert!(result.is_empty());
    }

    #[test]
    fn test_resolve_workspace_import_simple() {
        let mut map = WorkspaceMap::new();
        map.insert("common".to_string(), PathBuf::from("/project/packages/common"));
        map.insert("lib".to_string(), PathBuf::from("/project/lib"));

        // Test simple package import
        let result = resolve_workspace_import("common", &map);
        assert_eq!(result, Some(PathBuf::from("/project/packages/common")));

        // Test subpath import
        let result = resolve_workspace_import("common/constants/env", &map);
        assert!(result.is_some());
        let path = result.unwrap();
        assert!(path.to_string_lossy().contains("common"));
        assert!(path.to_string_lossy().contains("constants"));
    }

    #[test]
    fn test_resolve_workspace_import_scoped() {
        let mut map = WorkspaceMap::new();
        map.insert("@org/utils".to_string(), PathBuf::from("/project/packages/utils"));

        let result = resolve_workspace_import("@org/utils/helper", &map);
        assert!(result.is_some());
        let path = result.unwrap();
        assert!(path.to_string_lossy().contains("utils"));
        assert!(path.to_string_lossy().contains("helper"));
    }

    #[test]
    fn test_resolve_workspace_import_not_found() {
        let map = WorkspaceMap::new();

        // Non-workspace package
        let result = resolve_workspace_import("react", &map);
        assert!(result.is_none());

        // Relative path (should be skipped)
        let result = resolve_workspace_import("./local", &map);
        assert!(result.is_none());
    }
}
