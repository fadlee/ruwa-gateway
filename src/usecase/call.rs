//! `POST /call/reject` (GOWA `usecase/call.go`).

use serde::Deserialize;

use super::send::must_login;
use crate::{
    device::DeviceInstance,
    error::{ApiError, ApiResult},
    wa::jid,
};

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct RejectCallRequest {
    pub caller_jid: String,
    pub call_id: String,
}

pub async fn reject_call(device: &DeviceInstance, req: RejectCallRequest) -> ApiResult<()> {
    let (caller, call_id) = (req.caller_jid.trim(), req.call_id.trim());
    let mut errors = std::collections::BTreeMap::new();
    if caller.is_empty() {
        errors.insert("caller_jid", "cannot be blank".to_owned());
    }
    if call_id.is_empty() {
        errors.insert("call_id", "cannot be blank".to_owned());
    }
    super::chat::ozzo(errors)?;
    let client = must_login(device)?;
    let caller = jid::parse_jid(caller).map_err(ApiError::internal)?;
    let voip = client.voip();
    let reject = voip.reject_call(call_id, &caller, &caller);
    match tokio::time::timeout(std::time::Duration::from_secs(10), reject).await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(e)) => Err(ApiError::internal(format!("failed to reject call: {e}"))),
        Err(_) => Err(ApiError::internal(
            "failed to reject call: context deadline exceeded",
        )),
    }
}
