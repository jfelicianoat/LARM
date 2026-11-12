//! Analizador de almacenamiento (P018; spec §43; schema `StorageAnalysis`).
//!
//! Recorre la raíz de una ubicación y clasifica cada fichero:
//!
//! | Categoría | Qué es | Recuperable |
//! |---|---|---|
//! | MODELS | Ficheros registrados en `artifact_location` | Copias repetidas (mismo sha256) de más, solo si todas son `LARM_MANAGED` |
//! | PARTIALS | `staging/<job>/*.partial` | Si su job ya no está vivo |
//! | TEMPORARIES | Otros ficheros de `staging/` y `tmp/` | Si su job ya no está vivo |
//! | CACHES | `cache/` (derivado, regenerable) | Todo, con plan aprobado |
//! | RUNTIMES | `runtimes/` | Desconocido (lo gestiona el registro de runtimes) |
//! | ORPHANS | Blobs del CAS sin registro; ficheros sueltos sin registro | Solo los blobs; lo demás es del usuario |
//!
//! Nunca borra nada: la limpieza va por un plan aprobado.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use larm_domain::artifacts::{ArtifactLocation, Ownership};
use larm_domain::units::DecimalU64;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Category {
    Models,
    Runtimes,
    Caches,
    Partials,
    Temporaries,
    Orphans,
}

/// Categoría del schema `StorageAnalysis.categories[]`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CategoryUsage {
    pub kind: Category,
    pub used_bytes: Option<DecimalU64>,
    pub reclaimable_bytes: Option<DecimalU64>,
    pub resource_ids: Vec<String>,
    pub blocker_codes: Vec<String>,
}

/// `StorageAnalysis` (schema cerrado).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StorageAnalysis {
    pub storage_location_id: String,
    pub volume_key: String,
    pub free_bytes: Option<DecimalU64>,
    pub reserved_bytes: DecimalU64,
    pub categories: Vec<CategoryUsage>,
    pub snapshot_at: String,
}

/// Elemento recuperable (base de un plan de limpieza; sin rutas absolutas).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Reclaimable {
    pub category: Category,
    /// Ruta POSIX relativa a la raíz.
    pub relative_path: String,
    pub size_bytes: u64,
    pub reason: String,
}

/// Fichero de la raíz.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RootFile {
    pub relative: String,
    pub size: u64,
}

/// Lista los ficheros de la raíz sin seguir enlaces ni junctions.
///
/// # Errors
/// E/S.
pub fn list_root(root: &Path) -> std::io::Result<Vec<RootFile>> {
    let mut out = Vec::new();
    let mut stack = vec![(root.to_path_buf(), String::new())];
    while let Some((dir, prefix)) = stack.pop() {
        for entry in std::fs::read_dir(&dir)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            let relative = if prefix.is_empty() {
                name
            } else {
                format!("{prefix}/{name}")
            };
            let meta = std::fs::symlink_metadata(entry.path())?;
            #[cfg(windows)]
            let reparse = {
                use std::os::windows::fs::MetadataExt;
                meta.file_attributes() & 0x400 != 0
            };
            #[cfg(not(windows))]
            let reparse = meta.file_type().is_symlink();
            if reparse {
                continue;
            }
            if meta.is_dir() {
                stack.push((entry.path(), relative));
            } else if meta.is_file() {
                out.push(RootFile {
                    relative,
                    size: meta.len(),
                });
            }
        }
    }
    out.sort_by(|a, b| a.relative.cmp(&b.relative));
    Ok(out)
}

fn top(relative: &str) -> &str {
    relative.split('/').next().unwrap_or("")
}

/// Job dueño de un fichero de `staging/<job>/…`.
fn staging_job(relative: &str) -> Option<&str> {
    let mut parts = relative.split('/');
    (parts.next() == Some("staging"))
        .then(|| parts.next())
        .flatten()
}

/// Acumulado de una categoría: usado, recuperable, ids y bloqueos.
type Tally = (u64, Option<u64>, BTreeSet<String>, BTreeSet<String>);

/// Clasifica los ficheros. `live_jobs` son los jobs no terminados: sus
/// provisionales no se tocan.
#[must_use]
pub fn analyze(
    files: &[RootFile],
    rows: &[ArtifactLocation],
    live_jobs: &BTreeSet<String>,
) -> (BTreeMap<Category, CategoryUsage>, Vec<Reclaimable>) {
    let registered: BTreeMap<&str, Vec<&ArtifactLocation>> =
        rows.iter().fold(BTreeMap::new(), |mut m, r| {
            m.entry(r.relative_path.as_str())
                .or_insert_with(Vec::new)
                .push(r);
            m
        });
    let mut usage: BTreeMap<Category, Tally> = [
        Category::Models,
        Category::Runtimes,
        Category::Caches,
        Category::Partials,
        Category::Temporaries,
        Category::Orphans,
    ]
    .into_iter()
    .map(|c| (c, (0, Some(0), BTreeSet::new(), BTreeSet::new())))
    .collect();
    let mut items = Vec::new();
    let mut runtimes_seen = false;
    let mut add = |cat: Category,
                   size: u64,
                   reclaim: Option<(&str, String)>,
                   ids: &[String],
                   blocker: Option<&str>| {
        let Some(entry) = usage.get_mut(&cat) else {
            return;
        };
        entry.0 += size;
        if let Some((rel, reason)) = reclaim {
            entry.1 = entry.1.map(|r| r + size);
            items.push(Reclaimable {
                category: cat,
                relative_path: rel.to_string(),
                size_bytes: size,
                reason,
            });
        }
        entry.2.extend(ids.iter().cloned());
        if let Some(b) = blocker {
            entry.3.insert(b.to_string());
        }
    };
    // Copias repetidas de un mismo contenido entre ficheros registrados.
    let mut by_hash: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for (path, rs) in &registered {
        if let Some(sha) = rs.iter().find_map(|r| r.sha256.as_deref()) {
            by_hash.entry(sha).or_default().push(path);
        }
    }
    let duplicate_paths: BTreeSet<&str> = by_hash
        .values()
        .flat_map(|paths| paths.iter().skip(1).copied())
        .collect();
    for f in files {
        if let Some(rs) = registered.get(f.relative.as_str()) {
            let ids: Vec<String> = rs.iter().map(|r| r.id.clone()).collect();
            if duplicate_paths.contains(f.relative.as_str()) {
                let managed = rs.iter().all(|r| r.ownership == Ownership::LarmManaged);
                if managed {
                    add(
                        Category::Models,
                        f.size,
                        Some((&f.relative, "duplicate_copy".into())),
                        &ids,
                        None,
                    );
                } else {
                    add(
                        Category::Models,
                        f.size,
                        None,
                        &ids,
                        Some("duplicate_not_larm_managed"),
                    );
                }
            } else {
                add(Category::Models, f.size, None, &ids, None);
            }
            continue;
        }
        match top(&f.relative) {
            "staging" => {
                let live = staging_job(&f.relative).is_some_and(|j| live_jobs.contains(j));
                let cat = if f.relative.ends_with(".partial") {
                    Category::Partials
                } else {
                    Category::Temporaries
                };
                if live {
                    add(cat, f.size, None, &[], Some("job_active"));
                } else {
                    add(
                        cat,
                        f.size,
                        Some((&f.relative, "job_finished".into())),
                        &[],
                        None,
                    );
                }
            }
            "tmp" => add(
                Category::Temporaries,
                f.size,
                Some((&f.relative, "temporary".into())),
                &[],
                None,
            ),
            "cache" => add(
                Category::Caches,
                f.size,
                Some((&f.relative, "derived_cache".into())),
                &[],
                None,
            ),
            "runtimes" => {
                runtimes_seen = true;
                add(Category::Runtimes, f.size, None, &[], None);
            }
            "blobs" => add(
                Category::Orphans,
                f.size,
                Some((&f.relative, "unreferenced_blob".into())),
                &[],
                None,
            ),
            // Vistas y manifiestos son de LARM y se regeneran; no son huérfanos.
            "views" | "manifests" => add(Category::Models, f.size, None, &[], None),
            _ => add(
                Category::Orphans,
                f.size,
                None,
                &[],
                Some("not_owned_by_larm"),
            ),
        }
    }
    // Lo de los runtimes lo decide su registro: recuperable desconocido.
    if runtimes_seen {
        if let Some(e) = usage.get_mut(&Category::Runtimes) {
            e.1 = None;
        }
    }
    let usage = usage
        .into_iter()
        .map(|(kind, (used, reclaimable, ids, blockers))| {
            (
                kind,
                CategoryUsage {
                    kind,
                    used_bytes: DecimalU64::new(used),
                    reclaimable_bytes: reclaimable.and_then(DecimalU64::new),
                    resource_ids: ids.into_iter().collect(),
                    blocker_codes: blockers.into_iter().collect(),
                },
            )
        })
        .collect();
    items.sort();
    (usage, items)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use larm_domain::artifacts::ArtifactState;

    fn row(id: &str, path: &str, sha: &str, ownership: Ownership) -> ArtifactLocation {
        ArtifactLocation {
            id: id.into(),
            created_at: String::new(),
            updated_at: String::new(),
            row_version: 1,
            metadata: serde_json::Map::new(),
            artifact_id: format!("a-{id}"),
            storage_location_id: "l".into(),
            relative_path: path.into(),
            file_size: DecimalU64::new(10),
            sha256: Some(sha.repeat(64)),
            state: ArtifactState::Online,
            verified_at: None,
            ownership,
        }
    }

    fn f(relative: &str, size: u64) -> RootFile {
        RootFile {
            relative: relative.into(),
            size,
        }
    }

    #[test]
    fn categorias_y_recuperable() {
        let files = vec![
            f("modelos/a.gguf", 10),
            f("modelos/copia.gguf", 10),
            f("ext/b.gguf", 7),
            f("ext/b-copia.gguf", 7),
            f("staging/job-vivo/x.partial", 100),
            f("staging/job-muerto/y.partial", 50),
            f("staging/job-muerto/notas.tmp", 3),
            f("tmp/conv.bin", 4),
            f("cache/kv.bin", 20),
            f("runtimes/llama/llama-server.exe", 30),
            f("blobs/sha256/cc/cccc", 8),
            f("fotos/vacaciones.jpg", 1000),
            f("manifests/v.json", 1),
        ];
        let rows = vec![
            row("r1", "modelos/a.gguf", "a", Ownership::LarmManaged),
            row("r2", "modelos/copia.gguf", "a", Ownership::LarmManaged),
            row("r3", "ext/b.gguf", "b", Ownership::External),
            row("r4", "ext/b-copia.gguf", "b", Ownership::External),
        ];
        let live: BTreeSet<String> = ["job-vivo".to_string()].into_iter().collect();
        let (usage, items) = analyze(&files, &rows, &live);
        let u = |c: Category| usage[&c].clone();
        let n = |d: Option<DecimalU64>| d.map(DecimalU64::get);
        assert_eq!(
            (
                n(u(Category::Models).used_bytes),
                n(u(Category::Models).reclaimable_bytes)
            ),
            (Some(35), Some(10))
        );
        assert_eq!(
            u(Category::Models).blocker_codes,
            vec!["duplicate_not_larm_managed".to_string()]
        );
        assert_eq!(u(Category::Models).resource_ids.len(), 4);
        assert_eq!(
            (
                n(u(Category::Partials).used_bytes),
                n(u(Category::Partials).reclaimable_bytes)
            ),
            (Some(150), Some(50))
        );
        assert_eq!(
            u(Category::Partials).blocker_codes,
            vec!["job_active".to_string()]
        );
        assert_eq!(n(u(Category::Temporaries).reclaimable_bytes), Some(7));
        assert_eq!(n(u(Category::Caches).reclaimable_bytes), Some(20));
        assert_eq!(
            (
                n(u(Category::Runtimes).used_bytes),
                u(Category::Runtimes).reclaimable_bytes
            ),
            (Some(30), None)
        );
        assert_eq!(
            (
                n(u(Category::Orphans).used_bytes),
                n(u(Category::Orphans).reclaimable_bytes)
            ),
            (Some(1008), Some(8))
        );
        assert_eq!(
            u(Category::Orphans).blocker_codes,
            vec!["not_owned_by_larm".to_string()]
        );
        let paths: Vec<&str> = items.iter().map(|i| i.relative_path.as_str()).collect();
        assert!(paths.contains(&"modelos/copia.gguf") && paths.contains(&"blobs/sha256/cc/cccc"));
        assert!(
            !paths.contains(&"staging/job-vivo/x.partial"),
            "lo de un job vivo no se toca"
        );
        assert!(
            !paths.contains(&"fotos/vacaciones.jpg"),
            "lo del usuario nunca es recuperable"
        );
        assert!(
            !paths.contains(&"ext/b-copia.gguf"),
            "una copia EXTERNAL no se limpia"
        );
        // Determinista.
        assert_eq!(analyze(&files, &rows, &live).1, items);
    }
}
