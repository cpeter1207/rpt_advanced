/* SPDX-License-Identifier: GPL-2.0-only */
/** @file
 * @brief Shared routing boundary for deterministic and real-thread worker tests.
 */
#include "dtmf.h"
#include "link_hub.h"
#include "worker_dtmf_fixture.h"
#include <assert.h>

bool ra_test_dtmf_fail;
char ra_test_dtmf_digit;
unsigned int ra_test_dtmf_closed;
/** @brief Opaque detector identity. */
static int detector_identity;

/** @cond TEST_FIXTURE */
/** @brief Return the fixture detector identity or inject startup failure. */
struct ra_dtmf_detector *ra_dtmf_open(unsigned int rate) {
    assert(rate == 8000);
    return ra_test_dtmf_fail ? NULL : (struct ra_dtmf_detector *)&detector_identity;
}

/** @brief Return the fixture's configured digit without processing audio. */
char ra_dtmf_process(struct ra_dtmf_detector *detector, bool receiving, int16_t *audio,
                     size_t samples) {
    (void)receiving;
    (void)audio;
    (void)samples;
    assert(detector == (struct ra_dtmf_detector *)&detector_identity);
    return ra_test_dtmf_digit;
}

/** @copydoc ra_dtmf_close */
void ra_dtmf_close(struct ra_dtmf_detector *detector) {
    assert(detector == (struct ra_dtmf_detector *)&detector_identity);
    ++ra_test_dtmf_closed;
}
/** @endcond */

bool ra_link_hub_process(struct ra_link_hub *hub, struct ra_controller *controller, bool receiving,
                         int16_t *audio, size_t samples, uint64_t now_ms) {
    assert(hub);
    return ra_controller_process(controller, receiving, audio, samples, now_ms);
}
