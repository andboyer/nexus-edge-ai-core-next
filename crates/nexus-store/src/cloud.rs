//! Cloud-enrollment artefact persistence (single-row table).
//!
//! Populated by `nexus-engine enroll` after a successful round-trip to
//! cloud-side `enrollment-svc /v1/enroll`. Read on every subsequent
//! `nexus-engine serve` boot to decide whether to spawn the WSS tunnel.
//!
//! See `migrations/0013_cloud_enrollment.sql` for the schema.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::Row as _;

use crate::Store;

/// In-memory mirror of the `cloud_enrollment` row.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CloudEnrollment {
    /// Cloud-assigned `cores.id`.
    pub core_id: String,
    /// `wss://` URL the engine dials on boot.
    pub gateway_url: String,
    /// PEM-encoded mTLS leaf certificate signed by the internal CA.
    pub cert_pem: String,
    /// PEM-encoded PKCS#8 private key pairing with `cert_pem`.
    pub private_key_pem: String,
    /// PEM-encoded internal CA chain the engine validates the gateway
    /// against.
    pub ca_chain_pem: String,
    /// Initial entitlement JWT (`aud = "nexus-edge"`).
    pub entitlement_jwt: String,
    /// Optional Ed25519 verifying-key PEM (forward-compat from the
    /// cloud `EnrollResponse`).
    pub signing_key_pem: Option<String>,
    /// Optional `kid` to bind the verifying key to.
    pub signing_kid: Option<String>,
    /// UTC timestamp of the enrollment round-trip.
    pub enrolled_at: DateTime<Utc>,
}

impl Store {
    /// Persist (or overwrite) the cloud enrollment row.
    ///
    /// # Errors
    ///
    /// Returns [`crate::StoreError::Sqlx`] on database failure.
    pub async fn set_cloud_enrollment(&self, e: &CloudEnrollment) -> Result<(), crate::StoreError> {
        sqlx::query(
            r#"
            INSERT INTO cloud_enrollment
                (id, core_id, gateway_url, cert_pem, private_key_pem,
                 ca_chain_pem, entitlement_jwt, signing_key_pem, signing_kid)
            VALUES (1, ?, ?, ?, ?, ?, ?, ?, ?)
            ON CONFLICT(id) DO UPDATE SET
                core_id          = excluded.core_id,
                gateway_url      = excluded.gateway_url,
                cert_pem         = excluded.cert_pem,
                private_key_pem  = excluded.private_key_pem,
                ca_chain_pem     = excluded.ca_chain_pem,
                entitlement_jwt  = excluded.entitlement_jwt,
                signing_key_pem  = excluded.signing_key_pem,
                signing_kid      = excluded.signing_kid,
                enrolled_at      = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
            "#,
        )
        .bind(&e.core_id)
        .bind(&e.gateway_url)
        .bind(&e.cert_pem)
        .bind(&e.private_key_pem)
        .bind(&e.ca_chain_pem)
        .bind(&e.entitlement_jwt)
        .bind(e.signing_key_pem.as_deref())
        .bind(e.signing_kid.as_deref())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Read the single cloud-enrollment row, if present.
    ///
    /// # Errors
    ///
    /// Returns [`crate::StoreError::Sqlx`] on database failure.
    pub async fn get_cloud_enrollment(&self) -> Result<Option<CloudEnrollment>, crate::StoreError> {
        let row = sqlx::query(
            r#"
            SELECT core_id, gateway_url, cert_pem, private_key_pem,
                   ca_chain_pem, entitlement_jwt, signing_key_pem,
                   signing_kid, enrolled_at
              FROM cloud_enrollment
             WHERE id = 1
            "#,
        )
        .fetch_optional(&self.pool)
        .await?;
        let Some(row) = row else { return Ok(None) };
        let enrolled_at_str: String = row.try_get("enrolled_at")?;
        let enrolled_at = DateTime::parse_from_rfc3339(&enrolled_at_str)
            .map_err(|e| crate::StoreError::Decode(format!("enrolled_at parse: {e}")))?
            .with_timezone(&Utc);
        Ok(Some(CloudEnrollment {
            core_id: row.try_get("core_id")?,
            gateway_url: row.try_get("gateway_url")?,
            cert_pem: row.try_get("cert_pem")?,
            private_key_pem: row.try_get("private_key_pem")?,
            ca_chain_pem: row.try_get("ca_chain_pem")?,
            entitlement_jwt: row.try_get("entitlement_jwt")?,
            signing_key_pem: row.try_get("signing_key_pem")?,
            signing_kid: row.try_get("signing_kid")?,
            enrolled_at,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Store;
    use nexus_config::StoreConfig;
    use tempfile::TempDir;

    async fn fresh_store() -> (Store, TempDir) {
        let tmp = TempDir::new().unwrap();
        let cfg = StoreConfig {
            url: format!("sqlite://{}/store.db?mode=rwc", tmp.path().display()),
            ..StoreConfig::default()
        };
        let store = Store::open(&cfg).await.expect("open store");
        (store, tmp)
    }

    fn sample() -> CloudEnrollment {
        CloudEnrollment {
            core_id: "11111111-2222-3333-4444-555555555555".into(),
            gateway_url: "wss://gateway.test/v1/tunnel".into(),
            cert_pem: "-----BEGIN CERTIFICATE-----\nAAAA\n-----END CERTIFICATE-----\n".into(),
            private_key_pem: "-----BEGIN PRIVATE KEY-----\nBBBB\n-----END PRIVATE KEY-----\n"
                .into(),
            ca_chain_pem: "-----BEGIN CERTIFICATE-----\nCCCC\n-----END CERTIFICATE-----\n".into(),
            entitlement_jwt: "eyJ.fake.jwt".into(),
            signing_key_pem: Some(
                "-----BEGIN PUBLIC KEY-----\nDDDD\n-----END PUBLIC KEY-----\n".into(),
            ),
            signing_kid: Some("kid-1".into()),
            enrolled_at: Utc::now(), // overwritten by the DB default
        }
    }

    #[tokio::test]
    async fn round_trips_through_storage() {
        let (store, _tmp) = fresh_store().await;
        assert!(store.get_cloud_enrollment().await.unwrap().is_none());
        store.set_cloud_enrollment(&sample()).await.unwrap();
        let got = store
            .get_cloud_enrollment()
            .await
            .unwrap()
            .expect("present");
        assert_eq!(got.core_id, "11111111-2222-3333-4444-555555555555");
        assert_eq!(got.gateway_url, "wss://gateway.test/v1/tunnel");
        assert_eq!(got.signing_kid.as_deref(), Some("kid-1"));
    }

    #[tokio::test]
    async fn upsert_overwrites_in_place() {
        let (store, _tmp) = fresh_store().await;
        store.set_cloud_enrollment(&sample()).await.unwrap();
        let mut second = sample();
        second.core_id = "99999999-9999-9999-9999-999999999999".into();
        second.signing_kid = Some("kid-2".into());
        store.set_cloud_enrollment(&second).await.unwrap();
        let got = store
            .get_cloud_enrollment()
            .await
            .unwrap()
            .expect("present");
        assert_eq!(got.core_id, "99999999-9999-9999-9999-999999999999");
        assert_eq!(got.signing_kid.as_deref(), Some("kid-2"));
    }
}
