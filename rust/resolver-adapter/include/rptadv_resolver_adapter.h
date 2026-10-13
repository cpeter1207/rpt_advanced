/** @file rptadv_resolver_adapter.h
 * @brief Versioned synchronous system DNS capability, for control threads only.
 *
 * All inputs are borrowed UTF-8 byte extents, without a required NUL terminator.
 * Callbacks borrow output only until they return; they must not throw or unwind.
 * 0 means completed (SRV may have no result); 1 means unavailable, including
 * malformed DNS or an empty address result; -1 rejects a required pointer,
 * length, UTF-8 borrow, or contained panic. Callers own fallback policy.
 * Contexts permit concurrent resolutions. Stop all callers before destroying a
 * context; retain the shared object until every context has been destroyed.
 */
#ifndef RPTADV_RESOLVER_ADAPTER_H
#define RPTADV_RESOLVER_ADAPTER_H
#include <stddef.h>
#include <stdint.h>
#ifdef __cplusplus
extern "C" {
#endif
/** Borrow one SRV hostname and its port. */
typedef void (*rptadv_resolver_srv_sink_v1)(void *context, const uint8_t *host, size_t length,
                                            uint16_t port);
/** Borrow one SocketAddr string, including port and IPv6 brackets. */
typedef void (*rptadv_resolver_address_sink_v1)(void *context, const uint8_t *endpoint,
                                                size_t length);
/** Stable ABI 1 table. Every function is required. */
typedef struct rptadv_resolver_descriptor_v1 {
    uint32_t struct_size;              /**< Complete table size in bytes. */
    uint32_t abi_version;              /**< 1. */
    uint8_t capability[16];            /**< NUL-padded rptadv.resolver. */
    int32_t (*create)(void **context); /**< Create one context; output must be writable. */
    void (*destroy)(void *context); /**< Destroy once after all calls finish; NULL is harmless. */
    /** Resolve and select an SRV record using system DNS, priority and weight. */
    int32_t (*resolve_srv)(void *context, const uint8_t *service, size_t length,
                           rptadv_resolver_srv_sink_v1 result, void *result_context);
    /** Resolve host:port; preserve system address order, including mapped IPv4. */
    int32_t (*resolve_addresses)(void *context, const uint8_t *endpoint, size_t length,
                                 rptadv_resolver_address_sink_v1 result, void *result_context);
} rptadv_resolver_descriptor_v1;
/** Borrow the immutable ABI table while this library remains loaded.
 * @return Immutable descriptor; never NULL and never freed by the caller.
 */
const rptadv_resolver_descriptor_v1 *rptadv_resolver_adapter_descriptor_v1(void);
#ifdef __cplusplus
}
#endif
#endif
