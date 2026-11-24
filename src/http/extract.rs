//! Fiber `Bind().Body` equivalent: JSON, url-encoded form, or multipart
//! (text fields bound like a form, file parts collected separately).
//! Repeated form keys bind to `Vec` fields (e.g. `mentions`).
//!
//! A body that cannot be bound panics in GOWA and surfaces as a 500, so the
//! rejection is `INTERNAL_SERVER_ERROR` with the parser's message.

use std::collections::HashMap;

use axum::{
    extract::{FromRequest, Multipart, Request},
    http::header::CONTENT_TYPE,
};
use serde::de::DeserializeOwned;

use crate::{error::ApiError, media::Upload, usecase::schedule_time::ScheduleOptions};

pub struct Payload<T> {
    pub value: T,
    pub files: HashMap<String, Upload>,
    /// `scheduled_at`, `timezone`, … read from the same body (default when absent).
    pub schedule: ScheduleOptions,
}

impl<T> Payload<T> {
    pub fn file(&mut self, name: &str) -> Option<Upload> {
        self.files.remove(name)
    }
}

/// Body without file parts.
pub struct Body<T>(pub T);

impl<T, S> FromRequest<S> for Body<T>
where
    T: DeserializeOwned + Send,
    S: Send + Sync,
{
    type Rejection = ApiError;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        Ok(Self(Payload::<T>::from_request(req, state).await?.value))
    }
}

fn form<T: DeserializeOwned>(body: &str) -> Result<T, ApiError> {
    serde_html_form::from_str(body).map_err(ApiError::internal)
}

/// Only bodies that mention `scheduled_at` are bound for schedule options, so
/// other fields never fail a request that is not scheduled.
fn schedule_from_form(body: &str) -> Result<ScheduleOptions, ApiError> {
    if body.contains("scheduled_at") {
        form(body)
    } else {
        Ok(ScheduleOptions::default())
    }
}

impl<T, S> FromRequest<S> for Payload<T>
where
    T: DeserializeOwned + Send,
    S: Send + Sync,
{
    type Rejection = ApiError;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        let content_type = req
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_ascii_lowercase();

        if content_type.starts_with("multipart/form-data") {
            let mut multipart = Multipart::from_request(req, state)
                .await
                .map_err(|e| ApiError::internal(e.body_text()))?;
            let mut fields: Vec<(String, String)> = Vec::new();
            let mut files = HashMap::new();
            while let Some(field) = multipart
                .next_field()
                .await
                .map_err(|e| ApiError::internal(e.body_text()))?
            {
                let name = field.name().unwrap_or_default().to_owned();
                match field.file_name().map(str::to_owned) {
                    Some(filename) => {
                        let content_type = field.content_type().unwrap_or_default().to_owned();
                        let bytes = field
                            .bytes()
                            .await
                            .map_err(|e| ApiError::internal(e.body_text()))?;
                        files.insert(
                            name,
                            Upload {
                                filename,
                                content_type,
                                bytes: bytes.to_vec(),
                            },
                        );
                    }
                    None => {
                        let text = field
                            .text()
                            .await
                            .map_err(|e| ApiError::internal(e.body_text()))?;
                        fields.push((name, text));
                    }
                }
            }
            let encoded = serde_html_form::to_string(&fields).map_err(ApiError::internal)?;
            return Ok(Self {
                value: form(&encoded)?,
                files,
                schedule: schedule_from_form(&encoded)?,
            });
        }

        let bytes = axum::body::Bytes::from_request(req, state)
            .await
            .map_err(|e| ApiError::internal(e.body_text()))?;
        let (value, schedule) = if content_type.starts_with("application/x-www-form-urlencoded") {
            let text = String::from_utf8_lossy(&bytes);
            (form(&text)?, schedule_from_form(&text)?)
        } else if bytes.is_empty() {
            (
                serde_json::from_str("{}").map_err(ApiError::internal)?,
                ScheduleOptions::default(),
            )
        } else {
            let schedule = if bytes.windows(12).any(|w| w == b"scheduled_at") {
                serde_json::from_slice(&bytes).map_err(ApiError::internal)?
            } else {
                ScheduleOptions::default()
            };
            (
                serde_json::from_slice(&bytes).map_err(ApiError::internal)?,
                schedule,
            )
        };
        Ok(Self {
            value,
            files: HashMap::new(),
            schedule,
        })
    }
}
