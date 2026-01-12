use std::path::{Path, PathBuf};

use std::fs;

use crate::node_resolve::lib::resolve_from;
use crate::parser::types::Alias;
use crate::utils::alias::match_alias_pattern;
use crate::utils::path::join_paths;
use crate::utils::workspace::{resolve_workspace_import, WorkspaceMap};

pub async fn append_suffix(
    request: &str,
    extensions: &[String],
) -> Result<Option<String>, Box<dyn std::error::Error>> {
    for ext in extensions {
        let path_with_ext = format!("{}{}", request, ext);
        match fs::metadata(&path_with_ext) {
            Ok(metadata) => {
                if metadata.is_file() {
                    return Ok(Some(path_with_ext));
                }
            }
            Err(_) => {}
        }
    }

    // If request is a directory, try adding index suffix recursively
    match fs::metadata(request) {
        Ok(metadata) => {
            if metadata.is_dir() {
                let index_path = PathBuf::from(request).join("index");
                return append_suffix_boxed(&index_path.to_string_lossy(), extensions).await;
            }
        }
        Err(_) => {}
    }

    Ok(None)
}

async fn append_suffix_boxed(
    request: &str,
    extensions: &[String],
) -> Result<Option<String>, Box<dyn std::error::Error>> {
    Box::pin(append_suffix(request, extensions)).await
}

pub async fn simple_resolver(
    context: &str,
    request: &str,
    extensions: &Vec<String>,
    alias: Option<&Alias>,
    workspace_map: Option<&WorkspaceMap>,
) -> Result<Option<String>, Box<dyn std::error::Error>> {
    // 1. Try tsconfig path aliases first
    if let Some(alias) = alias {
        let root_str = alias.root.to_string_lossy().to_string();
        for (key, paths) in &alias.paths {
            for path in paths {
                if let Some(new_request) = match_alias_pattern(request, &root_str, key, path) {
                    let result = Box::pin(simple_resolver(
                        context,
                        &new_request,
                        extensions,
                        Some(alias),
                        workspace_map,
                    ))
                    .await?;
                    if result.is_some() {
                        return Ok(result);
                    }
                }
            }
        }

        let request_path = PathBuf::from(request);
        let root_joined = join_paths(&[&alias.root, &request_path]);
        if let Some(resolved) =
            append_suffix(&root_joined.to_string_lossy().into_owned(), extensions).await?
        {
            return Ok(Some(resolved));
        }
    }

    // 2. Handle absolute paths
    if Path::new(&request).is_absolute() {
        let result = append_suffix(request, extensions).await;
        return result;
    }

    // 3. Handle relative paths
    if request.starts_with('.') {
        let new_path = join_paths(&[&context, &request]);
        let result = append_suffix(&new_path.to_string_lossy().into_owned(), extensions).await;
        return result;
    }

    // 4. Try workspace packages (monorepo support)
    if let Some(workspace_map) = workspace_map {
        if let Some(workspace_path) = resolve_workspace_import(request, workspace_map) {
            let workspace_path_str = workspace_path.to_string_lossy().into_owned();
            if let Some(resolved) = append_suffix(&workspace_path_str, extensions).await? {
                return Ok(Some(resolved));
            }
        }
    }

    // 5. Try node_modules resolution
    let base_dir = PathBuf::from(&context);
    let pkg_path = Path::new(&request)
        .join("package.json")
        .to_string_lossy()
        .into_owned();

    // Handle package.json main field
    match resolve_from(&pkg_path, base_dir.clone()) {
        Ok(resolved_path) => {
            let pkg_json: serde_json::Value =
                serde_json::from_str(&fs::read_to_string(&resolved_path)?)?;
            if let Some(main) = pkg_json.get("main").or_else(|| pkg_json.get("module")) {
                let main_path: PathBuf = Path::new(main.as_str().unwrap()).to_path_buf();
                let parent_path: PathBuf = resolved_path.parent().unwrap().to_path_buf();
                let id: PathBuf = join_paths(&[&parent_path, &main_path]);
                return append_suffix(&id.to_string_lossy().into_owned(), extensions).await;
            }
        }
        Err(_) => {}
    }

    match resolve_from(&request, base_dir) {
        Ok(resolved_path) => {
            let result = resolved_path.to_string_lossy().into_owned();
            return Ok(Some(result));
        }
        Err(_) => {}
    }

    Ok(None)
}
