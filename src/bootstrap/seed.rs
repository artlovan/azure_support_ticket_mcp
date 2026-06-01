//! Embedded seed loader.
//!
//! The seed JSON is baked into the binary at build time and reloaded
//! into the SQLite cache when its `version` doesn't match what's already
//! recorded in `seed_meta`. Optional GitHub Release download is deferred
//! to a later slice; the embedded payload is always the offline fallback.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tracing::{debug, info};

use crate::cache::{now_unix, Cache, SupportServiceRow};
use crate::error::{AppError, AppResult};

/// Bytes embedded at compile time. The file path is resolved relative to
/// this source file so the build is self-contained.
pub const EMBEDDED_SEED: &[u8] = include_bytes!("../../data/support_services_seed.json");

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SeedFile {
    pub version: String,
    pub generated_at: String,
    pub source: String,
    pub services: Vec<SeedService>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SeedService {
    pub service_id: String,
    pub name: String,
    pub display_name: String,
    /// Curated grouping (e.g. "Compute", "Networking"). May be `None` for
    /// entries newly fetched from the live `Microsoft.Support/services` API
    /// before a human has hand-classified them — see `data/README.md` for
    /// the refresh workflow.
    #[serde(default)]
    pub group: Option<String>,
    #[serde(default)]
    pub resource_types: Vec<String>,
    #[serde(default)]
    pub metadata: serde_json::Value,
}

pub fn parse_embedded() -> AppResult<SeedFile> {
    serde_json::from_slice::<SeedFile>(EMBEDDED_SEED)
        .map_err(|e| AppError::Seed(format!("embedded seed parse failed: {e}")))
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    let out = h.finalize();
    out.iter().map(|b| format!("{b:02x}")).collect()
}

pub struct LoadOutcome {
    pub reloaded: bool,
    pub services_count: usize,
    pub version: String,
}

pub async fn load_into_cache_if_needed(cache: &Cache) -> AppResult<LoadOutcome> {
    let seed = parse_embedded()?;
    let cached_version = cache.get_seed_meta("version").await?;
    if cached_version.as_deref() == Some(seed.version.as_str()) {
        debug!(version = %seed.version, "seed already loaded");
        return Ok(LoadOutcome {
            reloaded: false,
            services_count: seed.services.len(),
            version: seed.version,
        });
    }

    info!(
        from = %cached_version.as_deref().unwrap_or("<none>"),
        to = %seed.version,
        services = seed.services.len(),
        "loading embedded seed into cache"
    );
    write_seed_to_cache(cache, &seed).await?;

    let sha = sha256_hex(EMBEDDED_SEED);
    cache.put_seed_meta("version", &seed.version).await?;
    cache.put_seed_meta("sha256", &sha).await?;
    cache.put_seed_meta("source", &seed.source).await?;
    cache
        .put_seed_meta("loaded_at", &now_unix().to_string())
        .await?;

    Ok(LoadOutcome {
        reloaded: true,
        services_count: seed.services.len(),
        version: seed.version,
    })
}

async fn write_seed_to_cache(cache: &Cache, seed: &SeedFile) -> AppResult<()> {
    let now = now_unix();
    let cloud = cache.cloud().to_string();

    // FULL SYNC: delete any seed-origin rows whose service_id is not in the
    // new seed. Without this, an upgrade from a larger seed to a smaller
    // one (e.g. Microsoft pruned 106 deprecated services from
    // Microsoft.Support/services between releases) would leave the dead
    // entries in the cache forever, and the resolver could rank them and
    // surface them to the user.
    //
    // The delete is scoped to `source = 'seed'` inside Cache, so rows
    // inserted by other paths (live ARM fetches by other tools) are never
    // touched.
    let keep_ids: Vec<String> = seed.services.iter().map(|s| s.service_id.clone()).collect();
    let removed = cache.delete_seed_services_not_in(&keep_ids).await?;
    if removed > 0 {
        info!(
            removed,
            "pruned deprecated services from cache (no longer in seed)"
        );
    }

    for s in &seed.services {
        let resource_types_json = if s.resource_types.is_empty() {
            None
        } else {
            Some(serde_json::to_string(&s.resource_types)?)
        };
        let metadata_json = if s.metadata.is_null() {
            None
        } else {
            Some(serde_json::to_string(&s.metadata)?)
        };
        let row = SupportServiceRow {
            cloud: cloud.clone(),
            service_id: s.service_id.clone(),
            name: s.name.clone(),
            display_name: s.display_name.clone(),
            service_group: s.group.clone(),
            resource_types_json,
            metadata_json,
            source: "seed".into(),
            updated_at: now,
            etag: None,
        };
        cache.upsert_support_service(&row).await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_seed_parses() {
        let s = parse_embedded().expect("seed parses");
        assert!(!s.services.is_empty());
        // Sanity bound: catch accidental truncation. The real catalog has
        // hundreds of services; if we ever land a seed with fewer than 100
        // entries it almost certainly means the file got mangled.
        assert!(
            s.services.len() >= 100,
            "embedded seed has only {} services — suspect a truncated file",
            s.services.len()
        );
        assert!(s.services.iter().any(|x| !x.resource_types.is_empty()));
    }

    #[tokio::test]
    async fn load_into_cache_idempotent() {
        let (cache, _path, _tmp) = crate::cache::db::open_in_temp_dir("AzurePublicCloud")
            .await
            .unwrap();
        let first = load_into_cache_if_needed(&cache).await.unwrap();
        assert!(first.reloaded);
        let second = load_into_cache_if_needed(&cache).await.unwrap();
        assert!(!second.reloaded);
        let n = cache.support_services_count().await.unwrap();
        // Seed may contain duplicate service_ids that collapse on upsert; the
        // db count is the floor.
        assert!(n > 0 && (n as usize) <= first.services_count);
    }

    #[tokio::test]
    async fn seed_reload_prunes_services_no_longer_in_new_seed() {
        // Regression test for the cache-staleness bug: when the seed
        // shrinks (e.g. Microsoft prunes deprecated services), the cache
        // must shrink too. Without the delete-sweep in write_seed_to_cache,
        // dead entries linger forever and the resolver could surface them.
        let (cache, _path, _tmp) = crate::cache::db::open_in_temp_dir("AzurePublicCloud")
            .await
            .unwrap();

        // Stage a "large" seed (services A, B, C, D) directly via the
        // writer.
        let large = SeedFile {
            version: "test-large".into(),
            generated_at: "test".into(),
            source: "test".into(),
            services: vec!["A", "B", "C", "D"]
                .into_iter()
                .map(|sid| SeedService {
                    service_id: sid.into(),
                    name: sid.into(),
                    display_name: format!("Service {sid}"),
                    group: Some("test".into()),
                    resource_types: vec![],
                    metadata: serde_json::Value::Null,
                })
                .collect(),
        };
        write_seed_to_cache(&cache, &large).await.unwrap();
        assert_eq!(cache.support_services_count().await.unwrap(), 4);

        // Now write a "small" seed (only A and B remain — C and D were
        // pruned upstream).
        let small = SeedFile {
            version: "test-small".into(),
            generated_at: "test".into(),
            source: "test".into(),
            services: vec!["A", "B"]
                .into_iter()
                .map(|sid| SeedService {
                    service_id: sid.into(),
                    name: sid.into(),
                    display_name: format!("Service {sid}"),
                    group: Some("test".into()),
                    resource_types: vec![],
                    metadata: serde_json::Value::Null,
                })
                .collect(),
        };
        write_seed_to_cache(&cache, &small).await.unwrap();
        assert_eq!(
            cache.support_services_count().await.unwrap(),
            2,
            "expected pruning C and D when the new seed only has A and B"
        );

        // Verify the right rows survived.
        let rows = cache.list_support_services().await.unwrap();
        let surviving_ids: std::collections::BTreeSet<_> =
            rows.iter().map(|r| r.service_id.clone()).collect();
        assert_eq!(
            surviving_ids,
            ["A".to_string(), "B".to_string()].into_iter().collect()
        );
    }

    #[tokio::test]
    async fn empty_keep_ids_does_not_wipe_the_cache() {
        // Belt-and-suspenders: even if a caller hands us an empty
        // keep-set (which would otherwise translate to "delete every
        // seed row"), refuse. Prevents an accidental cache wipe if a
        // malformed seed file ever made it past the parser.
        let (cache, _path, _tmp) = crate::cache::db::open_in_temp_dir("AzurePublicCloud")
            .await
            .unwrap();
        let staged = SeedFile {
            version: "test-stage".into(),
            generated_at: "test".into(),
            source: "test".into(),
            services: vec![SeedService {
                service_id: "X".into(),
                name: "X".into(),
                display_name: "Service X".into(),
                group: Some("test".into()),
                resource_types: vec![],
                metadata: serde_json::Value::Null,
            }],
        };
        write_seed_to_cache(&cache, &staged).await.unwrap();
        assert_eq!(cache.support_services_count().await.unwrap(), 1);

        let removed = cache.delete_seed_services_not_in(&[]).await.unwrap();
        assert_eq!(removed, 0, "must refuse to delete with empty keep set");
        assert_eq!(
            cache.support_services_count().await.unwrap(),
            1,
            "row must still be there"
        );
    }
}
