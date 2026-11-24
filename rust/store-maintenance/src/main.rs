//! Thin local client of the checked versioned dynamic product maintenance boundary.
#![deny(warnings)]
use libloading::Library;
use serde_json::{Value, json};
use std::{
    ffi::{c_char, c_void},
    path::Path,
    process::ExitCode,
};

#[repr(C)]
#[derive(Clone, Copy)]
struct Header {
    size: u32,
    abi: u32,
    capability: [u8; 16],
}
type Sink = unsafe extern "C" fn(*mut c_void, *const c_char, usize);
type Run = unsafe extern "C" fn(u32, *const c_char, usize, Option<Sink>, *mut c_void) -> i32;
#[repr(C)]
struct Descriptor {
    header: Header,
    run: Option<Run>,
}
fn product_valid(header: &Header) -> bool {
    header.size == 128 && header.abi == 7 && header.capability == *b"rptadv.prod7\0\0\0\0"
}
fn maintenance_header_valid(header: &Header) -> bool {
    header.size as usize == std::mem::size_of::<Descriptor>()
        && header.abi == 1
        && header.capability == *b"rptadv.store1\0\0\0"
}
fn maintenance_valid(api: &Descriptor) -> bool {
    maintenance_header_valid(&api.header) && api.run.is_some()
}
fn request(args: &[String]) -> Result<(u32, Vec<u8>), ()> {
    if args.len() < 3 {
        return Err(());
    }
    let mut value = json!({"store_root":args[1],"store_uuid":args[2]});
    let expected = |text: &str| {
        if text == "none" {
            Value::Null
        } else {
            json!(text)
        }
    };
    let operation = match (args[0].as_str(), args.len()) {
        ("init", 4) => {
            value["write_key_id"] = json!(args[3]);
            7
        }
        ("inspect", 3) => 1,
        ("gc", 4) => {
            value["expected_state_sha256"] = expected(&args[3]);
            2
        }
        ("backup", 4) => {
            value["other_root"] = json!(args[3]);
            3
        }
        ("restore", 7) => {
            value["other_root"] = json!(args[3]);
            value["write_key_id"] = json!(args[4]);
            value["expected_state_sha256"] = expected(&args[5]);
            value["working_sha256"] = json!(args[6]);
            4
        }
        ("rotate", 6) => {
            value["write_key_id"] = json!(args[3]);
            value["new_key_id"] = json!(args[4]);
            value["expected_state_sha256"] = expected(&args[5]);
            5
        }
        ("retain", 6) => {
            value["expected_state_sha256"] = expected(&args[3]);
            value["write_key_id"] = json!(args[4]);
            value["retained_recovery"] = serde_json::from_str(&args[5]).map_err(|_| ())?;
            6
        }
        _ => return Err(()),
    };
    let bytes = serde_json::to_vec(&value).map_err(|_| ())?;
    if bytes.len() > 64 * 1024 {
        return Err(());
    }
    Ok((operation, bytes))
}
struct Output {
    bytes: Vec<u8>,
    invalid: bool,
}
unsafe extern "C" fn output(context: *mut c_void, bytes: *const c_char, length: usize) {
    // This callback is invoked synchronously with our live Output as its context.
    let output = unsafe { &mut *context.cast::<Output>() };
    if bytes.is_null()
        || output
            .bytes
            .len()
            .checked_add(length)
            .is_none_or(|n| n > 64 * 1024)
        || output.bytes.try_reserve(length).is_err()
    {
        output.invalid = true;
        return;
    }
    // The checked product owns this borrowed readable result span for this call.
    output
        .bytes
        .extend_from_slice(unsafe { std::slice::from_raw_parts(bytes.cast::<u8>(), length) });
}
fn execute(operation: u32, request: &[u8]) -> Result<bool, ()> {
    let path = Path::new(env!("RPT_ADVANCED_LIBDIR")).join("librptadv_product.so.2");
    // Only the installed/private versioned product is loaded; it must obey its getter ABI.
    let library = unsafe { Library::new(path) }.map_err(|_| ())?;
    let product = unsafe {
        library.get::<unsafe extern "C" fn() -> *const Header>(b"rptadv_product_descriptor_v1\0")
    }
    .map_err(|_| ())?;
    let product = unsafe { product() };
    if product.is_null() || !product.is_aligned() || !product_valid(unsafe { &*product }) {
        return Err(());
    }
    let getter = unsafe {
        library.get::<unsafe extern "C" fn() -> *const Descriptor>(
            b"rptadv_product_store_maintenance_descriptor_v1\0",
        )
    }
    .map_err(|_| ())?;
    let api = unsafe { getter() };
    if api.is_null()
        || !api.is_aligned()
        || !maintenance_header_valid(unsafe { &*api.cast::<Header>() })
    {
        return Err(());
    }
    // The complete-size prefix was checked before borrowing the function-pointer tail.
    let api = unsafe { &*api };
    if !maintenance_valid(api) {
        return Err(());
    }
    let mut result = Output {
        bytes: Vec::new(),
        invalid: false,
    };
    // Retain the DSO and caller buffers through the synchronous versioned callback.
    let status = unsafe {
        api.run.ok_or(())?(
            operation,
            request.as_ptr().cast::<c_char>(),
            request.len(),
            Some(output),
            (&mut result as *mut Output).cast::<c_void>(),
        )
    };
    if result.invalid {
        return Err(());
    }
    let text = String::from_utf8(result.bytes).map_err(|_| ())?;
    if text.is_empty() {
        return Err(());
    }
    println!("{text}");
    Ok(status == 0)
}
fn main() -> ExitCode {
    let args: Vec<_> = std::env::args().skip(1).collect();
    let result = request(&args).and_then(|(operation, bytes)| execute(operation, &bytes));
    match result {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(()) => {
            eprintln!(
                "Protected-store command unavailable or invalid. Use init ROOT UUID KEY_ID; inspect ROOT UUID; gc ROOT UUID STATE; backup ROOT UUID DEST; restore ROOT UUID BACKUP KEY_ID STATE_OR_none WORKING_OR_empty; rotate ROOT UUID OLD_KEY_ID NEW_KEY_ID STATE; retain ROOT UUID STATE KEY_ID ROOTS_JSON."
            );
            ExitCode::FAILURE
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    unsafe extern "C" fn run(
        _: u32,
        _: *const c_char,
        _: usize,
        _: Option<Sink>,
        _: *mut c_void,
    ) -> i32 {
        0
    }
    #[test]
    fn optional_boundary_requires_complete_exact_versioned_table() {
        let mut api = Descriptor {
            header: Header {
                size: 32,
                abi: 1,
                capability: *b"rptadv.store1\0\0\0",
            },
            run: Some(run),
        };
        assert!(maintenance_valid(&api));
        api.header.size = 24;
        assert!(!maintenance_valid(&api));
        api.header.size = 32;
        api.header.abi = 2;
        assert!(!maintenance_valid(&api));
        api.header.abi = 1;
        api.header.capability[0] = 0;
        assert!(!maintenance_valid(&api));
        api.header.capability = *b"rptadv.store1\0\0\0";
        api.run = None;
        assert!(!maintenance_valid(&api));
    }
    #[test]
    fn old_product_table_remains_exactly_128_bytes_and_abi_seven() {
        let mut header = Header {
            size: 128,
            abi: 7,
            capability: *b"rptadv.prod7\0\0\0\0",
        };
        assert!(product_valid(&header));
        header.size = 136;
        assert!(!product_valid(&header));
        header.size = 128;
        header.abi = 8;
        assert!(!product_valid(&header));
        header.abi = 7;
        header.capability[0] = 0;
        assert!(!product_valid(&header));
    }
}
