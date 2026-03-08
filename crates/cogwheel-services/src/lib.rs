use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ServiceToggleMode {
    Inherit,
    Allow,
    Block,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ServiceManifest {
    pub service_id: String,
    pub display_name: String,
    pub category: String,
    pub risk_notes: String,
    pub allow_domains: Vec<String>,
    pub block_domains: Vec<String>,
    pub exceptions: Vec<String>,
}

pub fn built_in_service_manifests() -> Vec<ServiceManifest> {
    vec![
        ServiceManifest {
            service_id: "google-ads".to_string(),
            display_name: "Google Ads".to_string(),
            category: "advertising".to_string(),
            risk_notes: "Placeholder manifest until curated domain coverage is finalized."
                .to_string(),
            allow_domains: vec![],
            block_domains: vec![
                "doubleclick.net".to_string(),
                "googleadservices.com".to_string(),
            ],
            exceptions: vec![],
        },
        ServiceManifest {
            service_id: "tiktok".to_string(),
            display_name: "TikTok".to_string(),
            category: "social".to_string(),
            risk_notes: "Placeholder manifest until curated domain coverage is finalized."
                .to_string(),
            allow_domains: vec![],
            block_domains: vec!["tiktokv.com".to_string(), "byteoversea.com".to_string()],
            exceptions: vec![],
        },
    ]
}
