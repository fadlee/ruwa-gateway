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

use crate::{error::ApiError, media::Upload};

pub struct Payload<T> {
    pub value: T,
    pub files: HashMap<String, Upload>,
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
            });
        }

        let bytes = axum::body::Bytes::from_request(req, state)
            .await
            .map_err(|e| ApiError::internal(e.body_text()))?;
        let value = if content_type.starts_with("application/x-www-form-urlencoded") {
            form(&String::from_utf8_lossy(&bytes))?
        } else if bytes.is_empty() {
            serde_json::from_str("{}").map_err(ApiError::internal)?
        } else {
            serde_json::from_slice(&bytes).map_err(ApiError::internal)?
        };
        Ok(Self {
            value,
            files: HashMap::new(),
        })
    }
}
