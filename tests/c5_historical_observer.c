/* SPDX-License-Identifier: GPL-2.0-only */
/** @file
 * @brief Independently compiled Stage B callback: only its retained 96-byte header.
 */
#include <stdatomic.h>
#include <stddef.h>
#include <usbradioplus_product.h>

_Static_assert(ATOMIC_INT_LOCK_FREE == 2, "bounded scalar callback counters");
_Static_assert(sizeof(UrpProgrammeObservationV1) == 96, "retained Stage B extent");
_Static_assert(offsetof(UrpProgrammeObservationV1, programme_buffering) == 32,
               "retained Stage B buffering offset");
_Static_assert(sizeof(UrpProgrammeFenceBindingV2) == 40, "retained V2 envelope");

/** One preallocated observer context; no pointer escapes the synchronous callback. */
static struct observer_state {
    _Atomic unsigned calls;
    _Atomic unsigned invalid;
    unsigned expected_size;
} state;

/** Read only named fields from the old 96-byte record; ACK 2 says nothing about clocks. */
static uint32_t observe(void *context, const UrpProgrammeObservationV1 *value) {
    if (context != &state) {
        atomic_fetch_add_explicit(&state.invalid, 1, memory_order_relaxed);
        return 1;
    }
    struct observer_state *owner = context;
    if (!value || value->struct_size != owner->expected_size ||
        value->abi_version != URP_PROGRAMME_OBSERVATION_ABI_VERSION || !value->radio_generation ||
        !value->frame_count || value->reserved) {
        atomic_fetch_add_explicit(&owner->invalid, 1, memory_order_relaxed);
    }
    atomic_fetch_add_explicit(&owner->calls, 1, memory_order_relaxed);
    return 1;
}

/** Initialize before native create; caller retains this DSO through native destruction. */
void *c5_old_binding(unsigned expected_size) {
    static UrpProgrammeFenceBindingV2 binding;
    atomic_store(&state.calls, 0);
    atomic_store(&state.invalid, 0);
    state.expected_size = expected_size;
    binding = (UrpProgrammeFenceBindingV2){
        .base = {.struct_size = sizeof(binding), .abi_version = 2, .context = &state},
        .observed_permit = observe,
    };
    return &binding;
}

/** Control-only read after real native destruction joins its callbacks. */
unsigned c5_old_calls(void) { return atomic_load(&state.calls); }
/** Control-only validation status; the callback never reads a new clock tail. */
unsigned c5_old_invalid(void) { return atomic_load(&state.invalid); }
