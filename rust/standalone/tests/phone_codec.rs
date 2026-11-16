//! Actual loader/callback contracts using a fault-wrapping released codec DSO.
use rpt_advanced_standalone::phone::media::{
    AudioCodec, AudioProfile, DecodeProvenance, MediaError,
};
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
fn released_provider_named(name: &str) -> PathBuf {
    // SAFETY: the released descriptor's symbol address remains valid until its
    // owning library is dropped; dladdr's filename is copied while it is live.
    unsafe {
        let library = libloading::Library::new(name).unwrap();
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
    let conceal_calls = unsafe {
        library
            .get::<unsafe extern "C" fn() -> u32>(b"rptadv_test_conceal_calls\0")
            .unwrap()
    };
    let decode_calls = unsafe {
        library
            .get::<unsafe extern "C" fn() -> u32>(b"rptadv_test_decode_calls\0")
            .unwrap()
    };
    for (format, silence) in [(AudioProfile::Mulaw, 0xff), (AudioProfile::Alaw, 0xd5)] {
        let result = AudioCodec::load(format);
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
                | "short_provenance"
                | "missing_provenance"
                | "unsupported_provenance"
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
            let inspected = codec.inspect(&[silence; 160]);
            if fault == "inspect_malformed" {
                assert_eq!(inspected, Err(MediaError::InvalidPacket));
            } else if fault.starts_with("inspect_") {
                assert_eq!(inspected, Err(MediaError::Codec), "{fault}");
            } else {
                assert_eq!(inspected, Ok(160));
            }
            let mut pcm = [1.0; 160];
            let decoded = codec.decode(&[silence; 160], &mut pcm);
            if fault == "malformed_recover" || fault == "malformed_missing" {
                assert_eq!(decoded, Err(MediaError::Codec));
                assert_eq!(
                    unsafe { conceal_calls() },
                    0,
                    "never conceal after decode advanced"
                );
                assert_eq!(
                    unsafe { decode_calls() },
                    if format == AudioProfile::Mulaw { 1 } else { 2 }
                );
            } else if matches!(fault, "decode_error" | "decoded_frames")
                || fault.starts_with("inspect_")
                || fault.starts_with("span_")
            {
                assert_eq!(decoded, Err(MediaError::Codec), "{fault}");
            } else {
                assert_eq!(decoded, Ok(DecodeProvenance::Original));
                assert!(pcm.iter().all(|sample| sample.abs() < 0.001));
            }
            if fault.starts_with("conceal_") {
                let before = unsafe { conceal_calls() };
                assert_eq!(codec.conceal(160, &mut pcm), Err(MediaError::Codec));
                assert_eq!(unsafe { conceal_calls() }, before + 1);
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
    let released = released_provider_named("librptadv_codec_g711.so.1");
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
        "malformed_recover",
        "malformed_missing",
        "decode_error",
        "decoded_frames",
        "span_origin",
        "span_offset",
        "span_count",
        "conceal_error",
        "conceal_count",
        "inspect_malformed",
        "inspect_error",
        "inspect_zero",
        "inspect_span",
        "inspect_bound",
        "inspect_mode",
        "short_provenance",
        "missing_provenance",
        "unsupported_provenance",
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

#[test]
fn every_sip_profile_refuses_unavailable_provenance_before_activation() {
    const CHILD: &str = "RPTADV_TEST_PROVENANCE_PROFILE";
    if let Ok(index) = std::env::var(CHILD) {
        let profile = AudioProfile::ALL[index.parse::<usize>().unwrap()];
        assert!(
            matches!(AudioCodec::load(profile), Err(MediaError::Codec)),
            "{profile:?}"
        );
        return;
    }
    let directory =
        FixtureDirectory(std::env::temp_dir().join(format!("rptadv-probe-{}", std::process::id())));
    std::fs::create_dir(&directory.0).unwrap();
    for (index, profile) in AudioProfile::ALL.into_iter().enumerate() {
        let name = match profile {
            AudioProfile::Mulaw | AudioProfile::Alaw => "librptadv_codec_g711.so.1",
            AudioProfile::G722 | AudioProfile::G726 | AudioProfile::G726Aal2 => {
                "librptadv_codec_spandsp.so.1"
            }
            AudioProfile::Gsm => "librptadv_codec_gsm.so.1",
            AudioProfile::L16Narrowband | AudioProfile::L16Wideband => "librptadv_codec_pcm.so.1",
            AudioProfile::Ilbc30 => "librptadv_codec_ilbc.so.1",
        };
        let child_directory = directory.0.join(index.to_string());
        std::fs::create_dir(&child_directory).unwrap();
        let output = Command::new("cc")
            .args(["-shared", "-fPIC", "-Wall", "-Wextra", "-Werror", "-I"])
            .arg(
                std::env::var_os("RPTADV_CODEC_INCLUDE")
                    .unwrap_or_else(|| "/usr/include/rptadv-codec".into()),
            )
            .arg(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/phone_codec.c"))
            .args(["-ldl", "-o"])
            .arg(child_directory.join(name))
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let mut paths = vec![child_directory];
        paths.extend(std::env::split_paths(
            &std::env::var_os("LD_LIBRARY_PATH").unwrap_or_default(),
        ));
        for fault in [
            "short_provenance",
            "missing_provenance",
            "unsupported_provenance",
            "abi_version",
        ] {
            let child = Command::new("timeout")
                .arg("15")
                .arg(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "every_sip_profile_refuses_unavailable_provenance_before_activation",
                    "--nocapture",
                ])
                .env(CHILD, index.to_string())
                .env(FAULT, fault)
                .env(
                    "RPTADV_TEST_PROBE_PROFILE",
                    (profile.parameters().provider_profile as u32).to_string(),
                )
                .env("RPTADV_TEST_REAL_CODEC", released_provider_named(name))
                .env("LD_LIBRARY_PATH", std::env::join_paths(&paths).unwrap())
                .output()
                .unwrap();
            assert!(
                child.status.success(),
                "{profile:?}/{fault}: {}{}",
                String::from_utf8_lossy(&child.stdout),
                String::from_utf8_lossy(&child.stderr)
            );
        }
    }
}

#[test]
fn first_and_repeated_current_sip_decodes_allocate_no_runtime_workspace() {
    // The optional process-wide native guard also observes Rust's system allocator.
    // The dedicated qualification command requires and self-checks this interposer.
    let process = libloading::os::unix::Library::this();
    let begin = unsafe { process.get::<unsafe extern "C" fn()>(b"swr_test_realtime_begin\0") };
    let Ok(begin) = begin else {
        assert!(
            std::env::var_os("RPTADV_REQUIRE_ALLOC_GUARD").is_none(),
            "required native allocation guard is missing"
        );
        return;
    };
    let end = unsafe { process.get::<unsafe extern "C" fn()>(b"swr_test_realtime_end\0") }.unwrap();
    let control =
        unsafe { process.get::<unsafe extern "C" fn() -> i32>(b"swr_test_realtime_control\0") }
            .unwrap();
    assert_eq!(
        unsafe { control() },
        1,
        "prove both allocation and free interception"
    );
    for profile in AudioProfile::ALL {
        let p = profile.parameters();
        let mut codec = AudioCodec::load(profile).unwrap();
        let mut packet = vec![0; p.packet_bytes];
        codec
            .encode(&vec![0.125; p.packet_frames as usize], &mut packet)
            .unwrap();
        let mut pcm = vec![0.0; p.packet_frames as usize];
        for _ in 0..3 {
            unsafe { begin() };
            let result = codec.decode(&packet, &mut pcm);
            unsafe { end() };
            assert_eq!(result, Ok(DecodeProvenance::Original), "{profile:?}");
        }
        if profile == AudioProfile::Ilbc30 {
            packet[49] |= 1;
            for _ in 0..3 {
                unsafe { begin() };
                let result = codec.decode(&packet, &mut pcm);
                unsafe { end() };
                assert_eq!(result, Ok(DecodeProvenance::Concealed));
            }
        }
        unsafe { begin() };
        let result = codec.conceal(p.packet_frames, &mut pcm);
        unsafe { end() };
        assert!(result.is_ok(), "{profile:?}");
    }
}
