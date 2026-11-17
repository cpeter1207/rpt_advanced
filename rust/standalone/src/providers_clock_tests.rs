//! Hardware-free optional-clock storage and DSO-lifetime fixtures.
use super::*;
use crate::abi;

pub(crate) const AUDIO: &str = "librptadv_portaudio_alsa_adapter.so.2";

/// Copy exactly the advertised allocation, not a reference to a larger Rust table.
pub(crate) fn replace_audio(
    providers: &mut ProviderSet,
    api: &abi::rptadv_audio_adapter_descriptor,
    size: usize,
) {
    let mut bytes = vec![0_u64; size.div_ceil(8)].into_boxed_slice();
    unsafe {
        std::ptr::copy_nonoverlapping(
            std::ptr::from_ref(api).cast::<u8>(),
            bytes.as_mut_ptr().cast(),
            size.min(std::mem::size_of_val(api)),
        );
        bytes.as_mut_ptr().cast::<u32>().write(size as u32);
    }
    let pointer = NonNull::new(Box::leak(bytes).as_mut_ptr()).unwrap().cast();
    let loaded = providers
        .descriptors
        .iter_mut()
        .find(|p| p.library == AUDIO)
        .unwrap();
    loaded.descriptor = pointer;
    loaded.size = size;
}

#[test]
fn stage_c_clock_provider_reads_only_complete_nonnull_optional_field() {
    let mut providers = ProviderSet::load().unwrap();
    let api = *providers
        .typed_descriptor::<abi::rptadv_audio_adapter_descriptor>(AUDIO)
        .unwrap();
    assert_eq!(std::mem::size_of_val(&api), 216);
    assert!(api.monotonic_now_ns.is_some());
    for size in [16, 191, 192, 193, 199, 200, 208, 209, 215, 216, 224] {
        replace_audio(&mut providers, &api, size);
        let result = providers.media_clock_reader();
        if size != 192 && size < 200 {
            assert!(result.is_err(), "invalid required base {size}");
            continue;
        }
        let reader = result.unwrap();
        assert_eq!(reader.is_some(), size >= 216, "extent {size}");
        if let Some(reader) = reader {
            assert!(unsafe { reader() } > 0);
        }
    }
    let mut absent = api;
    absent.monotonic_now_ns = None;
    replace_audio(&mut providers, &absent, 216);
    assert!(providers.media_clock_reader().unwrap().is_none());
    let mut invalid = api;
    invalid.abi_version = 0;
    replace_audio(&mut providers, &invalid, 216);
    assert!(providers.media_clock_reader().is_err());
    // Retained storage bounds win even when an advertised extent is larger.
    replace_audio(&mut providers, &api, 208);
    let loaded = providers
        .descriptors
        .iter_mut()
        .find(|p| p.library == AUDIO)
        .unwrap();
    unsafe { loaded.descriptor.as_ptr().cast::<u32>().write(216) };
    assert!(providers.media_clock_reader().is_err());
    // Invalid retained extent cannot masquerade as absence even with a smaller advertisement.
    replace_audio(&mut providers, &api, 216);
    let loaded = providers
        .descriptors
        .iter_mut()
        .find(|p| p.library == AUDIO)
        .unwrap();
    loaded.size = 191;
    assert!(providers.media_clock_reader().is_err());
    providers.descriptors.retain(|p| p.library != AUDIO);
    assert!(matches!(
        providers.media_clock_reader(),
        Err(ProviderError::MissingDescriptor(AUDIO))
    ));
}

/// Attach a test DSO to the actual ProviderSet; destructor records the unload boundary.
pub(crate) fn attach_lifetime_fixture(
    providers: &mut ProviderSet,
    path: &std::path::Path,
    on_read: extern "C" fn(),
    on_unload: extern "C" fn(),
) {
    std::fs::create_dir_all(path).unwrap();
    let source = path.join("clock.c");
    std::fs::write(
        &source,
        r#"
#include <stdint.h>
#include <string.h>
static _Alignas(8) unsigned char descriptor[216];
static void (*read_notice)(void);
static void (*unload_notice)(void);
static uint64_t clock_read(void) { read_notice(); return 4242; }
void *clock_fixture(const void *source, void (*read)(void), void (*unload)(void)) {
    memcpy(descriptor, source, sizeof(descriptor));
    uint64_t (*reader)(void) = clock_read;
    memcpy(descriptor + 208, &reader, sizeof(reader));
    read_notice = read; unload_notice = unload;
    return descriptor;
}
__attribute__((destructor)) static void unloaded(void) { if (unload_notice) unload_notice(); }
"#,
    )
    .unwrap();
    let library = path.join("clock.so");
    assert!(
        std::process::Command::new("cc")
            .args([
                "-std=c11", "-Wall", "-Wextra", "-Werror", "-shared", "-fPIC"
            ])
            .arg(&source)
            .arg("-o")
            .arg(&library)
            .status()
            .unwrap()
            .success()
    );
    let library = unsafe { Library::new(library) }.unwrap();
    type Init =
        unsafe extern "C" fn(*const c_void, extern "C" fn(), extern "C" fn()) -> *mut c_void;
    let init = unsafe { library.get::<Init>(b"clock_fixture\0") }.unwrap();
    let loaded = providers
        .descriptors
        .iter_mut()
        .find(|p| p.library == AUDIO)
        .unwrap();
    let descriptor = unsafe { init(loaded.descriptor.as_ptr(), on_read, on_unload) };
    loaded.descriptor = NonNull::new(descriptor).unwrap();
    loaded.size = 216;
    providers._libraries.push(library);
}
