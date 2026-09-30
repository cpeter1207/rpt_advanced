/** @file
 * @brief Independently replaceable ABI 2 literal speech streaming capability.
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
    /** @brief Open a literal speech stream at source rate. */
    int32_t (*open_speech)(const void *context,
                              const struct rptadv_media_speech_request *request,
                              const struct rptadv_media_cancellation *cancellation,
                              struct rptadv_media_stream *output);
    /** @brief Read up to capacity samples; zero samples means end of stream. */
    int32_t (*read_stream)(void *handle,
                           const struct rptadv_media_cancellation *cancellation,
                           float *output, size_t capacity, size_t *read_count);
    /** @brief Close this provider's stream exactly once; null is harmless. */
    void (*close_stream)(void *handle);
};
/** @brief Return an immutable process-lifetime descriptor, never freed by callers. */
/** @return Immutable process-lifetime speech capability table. */
const struct rptadv_speech_descriptor *rptadv_speech_adapter_descriptor(void);
#ifdef __cplusplus
}
#endif
#endif
