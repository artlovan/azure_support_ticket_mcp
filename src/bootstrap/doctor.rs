//! `doctor` subcommand — lightweight environment / connectivity checks.
//!
//! Never blocks `serve`; this is a manual diagnostic tool only.

use tracing::info;

use crate::config::Config;
use crate::error::AppResult;

pub async fn run(config: &Config) -> AppResult<()> {
    println!("azure-support-ticket-mcp doctor");
    println!("------------------------------");
    println!("app dir:        {}", config.app_dir().display());
    println!("cache path:     {}", config.cache.path.display());
    println!("cloud:          {}", config.general.cloud);
    println!("drafts.store:   {}", config.drafts.store);
    println!("seed download:  {}", config.seed.auto_download);

    // Cache check — exercise the same initialization path as `serve`.
    match crate::bootstrap::init::ensure_initialized(config).await {
        Ok(state) => {
            let n = state.cache.support_services_count().await.unwrap_or(-1);
            println!(
                "cache:          OK  ({n} services, seed version {:?})",
                state.seed_version
            );
        }
        Err(e) => {
            println!("cache:          FAIL  ({e})");
        }
    }

    // az CLI presence (informational only)
    match which::which("az") {
        Ok(p) => println!("az cli:         FOUND  ({})", p.display()),
        Err(_) => println!("az cli:         NOT FOUND  (optional fallback unavailable)"),
    }

    // Network reachability probe (no auth, no body)
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .unwrap();
    match client.head("https://management.azure.com/").send().await {
        Ok(r) => println!("arm reachable:  OK  (HTTP {})", r.status()),
        Err(e) => println!("arm reachable:  FAIL  ({e})"),
    }

    info!("doctor completed");
    Ok(())
}
