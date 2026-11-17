/** @file
 * @brief Consumer-boundary faults forwarding to the installed G.711 provider.
 *
 * Each subprocess selects one fault before loading this test-only DSO. No codec
 * algorithm or Rust-private object is replaced. Real handles are counted through
 * their actual destruction, including partially completed codec construction.
 */
#include <assert.h>
#include <dlfcn.h>
#include <rptadv_codec.h>
#include <stdlib.h>
#include <string.h>

static const rptadv_codec_descriptor_v1_table *canonical;
static rptadv_codec_descriptor_v1_table descriptor;
static unsigned live_handles;

/** Compare the subprocess's immutable fault selection. */
static int fault(const char *name) {
    const char *selected = getenv("RPTADV_TEST_CODEC_FAULT");
    return selected != NULL && strcmp(selected, name) == 0;
}

/** Open a real encoder unless this constructor is the injected failure. */
static int32_t open_encoder(const rptadv_codec_config_v1 *config, void **out) {
    if (fault("encoder_error") || fault("encoder_null")) {
        *out = NULL;
        return fault("encoder_error") ? RPTADV_CODEC_FAILED : RPTADV_CODEC_COMPLETE;
    }
    int32_t status = canonical->open_encoder(config, out);
    if (*out != NULL)
        ++live_handles;
    return status;
}

/** Open a real decoder after the encoder has already been prepared. */
static int32_t open_decoder(const rptadv_codec_config_v1 *config, void **out) {
    if (fault("decoder_error") || fault("decoder_null")) {
        *out = NULL;
        return fault("decoder_error") ? RPTADV_CODEC_FAILED : RPTADV_CODEC_COMPLETE;
    }
    int32_t status = canonical->open_decoder(config, out);
    if (*out != NULL)
        ++live_handles;
    return status;
}

/** Return a provider error or one malformed result field after actual encoding. */
static int32_t encode(void *handle, const float *pcm, size_t frames, uint8_t *packet,
                      size_t capacity, uint32_t flags, rptadv_codec_encode_result_v1 *result) {
    if (fault("encode_error"))
        return RPTADV_CODEC_FAILED;
    int32_t status = canonical->encode(handle, pcm, frames, packet, capacity, flags, result);
    assert(status == RPTADV_CODEC_COMPLETE);
    if (fault("consumed_frames"))
        --result->consumed_frames;
    if (fault("encoded_bytes"))
        --result->encoded_bytes;
    if (fault("source_frames"))
        --result->source_frames;
    if (fault("packet_frames"))
        --result->packet_frames;
    return status;
}

/** Return a provider error or incomplete count after actual decoding. */
static int32_t decode(void *handle, const uint8_t *packet, size_t bytes, float *pcm,
                      size_t capacity, size_t *count) {
    if (fault("decode_error"))
        return RPTADV_CODEC_FAILED;
    int32_t status = canonical->decode(handle, packet, bytes, pcm, capacity, count);
    assert(status == RPTADV_CODEC_COMPLETE);
    if (fault("decoded_frames"))
        --*count;
    return status;
}

/** Observe cleanup without replacing the released provider's ownership. */
static void destroy(void *handle) {
    if (handle != NULL) {
        assert(live_handles > 0);
        --live_handles;
    }
    canonical->destroy(handle);
}

/** Number of real provider contexts still owned by the consumer. */
unsigned rptadv_test_codec_live_handles(void) { return live_handles; }

/** Load the real installed descriptor, then inject one explicit ABI failure. */
const rptadv_codec_descriptor_v1_table *rptadv_codec_descriptor_v1(void) {
    if (canonical == NULL) {
        const char *path = getenv("RPTADV_TEST_REAL_CODEC");
        assert(path != NULL);
        void *library = dlopen(path, RTLD_NOW | RTLD_LOCAL);
        assert(library != NULL);
        const rptadv_codec_descriptor_v1_table *(*entry)(void) =
            (const rptadv_codec_descriptor_v1_table *(*)(void))dlsym(library,
                                                                     "rptadv_codec_descriptor_v1");
        assert(entry != NULL);
        canonical = entry();
        assert(canonical != NULL && canonical->struct_size >= sizeof(descriptor));
    }
    descriptor = *canonical;
    descriptor.open_encoder = open_encoder;
    descriptor.open_decoder = open_decoder;
    descriptor.encode = encode;
    descriptor.decode = decode;
    descriptor.destroy = destroy;
    if (fault("null_table"))
        return NULL;
    if (fault("short_table"))
        descriptor.struct_size = sizeof(uint32_t);
    if (fault("abi_version"))
        descriptor.abi_version = 0;
    if (fault("capability"))
        descriptor.capability[0] = '?';
    if (fault("missing_encoder"))
        descriptor.open_encoder = NULL;
    if (fault("missing_decoder"))
        descriptor.open_decoder = NULL;
    if (fault("missing_encode"))
        descriptor.encode = NULL;
    if (fault("missing_decode"))
        descriptor.decode = NULL;
    if (fault("missing_destroy"))
        descriptor.destroy = NULL;
    return &descriptor;
}
