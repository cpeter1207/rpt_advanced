/** @file Child-only faults around the real released ring descriptor. */
#define _GNU_SOURCE
#include <dlfcn.h>
#include <math.h>
#include <pthread.h>
#include <rate_adjusting_pcm_ring3/rate_adjusting_pcm_ring3.h>
#include <stdatomic.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

struct fault_counts {
    uint64_t creates;
    uint64_t destroys;
    uint64_t timed_calls;
    uint64_t real_nonzero_forwarded;
    uint64_t injected_failures;
};

static pthread_once_t once = PTHREAD_ONCE_INIT;
static const struct rpcr3_descriptor *canonical;
static struct rpcr3_descriptor descriptor;
static bool timed_error;
static _Atomic uint64_t creates, destroys, timed_calls, real_nonzero_forwarded;
static _Atomic uint64_t injected_failures, fail_at;

static enum rpcr3_result create(const struct rpcr3_config *config, struct rpcr3_ring **output) {
    enum rpcr3_result result = canonical->ring_create(config, output);
    if (result == RPCR3_OK && output != NULL && *output != NULL) {
        atomic_fetch_add(&creates, 1);
    }
    return result;
}

static void destroy(struct rpcr3_ring *ring) {
    canonical->ring_destroy(ring);
    atomic_fetch_add(&destroys, 1);
}

static enum rpcr3_result render(struct rpcr3_ring *ring, float *sample,
                                struct rpcr3_timed_sample_v1 *timing) {
    /* Even adapter errors write ordinary output before reporting failure. */
    enum rpcr3_result result = canonical->ring_consumer_render_sample_timed(ring, sample, timing);
    uint64_t call = atomic_fetch_add(&timed_calls, 1) + 1;
    if (result == RPCR3_OK && (timing->flags & RPCR3_TIMED_REAL) != 0 && isfinite(*sample) &&
        fabsf(*sample) > 0.1f) {
        atomic_fetch_add(&real_nonzero_forwarded, 1);
    }
    uint64_t target = atomic_load_explicit(&fail_at, memory_order_acquire);
    if (result == RPCR3_OK && target != 0 && call >= target) {
        atomic_fetch_add(&injected_failures, 1);
        return RPCR3_ADAPTER_ERROR;
    }
    return result;
}

static void initialize(void) {
    const struct rpcr3_descriptor *(*get)(void) =
        (const struct rpcr3_descriptor *(*)(void))dlsym(RTLD_NEXT, "rpcr3_descriptor");
    const char *mode = getenv("RPTADV_TEST_PHONE_RING_CHILD");
    canonical = get == NULL ? NULL : get();
    if (canonical == NULL || canonical->struct_size < sizeof(descriptor) ||
        canonical->abi_version != RPCR3_ABI_VERSION || canonical->capability_name == NULL ||
        strcmp(canonical->capability_name, "rptadv.rate-adjusting-pcm-ring.f32") != 0 ||
        canonical->ring_create == NULL || canonical->ring_destroy == NULL ||
        canonical->ring_consumer_render_sample_timed == NULL || mode == NULL ||
        (strcmp(mode, "missing-tail") != 0 && strcmp(mode, "timed-error") != 0)) {
        fputs("invalid canonical ring or phone fault child mode\n", stderr);
        abort();
    }
    descriptor = *canonical;
    descriptor.ring_create = create;
    descriptor.ring_destroy = destroy;
    timed_error = strcmp(mode, "timed-error") == 0;
    if (timed_error) {
        descriptor.ring_consumer_render_sample_timed = render;
    } else {
        descriptor.struct_size =
            offsetof(struct rpcr3_descriptor, ring_consumer_render_sample_timed);
        descriptor.ring_consumer_render_sample_timed = NULL;
    }
}

/** Return a once-initialized, thereafter immutable compatible descriptor. */
const struct rpcr3_descriptor *rpcr3_descriptor(void) {
    pthread_once(&once, initialize);
    return &descriptor;
}

/** Arm the sole consumer's Nth subsequent call; all later errors stay armed. */
int rptadv_test_ring_arm_after(uint64_t distance) {
    pthread_once(&once, initialize);
    uint64_t current = atomic_load(&timed_calls);
    uint64_t expected = 0;
    if (!timed_error || distance == 0 || distance > UINT64_MAX - current) {
        return -1;
    }
    return atomic_compare_exchange_strong_explicit(&fail_at, &expected, current + distance,
                                                   memory_order_release, memory_order_relaxed)
               ? 0
               : -1;
}

/** Observe actual provider calls; never alter PCM, metadata, or owner state. */
void rptadv_test_ring_snapshot(struct fault_counts *output) {
    pthread_once(&once, initialize);
    *output = (struct fault_counts){atomic_load(&creates), atomic_load(&destroys),
                                    atomic_load(&timed_calls), atomic_load(&real_nonzero_forwarded),
                                    atomic_load(&injected_failures)};
}
