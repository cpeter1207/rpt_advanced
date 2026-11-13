/** @file
 * @brief Private native translation; no Sofia types escape this shim.
 */
#ifndef RPTADV_SIP_NATIVE_H
#define RPTADV_SIP_NATIVE_H
#include "include/rptadv_sip_adapter.h"
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
/** Set the host's current incoming-admission gate.
 * @param context Live native account.
 * @param enabled Nonzero only while the account accepts new calls.
 */
void sip_native_admission(void *context, int enabled);
/** Copy one changed call slot without releasing terminal ownership.
 * @param context Live native account.
 * @param call Writable complete call snapshot.
 * @return Zero on an observation or one when empty.
 */
int sip_native_next_call(void *context, rptadv_sip_call_v1 *call);
/** Answer or reject the exact invited call.
 * @param context Live native account.
 * @param call Exact token from next_call.
 * @param status 200 with SDP or 400..699.
 * @param answer NUL-terminated bounded SDP for success; NULL for rejection.
 * @return Zero, -2 for stale token, or -3 for invalid call state.
 */
int sip_native_respond(void *context, uint64_t call, uint32_t status, const char *answer);
/** End the exact call without discarding terminal ownership.
 * @param context Live native account.
 * @param call Exact live token.
 * @return Zero or -2 for stale token.
 */
int sip_native_hangup(void *context, uint64_t call);
/** Release only an observed terminal call.
 * @param context Live native account.
 * @param call Exact token.
 * @return Zero, one while live, or -2 for stale token.
 */
int sip_native_release(void *context, uint64_t call);
#endif
