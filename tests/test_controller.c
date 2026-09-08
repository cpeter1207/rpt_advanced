/* SPDX-License-Identifier: GPL-2.0-only */
/** @file
 * @brief Whole-controller receive, identification, mixing, and PTT sequences.
 */
#include "controller.h"
#include <assert.h>
#include <stdio.h>
#include <string.h>

/** @brief Fill a hardware block with alternating positive and negative rails.
 * @param audio Destination with 960 samples.
 */
static void rails(int16_t *audio) {
    for (size_t i = 0; i < 960; ++i) {
        audio[i] = i % 2 ? INT16_MIN : INT16_MAX;
    }
}

/** @brief Test complete controller sequences without hardware or independent timers.
 * @return Zero after assertions.
 */
int main(void) {
    int16_t audio[960] = {0};
    const int16_t prepared[] = {100, -100, 200, -200};
    struct ra_controller controller = {.rate = 8000, .full_duplex = true};
    assert(ra_controller_start(&controller, 0));
    assert(controller.status_speed_wpm == 20 && controller.status_frequency_hz == 800 &&
           controller.status_level_db == -6);
    assert(!ra_controller_process(&controller, false, audio, 160, 0));
    assert(ra_controller_process(&controller, true, NULL, 0, 100));
    rails(audio);
    assert(ra_controller_process(&controller, true, audio, 960, 120));
    assert(audio[0] == INT16_MAX && audio[1] == INT16_MIN);
    assert(!ra_controller_process(&controller, false, NULL, 0, 140));

    /* Status requests are Morse-prepared by control and deferred in half duplex. */
    char oversized[RA_CONTROLLER_STATUS_TEXT_MAX + 1];
    memset(oversized, 'E', sizeof(oversized));
    oversized[RA_CONTROLLER_STATUS_TEXT_MAX] = '\0';
    assert(!ra_controller_queue_status(&controller, NULL));
    assert(!ra_controller_queue_status(&controller, ""));
    assert(!ra_controller_queue_status(&controller, "E*"));
    assert(!ra_controller_queue_status(&controller, oversized));
    controller.full_duplex = false;
    /* A sample-free tick keeps PTT keyed after control starts a pending status. */
    assert(ra_controller_queue_status(&controller, "E"));
    assert(ra_controller_process(&controller, false, NULL, 0, 145));
    assert(controller.status_playing);
    assert(ra_controller_start(&controller, 0));
    for (size_t index = 0; index < RA_CONTROLLER_STATUS_QUEUE_DEPTH; ++index) {
        assert(ra_controller_queue_status(&controller, "E"));
    }
    assert(!ra_controller_queue_status(&controller, "E"));
    rails(audio);
    assert(!ra_controller_process(&controller, true, audio, 960, 150));
    assert(!audio[0] && !audio[959] && !atomic_load(&controller.status_read));
    assert(ra_controller_process(&controller, false, audio, 960, 160));
    assert(atomic_load(&controller.status_read) == 1 && !controller.status_playing);
    controller.full_duplex = true;
    for (size_t index = 1; index < RA_CONTROLLER_STATUS_QUEUE_DEPTH; ++index) {
        assert(ra_controller_process(&controller, false, audio, 960, 170 + index));
    }
    assert(!ra_controller_process(&controller, false, audio, 160, 180));
    assert(atomic_load(&controller.status_read) == RA_CONTROLLER_STATUS_QUEUE_DEPTH);
    /* Partial status defaults are invalid rather than silently mixing defaults with caller input.
     */
    controller.status_speed_wpm = 0;
    controller.status_frequency_hz = 800;
    controller.status_level_db = 0;
    assert(!ra_controller_start(&controller, 0));
    controller.status_frequency_hz = 0;
    controller.status_level_db = -6;
    assert(!ra_controller_start(&controller, 0));
    controller.status_speed_wpm = 25;
    controller.status_frequency_hz = 4000;
    controller.status_level_db = -12;
    assert(!ra_controller_start(&controller, 0));
    controller.status_frequency_hz = 1200;
    assert(ra_controller_start(&controller, 0));
    assert(ra_controller_queue_status(&controller, "E"));
    assert(ra_controller_process(&controller, false, audio, 960, 190));
    assert(controller.status_morse.speed == 25 && controller.status_morse.amplitude < 10000);
    /* A pending second status does not restart the Morse renderer already in progress. */
    assert(ra_controller_start(&controller, 0));
    assert(ra_controller_queue_status(&controller, "EEEE"));
    assert(ra_controller_queue_status(&controller, "EEEE"));
    assert(ra_controller_process(&controller, false, audio, 1, 191));
    assert(controller.status_playing);
    assert(ra_controller_process(&controller, false, audio, 1, 192));
    assert(controller.status_playing);

    struct ra_controller_id ids[2] = {{.settings = {.interval_ms = 100,
                                                    .priority = 5,
                                                    .regardless_of_activity = true,
                                                    .morse_text = "E",
                                                    .morse_speed_wpm = 20,
                                                    .morse_frequency_hz = 1000},
                                       .audio = prepared,
                                       .samples = 4},
                                      {.settings = {.interval_ms = 100,
                                                    .priority = 1,
                                                    .regardless_of_activity = true,
                                                    .morse_text = "E",
                                                    .morse_speed_wpm = 20,
                                                    .morse_frequency_hz = 1000}}};
    struct ra_id_rule rules[2];
    struct ra_id_state states[2];
    controller.ids = ids;
    controller.rules = rules;
    controller.states = states;
    controller.count = 2;
    controller.rate = 1000;
    assert(!ra_controller_start(&controller, 0));
    controller.rate = 8000;
    ids[0].settings.morse_frequency_hz = 4000;
    assert(!ra_controller_start(&controller, 0));
    ids[0].settings.morse_frequency_hz = 1000;
    controller.hang_ms = 20;
    assert(ra_controller_start(&controller, 0));
    /* A status reply preempts an ID but leaves that ID due for replay afterward. */
    assert(ra_controller_process(&controller, false, audio, 2, 100));
    assert(controller.playing == 0 && controller.playback.offset == 2);
    assert(ra_controller_queue_status(&controller, "E"));
    assert(ra_controller_process(&controller, false, audio, 960, 101));
    assert(controller.playing == SIZE_MAX && states[0].satisfied_ms == 0);
    assert(ra_controller_process(&controller, false, audio, 2, 102));
    assert(controller.playing == 0 && audio[0] == 100 && audio[1] == -100);
    assert(ra_controller_start(&controller, 0));
    assert(ra_controller_process(&controller, false, audio, 960, 100));
    assert(audio[0] == 100 && audio[3] == -200 && audio[4] == 0);
    assert(states[0].satisfied_ms == 100 && states[1].satisfied_ms == 100);
    assert(!ra_controller_process(&controller, false, audio, 160, 120));
    /* Reception chooses Morse and safely sums it with local voice. */
    rails(audio);
    assert(ra_controller_process(&controller, true, audio, 960, 200));
    assert(audio[2] == INT16_MAX && audio[7] == INT16_MIN);
    assert(states[0].satisfied_ms == 200 && states[1].satisfied_ms == 200);

    /* Interrupt prepared playback using a carrier event that consumes no samples. */
    assert(ra_controller_start(&controller, 0));
    assert(ra_controller_process(&controller, false, audio, 2, 100));
    assert(controller.playing == 0 && controller.playback.offset == 2);
    assert(ra_controller_process(&controller, true, NULL, 0, 101));
    assert(!controller.playback.prepared && controller.playback.offset == 2);
    assert(ra_controller_process(&controller, false, audio, 960, 102));
    assert(controller.playing == SIZE_MAX && states[0].satisfied_ms == 102);

    /* Linked receive has the same irreversible prepared-ID-to-Morse interruption behavior. */
    assert(ra_controller_start(&controller, 0));
    assert(ra_controller_process(&controller, false, audio, 2, 100));
    assert(controller.playing == 0 && controller.playback.prepared);
    controller.link_active = true;
    assert(ra_controller_process(&controller, false, audio, 960, 101));
    assert(!controller.playback.prepared && controller.playing == SIZE_MAX &&
           states[0].satisfied_ms == 101);
    controller.link_active = false;

    /* Half duplex defers a due ID, then transmits it after reception ends. */
    controller.full_duplex = false;
    assert(ra_controller_start(&controller, 0));
    rails(audio);
    assert(!ra_controller_process(&controller, true, audio, 960, 100));
    assert(controller.playing == SIZE_MAX && audio[0] == 0 && audio[959] == 0);
    assert(ra_controller_process(&controller, false, audio, 2, 120));
    assert(!ra_controller_process(&controller, true, NULL, 0, 121));
    assert(!ra_controller_process(&controller, true, audio, 160, 122));
    assert(controller.playback.offset == 2);
    assert(ra_controller_process(&controller, false, audio, 960, 140));
    assert(states[0].satisfied_ms == 140);

    /* Welcome sets do not become periodic, and require one full idle interval. */
    controller.full_duplex = true;
    controller.count = 1;
    ids[0].settings.first_key_only = true;
    assert(ra_controller_start(&controller, 0));
    assert(ra_controller_process(&controller, true, audio, 160, 50));
    assert(!states[0].first_key_pending && controller.playing == SIZE_MAX);
    assert(!ra_controller_process(&controller, false, audio, 160, 80));
    assert(ra_controller_process(&controller, true, NULL, 0, 200));
    assert(states[0].first_key_pending);
    assert(ra_controller_process(&controller, true, audio, 960, 220));
    assert(!states[0].first_key_pending && states[0].satisfied_ms == 220);
    assert(ra_controller_process(&controller, true, audio, 960, 500));
    assert(states[0].satisfied_ms == 220);

    /* Half-duplex welcome uses the idle period before RX, not its short release gap. */
    controller.full_duplex = false;
    controller.count = 2;
    assert(ra_controller_start(&controller, 0));
    assert(!ra_controller_process(&controller, true, audio, 160, 200));
    assert(!states[0].first_key_pending);
    assert(ra_controller_process(&controller, false, audio, 960, 220));
    assert(states[0].satisfied_ms == 220 && states[1].satisfied_ms == 220);

    /* No terminal fallback must not count an empty interrupted ID as successful. */
    controller.full_duplex = true;
    controller.count = 1;
    ids[0].settings.first_key_only = false;
    ids[0].settings.morse_text = "";
    assert(ra_controller_start(&controller, 0));
    assert(ra_controller_process(&controller, true, audio, 960, 100));
    assert(states[0].satisfied_ms == 0 && controller.playing == SIZE_MAX);
    /* Remote audio keys half duplex without repeating local receiver samples. */
    controller.count = 0;
    controller.full_duplex = false;
    controller.hang_ms = 0;
    assert(ra_controller_start(&controller, 0));
    int16_t remote[] = {100, -100};
    controller.link_active = true;
    controller.link_audio = remote;
    audio[0] = audio[1] = 1000;
    assert(ra_controller_process(&controller, false, audio, 2, 100));
    assert(audio[0] == 100 && audio[1] == -100 && controller.last_activity_ms == 100);
    assert(!ra_controller_process(&controller, true, audio, 2, 120));
    assert(!audio[0] && !audio[1]);
    controller.full_duplex = true;
    audio[0] = INT16_MAX;
    audio[1] = INT16_MIN;
    assert(ra_controller_process(&controller, true, audio, 2, 140));
    assert(audio[0] == INT16_MAX && audio[1] == INT16_MIN);
    controller.link_audio = NULL;
    assert(ra_controller_process(&controller, false, audio, 2, 160));
    assert(!audio[0] && !audio[1]);
    controller.link_active = false;
    assert(!ra_controller_process(&controller, false, audio, 2, 180));
    puts("node controller audio and identification sequences passed");
    return 0;
}
