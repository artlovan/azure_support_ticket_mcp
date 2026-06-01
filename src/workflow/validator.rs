//! Draft validation. Pure functions over a `TicketDraft`.

use serde::Serialize;

use super::draft::{TicketDraft, SEVERITY_VALUES};

#[derive(Debug, Clone, Serialize)]
pub struct ValidationIssue {
    pub field: String,
    pub message: String,
    pub severity: IssueSeverity,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum IssueSeverity {
    Error,
    Warning,
}

#[derive(Debug, Clone, Serialize)]
pub struct ValidationReport {
    pub valid: bool,
    pub errors: Vec<ValidationIssue>,
    pub warnings: Vec<ValidationIssue>,
}

impl ValidationReport {
    pub fn into_result(self) -> Result<Self, String> {
        if self.valid {
            Ok(self)
        } else {
            let msg = self
                .errors
                .iter()
                .map(|e| format!("{}: {}", e.field, e.message))
                .collect::<Vec<_>>()
                .join("; ");
            Err(msg)
        }
    }
}

pub fn validate(d: &TicketDraft) -> ValidationReport {
    let mut errors = Vec::new();
    let mut warnings = Vec::new();

    macro_rules! require {
        ($field:expr, $name:expr) => {
            if $field.as_deref().map(str::is_empty).unwrap_or(true) {
                errors.push(ValidationIssue {
                    field: $name.into(),
                    message: "required".into(),
                    severity: IssueSeverity::Error,
                });
            }
        };
    }

    require!(d.tenant_id, "tenant_id");
    require!(d.subscription_id, "subscription_id");
    require!(d.service_id, "service_id");
    require!(d.problem_classification_id, "problem_classification_id");
    require!(d.title, "title");
    require!(d.description, "description");
    require!(d.severity, "severity");
    require!(d.advanced_diagnostic_consent, "advanced_diagnostic_consent");

    let c = &d.contact_details;
    require!(c.first_name, "contact_details.first_name");
    require!(c.last_name, "contact_details.last_name");
    require!(c.country, "contact_details.country");
    require!(
        c.preferred_contact_method,
        "contact_details.preferred_contact_method"
    );
    require!(
        c.preferred_support_language,
        "contact_details.preferred_support_language"
    );
    require!(c.preferred_time_zone, "contact_details.preferred_time_zone");
    require!(
        c.primary_email_address,
        "contact_details.primary_email_address"
    );

    if let Some(sev) = d.severity.as_deref() {
        if !SEVERITY_VALUES.iter().any(|s| s.eq_ignore_ascii_case(sev)) {
            errors.push(ValidationIssue {
                field: "severity".into(),
                message: format!("must be one of {SEVERITY_VALUES:?}"),
                severity: IssueSeverity::Error,
            });
        }
        // Phone required for critical+
        let high = matches!(
            sev.to_ascii_lowercase().as_str(),
            "critical" | "highestcriticalimpact"
        );
        if high && c.phone_number.as_deref().map(str::is_empty).unwrap_or(true) {
            errors.push(ValidationIssue {
                field: "contact_details.phone_number".into(),
                message: "phone_number is required for critical / highestcriticalimpact severity"
                    .into(),
                severity: IssueSeverity::Error,
            });
        }
    }

    if let Some(consent) = d.advanced_diagnostic_consent.as_deref() {
        if !matches!(consent, "Yes" | "No") {
            errors.push(ValidationIssue {
                field: "advanced_diagnostic_consent".into(),
                message: "must be 'Yes' or 'No'".into(),
                severity: IssueSeverity::Error,
            });
        }
    }

    if let Some(method) = c.preferred_contact_method.as_deref() {
        if !matches!(method.to_ascii_lowercase().as_str(), "email" | "phone") {
            errors.push(ValidationIssue {
                field: "contact_details.preferred_contact_method".into(),
                message: "must be 'email' or 'phone'".into(),
                severity: IssueSeverity::Error,
            });
        }
        if method.eq_ignore_ascii_case("phone")
            && c.phone_number.as_deref().map(str::is_empty).unwrap_or(true)
        {
            errors.push(ValidationIssue {
                field: "contact_details.phone_number".into(),
                message: "phone_number is required when preferred_contact_method = 'phone'".into(),
                severity: IssueSeverity::Error,
            });
        }
    }

    if d.resource_id.is_none() && d.technical_ticket_details.resource_id.is_none() {
        warnings.push(ValidationIssue {
            field: "resource_id".into(),
            message: "no resource scope provided; Azure may route this ticket more slowly".into(),
            severity: IssueSeverity::Warning,
        });
    }

    ValidationReport {
        valid: errors.is_empty(),
        errors,
        warnings,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn good_draft() -> TicketDraft {
        let mut d = TicketDraft::new();
        d.tenant_id = Some("t".into());
        d.subscription_id = Some("s".into());
        d.service_id = Some("svc".into());
        d.problem_classification_id = Some("pc".into());
        d.title = Some("AKS scale".into());
        d.description = Some("nodes not scaling".into());
        d.severity = Some("moderate".into());
        d.advanced_diagnostic_consent = Some("Yes".into());
        d.contact_details.first_name = Some("Ada".into());
        d.contact_details.last_name = Some("Lovelace".into());
        d.contact_details.country = Some("USA".into());
        d.contact_details.preferred_contact_method = Some("email".into());
        d.contact_details.preferred_support_language = Some("en-us".into());
        d.contact_details.preferred_time_zone = Some("Pacific Standard Time".into());
        d.contact_details.primary_email_address = Some("ada@example.com".into());
        d.resource_id = Some("/subscriptions/s/resourceGroups/rg/providers/p/c/n".into());
        d
    }

    #[test]
    fn complete_draft_is_valid() {
        let r = validate(&good_draft());
        assert!(r.valid, "errors: {:?}", r.errors);
    }

    #[test]
    fn critical_severity_requires_phone() {
        let mut d = good_draft();
        d.severity = Some("critical".into());
        let r = validate(&d);
        assert!(!r.valid);
        assert!(r.errors.iter().any(|e| e.field.contains("phone_number")));
        d.contact_details.phone_number = Some("+15551234567".into());
        assert!(validate(&d).valid);
    }

    #[test]
    fn invalid_consent_rejected() {
        let mut d = good_draft();
        d.advanced_diagnostic_consent = Some("maybe".into());
        assert!(!validate(&d).valid);
    }

    #[test]
    fn missing_resource_is_warning_only() {
        let mut d = good_draft();
        d.resource_id = None;
        let r = validate(&d);
        assert!(r.valid);
        assert!(!r.warnings.is_empty());
    }
}
