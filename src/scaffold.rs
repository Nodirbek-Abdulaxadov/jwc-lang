use std::path::Path;

use anyhow::{anyhow, bail, Context, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NewTemplate {
    Minimal,
    Api,
}

pub fn create_new_project(target: &Path, name: Option<&str>, template: NewTemplate) -> Result<()> {
    if target.exists() {
        if !target.is_dir() {
            bail!("Target path exists but is not a directory: {}", target.display());
        }
        if target.read_dir()?.next().is_some() {
            bail!(
                "Target directory is not empty: {}. Use an empty directory for `jwc new`",
                target.display()
            );
        }
    } else {
        std::fs::create_dir_all(target)
            .with_context(|| format!("Failed to create directory {}", target.display()))?;
    }

    let project_name = match name {
        Some(n) if !n.trim().is_empty() => n.trim().to_string(),
        _ => target
            .file_name()
            .and_then(|s| s.to_str())
            .map(|s| s.to_string())
            .ok_or_else(|| anyhow!("Could not infer project name from path {}", target.display()))?,
    };

    let manifest_path = target.join("jwcproj.json");
    let main_path = target.join("main.jwc");

    let manifest = format!(
        "{{\n  \"name\": \"{}\",\n  \"version\": \"0.1.0\",\n  \"migrationsDir\": \"migrations\"\n}}\n",
        project_name
    );

    let main = match template {
        NewTemplate::Minimal => r#"context AppDb : Postgres;

entity Todo {
    id int pk;
    title varchar(100);
    done bool;
}

route get "/" {
    return "JWC app is running";
}
"#,
        NewTemplate::Api => r#"context AppDb : Postgres;

entity Todo {
    id int pk;
    title string(120);
    done bool;
}

route get "/" {
    return "Todo API running";
}

route get "/todos" {
    return "[{\"id\":1,\"title\":\"demo\",\"done\":false}]";
}

route post "/todos" {
    return [201, "{\"status\":\"created\"}"];
}
"#,
    };

    std::fs::write(&manifest_path, manifest)
        .with_context(|| format!("Failed to write {}", manifest_path.display()))?;
    std::fs::write(&main_path, main)
        .with_context(|| format!("Failed to write {}", main_path.display()))?;

    std::fs::create_dir_all(target.join("migrations"))
        .with_context(|| format!("Failed to create migrations directory in {}", target.display()))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn creates_new_project_scaffold() {
        let uniq = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("jwc_new_test_{uniq}"));

        create_new_project(&dir, Some("demo-app"), NewTemplate::Minimal).unwrap();

        assert!(dir.join("jwcproj.json").is_file());
        assert!(dir.join("main.jwc").is_file());
        assert!(dir.join("migrations").is_dir());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn creates_api_template_with_todos_route() {
        let uniq = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("jwc_new_api_test_{uniq}"));

        create_new_project(&dir, Some("demo-api"), NewTemplate::Api).unwrap();

        let main = std::fs::read_to_string(dir.join("main.jwc")).unwrap();
        assert!(main.contains("route get \"/todos\""));
        assert!(main.contains("route post \"/todos\""));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
