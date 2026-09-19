/** @file
 * @brief Independently replaceable ABI 1 literal speech preparation capability.
 */
#ifndef RPTADV_SPEECH_ADAPTER_H
#define RPTADV_SPEECH_ADAPTER_H
#include "rptadv_media_types.h"
#ifdef __cplusplus
extern "C" {
#endif
/** @brief Exact NUL-padded capability name. */
#define RPTADV_SPEECH_CAPABILITY "rptadv.speech"
/** @brief Validate the full table before opening any context. */
struct rptadv_speech_descriptor {
    uint32_t struct_size; /**< Complete readable table size. */
    uint32_t abi_version; /**< Required RPTADV_MEDIA_ABI_VERSION. */
    char capability[16];  /**< Exact RPTADV_SPEECH_CAPABILITY, NUL padded. */
    /** @brief Copy configuration; initialize context to null on failure. */
    int32_t (*create)(const struct rptadv_media_config *config, void **context);
    /** @brief Destroy after all preparations stop; null is harmless. */
    void (*destroy)(void *context);
    /** @brief Synthesize literal text at source rate; failure empties output. */
    int32_t (*prepare_speech)(const void *context,
                              const struct rptadv_media_speech_request *request,
                              const struct rptadv_media_cancellation *cancellation,
                              struct rptadv_media_audio *output);
    /** @brief Return this provider's audio handle exactly once; null is harmless. */
    void (*release_audio)(void *handle);
};
/** @brief Return an immutable process-lifetime descriptor, never freed by callers. */
/** @return Immutable process-lifetime speech capability table. */
const struct rptadv_speech_descriptor *rptadv_speech_adapter_descriptor(void);
#ifdef __cplusplus
}
#endif
#endif
