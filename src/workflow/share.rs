//! Format a copy/paste-friendly share message after ticket creation.
//!
//! MVP: local string formatter only. No outbound posting integrations.

use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct ShareInputs<'a> {
    pub ticket_name: &'a str,
    pub title: &'a str,
    pub severity: &'a str,
    pub tenant_id: Option<&'a str>,
    pub subscription_id: &'a str,
    pub subscription_display_name: Option<&'a str>,
    pub resource_id: Option<&'a str>,
    pub status: &'a str,
    pub portal_url: Option<&'a str>,
    pub summary: Option<&'a str>,
}

pub fn format_share_markdown(i: &ShareInputs<'_>) -> String {
    let mut out = String::new();
    out.push_str(&format!("Azure support ticket opened: {}\n", i.ticket_name));
    out.push_str(&format!("Title: {}\n", i.title));
    out.push_str(&format!("Severity: {}\n", severity_label(i.severity)));
    if let Some(t) = i.tenant_id.filter(|s| !s.is_empty()) {
        out.push_str(&format!("Tenant: {t}\n"));
    }
    let sub = match i.subscription_display_name {
        Some(n) if !n.is_empty() => format!("{n} ({})", i.subscription_id),
        _ => i.subscription_id.to_string(),
    };
    out.push_str(&format!("Subscription: {sub}\n"));
    if let Some(r) = i.resource_id {
        out.push_str(&format!("Resource: {r}\n"));
    }
    out.push_str(&format!("Status: {}\n", i.status));
    if let Some(u) = i.portal_url {
        out.push_str(&format!("Portal: {u}\n"));
    }
    if let Some(s) = i.summary {
        out.push_str(&format!("Summary: {s}\n"));
    }
    out
}

/// Build the Azure portal deep-link to a support ticket. Mirrors the format
/// the `azure-support-slack-bot` produces (the older `#blade/...DetailBlade/`
/// path no longer loads in the portal). Requires the subscription ID because
/// the portal route is keyed off the full ARM resource ID.
pub fn portal_url_for_ticket(subscription_id: &str, ticket_name: &str) -> String {
    let arm_id = format!(
        "/subscriptions/{subscription_id}/providers/Microsoft.Support/supportTickets/{ticket_name}"
    );
    let encoded = urlencoding::encode(&arm_id);
    format!(
        "https://portal.azure.com/#view/Microsoft_Azure_Support/SupportRequestDetails.ReactView/id/{encoded}/portalJourney~/true"
    )
}

/// Map Azure REST severity code to the human-friendly portal label.
///
/// | REST                     | Portal text                 |
/// |--------------------------|-----------------------------|
/// | `minimal`                | `C - Minimal impact`        |
/// | `moderate`               | `B - Moderate impact`       |
/// | `critical`               | `A - Critical impact`       |
/// | `highestcriticalimpact`  | `A - Critical impact (highest, Premier)` |
pub fn severity_label(sev: &str) -> String {
    match sev.to_ascii_lowercase().as_str() {
        "minimal" => "C - Minimal impact".into(),
        "moderate" => "B - Moderate impact".into(),
        "critical" => "A - Critical impact".into(),
        "highestcriticalimpact" => "A - Critical impact (highest, Premier)".into(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_complete_message() {
        let s = format_share_markdown(&ShareInputs {
            ticket_name: "1234567890000000",
            title: "AKS scale",
            severity: "moderate",
            tenant_id: Some("11111111-aaaa-bbbb-cccc-222222222222"),
            subscription_id: "sub-1",
            subscription_display_name: Some("Prod"),
            resource_id: Some("/subscriptions/sub-1/.../prod-aks"),
            status: "Open",
            portal_url: Some("https://portal.azure.com/..."),
            summary: Some("nodes failing to scale"),
        });
        assert!(s.contains("Azure support ticket opened: 1234567890000000"));
        assert!(s.contains("Subscription: Prod (sub-1)"));
        assert!(s.contains("Resource: /subscriptions/sub-1/.../prod-aks"));
        assert!(s.contains("Severity: B - Moderate impact"));
        assert!(s.contains("Tenant: 11111111-aaaa-bbbb-cccc-222222222222"));
    }

    #[test]
    fn portal_url_matches_reactview_format() {
        let url = portal_url_for_ticket(
            "00000000-0000-0000-0000-000000000001",
            "99999999-9999-9999-9999-999999999999",
        );
        // Path segments must be URL-encoded; the slash characters become %2F.
        assert!(url.contains("SupportRequestDetails.ReactView"));
        assert!(url.contains("%2Fsubscriptions%2F00000000-0000-0000-0000-000000000001"));
        assert!(url.contains("%2Fproviders%2FMicrosoft.Support%2FsupportTickets%2F99999999-9999-9999-9999-999999999999"));
        assert!(url.ends_with("/portalJourney~/true"));
    }

    #[test]
    fn severity_labels() {
        assert_eq!(severity_label("minimal"), "C - Minimal impact");
        assert_eq!(severity_label("moderate"), "B - Moderate impact");
        assert_eq!(severity_label("critical"), "A - Critical impact");
        assert_eq!(
            severity_label("highestcriticalimpact"),
            "A - Critical impact (highest, Premier)"
        );
        assert_eq!(severity_label("Moderate"), "B - Moderate impact"); // case-insensitive
        assert_eq!(severity_label("unknown"), "unknown");
    }
}
