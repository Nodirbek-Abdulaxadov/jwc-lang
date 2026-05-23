//! `jwc serve` command implementation split out from main.rs.
//!
//! Default mode loads the project once and hands off to `server::serve`
//! which never returns. `--watch` mode spawns a child `jwc serve` and
//! restarts it whenever a `.jwc` file under the project root changes
//! (via `notify`'s recommended watcher + a 250ms debounce so a noisy
//! editor save doesn't trigger half a dozen restarts).

use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};

use crate::{project, server};

/// Top-level dispatch for `jwc serve [--watch]`.
pub fn run(path: Option<PathBuf>, port: u16, request_logging: bool, watch: bool) -> Result<()> {
    let target = path.clone().unwrap_or(std::env::current_dir()?);
    let root = if target.is_dir() {
        project::find_project_root(&target)?
    } else {
        target
            .parent()
            .ok_or_else(|| anyhow!("Invalid project path"))?
            .to_path_buf()
    };

    if watch {
        serve_with_watch(&root, port, request_logging)
    } else {
        project::load_dotenv(&root);
        let loaded = project::load_project_from_root(&root)?;
        loaded.manifest.ensure_runnable()?;
        server::serve(&loaded.program, port, request_logging)?;
        Ok(())
    }
}

/// `--watch` mode: spawn a child `jwc serve` and restart it whenever a
/// `.jwc` change arrives under `root`. Exits cleanly when the channel
/// closes (parent shutdown).
fn serve_with_watch(root: &Path, port: u16, request_logging: bool) -> Result<()> {
    use notify::{event::EventKind, RecommendedWatcher, RecursiveMode, Watcher};
    use std::process::Command as SysCommand;
    use std::sync::mpsc;
    use std::time::Duration;

    let exe = std::env::current_exe()?;
    let (tx, rx) = mpsc::channel::<notify::Event>();
    let mut watcher: RecommendedWatcher = notify::recommended_watcher(move |res| {
        if let Ok(event) = res {
            let _ = tx.send(event);
        }
    })?;
    watcher.watch(root, RecursiveMode::Recursive)?;

    println!("[watch] Watching {} for .jwc changes", root.display());

    loop {
        let mut cmd = SysCommand::new(&exe);
        cmd.arg("serve")
            .arg("--port")
            .arg(port.to_string())
            .arg(root);
        if request_logging {
            cmd.arg("--request-logging");
        }
        let mut child = cmd
            .spawn()
            .with_context(|| "watch: failed to spawn child")?;
        println!("[watch] Server started (pid {})", child.id());

        // Drain any backlog so the first event after spawn isn't a stale one.
        while rx.try_recv().is_ok() {}

        // Wait until a .jwc change arrives.
        loop {
            let Ok(event) = rx.recv() else {
                let _ = child.kill();
                let _ = child.wait();
                return Ok(());
            };
            if !matches!(
                event.kind,
                EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_)
            ) {
                continue;
            }
            if event.paths.iter().any(|p| is_jwc_path(p)) {
                break;
            }
        }

        println!("[watch] Change detected, restarting server...");
        let _ = child.kill();
        let _ = child.wait();

        // Debounce: drain rapid-fire follow-up events for ~250 ms.
        std::thread::sleep(Duration::from_millis(250));
        while rx.try_recv().is_ok() {}
    }
}

fn is_jwc_path(p: &Path) -> bool {
    p.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("jwc"))
        .unwrap_or(false)
}
