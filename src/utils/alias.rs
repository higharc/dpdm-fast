use dashmap::DashMap;
use lazy_static::lazy_static;
use regex::Regex;
use std::sync::Arc;

use super::path::join_paths;

lazy_static! {
    static ref CACHE: Arc<DashMap<String, Option<String>>> = Arc::new(DashMap::new());
    static ref REGEX_CACHE: Arc<DashMap<String, Regex>> = Arc::new(DashMap::new());
}

/// Transforms an import source using a path alias pattern.
/// 
/// This function only performs pattern matching and path transformation.
/// It does NOT check if the resulting file exists - that's the caller's responsibility
/// (using proper extension and index file resolution).
/// 
/// # Arguments
/// * `source` - The import specifier (e.g., "@/components/Button")
/// * `root` - The base URL/root directory for path resolution
/// * `alias` - The alias pattern (e.g., "@/*" or "config")
/// * `path` - The mapped path pattern (e.g., "./src/*" or "./src/config.ts")
/// 
/// # Returns
/// * `Some(transformed_path)` if the source matches the alias pattern
/// * `None` if the source doesn't match the alias pattern
pub fn match_alias_pattern(source: &str, root: &str, alias: &str, path: &str) -> Option<String> {
    let cache_key = format!("{}|{}|{}|{}", source, root, alias, path);

    if let Some(cached_result) = CACHE.get(&cache_key) {
        return cached_result.clone();
    }

    // Step 1: Create regex to match alias pattern, replacing `*` with `(.*)`
    let alias_regex = REGEX_CACHE.entry(alias.to_string()).or_insert_with(|| {
        let alias_regex_str = regex::escape(alias).replace(r"\*", r"(.*)");
        Regex::new(&format!("^{}$", alias_regex_str)).unwrap()
    });

    // Step 2: Check if source matches the alias pattern
    if let Some(captures) = alias_regex.captures(source) {
        // If matched, get the wildcard capture (empty string if no wildcard)
        let wildcard_part = captures.get(1).map_or("", |m| m.as_str());

        // Step 3: Replace `*` in the path with the captured wildcard part
        let transformed_path = path.replace('*', wildcard_part);

        // Step 4: Join root and transformed_path to create full path
        let full_path = join_paths(&[root, &transformed_path]);
        let full_path_str = full_path.to_string_lossy().to_string();

        // NOTE: We intentionally do NOT check file existence here.
        // The caller (simple_resolver) will handle extension resolution
        // (e.g., trying .ts, .tsx, /index.ts, etc.)

        CACHE.insert(cache_key, Some(full_path_str.clone()));

        return Some(full_path_str);
    }

    // Source doesn't match this alias pattern
    CACHE.insert(cache_key, None);
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// Helper to normalize path separators for cross-platform comparison
    fn normalize_path(p: &str) -> PathBuf {
        PathBuf::from(p)
    }

    #[test]
    fn test_match_alias_pattern_with_wildcard() {
        let result = match_alias_pattern("@/components/Button", "/User/App", "@/*", "./src/*");
        assert!(result.is_some());
        assert_eq!(
            PathBuf::from(result.unwrap()),
            normalize_path("/User/App/src/components/Button")
        );
    }

    #[test]
    fn test_match_alias_pattern_source_without_wildcard() {
        assert_eq!(
            match_alias_pattern("./components/Button", "/User/App", "@/*", "./src/*"),
            None
        );
        assert_eq!(
            match_alias_pattern("react", "/User/App", "@/*", "./src/*"),
            None
        );
    }

    #[test]
    fn test_match_alias_pattern_with_long_alias() {
        let result = match_alias_pattern(
            "@/components/Button",
            "/User/App",
            "@/components/*",
            "./src/*",
        );
        assert!(result.is_some());
        assert_eq!(
            PathBuf::from(result.unwrap()),
            normalize_path("/User/App/src/Button")
        );
    }

    #[test]
    fn test_match_alias_pattern_with_like_alias_in_path() {
        let result =
            match_alias_pattern("@/components/Button_@/A.js", "/User/App", "@/*", "./src/*");
        assert!(result.is_some());
        assert_eq!(
            PathBuf::from(result.unwrap()),
            normalize_path("/User/App/src/components/Button_@/A.js")
        );
    }

    #[test]
    fn test_match_alias_pattern_with_all_match_regex() {
        let result = match_alias_pattern("components/Button", "/User/App", "*", "./src/*");
        assert!(result.is_some());
        assert_eq!(
            PathBuf::from(result.unwrap()),
            normalize_path("/User/App/src/components/Button")
        );
    }

    #[test]
    fn test_match_alias_pattern_exact_match_no_wildcard() {
        // Test exact pattern matching (no wildcard in alias)
        let result = match_alias_pattern("config", "/User/App", "config", "./src/config.ts");
        assert!(result.is_some());
        assert_eq!(
            PathBuf::from(result.unwrap()),
            normalize_path("/User/App/src/config.ts")
        );
    }

    #[test]
    fn test_match_alias_pattern_exact_match_no_match() {
        // Non-matching exact pattern
        let result = match_alias_pattern("other", "/User/App", "config", "./src/config.ts");
        assert!(result.is_none());
    }
}
