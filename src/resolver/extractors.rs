//! Deterministic extractors for resource IDs, provider types, and portal URLs.
//!
//! These are pure functions — easy to unit-test, no async, no I/O.

use once_cell::sync::Lazy;
use regex::Regex;
use serde::Serialize;
use url::Url;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ParsedResourceId {
    pub subscription_id: String,
    pub resource_group: Option<String>,
    pub provider: String,      // e.g. Microsoft.Compute
    pub resource_type: String, // e.g. Microsoft.Compute/virtualMachines
    pub name: String,
}

static RID_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r"(?i)^/subscriptions/(?P<sub>[0-9a-f-]{36})(?:/resourceGroups/(?P<rg>[^/]+))?/providers/(?P<prov>[^/]+)/(?P<rest>.+)$"
    )
    .unwrap()
});

/// Parse an ARM resource id. Returns `None` for paths that aren't full
/// resource ids (e.g. subscription-only paths).
pub fn parse_resource_id(s: &str) -> Option<ParsedResourceId> {
    let caps = RID_RE.captures(s.trim())?;
    let sub = caps.name("sub")?.as_str().to_string();
    let rg = caps.name("rg").map(|m| m.as_str().to_string());
    let prov = caps.name("prov")?.as_str().to_string();
    let rest = caps.name("rest")?.as_str();
    // rest looks like "type/name" or "type/parent/subtype/name". Portal URLs
    // often append a UI segment (e.g. ".../prod-aks/overview"); strip a single
    // trailing segment to recover an even-arity path.
    let mut parts: Vec<&str> = rest.split('/').collect();
    if parts.len() >= 3 && !parts.len().is_multiple_of(2) {
        parts.pop();
    }
    if parts.len() < 2 || !parts.len().is_multiple_of(2) {
        return None;
    }
    let type_parts: Vec<&str> = parts.iter().step_by(2).copied().collect();
    let resource_type = format!("{prov}/{}", type_parts.join("/"));
    let name = parts.last().copied()?.to_string();
    Some(ParsedResourceId {
        subscription_id: sub,
        resource_group: rg,
        provider: prov,
        resource_type,
        name,
    })
}

/// Try to extract subscription + resource id from an Azure portal URL.
/// Handles the common fragment form `https://portal.azure.com/#@tenant/resource/<arm-id>/...`.
pub fn parse_portal_url(s: &str) -> Option<ParsedResourceId> {
    let url = Url::parse(s.trim()).ok()?;
    let fragment = url.fragment()?;
    // Look for "/subscriptions/" segment anywhere in the fragment.
    let idx = fragment.to_ascii_lowercase().find("/subscriptions/")?;
    let mut tail = &fragment[idx..];
    // Trim trailing query-like chunks
    if let Some(end) = tail.find('?') {
        tail = &tail[..end];
    }
    parse_resource_id(tail)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_simple_vm_id() {
        let id = "/subscriptions/11111111-1111-1111-1111-111111111111/resourceGroups/rg1/providers/Microsoft.Compute/virtualMachines/vm1";
        let p = parse_resource_id(id).unwrap();
        assert_eq!(p.subscription_id, "11111111-1111-1111-1111-111111111111");
        assert_eq!(p.resource_group.as_deref(), Some("rg1"));
        assert_eq!(p.resource_type, "Microsoft.Compute/virtualMachines");
        assert_eq!(p.name, "vm1");
    }

    #[test]
    fn parses_nested_sql_db() {
        let id = "/subscriptions/11111111-1111-1111-1111-111111111111/resourceGroups/rg/providers/Microsoft.Sql/servers/srv/databases/db1";
        let p = parse_resource_id(id).unwrap();
        assert_eq!(p.resource_type, "Microsoft.Sql/servers/databases");
        assert_eq!(p.name, "db1");
    }

    #[test]
    fn rejects_subscription_only() {
        assert!(parse_resource_id("/subscriptions/11111111-1111-1111-1111-111111111111").is_none());
    }

    #[test]
    fn portal_url_extraction() {
        let u = "https://portal.azure.com/#@contoso.onmicrosoft.com/resource/subscriptions/11111111-1111-1111-1111-111111111111/resourceGroups/rg/providers/Microsoft.ContainerService/managedClusters/prod-aks/overview";
        let p = parse_portal_url(u).unwrap();
        assert_eq!(
            p.resource_type,
            "Microsoft.ContainerService/managedClusters"
        );
        assert_eq!(p.name, "prod-aks");
    }
}
