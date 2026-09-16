/** @file
 * @brief Test the independently loaded Rust descriptor through its C header.
 */
#ifdef FILE_ADAPTER
#include "rptadv_file_adapter.h"
#define DESCRIPTOR rptadv_file_descriptor
#define GET_DESCRIPTOR "rptadv_file_adapter_descriptor"
#define CAPABILITY "rptadv.file\0\0\0\0\0"
#else
#include "rptadv_speech_adapter.h"
#define DESCRIPTOR rptadv_speech_descriptor
#define GET_DESCRIPTOR "rptadv_speech_adapter_descriptor"
#define CAPABILITY "rptadv.speech\0\0\0"
#endif
#include <assert.h>
#include <dlfcn.h>
#include <string.h>

/** @brief A completed cancellation request. */
static uint32_t cancelled(const void *context) {
    (void)context;
    return 1;
}

/** @brief Load the candidate shared object and check ABI/error ownership. */
int main(int argc, char **argv) {
    assert(argc == 2);
    void *library = dlopen(argv[1], RTLD_NOW | RTLD_LOCAL);
    assert(library);
    const struct DESCRIPTOR *(*get_descriptor)(void);
    *(void **)(&get_descriptor) = dlsym(library, GET_DESCRIPTOR);
    assert(get_descriptor);
    const struct DESCRIPTOR *table = get_descriptor();
    assert(table->struct_size == sizeof(*table));
    assert(table->abi_version == RPTADV_MEDIA_ABI_VERSION);
    assert(!memcmp(table->capability, CAPABILITY, 16));
    assert(table->create && table->destroy && table->release_audio);
    struct rptadv_media_config config = {
        sizeof(config), RPTADV_MEDIA_ABI_VERSION, "unused", "/tmp", 30000, NULL, NULL};
    void *context = (void *)1;
    config.abi_version = 99;
    assert(table->create(&config, &context) == RPTADV_MEDIA_INVALID_REQUEST);
    assert(!context);
    config.abi_version = RPTADV_MEDIA_ABI_VERSION;
    assert(table->create(&config, &context) == RPTADV_MEDIA_OK && context);
    struct rptadv_media_cancellation cancellation = {NULL, cancelled};
    struct rptadv_media_audio audio = {(void *)1, (const float *)1, 1, 1};
#ifdef FILE_ADAPTER
    assert(table->prepare_file);
    assert(table->prepare_file(context, "/missing", &cancellation, &audio) ==
           RPTADV_MEDIA_CANCELLED);
    assert(!audio.handle && !audio.samples && !audio.sample_count && !audio.sample_rate_hz);
#else
    assert(table->prepare_speech);
    struct rptadv_media_speech_request speech = {"hello", "model", 0, 0};
    assert(table->prepare_speech(context, &speech, &cancellation, &audio) ==
           RPTADV_MEDIA_INVALID_REQUEST);
    assert(!audio.handle && !audio.samples && !audio.sample_count && !audio.sample_rate_hz);
#endif
    table->release_audio(NULL);
    table->destroy(context);
    table->destroy(NULL);
    /* Never dlclose a selected adapter; unloading belongs to process teardown. */
    return 0;
}
