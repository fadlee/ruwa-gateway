//! Device registry (GOWA `DeviceRecord` + `DeviceWebhookConfig`).

use diesel::prelude::*;

use super::Db;

diesel::table! {
    devices (device_id) {
        device_id -> Text,
        storage_key -> Text,
        display_name -> Text,
        jid -> Text,
        webhook_url -> Nullable<Text>,
        webhook_secret -> Text,
        webhook_events -> Text,
        webhook_insecure_skip_verify -> Bool,
        created_at -> BigInt,
        updated_at -> BigInt,
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Queryable, Selectable, Insertable)]
#[diesel(table_name = devices)]
pub struct DeviceRecord {
    pub device_id: String,
    pub storage_key: String,
    pub display_name: String,
    pub jid: String,
    pub webhook_url: Option<String>,
    pub webhook_secret: String,
    pub webhook_events: String,
    pub webhook_insecure_skip_verify: bool,
    pub created_at: i64,
    pub updated_at: i64,
}

/// Per-device webhook settings as the API exposes them.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DeviceWebhook {
    pub webhook_url: Option<String>,
    pub webhook_secret: String,
    pub webhook_events: String,
    pub webhook_insecure_skip_verify: bool,
}

impl DeviceRecord {
    pub fn new(device_id: &str, storage_key: &str, now: i64) -> Self {
        Self {
            device_id: device_id.to_owned(),
            storage_key: storage_key.to_owned(),
            display_name: String::new(),
            jid: String::new(),
            webhook_url: None,
            webhook_secret: String::new(),
            webhook_events: String::new(),
            webhook_insecure_skip_verify: false,
            created_at: now,
            updated_at: now,
        }
    }

    pub fn webhook(&self) -> DeviceWebhook {
        DeviceWebhook {
            webhook_url: self.webhook_url.clone(),
            webhook_secret: self.webhook_secret.clone(),
            webhook_events: self.webhook_events.clone(),
            webhook_insecure_skip_verify: self.webhook_insecure_skip_verify,
        }
    }
}

#[derive(Clone)]
pub struct Registry {
    db: Db,
}

impl Registry {
    pub fn new(db: Db) -> Self {
        Self { db }
    }

    pub async fn all(&self) -> anyhow::Result<Vec<DeviceRecord>> {
        self.db
            .run(|c| {
                devices::table
                    .order(devices::created_at.asc())
                    .select(DeviceRecord::as_select())
                    .load(c)
            })
            .await
    }

    pub async fn insert(&self, record: DeviceRecord) -> anyhow::Result<()> {
        self.db
            .run(move |c| {
                diesel::insert_into(devices::table)
                    .values(&record)
                    .execute(c)
            })
            .await?;
        Ok(())
    }

    pub async fn delete(&self, device_id: &str) -> anyhow::Result<bool> {
        let id = device_id.to_owned();
        let n = self
            .db
            .run(move |c| diesel::delete(devices::table.find(id)).execute(c))
            .await?;
        Ok(n > 0)
    }

    pub async fn set_identity(
        &self,
        device_id: &str,
        jid: &str,
        display_name: &str,
        now: i64,
    ) -> anyhow::Result<()> {
        let (id, jid, name) = (
            device_id.to_owned(),
            jid.to_owned(),
            display_name.to_owned(),
        );
        self.db
            .run(move |c| {
                diesel::update(devices::table.find(id))
                    .set((
                        devices::jid.eq(jid),
                        devices::display_name.eq(name),
                        devices::updated_at.eq(now),
                    ))
                    .execute(c)
            })
            .await?;
        Ok(())
    }

    pub async fn set_webhook(
        &self,
        device_id: &str,
        hook: DeviceWebhook,
        now: i64,
    ) -> anyhow::Result<()> {
        let id = device_id.to_owned();
        self.db
            .run(move |c| {
                diesel::update(devices::table.find(id))
                    .set((
                        devices::webhook_url.eq(hook.webhook_url),
                        devices::webhook_secret.eq(hook.webhook_secret),
                        devices::webhook_events.eq(hook.webhook_events),
                        devices::webhook_insecure_skip_verify.eq(hook.webhook_insecure_skip_verify),
                        devices::updated_at.eq(now),
                    ))
                    .execute(c)
            })
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn roundtrip() {
        let reg = Registry::new(Db::open(":memory:").unwrap());
        reg.insert(DeviceRecord::new("a", "a", 1)).await.unwrap();
        reg.insert(DeviceRecord::new("b", "b", 2)).await.unwrap();
        reg.set_identity("a", "628@s.whatsapp.net", "Me", 3)
            .await
            .unwrap();
        let hook = DeviceWebhook {
            webhook_url: Some("https://x".into()),
            webhook_secret: "s".into(),
            webhook_events: "message".into(),
            webhook_insecure_skip_verify: true,
        };
        reg.set_webhook("b", hook.clone(), 4).await.unwrap();

        let all = reg.all().await.unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].jid, "628@s.whatsapp.net");
        assert_eq!(all[1].webhook(), hook);

        assert!(reg.delete("a").await.unwrap());
        assert!(!reg.delete("a").await.unwrap());
        assert!(reg.insert(DeviceRecord::new("b", "b2", 5)).await.is_err());
    }
}
