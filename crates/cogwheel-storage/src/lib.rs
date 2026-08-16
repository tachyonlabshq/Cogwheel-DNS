use chrono::{DateTime, Utc};
use cogwheel_policy::RulesetArtifact;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::{Arc, Mutex};
use thiserror::Error;
use uuid::Uuid;

use base64::Engine as _;
use ed25519_dalek::{Signer, SigningKey};
use rand_core::OsRng;

const MIGRATION_0001: &str = include_str!("../migrations/0001_init.sql");
const MIGRATION_0002: &str = include_str!("../migrations/0002_ruleset_artifacts.sql");
const MIGRATION_0003: &str = include_str!("../migrations/0003_source_metadata.sql");
const MIGRATION_0004: &str = include_str!("../migrations/0004_source_verification_strictness.sql");
const MIGRATION_0005: &str = include_str!("../migrations/0005_devices_security_events.sql");
const MIGRATION_0006: &str = include_str!("../migrations/0006_device_protection_override.sql");
const MIGRATION_0007: &str = include_str!("../migrations/0007_device_allowed_domains.sql");
const MIGRATION_0008: &str = include_str!("../migrations/0008_device_service_overrides.sql");
const MIGRATION_0009: &str = include_str!("../migrations/0009_notification_deliveries.sql");
const MIGRATION_0010: &str = include_str!("../migrations/0010_config_version.sql");

pub const SCHEMA_VERSION: u32 = 10;
pub const CONFIG_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Error)]
pub enum StorageError {
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error(transparent)]
    Serde(#[from] serde_json::Error),
    #[error(transparent)]
    Uuid(#[from] uuid::Error),
    #[error(transparent)]
    Chrono(#[from] chrono::ParseError),
    #[error("internal storage error: {0}")]
    Internal(String),
}

/// Lock the SQLite connection.
///
/// Every public method funnels through here. A poisoned mutex means another thread panicked while
/// holding the connection; returning an error lets the caller surface a failed request instead of
/// cascading that one panic through every subsequent database call for the life of the process.
fn lock_connection(
    connection: &Mutex<Connection>,
) -> Result<std::sync::MutexGuard<'_, Connection>, StorageError> {
    connection
        .lock()
        .map_err(|_| StorageError::Internal("storage connection lock poisoned".to_string()))
}

/// Decode a UUID stored as text in a result row.
///
/// A malformed id means the database is damaged — an appliance's SQLite file can be truncated by a
/// power cut mid-write. That should fail the query it appears in, not abort the process, so this
/// converts the parse failure into a row-level conversion error the caller can propagate.
fn row_uuid(column: usize, value: &str) -> rusqlite::Result<Uuid> {
    Uuid::parse_str(value).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            column,
            rusqlite::types::Type::Text,
            Box::new(error),
        )
    })
}

/// Decode an optional UUID column, preserving `NULL` as `None`.
fn row_uuid_opt(column: usize, value: Option<&str>) -> rusqlite::Result<Option<Uuid>> {
    value.map(|raw| row_uuid(column, raw)).transpose()
}

#[derive(Debug, Clone)]
pub struct Storage {
    connection: Arc<Mutex<Connection>>,
    node_identity: Arc<NodeIdentity>,
}

#[derive(Debug, Clone)]
pub struct NodeIdentity {
    pub key: Arc<SigningKey>,
    pub public_b64: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceRecord {
    pub id: Uuid,
    pub name: String,
    pub url: String,
    pub kind: String,
    pub enabled: bool,
    pub refresh_interval_minutes: i64,
    pub profile: String,
    pub verification_strictness: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RulesetRecord {
    pub id: Uuid,
    pub hash: String,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub artifact_json: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEvent {
    pub id: Uuid,
    pub event_type: String,
    pub payload: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DeviceServiceOverrideRecord {
    pub service_id: String,
    pub mode: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceRecord {
    pub id: Uuid,
    pub name: String,
    pub ip_address: String,
    pub policy_mode: String,
    pub blocklist_profile_override: Option<String>,
    pub protection_override: String,
    pub allowed_domains: Vec<String>,
    pub service_overrides: Vec<DeviceServiceOverrideRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecurityEventRecord {
    pub id: Uuid,
    pub device_id: Option<Uuid>,
    pub device_name: Option<String>,
    pub client_ip: String,
    pub domain: String,
    pub classifier_score: f64,
    pub severity: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NotificationDeliveryRecord {
    pub id: Uuid,
    pub event_type: String,
    pub status: String,
    pub severity: String,
    pub title: String,
    pub summary: String,
    pub domain: String,
    pub device_name: Option<String>,
    pub client_ip: String,
    pub attempts: usize,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncEnvelope {
    pub node_public_key: String,
    pub timestamp: DateTime<Utc>,
    pub nonce: String,
    pub payload_b64: String,
    pub signature_b64: String,
}

fn sync_signing_message(timestamp: &DateTime<Utc>, nonce: &str, payload: &[u8]) -> Vec<u8> {
    let mut message = timestamp.to_rfc3339().into_bytes();
    message.push(b'|');
    message.extend_from_slice(nonce.as_bytes());
    message.push(b'|');
    message.extend_from_slice(payload);
    message
}

impl Storage {
    pub fn sign_sync_payload(&self, payload: &[u8]) -> SyncEnvelope {
        let timestamp = Utc::now();
        let nonce = Uuid::new_v4().to_string();
        let message = sync_signing_message(&timestamp, &nonce, payload);
        let signature = self.node_identity.key.sign(&message);
        let signature_b64 =
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(signature.to_bytes());
        let payload_b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(payload);

        SyncEnvelope {
            node_public_key: self.node_identity.public_b64.clone(),
            timestamp,
            nonce,
            payload_b64,
            signature_b64,
        }
    }

    pub fn verify_sync_envelope(envelope: &SyncEnvelope) -> Result<Vec<u8>, StorageError> {
        use ed25519_dalek::Signature;
        use ed25519_dalek::{Verifier, VerifyingKey};

        let pub_bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(&envelope.node_public_key)
            .map_err(|_| StorageError::Internal("invalid public key base64".to_string()))?;
        let pub_bytes_array: [u8; 32] = pub_bytes
            .try_into()
            .map_err(|_| StorageError::Internal("invalid public key length".to_string()))?;
        let verifying_key = VerifyingKey::from_bytes(&pub_bytes_array)
            .map_err(|_| StorageError::Internal("invalid verifying key bytes".to_string()))?;

        let sig_bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(&envelope.signature_b64)
            .map_err(|_| StorageError::Internal("invalid signature base64".to_string()))?;
        let sig_bytes_array: [u8; 64] = sig_bytes
            .try_into()
            .map_err(|_| StorageError::Internal("invalid signature length".to_string()))?;
        let signature = Signature::from_bytes(&sig_bytes_array);

        let payload_bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(&envelope.payload_b64)
            .map_err(|_| StorageError::Internal("invalid payload base64".to_string()))?;
        let message = sync_signing_message(&envelope.timestamp, &envelope.nonce, &payload_bytes);

        verifying_key
            .verify(&message, &signature)
            .map_err(|_| StorageError::Internal("signature verification failed".to_string()))?;

        Ok(payload_bytes)
    }

    pub async fn connect(database_url: &str) -> Result<Self, StorageError> {
        let path = database_url
            .strip_prefix("sqlite://")
            .unwrap_or(database_url);
        if let Some(parent) = Path::new(path).parent() {
            std::fs::create_dir_all(parent).ok();
        }

        let connection = Connection::open(path)?;
        connection.pragma_update(None, "journal_mode", "WAL")?;
        connection.pragma_update(None, "foreign_keys", "ON")?;
        apply_migrations(&connection)?;

        let node_identity_b64: Option<String> = connection
            .query_row(
                "SELECT value FROM settings WHERE key = 'node_identity_v1'",
                [],
                |row| row.get(0),
            )
            .optional()?;

        let signing_key = if let Some(b64) = node_identity_b64 {
            let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
                .decode(&b64)
                .map_err(|_| StorageError::Internal("invalid identity base64".to_string()))?;
            let bytes_array: [u8; 32] = bytes
                .try_into()
                .map_err(|_| StorageError::Internal("invalid identity length".to_string()))?;
            SigningKey::from_bytes(&bytes_array)
        } else {
            let key = SigningKey::generate(&mut OsRng);
            let b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(key.to_bytes());
            connection.execute(
                "INSERT INTO settings (key, value, updated_at) VALUES ('node_identity_v1', ?1, CURRENT_TIMESTAMP)",
                params![b64],
            )?;
            key
        };

        let public_b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(signing_key.verifying_key().to_bytes());

        Ok(Self {
            connection: Arc::new(Mutex::new(connection)),
            node_identity: Arc::new(NodeIdentity {
                key: Arc::new(signing_key),
                public_b64,
            }),
        })
    }

    pub fn identity(&self) -> Arc<NodeIdentity> {
        self.node_identity.clone()
    }

    pub async fn upsert_setting(&self, key: &str, value: &str) -> Result<(), StorageError> {
        let connection = lock_connection(&self.connection)?;
        connection.execute(
            "INSERT INTO settings (key, value, updated_at) VALUES (?1, ?2, CURRENT_TIMESTAMP)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = CURRENT_TIMESTAMP",
            params![key, value],
        )?;
        Ok(())
    }

    pub async fn get_setting(&self, key: &str) -> Result<Option<String>, StorageError> {
        let connection = lock_connection(&self.connection)?;
        connection
            .query_row(
                "SELECT value FROM settings WHERE key = ?1 LIMIT 1",
                params![key],
                |row| row.get(0),
            )
            .optional()
            .map_err(StorageError::from)
    }

    pub async fn insert_source(&self, source: &SourceRecord) -> Result<(), StorageError> {
        let connection = lock_connection(&self.connection)?;
        connection.execute(
            "INSERT OR REPLACE INTO sources (id, name, url, kind, enabled, refresh_interval_minutes, profile, verification_strictness, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)",
            params![
                source.id.to_string(),
                source.name,
                source.url,
                source.kind,
                source.enabled,
                source.refresh_interval_minutes,
                source.profile,
                source.verification_strictness,
            ],
        )?;
        Ok(())
    }

    pub async fn list_sources(&self) -> Result<Vec<SourceRecord>, StorageError> {
        let connection = lock_connection(&self.connection)?;
        let mut statement = connection.prepare(
            "SELECT id, name, url, kind, enabled, refresh_interval_minutes, profile, verification_strictness FROM sources ORDER BY name ASC",
        )?;
        let rows = statement.query_map([], |row| {
            Ok(SourceRecord {
                id: row_uuid(0, &row.get::<_, String>(0)?)?,
                name: row.get(1)?,
                url: row.get(2)?,
                kind: row.get(3)?,
                enabled: row.get(4)?,
                refresh_interval_minutes: row.get(5)?,
                profile: row.get(6)?,
                verification_strictness: row.get(7)?,
            })
        })?;

        rows.collect::<Result<Vec<_>, _>>()
            .map_err(StorageError::from)
    }

    pub async fn delete_source(&self, source_id: Uuid) -> Result<bool, StorageError> {
        let connection = lock_connection(&self.connection)?;
        let changed = connection.execute(
            "DELETE FROM sources WHERE id = ?1",
            params![source_id.to_string()],
        )?;
        Ok(changed > 0)
    }

    pub async fn upsert_device(&self, device: &DeviceRecord) -> Result<(), StorageError> {
        let connection = lock_connection(&self.connection)?;
        connection.execute(
            "INSERT OR REPLACE INTO devices (id, name, ip_address, policy_mode, blocklist_profile_override, protection_override, allowed_domains_json, service_overrides_json, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)",
            params![
                device.id.to_string(),
                device.name,
                device.ip_address,
                device.policy_mode,
                device.blocklist_profile_override,
                device.protection_override,
                serde_json::to_string(&device.allowed_domains)?,
                serde_json::to_string(&device.service_overrides)?,
            ],
        )?;
        Ok(())
    }

    pub async fn delete_device(&self, device_id: Uuid) -> Result<bool, StorageError> {
        let connection = lock_connection(&self.connection)?;
        let changed = connection.execute(
            "DELETE FROM devices WHERE id = ?1",
            params![device_id.to_string()],
        )?;
        Ok(changed > 0)
    }

    pub async fn list_devices(&self) -> Result<Vec<DeviceRecord>, StorageError> {
        let connection = lock_connection(&self.connection)?;
        let mut statement = connection.prepare(
            "SELECT id, name, ip_address, policy_mode, blocklist_profile_override, protection_override, allowed_domains_json, service_overrides_json FROM devices ORDER BY name ASC",
        )?;
        let rows = statement.query_map([], |row| {
            Ok(DeviceRecord {
                id: row_uuid(0, &row.get::<_, String>(0)?)?,
                name: row.get(1)?,
                ip_address: row.get(2)?,
                policy_mode: row.get(3)?,
                blocklist_profile_override: row.get(4)?,
                protection_override: row.get(5)?,
                allowed_domains: serde_json::from_str(&row.get::<_, String>(6)?)
                    .unwrap_or_default(),
                service_overrides: serde_json::from_str(&row.get::<_, String>(7)?)
                    .unwrap_or_default(),
            })
        })?;

        rows.collect::<Result<Vec<_>, _>>()
            .map_err(StorageError::from)
    }

    pub async fn find_device_by_ip(
        &self,
        ip_address: &str,
    ) -> Result<Option<DeviceRecord>, StorageError> {
        let connection = lock_connection(&self.connection)?;
        let mut statement = connection.prepare(
            "SELECT id, name, ip_address, policy_mode, blocklist_profile_override, protection_override, allowed_domains_json, service_overrides_json FROM devices WHERE ip_address = ?1",
        )?;

        statement
            .query_row(params![ip_address], |row| {
                Ok(DeviceRecord {
                    id: row_uuid(0, &row.get::<_, String>(0)?)?,
                    name: row.get(1)?,
                    ip_address: row.get(2)?,
                    policy_mode: row.get(3)?,
                    blocklist_profile_override: row.get(4)?,
                    protection_override: row.get(5)?,
                    allowed_domains: serde_json::from_str(&row.get::<_, String>(6)?)
                        .unwrap_or_default(),
                    service_overrides: serde_json::from_str(&row.get::<_, String>(7)?)
                        .unwrap_or_default(),
                })
            })
            .optional()
            .map_err(StorageError::from)
    }

    pub async fn record_security_event(
        &self,
        event: &SecurityEventRecord,
    ) -> Result<(), StorageError> {
        let connection = lock_connection(&self.connection)?;
        connection.execute(
            "INSERT INTO security_events (id, device_id, device_name, client_ip, domain, classifier_score, severity, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                event.id.to_string(),
                event.device_id.map(|value| value.to_string()),
                event.device_name,
                event.client_ip,
                event.domain,
                event.classifier_score,
                event.severity,
                event.created_at.to_rfc3339(),
            ],
        )?;
        Ok(())
    }

    pub async fn recent_security_events(
        &self,
        limit: i64,
    ) -> Result<Vec<SecurityEventRecord>, StorageError> {
        let connection = lock_connection(&self.connection)?;
        let mut statement = connection.prepare(
            "SELECT id, device_id, device_name, client_ip, domain, classifier_score, severity, created_at FROM security_events ORDER BY created_at DESC LIMIT ?1",
        )?;
        let rows = statement.query_map(params![limit], |row| {
            let device_id = row.get::<_, Option<String>>(1)?;
            Ok(SecurityEventRecord {
                id: row_uuid(0, &row.get::<_, String>(0)?)?,
                device_id: row_uuid_opt(1, device_id.as_deref())?,
                device_name: row.get(2)?,
                client_ip: row.get(3)?,
                domain: row.get(4)?,
                classifier_score: row.get(5)?,
                severity: row.get(6)?,
                created_at: parse_datetime(&row.get::<_, String>(7)?).map_err(to_sqlite_error)?,
            })
        })?;

        rows.collect::<Result<Vec<_>, _>>()
            .map_err(StorageError::from)
    }

    pub async fn record_ruleset(&self, ruleset: &RulesetRecord) -> Result<(), StorageError> {
        let connection = lock_connection(&self.connection)?;
        connection.execute(
            "INSERT INTO rulesets (id, hash, status, created_at, artifact_json) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                ruleset.id.to_string(),
                ruleset.hash,
                ruleset.status,
                ruleset.created_at.to_rfc3339(),
                ruleset.artifact_json,
            ],
        )?;
        Ok(())
    }

    pub async fn list_rulesets(&self) -> Result<Vec<RulesetRecord>, StorageError> {
        let connection = lock_connection(&self.connection)?;
        let mut statement = connection.prepare(
            "SELECT id, hash, status, created_at, artifact_json FROM rulesets ORDER BY created_at DESC",
        )?;
        let rows = statement.query_map([], decode_ruleset_row)?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(StorageError::from)
    }

    pub async fn activate_ruleset(&self, ruleset_id: Uuid) -> Result<(), StorageError> {
        let mut connection = lock_connection(&self.connection)?;
        let transaction = connection.transaction()?;
        transaction.execute(
            "UPDATE rulesets SET status = 'previous' WHERE status = 'active'",
            [],
        )?;
        transaction.execute(
            "UPDATE rulesets SET status = 'active' WHERE id = ?1",
            params![ruleset_id.to_string()],
        )?;
        transaction.execute(
            "UPDATE active_ruleset SET ruleset_id = ?1, activated_at = CURRENT_TIMESTAMP WHERE slot = 1",
            params![ruleset_id.to_string()],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub async fn active_ruleset(&self) -> Result<Option<RulesetRecord>, StorageError> {
        let connection = lock_connection(&self.connection)?;
        connection
            .query_row(
                "SELECT id, hash, status, created_at, artifact_json FROM rulesets WHERE status = 'active' LIMIT 1",
                [],
                decode_ruleset_row,
            )
            .optional()
            .map_err(StorageError::from)
    }

    pub async fn previous_ruleset(&self) -> Result<Option<RulesetRecord>, StorageError> {
        let connection = lock_connection(&self.connection)?;
        connection
            .query_row(
                "SELECT id, hash, status, created_at, artifact_json FROM rulesets WHERE status = 'previous' ORDER BY created_at DESC LIMIT 1",
                [],
                decode_ruleset_row,
            )
            .optional()
            .map_err(StorageError::from)
    }

    pub async fn rollback_to_previous_ruleset(
        &self,
    ) -> Result<Option<RulesetArtifact>, StorageError> {
        let Some(previous) = self.previous_ruleset().await? else {
            return Ok(None);
        };

        self.activate_ruleset(previous.id).await?;
        let artifact = serde_json::from_str::<RulesetArtifact>(&previous.artifact_json)?;
        Ok(Some(artifact))
    }

    pub async fn record_audit_event(&self, event: &AuditEvent) -> Result<(), StorageError> {
        let connection = lock_connection(&self.connection)?;
        connection.execute(
            "INSERT INTO audit_events (id, event_type, payload, created_at) VALUES (?1, ?2, ?3, ?4)",
            params![
                event.id.to_string(),
                event.event_type,
                event.payload,
                event.created_at.to_rfc3339(),
            ],
        )?;
        Ok(())
    }

    pub async fn recent_audit_events(&self, limit: i64) -> Result<Vec<AuditEvent>, StorageError> {
        let connection = lock_connection(&self.connection)?;
        let mut statement = connection.prepare(
            "SELECT id, event_type, payload, created_at FROM audit_events ORDER BY created_at DESC LIMIT ?1",
        )?;
        let rows = statement.query_map(params![limit], |row| {
            Ok(AuditEvent {
                id: row_uuid(0, &row.get::<_, String>(0)?)?,
                event_type: row.get(1)?,
                payload: row.get(2)?,
                created_at: parse_datetime(&row.get::<_, String>(3)?).map_err(to_sqlite_error)?,
            })
        })?;

        rows.collect::<Result<Vec<_>, _>>()
            .map_err(StorageError::from)
    }

    pub async fn record_notification_delivery(
        &self,
        delivery: &NotificationDeliveryRecord,
    ) -> Result<(), StorageError> {
        let connection = lock_connection(&self.connection)?;
        connection.execute(
            "INSERT INTO notification_deliveries (id, event_type, status, severity, title, summary, domain, device_name, client_ip, attempts, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                delivery.id.to_string(),
                delivery.event_type,
                delivery.status,
                delivery.severity,
                delivery.title,
                delivery.summary,
                delivery.domain,
                delivery.device_name,
                delivery.client_ip,
                delivery.attempts as i64,
                delivery.created_at.to_rfc3339(),
            ],
        )?;
        Ok(())
    }

    pub async fn recent_notification_deliveries(
        &self,
        limit: i64,
    ) -> Result<Vec<NotificationDeliveryRecord>, StorageError> {
        let connection = lock_connection(&self.connection)?;
        let mut statement = connection.prepare(
            "SELECT id, event_type, status, severity, title, summary, domain, device_name, client_ip, attempts, created_at FROM notification_deliveries ORDER BY created_at DESC LIMIT ?1",
        )?;
        let rows = statement.query_map(params![limit], |row| {
            Ok(NotificationDeliveryRecord {
                id: row_uuid(0, &row.get::<_, String>(0)?)?,
                event_type: row.get(1)?,
                status: row.get(2)?,
                severity: row.get(3)?,
                title: row.get(4)?,
                summary: row.get(5)?,
                domain: row.get(6)?,
                device_name: row.get(7)?,
                client_ip: row.get(8)?,
                attempts: row.get::<_, i64>(9)? as usize,
                created_at: parse_datetime(&row.get::<_, String>(10)?).map_err(to_sqlite_error)?,
            })
        })?;

        rows.collect::<Result<Vec<_>, _>>()
            .map_err(StorageError::from)
    }

    pub fn get_config_version(&self) -> Result<u32, StorageError> {
        let connection = lock_connection(&self.connection)?;
        let mut statement = connection.prepare("SELECT version FROM config_schema WHERE id = 1")?;
        let version: u32 = statement.query_row([], |row| row.get(0))?;
        Ok(version)
    }
}

/// Whether a migration error just means "this migration is already applied".
///
/// Migrations 0002+ are additive `ALTER TABLE ADD COLUMN` / `CREATE ... IF NOT EXISTS` batches with
/// no version ledger, so re-running them on an up-to-date database is expected to fail in exactly
/// these ways. Everything else -- a full disk, a locked database, a genuine SQL error -- is a real
/// failure that must stop startup rather than leave the server running against a schema that is
/// missing tables or columns, turning every dependent query into an opaque 500 later.
fn is_already_applied(error: &rusqlite::Error) -> bool {
    let message = error.to_string().to_ascii_lowercase();
    message.contains("duplicate column name")
        || message.contains("already exists")
        || message.contains("duplicate index")
}

fn apply_migrations(connection: &Connection) -> Result<(), StorageError> {
    connection.execute_batch(MIGRATION_0001)?;

    const ADDITIVE_MIGRATIONS: [(&str, &str); 9] = [
        ("0002", MIGRATION_0002),
        ("0003", MIGRATION_0003),
        ("0004", MIGRATION_0004),
        ("0005", MIGRATION_0005),
        ("0006", MIGRATION_0006),
        ("0007", MIGRATION_0007),
        ("0008", MIGRATION_0008),
        ("0009", MIGRATION_0009),
        ("0010", MIGRATION_0010),
    ];

    for (name, sql) in ADDITIVE_MIGRATIONS {
        match connection.execute_batch(sql) {
            Ok(()) => {}
            Err(error) if is_already_applied(&error) => {
                tracing::debug!(migration = name, "migration already applied");
            }
            Err(error) => {
                tracing::error!(migration = name, %error, "migration failed");
                return Err(StorageError::Internal(format!(
                    "migration {name} failed: {error}"
                )));
            }
        }
    }
    Ok(())
}

fn decode_ruleset_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<RulesetRecord> {
    Ok(RulesetRecord {
        id: row_uuid(0, &row.get::<_, String>(0)?)?,
        hash: row.get(1)?,
        status: row.get(2)?,
        created_at: parse_datetime(&row.get::<_, String>(3)?).map_err(to_sqlite_error)?,
        artifact_json: row.get(4)?,
    })
}

fn parse_datetime(value: &str) -> Result<DateTime<Utc>, chrono::ParseError> {
    Ok(DateTime::parse_from_rfc3339(value)?.with_timezone(&Utc))
}

fn to_sqlite_error(error: chrono::ParseError) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(error))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Migrations run on every boot, so they must be safe to apply repeatedly.
    ///
    /// Regression guard for a real outage: migration 0010 ended with a plain
    /// `INSERT INTO config_migrations`, whose `version` column is UNIQUE. That failure was
    /// invisible while migration results were discarded, but once they became fatal it meant the
    /// server refused to start against ANY pre-existing database — every install would have been
    /// bricked by the upgrade that was meant to make failures visible.
    #[test]
    fn migrations_are_idempotent_across_restarts() {
        let connection = Connection::open_in_memory().expect("open in-memory database");

        apply_migrations(&connection).expect("first boot should migrate cleanly");
        apply_migrations(&connection).expect("second boot must not fail on an existing database");
        apply_migrations(&connection).expect("third boot must still succeed");

        // The seeded row must exist exactly once, not be duplicated by the re-runs.
        let count: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM config_migrations WHERE version = 1",
                [],
                |row| row.get(0),
            )
            .expect("query seeded migration row");
        assert_eq!(
            count, 1,
            "re-running migrations must not duplicate seeded rows"
        );
    }

    /// A genuine SQL failure must stop startup rather than leaving the server running against a
    /// schema that is missing tables or columns.
    #[test]
    fn a_real_migration_error_is_not_mistaken_for_already_applied() {
        let duplicate_column = rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(1),
            Some("duplicate column name: foo".to_string()),
        );
        assert!(is_already_applied(&duplicate_column));

        let disk_full = rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(13),
            Some("database or disk is full".to_string()),
        );
        assert!(
            !is_already_applied(&disk_full),
            "a real failure must not be treated as an already-applied migration"
        );
    }
}
