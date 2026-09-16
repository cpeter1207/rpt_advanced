/** @file
 * @brief Independently replaceable ABI 1 local-file preparation capability.
 */
#ifndef RPTADV_FILE_ADAPTER_H
#define RPTADV_FILE_ADAPTER_H
#include "rptadv_media_types.h"
#ifdef __cplusplus
extern "C" {
#endif
/** @brief Exact NUL-padded capability name. */
#define RPTADV_FILE_CAPABILITY "rptadv.file"
/** @brief Validate the full table before opening any context. */
struct rptadv_file_descriptor {
    uint32_t struct_size; /**< Complete readable table size. */
    uint32_t abi_version; /**< Required RPTADV_MEDIA_ABI_VERSION. */
    char capability[16];  /**< Exact RPTADV_FILE_CAPABILITY, NUL padded. */
    /** @brief Copy configuration; initialize context to null on failure. */
    int32_t (*create)(const struct rptadv_media_config *config, void **context);
    /** @brief Destroy after all preparations stop; null is harmless. */
    void (*destroy)(void *context);
    /** @brief Decode an opened local file at source rate; failure empties output. */
    int32_t (*prepare_file)(const void *context, const char *path,
                            const struct rptadv_media_cancellation *cancellation,
                            struct rptadv_media_audio *output);
    /** @brief Return this provider's audio handle exactly once; null is harmless. */
    void (*release_audio)(void *handle);
};
/** @brief Return an immutable process-lifetime descriptor, never freed by callers. */
/** @return Immutable process-lifetime file capability table. */
const struct rptadv_file_descriptor *rptadv_file_adapter_descriptor(void);
#ifdef __cplusplus
}
#endif
#endif
