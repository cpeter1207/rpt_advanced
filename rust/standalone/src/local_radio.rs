//! Local tuner requests use the product's serialized radio-maintenance boundary.

use crate::{abi, runtime::ProductRuntime};
use serde_json::{Value, json};
use std::{
    ffi::{c_char, c_void},
    mem::size_of,
    ptr,
};

impl ProductRuntime {
    pub(crate) fn local_radio_request(&self, request: &Value) -> Result<Value, String> {
        match request["operation"].as_str() {
            Some("nodes") => {
                let configuration = self.configuration_text();
                let nodes: Vec<_> = crate::resolve_radio_nodes(&configuration)
                    .map_err(|error| error.to_string())?
                    .into_iter()
                    .filter(|radio| radio.enabled)
                    .map(|radio| radio.node.as_str().to_owned())
                    .collect();
                Ok(json!(nodes))
            }
            Some("configuration-path") => {
                let path = self
                    .fixed_configuration_path()
                    .ok_or("configuration path unavailable")?
                    .canonicalize()
                    .map_err(|error| error.to_string())?;
                Ok(json!(
                    path.to_str().ok_or("configuration path is not UTF-8")?
                ))
            }
            Some(operation) => {
                let node = request["node"].as_str().ok_or("missing local node")?;
                let mut native = decode(operation, request)?;
                let text = self.local_radio_control(node, &mut native)?;
                match native.operation {
                    1 | 5 | 6 => Ok(json!(text)),
                    2 => Ok(
                        json!({"command":native.command.command,"target":native.command.target,
                        "value":native.command.value,"flags":native.command.flags,
                        "eeprom_words":native.command.eeprom_words.as_slice()}),
                    ),
                    _ => Ok(Value::Null),
                }
            }
            None => Err("missing local operation".into()),
        }
    }

    pub(crate) fn clear_local_manual(&self, node: &str) {
        let mut request = empty(4);
        if let Err(error) = self.local_radio_control(node, &mut request) {
            eprintln!("rpt-advanced manual radio cleanup for {node} failed: {error}");
        }
    }

    fn local_radio_control(
        &self,
        node: &str,
        request: &mut abi::rptadv_radio_control_v1,
    ) -> Result<String, String> {
        let call = self
            .product_descriptor()
            .radio_control
            .ok_or("radio maintenance unavailable")?;
        let mut text = Ok(String::new());
        // SAFETY: the product copies the request before dispatch and invokes the
        // synchronous sink while this caller retains all request/result storage.
        let status = unsafe {
            call(
                node.as_ptr().cast(),
                node.len(),
                request,
                Some(copy_text),
                ptr::from_mut(&mut text).cast(),
            )
        };
        if status != 0 {
            return Err(format!("radio maintenance failed ({status})"));
        }
        text
    }
}

fn empty(operation: u32) -> abi::rptadv_radio_control_v1 {
    let mut request: abi::rptadv_radio_control_v1 = unsafe { std::mem::zeroed() };
    request.struct_size = size_of_val(&request) as u32;
    request.abi_version = 1;
    request.operation = operation;
    request.command.struct_size = size_of::<abi::UrpAstChannelCommand>() as u32;
    request.command.abi_version = 1;
    request
}

fn number(request: &Value, field: &str) -> Result<u32, String> {
    request[field]
        .as_u64()
        .and_then(|value| value.try_into().ok())
        .ok_or_else(|| format!("invalid {field}"))
}

fn decode(operation: &str, request: &Value) -> Result<abi::rptadv_radio_control_v1, String> {
    let operation = match operation {
        "status" => 1,
        "command" => 2,
        "transmit" => 3,
        "processing-stats" => 5,
        "active-configuration" => 6,
        _ => return Err("unknown radio operation".into()),
    };
    let mut native = empty(operation);
    if operation == 2 {
        native.command.command = number(request, "command")?;
        native.command.target = number(request, "target")?;
        native.command.value = request["value"].as_i64().ok_or("invalid command value")?;
        native.command.flags = number(request, "flags")?;
        if let Some(words) = request.get("eeprom_words") {
            let words = words
                .as_array()
                .filter(|words| words.len() == 64)
                .ok_or("EEPROM requires 64 words")?;
            for (output, value) in native.command.eeprom_words.iter_mut().zip(words) {
                *output = value
                    .as_u64()
                    .and_then(|value| value.try_into().ok())
                    .ok_or("invalid EEPROM word")?;
            }
        }
    } else if operation == 3 {
        native.keyed = u32::from(
            request["keyed"]
                .as_bool()
                .ok_or("invalid transmit request")?,
        );
        if request.get("forced_ctcss_tenths_hz").is_some() {
            native.forced_ctcss_tenths_hz = number(request, "forced_ctcss_tenths_hz")?;
        }
    }
    Ok(native)
}

unsafe extern "C" fn copy_text(context: *mut c_void, pointer: *const c_char, length: usize) {
    // SAFETY: only local_radio_control invokes this sink with retained output storage.
    let output = unsafe { &mut *context.cast::<Result<String, String>>() };
    if length > 65_536 || (pointer.is_null() && length != 0) {
        *output = Err("invalid radio result span".into());
        return;
    }
    let bytes = if length == 0 {
        &[]
    } else {
        // SAFETY: the provider advertises a complete readable span for this invocation.
        unsafe { std::slice::from_raw_parts(pointer.cast::<u8>(), length) }
    };
    match std::str::from_utf8(bytes) {
        Ok(text) => {
            if let Ok(value) = output {
                if value.len().saturating_add(text.len()) <= 65_536 {
                    value.push_str(text);
                } else {
                    *output = Err("radio result exceeds frame limit".into());
                }
            }
        }
        Err(_) => *output = Err("invalid UTF-8 radio result".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn typed_commands_preserve_units_and_reject_invalid_wire_values() {
        let command = json!({"command":2,"target":1,"value":-30,"flags":0});
        let decoded = decode("command", &command).unwrap();
        assert_eq!(decoded.command.value, -30);
        assert_eq!(decoded.command.target, 1);
        assert_eq!(
            decode(
                "transmit",
                &json!({"keyed":true,"forced_ctcss_tenths_hz":1148})
            )
            .unwrap()
            .forced_ctcss_tenths_hz,
            1148
        );
        for command in [
            json!({}),
            json!({"command":-1,"target":1,"value":0,"flags":0}),
            json!({"command":5,"target":0,"value":0,"flags":0,"eeprom_words":[1]}),
        ] {
            assert!(decode("command", &command).is_err());
        }
        assert!(decode("transmit", &json!({"keyed":1})).is_err());
        assert!(decode("shell", &json!({})).is_err());
        let words = vec![65535_u16; 64];
        let request = json!({"command":5,"target":0,"value":0,"flags":0,"eeprom_words":words});
        assert_eq!(
            decode("command", &request).unwrap().command.eeprom_words[63],
            65535
        );
        let mut invalid = request;
        invalid["eeprom_words"][0] = json!(65536);
        assert!(decode("command", &invalid).is_err());
        assert_eq!(
            decode("transmit", &json!({"keyed":false}))
                .unwrap()
                .forced_ctcss_tenths_hz,
            0
        );
        assert!(
            decode(
                "transmit",
                &json!({"keyed":true,"forced_ctcss_tenths_hz":-1})
            )
            .is_err()
        );
    }

    #[test]
    fn radio_text_sink_rejects_bad_spans_and_retains_the_first_error() {
        let mut output: Result<String, String> = Ok(String::new());
        let pointer = ptr::from_mut(&mut output).cast();
        unsafe { copy_text(pointer, ptr::null(), 0) };
        assert_eq!(output.as_deref(), Ok(""));
        let invalid = [0xff_u8];
        for (bytes, length) in [
            (ptr::null(), 1),
            (ptr::null(), 65_537),
            (invalid.as_ptr().cast(), 1),
        ] {
            output = Ok(String::new());
            unsafe { copy_text(ptr::from_mut(&mut output).cast(), bytes, length) };
            assert!(output.is_err());
        }
        output = Ok("a".repeat(65_536));
        unsafe { copy_text(ptr::from_mut(&mut output).cast(), c"x".as_ptr(), 1) };
        assert!(output.is_err());
        unsafe { copy_text(ptr::from_mut(&mut output).cast(), c"valid".as_ptr(), 5) };
        assert!(output.is_err());
    }
}
