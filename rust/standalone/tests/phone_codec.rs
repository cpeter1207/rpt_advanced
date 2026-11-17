//! Actual loader/callback contracts using a fault-wrapping released codec DSO.
use rpt_advanced_standalone::phone::media::{G711, G711Codec, MediaError};
use std::{ffi::CStr, path::PathBuf, process::Command};

const FAULT: &str = "RPTADV_TEST_CODEC_FAULT";
const TEST: &str = "codec_rejects_provider_faults_and_reclaims_partial_construction";

struct FixtureDirectory(PathBuf);
impl Drop for FixtureDirectory {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}

/// Resolve the actual loaded release, never a guessed multiarch path or a mock codec.
fn released_provider() -> PathBuf {
    // SAFETY: the released descriptor's symbol address remains valid until its
    // owning library is dropped; dladdr's filename is copied while it is live.
    unsafe {
        let library = libloading::Library::new("librptadv_codec_g711.so.1").unwrap();
        let entry = library
            .get::<unsafe extern "C" fn()>(b"rptadv_codec_descriptor_v1\0")
            .unwrap();
        let mut info: libc::Dl_info = std::mem::zeroed();
        assert_ne!(libc::dladdr(*entry as *const libc::c_void, &mut info), 0);
        let path = CStr::from_ptr(info.dli_fname).to_str().unwrap();
        std::fs::canonicalize(path).unwrap()
    }
}

fn exercise_fault(fault: &str) {
    // SAFETY: the child loads only the controlled fixture DSO. The count symbol
    // returns an integer, and its library remains live through both codec drops.
    let library = unsafe { libloading::Library::new("librptadv_codec_g711.so.1") }.unwrap();
    let live = unsafe {
        library
            .get::<unsafe extern "C" fn() -> u32>(b"rptadv_test_codec_live_handles\0")
            .unwrap()
    };
    for (format, silence) in [(G711::Mulaw, 0xff), (G711::Alaw, 0xd5)] {
        let result = G711Codec::load(format);
        if matches!(
            fault,
            "null_table"
                | "short_table"
                | "abi_version"
                | "capability"
                | "missing_encoder"
                | "missing_decoder"
                | "missing_encode"
                | "missing_decode"
                | "missing_destroy"
                | "encoder_error"
                | "encoder_null"
                | "decoder_error"
                | "decoder_null"
        ) {
            assert!(matches!(result, Err(MediaError::Codec)), "{fault}");
        } else {
            let mut codec = result.unwrap();
            assert_eq!(unsafe { live() }, 2);
            let mut packet = [0; 160];
            let encoded = codec.encode(&[0.0; 160], &mut packet);
            if matches!(
                fault,
                "encode_error"
                    | "consumed_frames"
                    | "encoded_bytes"
                    | "source_frames"
                    | "packet_frames"
            ) {
                assert_eq!(encoded, Err(MediaError::Codec), "{fault}");
            } else {
                assert_eq!(encoded, Ok(()));
                assert_eq!(packet, [silence; 160]);
            }
            let mut pcm = [1.0; 160];
            let decoded = codec.decode(&[silence; 160], &mut pcm);
            if matches!(fault, "decode_error" | "decoded_frames") {
                assert_eq!(decoded, Err(MediaError::Codec), "{fault}");
            } else {
                assert_eq!(decoded, Ok(()));
                assert!(pcm.iter().all(|sample| sample.abs() < 0.001));
            }
            drop(codec);
        }
        assert_eq!(
            unsafe { live() },
            0,
            "{fault} leaked a real provider context"
        );
    }
}

#[test]
fn codec_rejects_provider_faults_and_reclaims_partial_construction() {
    if let Ok(fault) = std::env::var(FAULT) {
        exercise_fault(&fault);
        return;
    }
    let directory = FixtureDirectory(
        std::env::temp_dir().join(format!("rptadv-phone-codec-{}", std::process::id())),
    );
    std::fs::create_dir(&directory.0).unwrap();
    let source = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/phone_codec.c");
    let compiled = Command::new("timeout")
        .args([
            "15", "cc", "-shared", "-fPIC", "-Wall", "-Wextra", "-Werror", "-I",
        ])
        .arg(
            std::env::var_os("RPTADV_CODEC_INCLUDE")
                .unwrap_or_else(|| "/usr/include/rptadv-codec".into()),
        )
        .arg(source)
        .args(["-ldl", "-o"])
        .arg(directory.0.join("librptadv_codec_g711.so.1"))
        .output()
        .unwrap();
    assert!(
        compiled.status.success(),
        "{}",
        String::from_utf8_lossy(&compiled.stderr)
    );
    let released = released_provider();
    let mut paths = vec![directory.0.clone()];
    if let Some(original) = std::env::var_os("LD_LIBRARY_PATH") {
        paths.extend(std::env::split_paths(&original));
    }
    let paths = std::env::join_paths(paths).unwrap();
    for fault in [
        "none",
        "null_table",
        "short_table",
        "abi_version",
        "capability",
        "missing_encoder",
        "missing_decoder",
        "missing_encode",
        "missing_decode",
        "missing_destroy",
        "encoder_error",
        "encoder_null",
        "decoder_error",
        "decoder_null",
        "encode_error",
        "consumed_frames",
        "encoded_bytes",
        "source_frames",
        "packet_frames",
        "decode_error",
        "decoded_frames",
    ] {
        let mut command = Command::new("timeout");
        command
            .arg("15")
            .arg(std::env::current_exe().unwrap())
            .args(["--exact", TEST, "--test-threads=1", "--nocapture"])
            .env(FAULT, fault)
            .env("RPTADV_TEST_REAL_CODEC", &released)
            .env("LD_LIBRARY_PATH", &paths);
        if let Some(mut profile) = std::env::var_os("LLVM_PROFILE_FILE") {
            // Keep the export directory and module substitutions, with a unique
            // child filename even when the caller supplied a literal path.
            profile.push(format!(".phone-codec-{fault}-%p.profraw"));
            command.env("LLVM_PROFILE_FILE", profile);
        }
        let child = command.output().unwrap();
        assert!(
            child.status.success()
                && String::from_utf8_lossy(&child.stdout).contains("1 passed; 0 failed"),
            "{fault}: {}{}",
            String::from_utf8_lossy(&child.stdout),
            String::from_utf8_lossy(&child.stderr)
        );
    }
}
