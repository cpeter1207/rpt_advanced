/* SPDX-License-Identifier: GPL-2.0-only */
/** @file
 * @brief Real-thread start, hardware-event pacing, stop, and replacement-controller test.
 */
#include <asterisk.h>

#include "worker.h"
#include <assert.h>
#include <asterisk/channel.h>
#include <poll.h>
#include <stdio.h>
#include <unistd.h>

/** @brief Pipe used as the fixture's hardware readiness source. */
static int ticks[2];
/** @brief Acknowledgments emitted after a complete controller exchange. */
static int acknowledgments[2];
/** @brief Channels released by completed workers. Read only after joining. */
static unsigned int released;

/** @brief Wait for simulated hardware readiness, preserving the shutdown timeout.
 * @param channel Unused fixture channel.
 * @param milliseconds Worker wait bound.
 * @return Poll result.
 */
int ast_waitfor(struct ast_channel *channel, int milliseconds) {
    (void)channel;
    struct pollfd descriptor = {.fd = ticks[0], .events = POLLIN};
    return poll(&descriptor, 1, milliseconds);
}

/** @brief Consume exactly one hardware event and invoke the real controller.
 * @param state Worker-owned exchange state.
 * @param channel Unused fixture channel.
 * @return Zero after successful exchange.
 */
int __wrap_ra_radio_exchange(struct ra_radio *state, struct ast_channel *channel) {
    (void)channel;
    char tick;
    assert(read(ticks[0], &tick, 1) == 1);
    int16_t audio[160] = {123};
    state->keyed = state->render(state->context, true, audio, 160);
    /* Full duplex repeats the input; half duplex cannot transmit local RX. */
    char output = state->keyed ? 'F' : 'H';
    assert(audio[0] == (state->keyed ? 123 : 0));
    assert(write(acknowledgments[1], &output, 1) == 1);
    return 0;
}

/** @brief Validate explicit unkey during worker exit.
 * @param channel Unused fixture channel.
 * @param condition Expected unkey indication.
 * @return Zero.
 */
int ast_indicate(struct ast_channel *channel, int condition) {
    (void)channel;
    assert(condition == AST_CONTROL_RADIO_UNKEY);
    return 0;
}

/** @brief Record a completed worker's channel release.
 * @param channel Unused fixture channel.
 */
void ast_hangup(struct ast_channel *channel) {
    (void)channel;
    ++released;
}

/** @brief Exercise two real worker lifetimes with different controller configurations.
 * @return Zero after pacing and cleanup assertions.
 */
int main(void) {
    assert(pipe(ticks) == 0 && pipe(acknowledgments) == 0);
    struct ra_controller controller = {.rate = 8000, .full_duplex = true};
    struct ra_worker worker = {.controller = &controller};
    for (unsigned int pass = 0; pass < 2; ++pass) {
        assert(ra_controller_start(&controller, 0));
        assert(ra_worker_start(&worker) == 0);
        const char input[] = "123";
        assert(write(ticks[1], input, 3) == 3);
        struct pollfd ack = {.fd = acknowledgments[0], .events = POLLIN};
        for (unsigned int frame = 0; frame < 3; ++frame) {
            assert(poll(&ack, 1, 30000) == 1);
            char output;
            assert(read(acknowledgments[0], &output, 1) == 1);
            assert(output == (pass ? 'H' : 'F'));
        }
        ra_worker_stop(&worker);
        assert(worker.result == 0 && released == pass + 1);
        assert(poll(&ack, 1, 0) == 0);
        controller.full_duplex = false;
    }
    assert(close(ticks[0]) == 0 && close(ticks[1]) == 0);
    assert(close(acknowledgments[0]) == 0 && close(acknowledgments[1]) == 0);
    puts("real channel-worker thread and replacement-controller tests passed");
    return 0;
}
