use anyhow::{Context, Result};
use axum::extract::{FromRef, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use cogwheel_api::{ApiEnvelope, ApiState, AppConfig, router};
use cogwheel_classifier::ClassifierSettings;
use cogwheel_dns_core::{DnsRuntime, DnsRuntimeConfig};
use cogwheel_lists::{
    SourceDefinition, SourceKind, build_policy_engine, parse_source, verify_candidate,
};
use cogwheel_policy::{BlockMode, PolicyEngine};
use cogwheel_storage::{AuditEvent, RulesetRecord, SourceRecord, Storage};
use hickory_resolver::TokioResolver;
use hickory_resolver::config::{
    NameServerConfig, NameServerConfigGroup, ResolverConfig, ResolverOpts,
};
use hickory_resolver::name_server::TokioConnectionProvider;
use hickory_resolver::proto::xfer::Protocol;
use prometheus_client::metrics::counter::Counter;
use prometheus_client::registry::Registry;
use std::collections::HashSet;
use std::sync::Arc;
use tower_http::trace::TraceLayer;
use tracing_subscriber::EnvFilter;
use url::Url;
use uuid::Uuid;

#[derive(Clone, FromRef)]
struct ServerState {
    api_state: ApiState,
    storage: Arc<Storage>,
    dns_runtime: Arc<DnsRuntime>,
}

#[derive(serde::Serialize)]
struct RulesetSummary {
    id: Uuid,
    hash: String,
    status: String,
    created_at: chrono::DateTime<chrono::Utc>,
}

#[tokio::main]
async fn main() -> Result<()> {
    init_tracing();

    if std::env::args().nth(1).as_deref() == Some("healthcheck") {
        return Ok(());
    }

    let config = AppConfig::load()?;
    let storage = Arc::new(Storage::connect(&config.storage.database_url).await?);

    let default_source = SourceRecord {
        id: Uuid::new_v4(),
        name: "baseline".to_string(),
        url: "https://example.com/baseline.txt".to_string(),
        kind: "domains".to_string(),
        enabled: true,
    };
    storage.insert_source(&default_source).await?;

    let parsed = parse_source(
        SourceDefinition {
            id: default_source.id,
            name: default_source.name.clone(),
            url: Url::parse(&default_source.url)?,
            kind: SourceKind::Domains,
            enabled: true,
        },
        "ads.example.com\ntracker.example.com",
    );

    let protected_domains = HashSet::from(["connectivitycheck.gstatic.com".to_string()]);
    let verification = verify_candidate(std::slice::from_ref(&parsed), &protected_domains);
    anyhow::ensure!(
        verification.passed,
        "default ruleset failed verification: {:?}",
        verification.notes
    );

    let policy = Arc::new(build_policy_engine(
        vec![parsed],
        protected_domains,
        BlockMode::NullIp,
    ));
    storage
        .record_ruleset(&RulesetRecord {
            id: policy.artifact().id,
            hash: policy.artifact().hash.clone(),
            status: "active".to_string(),
            created_at: policy.artifact().created_at,
            artifact_json: serde_json::to_string(policy.artifact())?,
        })
        .await?;
    storage.activate_ruleset(policy.artifact().id).await?;
    storage
        .record_audit_event(&AuditEvent {
            id: Uuid::new_v4(),
            event_type: "ruleset.activated".to_string(),
            payload: serde_json::json!({
                "ruleset_id": policy.artifact().id,
                "hash": policy.artifact().hash,
                "reason": "bootstrap",
            })
            .to_string(),
            created_at: chrono::Utc::now(),
        })
        .await?;

    let mut registry = Registry::default();
    let startup_counter: Counter<u64> = Counter::default();
    registry.register(
        "cogwheel_startups_total",
        "Number of server startups",
        startup_counter.clone(),
    );
    startup_counter.inc();
    let registry = Arc::new(registry);

    let resolver = build_resolver(&config.upstream.servers)?;
    let dns_runtime = Arc::new(DnsRuntime::new(
        resolver,
        policy,
        ClassifierSettings::default(),
    ));

    let dns_handle = tokio::spawn({
        let runtime = dns_runtime.clone();
        let dns_config = DnsRuntimeConfig {
            udp_bind_addr: config.server.dns_udp_bind_addr,
            tcp_bind_addr: config.server.dns_tcp_bind_addr,
        };
        async move { runtime.serve(dns_config).await }
    });

    let app_state = ServerState {
        api_state: ApiState { registry },
        storage,
        dns_runtime,
    };
    let app = router(app_state.clone())
        .merge(admin_router())
        .with_state(app_state)
        .layer(TraceLayer::new_for_http());
    let listener = tokio::net::TcpListener::bind(config.server.http_bind_addr)
        .await
        .context("bind http listener")?;

    tokio::select! {
        result = dns_handle => {
            result.context("dns task join failure")??;
        }
        result = axum::serve(listener, app) => {
            result.context("http server failure")?;
        }
    }

    Ok(())
}

fn init_tracing() {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::from_default_env().add_directive("info".parse().expect("valid directive")),
        )
        .json()
        .init();
}

fn build_resolver(servers: &[String]) -> Result<TokioResolver> {
    let mut group = NameServerConfigGroup::new();
    for server in servers {
        let socket_addr = server
            .parse()
            .with_context(|| format!("invalid upstream server: {server}"))?;
        group.push(NameServerConfig::new(socket_addr, Protocol::Udp));
        group.push(NameServerConfig::new(socket_addr, Protocol::Tcp));
    }

    let config = ResolverConfig::from_parts(None, vec![], group);
    Ok(
        TokioResolver::builder_with_config(config, TokioConnectionProvider::default())
            .with_options(ResolverOpts::default())
            .build(),
    )
}

fn admin_router() -> Router<ServerState> {
    Router::new()
        .route("/api/v1/sources", get(list_sources))
        .route("/api/v1/rulesets", get(list_rulesets))
        .route("/api/v1/rulesets/rollback", post(rollback_ruleset))
        .route("/api/v1/audit-events", get(list_audit_events))
}

async fn list_sources(
    State(state): State<ServerState>,
) -> Result<Json<ApiEnvelope<Vec<SourceRecord>>>, axum::http::StatusCode> {
    state
        .storage
        .list_sources()
        .await
        .map(|data| Json(ApiEnvelope { data }))
        .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)
}

async fn list_rulesets(
    State(state): State<ServerState>,
) -> Result<Json<ApiEnvelope<Vec<RulesetSummary>>>, axum::http::StatusCode> {
    state
        .storage
        .list_rulesets()
        .await
        .map(|rows| {
            Json(ApiEnvelope {
                data: rows
                    .into_iter()
                    .map(|row| RulesetSummary {
                        id: row.id,
                        hash: row.hash,
                        status: row.status,
                        created_at: row.created_at,
                    })
                    .collect(),
            })
        })
        .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)
}

async fn rollback_ruleset(
    State(state): State<ServerState>,
) -> Result<Json<ApiEnvelope<RulesetSummary>>, axum::http::StatusCode> {
    let Some(artifact) = state
        .storage
        .rollback_to_previous_ruleset()
        .await
        .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?
    else {
        return Err(axum::http::StatusCode::NOT_FOUND);
    };

    state
        .dns_runtime
        .replace_policy(Arc::new(PolicyEngine::new(artifact.clone())));
    state
        .storage
        .record_audit_event(&AuditEvent {
            id: Uuid::new_v4(),
            event_type: "ruleset.rollback".to_string(),
            payload: serde_json::json!({
                "ruleset_id": artifact.id,
                "hash": artifact.hash,
            })
            .to_string(),
            created_at: chrono::Utc::now(),
        })
        .await
        .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(Json(ApiEnvelope {
        data: RulesetSummary {
            id: artifact.id,
            hash: artifact.hash,
            status: "active".to_string(),
            created_at: artifact.created_at,
        },
    }))
}

async fn list_audit_events(
    State(state): State<ServerState>,
) -> Result<Json<ApiEnvelope<Vec<AuditEvent>>>, axum::http::StatusCode> {
    state
        .storage
        .recent_audit_events(20)
        .await
        .map(|data| Json(ApiEnvelope { data }))
        .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)
}
