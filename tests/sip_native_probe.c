/** @file
 * @brief Test-only observations around the owned shim, without vendor changes.
 */
#define _GNU_SOURCE
#include <sofia-sip/nua.h>
#include <stddef.h>

/** Count explicit owned-handle retirement, never alter native behavior. */
static size_t destroyed_handles;
/** Record the real handle release before forwarding unchanged. */
static void observed_destroy(nua_handle_t *handle) {
    ++destroyed_handles;
    nua_handle_destroy(handle);
}
#define nua_handle_destroy observed_destroy
#include "../rust/sip-adapter/native.c"
#undef nua_handle_destroy

/** Return test-only explicit retirement count.
 * @return Number of real native handle releases through the owned shim.
 */
size_t sip_test_destroyed_handles(void) { return destroyed_handles; }
