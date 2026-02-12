mod ast;
mod diag;
mod lexer;
mod parser;
mod sql;
mod migrate;
mod query_sql;
mod runner;
mod server;
mod db;
mod config;
mod migrations;
mod schema_snapshot;
mod project;
mod manifest;
mod scaffold;

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand, ValueEnum};

#[derive(Copy, Clone, Debug, Eq, PartialEq, ValueEnum)]
enum NewTemplateArg {
    Minimal,
    Api,
}

#[derive(Parser)]
#[command(name = "jwc", version, about = "JWC (Just Web Code) prototype CLI")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Create a new JWC project scaffold
    New {
        /// Target directory to create
        path: PathBuf,
        /// Optional project name for jwcproj.json
        #[arg(long)]
        name: Option<String>,
        /// Project template (default: minimal)
        #[arg(long, value_enum, default_value_t = NewTemplateArg::Minimal)]
        template: NewTemplateArg,
    },
    /// Parse and validate a .jwc source file
    Check { file: PathBuf },
    /// Generate PostgreSQL CREATE TABLE SQL from entity declarations
    GenSql { file: PathBuf },
    /// Generate PostgreSQL migration SQL by diffing two JWC files (old -> new)
    DiffSql { old: PathBuf, new: PathBuf },

    /// Create a new migration file by diffing current schema vs local snapshot
    MigrateAdd {
        file: PathBuf,
        /// Human name for the migration (used in filename)
        #[arg(long)]
        name: String,
        /// Optional path to config file (config.json)
        #[arg(long)]
        config: Option<PathBuf>,
    },

    /// Print the SQL that would be generated (current schema vs local snapshot)
    MigratePlan {
        file: PathBuf,
        /// Optional path to config file (config.json)
        #[arg(long)]
        config: Option<PathBuf>,
    },

    /// Apply pending local migrations from the migrations folder to Postgres
    MigrateApply {
        file: PathBuf,
        /// Optional path to config file (config.json)
        #[arg(long)]
        config: Option<PathBuf>,
        /// Postgres connection string (overrides config + env + dbcontext)
        #[arg(long)]
        db_url: Option<String>,
        /// Pick dbcontext by name (overrides config)
        #[arg(long)]
        dbcontext: Option<String>,
    },

    /// List applied migrations in Postgres (__jwc_migrations)
    MigrateStatus {
        /// A JWC file to read dbcontext url from (optional, but recommended)
        file: Option<PathBuf>,
        /// Optional path to config file (config.json)
        #[arg(long)]
        config: Option<PathBuf>,
        /// Postgres connection string (overrides env + dbcontext)
        #[arg(long)]
        db_url: Option<String>,
        /// Pick dbcontext by name (if dbcontext provides url)
        #[arg(long)]
        dbcontext: Option<String>,
        /// Limit number of rows printed
        #[arg(long, default_value_t = 50)]
        limit: usize,
    },
    /// Generate PostgreSQL SELECT SQL for parsed select statements
    GenQuerySql { file: PathBuf },
    /// Run a minimal JWC program (supports function main() with print statements)
    Run { file: PathBuf },
    /// Start a minimal HTTP server from a JWC project manifest (jwcproj.json)
    Serve {
        /// Project directory or manifest path (defaults to current directory)
        project: Option<PathBuf>,
        #[arg(long, default_value_t = 3000)]
        port: u16,
        /// Optional path to config file (config.json)
        #[arg(long)]
        config: Option<PathBuf>,
        /// Postgres connection string (overrides config + env + dbcontext)
        #[arg(long)]
        db_url: Option<String>,
        /// Pick dbcontext by name (overrides config)
        #[arg(long)]
        dbcontext: Option<String>,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Command::New {
            path,
            name,
            template,
        } => {
            let template = match template {
                NewTemplateArg::Minimal => scaffold::NewTemplate::Minimal,
                NewTemplateArg::Api => scaffold::NewTemplate::Api,
            };
            scaffold::create_new_project(&path, name.as_deref(), template)?;
            println!("Created JWC project: {}", path.display());
            println!("Next: jwc serve {} --port 3000", path.display());
        }
        Command::Check { file } => {
            let _loaded = project::load_program_from_path(&file)?;
            println!("OK");
        }
        Command::GenSql { file } => {
            let loaded = project::load_schema_program_from_path(&file)?;
            let sql = sql::generate_postgres_schema_sql(&loaded.program)?;
            print!("{}", sql);
        }
        Command::DiffSql { old, new } => {
            let old_loaded = project::load_schema_program_from_path(&old)?;
            let new_loaded = project::load_schema_program_from_path(&new)?;
            let sql = migrate::generate_postgres_migration_sql(&old_loaded.program, &new_loaded.program)?;
            print!("{}", sql);
        }

        Command::MigrateAdd { file, name, config } => {
            let loaded = project::load_schema_program_from_path(&file)?;
            let base_dir = loaded.project_dir.clone();
            let config = config.or(loaded.config_path.clone());
            let mut cfg = config::load_config_in_dir(&base_dir, config)?;
            if cfg.database_url.is_none() {
                cfg.database_url = loaded.inline_config.database_url.clone();
            }
            if cfg.dbcontext.is_none() {
                cfg.dbcontext = loaded.inline_config.dbcontext.clone();
            }
            if cfg.migrations_dir.is_none() {
                cfg.migrations_dir = loaded.inline_config.migrations_dir.clone();
            }
            if loaded.migrations_dir.is_some() {
                cfg.migrations_dir = loaded.migrations_dir.clone();
            }
            let (id, sql_path) =
                migrations::create_migration(&base_dir, &cfg, &name, &loaded.program)?;
            println!("Created migration: {}", id);
            println!("SQL: {}", sql_path.display());
        }

        Command::MigratePlan { file, config } => {
            let loaded = project::load_schema_program_from_path(&file)?;
            let base_dir = loaded.project_dir.clone();
            let config = config.or(loaded.config_path.clone());
            let mut cfg = config::load_config_in_dir(&base_dir, config)?;
            if cfg.database_url.is_none() {
                cfg.database_url = loaded.inline_config.database_url.clone();
            }
            if cfg.dbcontext.is_none() {
                cfg.dbcontext = loaded.inline_config.dbcontext.clone();
            }
            if cfg.migrations_dir.is_none() {
                cfg.migrations_dir = loaded.inline_config.migrations_dir.clone();
            }
            if loaded.migrations_dir.is_some() {
                cfg.migrations_dir = loaded.migrations_dir.clone();
            }
            let new_program = loaded.program;

            let paths = migrations::migration_paths(&base_dir, &cfg);

            let prev = schema_snapshot::load_snapshot(&paths.snapshot_path)?;
            let old_program = match prev {
                Some(s) => s.to_program(),
                None => ast::Program::new(),
            };

            let sql = migrate::generate_postgres_migration_sql(&old_program, &new_program)?;
            print!("{}", sql);
        }

        Command::MigrateApply {
            file,
            config,
            db_url,
            dbcontext,
        } => {
            let loaded = project::load_schema_program_from_path(&file)?;
            let base_dir = loaded.project_dir.clone();
            let config = config.or(loaded.config_path.clone());
            let mut cfg = config::load_config_in_dir(&base_dir, config)?;
            if cfg.database_url.is_none() {
                cfg.database_url = loaded.inline_config.database_url.clone();
            }
            if cfg.dbcontext.is_none() {
                cfg.dbcontext = loaded.inline_config.dbcontext.clone();
            }
            if cfg.migrations_dir.is_none() {
                cfg.migrations_dir = loaded.inline_config.migrations_dir.clone();
            }
            if loaded.migrations_dir.is_some() {
                cfg.migrations_dir = loaded.migrations_dir.clone();
            }
            let program = loaded.program;

            let paths = migrations::migration_paths(&base_dir, &cfg);

            let db_url = db::resolve_db_url(
                &program,
                db_url.or(cfg.database_url),
                dbcontext.or(cfg.dbcontext),
            )?;

            let files = migrations::list_local_migration_sql_files(&paths.dir)?;
            if files.is_empty() {
                println!("No local migrations found in {}", paths.dir.display());
                return Ok(());
            }

            let mut applied = 0usize;
            for p in files {
                let name = migrations::migration_name_from_sql_path(&p)?;
                let sql = std::fs::read_to_string(&p)
                    .with_context(|| format!("Failed to read {}", p.display()))?;
                match db::apply_postgres_migration_outcome(&db_url, &name, &sql)? {
                    db::ApplyOutcome::Applied => {
                        applied += 1;
                        println!("Applied: {}", name);
                    }
                    db::ApplyOutcome::AlreadyApplied => {
                        println!("Skipped (already applied): {}", name);
                    }
                }
            }

            println!("Done. Applied {} migrations.", applied);
        }

        Command::MigrateStatus {
            file,
            config,
            db_url,
            dbcontext,
            limit,
        } => {
            let (_base_dir, cfg, program) = if let Some(file) = file {
                let loaded = project::load_schema_program_from_path(&file)?;
                let base_dir = loaded.project_dir.clone();
                let config = config.or(loaded.config_path.clone());
                let mut cfg = config::load_config_in_dir(&base_dir, config)?;
                if cfg.database_url.is_none() {
                    cfg.database_url = loaded.inline_config.database_url.clone();
                }
                if cfg.dbcontext.is_none() {
                    cfg.dbcontext = loaded.inline_config.dbcontext.clone();
                }
                if cfg.migrations_dir.is_none() {
                    cfg.migrations_dir = loaded.inline_config.migrations_dir.clone();
                }
                if loaded.migrations_dir.is_some() {
                    cfg.migrations_dir = loaded.migrations_dir.clone();
                }
                (base_dir, cfg, loaded.program)
            } else {
                let base_dir = PathBuf::from(".");
                let cfg = config::load_config_in_dir(&base_dir, config)?;
                (base_dir, cfg, crate::ast::Program::new())
            };

            let db_url = db::resolve_db_url(
                &program,
                db_url.or(cfg.database_url),
                dbcontext.or(cfg.dbcontext),
            )?;
            let migrations = db::list_postgres_migrations(&db_url, limit)?;

            if migrations.is_empty() {
                println!("No migrations applied.");
                return Ok(());
            }

            for m in migrations {
                println!("{}\t{}\t{}", m.applied_at, m.name, m.sql_sha256);
            }
        }
        Command::GenQuerySql { file } => {
            let loaded = project::load_program_from_path(&file)?;
            let sql = query_sql::generate_postgres_queries_sql(&loaded.program)?;
            print!("{}", sql);
        }
        Command::Run { file } => {
            let loaded = project::load_program_from_path(&file)?;
            let output = runner::run_main(&loaded.program)?;
            print!("{}", output);
        }
        Command::Serve {
            project,
            port,
            config,
            db_url,
            dbcontext,
        } => {
            let project = project.unwrap_or_else(|| PathBuf::from("."));
            let project_manifest = if project.is_dir() {
                project.join("jwcproj.json")
            } else {
                project.clone()
            };

            if !manifest::is_manifest_path(&project_manifest) {
                anyhow::bail!(
                    "jwc serve expects a project manifest (jwcproj.json) or a project directory containing it. Got: {}",
                    project_manifest.display()
                );
            }

            let loaded = project::load_program_from_path(&project_manifest)?;
            let base_dir = loaded.project_dir.clone();
            let config = config.or(loaded.config_path.clone());
            let mut cfg = config::load_config_in_dir(&base_dir, config)?;
            if cfg.database_url.is_none() {
                cfg.database_url = loaded.inline_config.database_url.clone();
            }
            if cfg.dbcontext.is_none() {
                cfg.dbcontext = loaded.inline_config.dbcontext.clone();
            }
            if cfg.migrations_dir.is_none() {
                cfg.migrations_dir = loaded.inline_config.migrations_dir.clone();
            }
            if loaded.migrations_dir.is_some() {
                cfg.migrations_dir = loaded.migrations_dir.clone();
            }

            // Optional: only resolve DB url if one is available from flags/config/env/dbcontext.
            let resolved = match db::resolve_db_url(
                &loaded.program,
                db_url.or(cfg.database_url),
                dbcontext.or(cfg.dbcontext),
            ) {
                Ok(u) => Some(u),
                Err(e) => {
                    let msg = e.to_string();
                    if msg.contains("No database url provided") {
                        None
                    } else {
                        return Err(e);
                    }
                }
            };

            server::serve_with_db_url(&loaded.program, port, resolved)?;
        }
    }

    Ok(())
}
