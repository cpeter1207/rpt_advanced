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
static unsigned conceal_calls;
static unsigned decode_calls;
static uint32_t decoder_profile;

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
    decoder_profile = config->profile_id;
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
    ++decode_calls;
    if (fault("malformed_recover") || fault("malformed_missing"))
        return RPTADV_CODEC_MALFORMED;
    if (fault("decoded_frames"))
        --*count;
    return status;
}

/** Forward exactly one provenance decode; faults occur after native advancement. */
static int32_t decode_with_provenance(void *handle, const uint8_t *packet, size_t bytes, float *pcm,
                                      size_t capacity, rptadv_codec_decode_span_v1 *spans,
                                      size_t span_capacity, rptadv_codec_decode_result_v1 *result) {
    const char *selected = getenv("RPTADV_TEST_PROBE_PROFILE");
    if (bytes == 0 && fault("unsupported_provenance") &&
        (selected == NULL || decoder_profile == strtoul(selected, NULL, 10)))
        return RPTADV_CODEC_UNSUPPORTED;
    int32_t status = canonical->decode_with_provenance(handle, packet, bytes, pcm, capacity, spans,
                                                       span_capacity, result);
    if (bytes == 0)
        return status;
    ++decode_calls;
    assert(status == RPTADV_CODEC_COMPLETE);
    if (fault("malformed_recover") || fault("malformed_missing") || fault("decode_error")) {
        result->frames = 0;
        result->span_count = 0;
        return fault("decode_error") ? RPTADV_CODEC_FAILED : RPTADV_CODEC_MALFORMED;
    }
    if (fault("decoded_frames"))
        --result->frames;
    if (fault("span_origin"))
        spans[0].origin = 2;
    if (fault("span_offset"))
        ++spans[0].offset_frames;
    if (fault("span_count"))
        result->span_count = span_capacity + 1;
    if (fault("native_recovered")) {
        assert(result->span_count == 1);
        spans[0].origin = 1;
        for (size_t i = 0; i < result->frames; ++i)
            pcm[i] = 0.25f;
    }
    return status;
}

unsigned rptadv_test_decode_calls(void) { return decode_calls; }

/** Preserve native inspection except for one explicit status or result fault. */
static int32_t inspect(void *handle, const uint8_t *packet, size_t bytes,
                       rptadv_codec_packet_info_v1 *result) {
    if (fault("inspect_malformed"))
        return RPTADV_CODEC_MALFORMED;
    if (fault("inspect_error"))
        return RPTADV_CODEC_FAILED;
    int32_t status = canonical->inspect(handle, packet, bytes, result);
    assert(status == RPTADV_CODEC_COMPLETE);
    if (fault("inspect_zero"))
        result->frames = 0;
    if (fault("inspect_span"))
        --result->frames;
    if (fault("inspect_bound"))
        result->frames = 241;
    if (fault("inspect_mode"))
        ++result->mode;
    return status;
}

/** Script explicit decoder recovery without replacing any production hook. */
static int32_t conceal(void *handle, uint32_t frames, float *pcm, size_t capacity, size_t *count) {
    ++conceal_calls;
    if (fault("conceal_error"))
        return RPTADV_CODEC_FAILED;
    if (fault("conceal_count")) {
        *count = frames - 1;
        return RPTADV_CODEC_COMPLETE;
    }
    if (fault("malformed_recover") || fault("malformed_missing")) {
        if (fault("malformed_missing")) {
            *count = 0;
            return RPTADV_CODEC_UNSUPPORTED;
        }
        assert(capacity >= frames);
        for (uint32_t i = 0; i < frames; ++i)
            pcm[i] = 0.25f;
        *count = frames;
        return RPTADV_CODEC_COMPLETE;
    }
    return canonical->conceal(handle, frames, pcm, capacity, count);
}

unsigned rptadv_test_conceal_calls(void) { return conceal_calls; }

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
    descriptor.decode_with_provenance = decode_with_provenance;
    descriptor.inspect = inspect;
    descriptor.conceal = conceal;
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
    if (fault("missing_provenance"))
        descriptor.decode_with_provenance = NULL;
    if (fault("short_provenance"))
        descriptor.struct_size = RPTADV_CODEC_DESCRIPTOR_ACTIVE_SIZE;
    if (fault("missing_destroy"))
        descriptor.destroy = NULL;
    return &descriptor;
}
