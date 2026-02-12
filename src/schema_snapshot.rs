use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::ast::{EntityDecl, FieldDecl, FieldMods, Program, TypeSpec};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SchemaSnapshot {
    pub entities: Vec<EntitySnapshot>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntitySnapshot {
    pub name: String,
    pub fields: Vec<FieldSnapshot>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FieldSnapshot {
    pub name: String,
    pub ty: TypeSnapshot,
    pub mods: FieldModsSnapshot,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TypeSnapshot {
    pub name: String,
    pub args: Vec<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct FieldModsSnapshot {
    pub nullable: bool,
    pub unique: bool,
    pub primary_key: bool,
}

impl SchemaSnapshot {
    pub fn from_program(program: &Program) -> Self {
        Self {
            entities: program
                .entities
                .iter()
                .map(|e| EntitySnapshot {
                    name: e.name.clone(),
                    fields: e
                        .fields
                        .iter()
                        .map(|f| FieldSnapshot {
                            name: f.name.clone(),
                            ty: TypeSnapshot {
                                name: f.ty.name.clone(),
                                args: f.ty.args.clone(),
                            },
                            mods: FieldModsSnapshot {
                                nullable: f.mods.nullable,
                                unique: f.mods.unique,
                                primary_key: f.mods.primary_key,
                            },
                        })
                        .collect(),
                })
                .collect(),
        }
    }

    pub fn to_program(&self) -> Program {
        let mut p = Program::new();
        p.entities = self
            .entities
            .iter()
            .map(|e| EntityDecl {
                name: e.name.clone(),
                fields: e
                    .fields
                    .iter()
                    .map(|f| FieldDecl {
                        name: f.name.clone(),
                        ty: TypeSpec {
                            name: f.ty.name.clone(),
                            args: f.ty.args.clone(),
                        },
                        mods: FieldMods {
                            nullable: f.mods.nullable,
                            unique: f.mods.unique,
                            primary_key: f.mods.primary_key,
                        },
                    })
                    .collect(),
            })
            .collect();
        p
    }
}

pub fn load_snapshot(path: &std::path::Path) -> Result<Option<SchemaSnapshot>> {
    if !path.exists() {
        return Ok(None);
    }
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("Failed to read snapshot {}", path.display()))?;
    let snap: SchemaSnapshot = serde_json::from_str(&raw)
        .with_context(|| format!("Failed to parse snapshot {}", path.display()))?;
    Ok(Some(snap))
}

pub fn save_snapshot(path: &std::path::Path, snap: &SchemaSnapshot) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| {
            format!("Failed to create snapshot directory {}", parent.display())
        })?;
    }
    let json = serde_json::to_string_pretty(snap).context("Failed to serialize snapshot")?;
    std::fs::write(path, json)
        .with_context(|| format!("Failed to write snapshot {}", path.display()))?;
    Ok(())
}
