//! Explicit administrative telephone operations on the existing local socket.
use super::{ProductRuntime, abi};
use serde_json::{Value, json};

impl ProductRuntime {
    /// The product copies borrowed fields synchronously and owns the accepted call.
    /// Socket closure releases no telephone state; hangup requires its exact ID.
    pub(crate) fn local_phone_request(
        &self,
        operation: &str,
        request: &Value,
    ) -> Result<Value, String> {
        if self.host.is_none() || self.reload_fatal() {
            return Err("telephone control unavailable".into());
        }
        let call = self
            .product
            .phone_command
            .ok_or("telephone control unavailable")?;
        let local = request["node"].as_str().ok_or("missing local node")?;
        let id = match request.get("call_id") {
            Some(value) => value.as_u64().ok_or("invalid telephone call ID")?,
            None => 0,
        };
        let (operation, account, destination) = match operation {
            "phone-dial" if id == 0 => (
                abi::RPTADV_PHONE_DIAL,
                request["account"]
                    .as_str()
                    .ok_or("missing telephone account")?,
                request["destination"]
                    .as_str()
                    .ok_or("missing telephone destination")?,
            ),
            "phone-hangup"
                if id != 0
                    && ["account", "destination"].into_iter().all(|field| {
                        request
                            .get(field)
                            .is_none_or(|value| value.as_str() == Some(""))
                    }) =>
            {
                (abi::RPTADV_PHONE_HANGUP, "", "")
            }
            _ => return Err("invalid telephone operation".into()),
        };
        let native = abi::rptadv_phone_command_v1 {
            struct_size: std::mem::size_of::<abi::rptadv_phone_command_v1>() as u32,
            abi_version: abi::RPTADV_PHONE_COMMAND_ABI_VERSION,
            operation,
            account: account.as_ptr().cast(),
            account_length: account.len(),
            destination: destination.as_ptr().cast(),
            destination_length: destination.len(),
            call_id: id,
        };
        let mut accepted = 0;
        // SAFETY: the retained descriptor owns its code; all UTF-8 spans, request
        // and output remain live for this synchronous, non-audio invocation.
        let status = unsafe { call(local.as_ptr().cast(), local.len(), &native, &mut accepted) };
        if status != 0 {
            return Err(format!("telephone operation failed ({status})"));
        }
        Ok(json!({"accepted":true,"call_id":accepted}))
    }
}
