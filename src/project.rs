use std::path::{Path, PathBuf};
use std::collections::BTreeSet;

use anyhow::{anyhow, Context, Result};

use crate::{ast::Program, parser};
use crate::manifest;

pub struct LoadedProgram {
    pub project_dir: PathBuf,
    pub config_path: Option<PathBuf>,
    pub migrations_dir: Option<String>,
    pub project_name: Option<String>,
    pub project_version: Option<String>,
    pub inline_config: crate::config::JwcConfig,
    pub source: String,
    pub program: Program,
}

pub fn load_program_from_path(path: &Path) -> Result<LoadedProgram> {
    if manifest::is_manifest_path(path) {
        return load_program_from_manifest(path);
    }

    if path.is_dir() {
        // Content-based project resolution:
        // 1) If the directory contains a manifest, use it.
        // 2) Else if it contains main.jwc, treat that as the whole project.
        // 3) Else error (we no longer auto-scan directories).
        let p1 = path.join("jwcproj.json");
        if p1.is_file() {
            return load_program_from_manifest(&p1);
        }

        let main = path.join("main.jwc");
        if main.is_file() {
            return load_program_from_path(&main);
        }

        return Err(anyhow!(
            "Directory {} is not a JWC project. Add jwcproj.json or main.jwc",
            path.display()
        ));
    } else {
        let src = std::fs::read_to_string(path)
            .with_context(|| format!("Failed to read {}", path.display()))?;
        let program = parser::parse_program(&src)
            .with_context(|| format!("Parse failed for {}", path.display()))?;
        parser::validate_program(&program)?;

        let project_dir = path
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| PathBuf::from("."));
        Ok(LoadedProgram {
            project_dir,
            config_path: None,
            migrations_dir: None,
            project_name: None,
            project_version: None,
            inline_config: crate::config::JwcConfig::default(),
            source: src,
            program,
        })
    }
}

pub fn load_schema_program_from_path(path: &Path) -> Result<LoadedProgram> {
    if manifest::is_manifest_path(path) {
        return load_schema_program_from_manifest(path);
    }

    if path.is_dir() {
        // Same project resolution rules as load_program_from_path.
        let p1 = path.join("jwcproj.json");
        if p1.is_file() {
            return load_schema_program_from_manifest(&p1);
        }

        let main = path.join("main.jwc");
        if main.is_file() {
            return load_schema_program_from_path(&main);
        }

        return Err(anyhow!(
            "Directory {} is not a JWC project. Add jwcproj.json or main.jwc",
            path.display()
        ));
    }

    let src = std::fs::read_to_string(path)
        .with_context(|| format!("Failed to read {}", path.display()))?;
    let program = parser::parse_program_schema_only(&src)
        .with_context(|| format!("Parse failed for {}", path.display()))?;
    parser::validate_program(&program)?;

    let project_dir = path
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."));
    Ok(LoadedProgram {
        project_dir,
        config_path: None,
        migrations_dir: None,
        project_name: None,
        project_version: None,
        inline_config: crate::config::JwcConfig::default(),
        source: src,
        program,
    })
}

fn load_program_from_manifest(manifest_path: &Path) -> Result<LoadedProgram> {
    let project_dir = manifest_path
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."));

    let m = manifest::JwcProjectManifest::load(manifest_path)?;

    let inline_cfg = crate::config::JwcConfig {
        database_url: m
            .default_connection_string()
            .or(m.database_url.clone()),
        dbcontext: m.dbcontext.clone(),
        migrations_dir: m.migrations_dir.clone(),
    };

    let mut files = Vec::<PathBuf>::new();

    if let Some(list) = &m.files {
        for f in list {
            let p = manifest::resolve_relative(&project_dir, f);
            if p.is_file() {
                files.push(p);
            } else {
                return Err(anyhow!("Manifest file entry not found: {}", p.display()));
            }
        }
    }

    if let Some(dirs) = &m.dirs {
        for d in dirs {
            let p = manifest::resolve_relative(&project_dir, d);
            if p.is_dir() {
                files.extend(collect_jwc_files(&p)?);
            } else {
                return Err(anyhow!("Manifest dir entry not found: {}", p.display()));
            }
        }
    }

    if files.is_empty() {
        // If the manifest doesn't specify sources, use main.jwc (single-file project).
        let main = project_dir.join("main.jwc");
        if main.is_file() {
            files.push(main);
        } else {
            // If there's exactly one .jwc file in the project root, use it.
            // This keeps web apps simple (no forced main.jwc), while still avoiding directory auto-scan.
            let mut root_jwc_files = Vec::<PathBuf>::new();
            for entry in std::fs::read_dir(&project_dir)
                .with_context(|| format!("Failed to read dir {}", project_dir.display()))?
            {
                let entry = entry?;
                let path = entry.path();
                if !path.is_file() {
                    continue;
                }
                if path
                    .extension()
                    .and_then(|e| e.to_str())
                    .map(|e| e.eq_ignore_ascii_case("jwc"))
                    .unwrap_or(false)
                {
                    root_jwc_files.push(path);
                }
            }
            root_jwc_files.sort_by(|a, b| {
                a.file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or("")
                    .cmp(b.file_name().and_then(|s| s.to_str()).unwrap_or(""))
            });

            if root_jwc_files.len() == 1 {
                files.push(root_jwc_files.remove(0));
            } else if root_jwc_files.is_empty() {
                return Err(anyhow!(
                    "Project manifest {} did not specify any sources. Add 'files'/'dirs' or create a .jwc file (or main.jwc)",
                    manifest_path.display()
                ));
            } else {
                return Err(anyhow!(
                    "Project manifest {} did not specify any sources and no main.jwc exists. Found {} .jwc files in project root; please specify 'files' or 'dirs'",
                    manifest_path.display(),
                    root_jwc_files.len()
                ));
            }
        }
    }

    // Dedupe and sort deterministically relative to project root.
    let mut uniq = BTreeSet::<PathBuf>::new();
    for f in files {
        uniq.insert(f);
    }
    let mut files: Vec<PathBuf> = uniq.into_iter().collect();
    files.sort_by(|a, b| {
        let ar = a
            .strip_prefix(&project_dir)
            .unwrap_or(a)
            .to_string_lossy()
            .replace('\\', "/");
        let br = b
            .strip_prefix(&project_dir)
            .unwrap_or(b)
            .to_string_lossy()
            .replace('\\', "/");
        ar.cmp(&br)
    });

    let mut src = String::new();
    for f in &files {
        let rel = f
            .strip_prefix(&project_dir)
            .unwrap_or(f)
            .to_string_lossy()
            .replace('\\', "/");
        let body = std::fs::read_to_string(f)
            .with_context(|| format!("Failed to read {}", f.display()))?;
        src.push_str(&format!("// --- file: {} ---\n", rel));
        src.push_str(&body);
        if !src.ends_with('\n') {
            src.push('\n');
        }
        src.push('\n');
    }

    let program = parser::parse_program(&src).with_context(|| {
        format!(
            "Parse failed for project manifest {}",
            manifest_path.display()
        )
    })?;
    parser::validate_program(&program)?;

    let config_path = m.config.map(|p| manifest::resolve_relative(&project_dir, &p));
    let migrations_dir = m.migrations_dir.clone();

    Ok(LoadedProgram {
        project_dir,
        config_path,
        migrations_dir,
        project_name: m.name,
        project_version: m.version,
        inline_config: inline_cfg,
        source: src,
        program,
    })
}

fn load_schema_program_from_manifest(manifest_path: &Path) -> Result<LoadedProgram> {
    let project_dir = manifest_path
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."));

    let m = manifest::JwcProjectManifest::load(manifest_path)?;

    let inline_cfg = crate::config::JwcConfig {
        database_url: m
            .default_connection_string()
            .or(m.database_url.clone()),
        dbcontext: m.dbcontext.clone(),
        migrations_dir: m.migrations_dir.clone(),
    };

    let mut files = Vec::<PathBuf>::new();

    if let Some(list) = &m.files {
        for f in list {
            let p = manifest::resolve_relative(&project_dir, f);
            if p.is_file() {
                files.push(p);
            } else {
                return Err(anyhow!("Manifest file entry not found: {}", p.display()));
            }
        }
    }

    if let Some(dirs) = &m.dirs {
        for d in dirs {
            let p = manifest::resolve_relative(&project_dir, d);
            if p.is_dir() {
                files.extend(collect_jwc_files(&p)?);
            } else {
                return Err(anyhow!("Manifest dir entry not found: {}", p.display()));
            }
        }
    }

    if files.is_empty() {
        let main = project_dir.join("main.jwc");
        if main.is_file() {
            files.push(main);
        } else {
            let mut root_jwc_files = Vec::<PathBuf>::new();
            for entry in std::fs::read_dir(&project_dir)
                .with_context(|| format!("Failed to read dir {}", project_dir.display()))?
            {
                let entry = entry?;
                let path = entry.path();
                if !path.is_file() {
                    continue;
                }
                if path
                    .extension()
                    .and_then(|e| e.to_str())
                    .map(|e| e.eq_ignore_ascii_case("jwc"))
                    .unwrap_or(false)
                {
                    root_jwc_files.push(path);
                }
            }
            root_jwc_files.sort_by(|a, b| {
                a.file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or("")
                    .cmp(b.file_name().and_then(|s| s.to_str()).unwrap_or(""))
            });

            if root_jwc_files.len() == 1 {
                files.push(root_jwc_files.remove(0));
            } else if root_jwc_files.is_empty() {
                return Err(anyhow!(
                    "Project manifest {} did not specify any sources. Add 'files'/'dirs' or create a .jwc file (or main.jwc)",
                    manifest_path.display()
                ));
            } else {
                return Err(anyhow!(
                    "Project manifest {} did not specify any sources and no main.jwc exists. Found {} .jwc files in project root; please specify 'files' or 'dirs'",
                    manifest_path.display(),
                    root_jwc_files.len()
                ));
            }
        }
    }

    let mut uniq = BTreeSet::<PathBuf>::new();
    for f in files {
        uniq.insert(f);
    }
    let mut files: Vec<PathBuf> = uniq.into_iter().collect();
    files.sort_by(|a, b| {
        let ar = a
            .strip_prefix(&project_dir)
            .unwrap_or(a)
            .to_string_lossy()
            .replace('\\', "/");
        let br = b
            .strip_prefix(&project_dir)
            .unwrap_or(b)
            .to_string_lossy()
            .replace('\\', "/");
        ar.cmp(&br)
    });

    let mut src = String::new();
    for f in &files {
        let rel = f
            .strip_prefix(&project_dir)
            .unwrap_or(f)
            .to_string_lossy()
            .replace('\\', "/");
        let body = std::fs::read_to_string(f)
            .with_context(|| format!("Failed to read {}", f.display()))?;
        src.push_str(&format!("// --- file: {} ---\n", rel));
        src.push_str(&body);
        if !src.ends_with('\n') {
            src.push('\n');
        }
        src.push('\n');
    }

    let program = parser::parse_program_schema_only(&src).with_context(|| {
        format!(
            "Parse failed for project manifest {}",
            manifest_path.display()
        )
    })?;
    parser::validate_program(&program)?;

    let config_path = m.config.map(|p| manifest::resolve_relative(&project_dir, &p));
    let migrations_dir = m.migrations_dir.clone();

    Ok(LoadedProgram {
        project_dir,
        config_path,
        migrations_dir,
        project_name: m.name,
        project_version: m.version,
        inline_config: inline_cfg,
        source: src,
        program,
    })
}

fn collect_jwc_files(root: &Path) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    walk_dir(root, root, &mut out)?;
    out.sort_by(|a, b| {
        let ar = a
            .strip_prefix(root)
            .unwrap_or(a)
            .to_string_lossy()
            .replace('\\', "/");
        let br = b
            .strip_prefix(root)
            .unwrap_or(b)
            .to_string_lossy()
            .replace('\\', "/");
        ar.cmp(&br)
    });
    Ok(out)
}

fn walk_dir(root: &Path, dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    for entry in std::fs::read_dir(dir)
        .with_context(|| format!("Failed to read dir {}", dir.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();

        if path.is_dir() {
            // Skip common noise folders
            if name.starts_with('.')
                || name.eq_ignore_ascii_case("target")
                || name.eq_ignore_ascii_case("migrations")
            {
                continue;
            }
            walk_dir(root, &path, out)?;
            continue;
        }

        if path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.eq_ignore_ascii_case("jwc"))
            .unwrap_or(false)
        {
            out.push(path);
        }
    }

    // Ensure deterministic order per-directory even before final sort
    out.retain(|p| p.exists() && p.is_file());

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn manifest_without_sources_can_use_single_root_jwc_file() {
        let uniq = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("jwc_test_proj_{uniq}"));
        std::fs::create_dir_all(&dir).unwrap();

        let manifest_path = dir.join("jwcproj.json");
        std::fs::write(&manifest_path, r#"{ "name": "t" }"#).unwrap();
        let jwc_path = dir.join("api.jwc");
        std::fs::write(
            &jwc_path,
            r#"route get "/ping" { return "pong"; }"#,
        )
        .unwrap();

        let loaded = load_program_from_path(&manifest_path).unwrap();
        assert_eq!(loaded.program.routes.len(), 1);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
