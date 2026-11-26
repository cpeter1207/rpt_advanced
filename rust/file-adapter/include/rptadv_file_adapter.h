/** @file
 * @brief Independently replaceable ABI 3 local-file streaming capability.
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
    /** @brief Open a local file stream; initialize handle to null on error. */
    int32_t (*open_file)(const void *context, const char *path,
                         const struct rptadv_media_cancellation *cancellation, void **handle);
    /** @brief Advance once; only successful SAMPLES makes output consumable.
     * Provide a nonempty buffer and a separate, writable result. On error the
     * provider initializes a valid result to PENDING with zero payload fields.
     */
    int32_t (*step_stream)(void *handle, const struct rptadv_media_cancellation *cancellation,
                           float *output, size_t capacity, struct rptadv_media_step *result);
    /** @brief Close this provider's stream exactly once; null is harmless. */
    void (*close_stream)(void *handle);
};
/** @brief Return an immutable process-lifetime descriptor, never freed by callers. */
/** @return Immutable process-lifetime file capability table. */
const struct rptadv_file_descriptor *rptadv_file_adapter_descriptor(void);
#ifdef __cplusplus
}
#endif
#endif
