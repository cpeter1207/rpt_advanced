/** @file
 * @brief Private native translation; no Sofia types escape this shim.
 */
#ifndef RPTADV_SIP_NATIVE_H
#define RPTADV_SIP_NATIVE_H
#include <stddef.h>
#include <stdint.h>
/** Numeric event plus optional bounded quoted challenge realm, borrowed synchronously. */
typedef void (*sip_native_sink)(void *, uint32_t, uint32_t, uint32_t, const char *, size_t);
/** Prepare one native loop on the calling owner; no account traffic.
 * @param bind Numeric SIP/UDP local endpoint.
 * @param sink Synchronous private observation callback.
 * @param context Stable caller-owned callback allocation.
 * @return Native context, or NULL after complete setup cleanup.
 */
void *sip_native_create(const char *bind, sip_native_sink sink, void *context);
/** True only on the creation thread.
 * @param context Live native context.
 * @return Nonzero on the creation thread.
 */
int sip_native_owner(void *context);
/** Service one nonblocking native turn.
 * @param context Live native context.
 */
void sip_native_step(void *context);
/** Start the account; strings are copied by NUA.
 * @param context Prepared native context.
 * @param registrar SIP registrar authority.
 * @param aor SIP address of record.
 * @param hop Numeric SIP/UDP transport next hop.
 * @param expires Decimal requested lifetime.
 * @return Zero when started, one while Contact preparation is pending, negative on failure.
 */
int sip_native_register(void *context, const char *registrar, const char *aor, const char *hop,
                        const char *expires);
/** Provide credentials only to the existing challenge.
 * @param context Live native context.
 * @param credentials Bounded Sofia-formatted authentication value, copied synchronously.
 */
void sip_native_authenticate(void *context, const char *credentials);
/** Remove the registration explicitly.
 * @param context Activated native context.
 */
void sip_native_unregister(void *context);
/** Cancel a failed account and its automatic refresh.
 * @param context Live native context; remote binding removal is not implied.
 */
void sip_native_cancel(void *context);
/** Begin graceful asynchronous shutdown.
 * @param context Live native context whose owner continues servicing.
 */
void sip_native_shutdown(void *context);
/** Free only after successful native shutdown.
 * @param context Quiescent native context, consumed by this call.
 */
void sip_native_destroy(void *context);
#endif
