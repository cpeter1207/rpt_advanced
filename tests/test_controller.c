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
    assert(!ra_controller_process(&controller, false, NULL, 0, 1));
    assert(controller.status_speed_wpm == 20 && controller.status_frequency_hz == 800 &&
           controller.status_level_db == -6);
    assert(!ra_controller_process(&controller, false, audio, 160, 0));
    assert(ra_controller_process(&controller, true, NULL, 0, 100));
    rails(audio);
    assert(ra_controller_process(&controller, true, audio, 960, 120));
    assert(audio[0] == INT16_MAX && audio[1] == INT16_MIN);
    assert(!ra_controller_process(&controller, false, NULL, 0, 140));

    /* RF status is prepared by control and deferred while receiving and for post-unkey silence. */
    char oversized[RA_CONTROLLER_STATUS_TEXT_MAX + 1];
    memset(oversized, 'E', sizeof(oversized));
    oversized[RA_CONTROLLER_STATUS_TEXT_MAX] = '\0';
    assert(!ra_controller_queue_status(&controller, NULL, NULL, 0));
    assert(!ra_controller_queue_status(&controller, "", NULL, 0));
    assert(!ra_controller_queue_status(&controller, "E*", NULL, 0));
    assert(!ra_controller_queue_status(&controller, oversized, NULL, 0));
    assert(!ra_controller_queue_status(&controller, "E", NULL, 1));
    assert(!ra_controller_queue_status(&controller, "E", audio, 0));
    controller.full_duplex = false;
    /* A sample-free tick keeps PTT keyed after control starts a pending status. */
    assert(ra_controller_queue_status(&controller, "E", NULL, 0));
    assert(!ra_controller_process(&controller, false, NULL, 0, 145));
    assert(ra_controller_process(&controller, false, NULL, 0, 390));
    assert(controller.status_playing);
    assert(ra_controller_start(&controller, 0));
    for (size_t index = 0; index < RA_CONTROLLER_STATUS_QUEUE_DEPTH; ++index) {
        assert(ra_controller_queue_status(&controller, "E", NULL, 0));
    }
    assert(!ra_controller_queue_status(&controller, "E", NULL, 0));
    rails(audio);
    assert(!ra_controller_process(&controller, true, audio, 960, 150));
    assert(!audio[0] && !audio[959] && !atomic_load(&controller.status_read));
    assert(!ra_controller_process(&controller, false, audio, 960, 160));
    assert(!controller.status_playing && !atomic_load(&controller.status_read));
    assert(ra_controller_process(&controller, false, audio, 960, 410));
    assert(atomic_load(&controller.status_read) == 1 && !controller.status_playing);
    controller.full_duplex = true;
    for (size_t index = 1; index < RA_CONTROLLER_STATUS_QUEUE_DEPTH; ++index) {
        assert(ra_controller_process(&controller, false, audio, 960, 410 + index));
    }
    assert(!ra_controller_process(&controller, false, audio, 160, 420));
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
    assert(ra_controller_queue_status(&controller, "E", NULL, 0));
    assert(ra_controller_process(&controller, false, audio, 960, 250));
    assert(controller.status_playback.morse.speed == 25 &&
           controller.status_playback.morse.amplitude < 10000);
    /* A pending second status does not restart the prepared playback already in progress. */
    assert(ra_controller_start(&controller, 0));
    assert(ra_controller_queue_status(&controller, "EEEE", NULL, 0));
    assert(ra_controller_queue_status(&controller, "EEEE", NULL, 0));
    assert(ra_controller_process(&controller, false, audio, 1, 250));
    assert(controller.status_playing);
    assert(ra_controller_process(&controller, false, audio, 1, 251));
    assert(controller.status_playing);

    /* Prepared telemetry waits through the full unkey guard then plays speech before Morse. */
    int16_t spoken_status[] = {123};
    int16_t next_spoken_status[] = {-234};
    int16_t *released[1];
    assert(ra_controller_start(&controller, 0));
    memset(audio, 0, sizeof(audio));
    assert(ra_controller_queue_status(&controller, "E", spoken_status, 1));
    assert(ra_controller_queue_status(&controller, "E", next_spoken_status, 1));
    assert(!ra_controller_process(&controller, false, audio, 1, 249));
    assert(!audio[0]);
    assert(ra_controller_process(&controller, false, audio, 1, 250));
    assert(audio[0] == spoken_status[0]);
    assert(ra_controller_process(&controller, false, audio, 1, 251));
    assert(ra_controller_process(&controller, false, audio, 1, 252));
    assert(audio[0] == next_spoken_status[0]);
    assert(ra_controller_process(&controller, false, audio, 1, 253));
    assert(ra_controller_reclaim_status(&controller, released, 1) == 1);
    assert(released[0] == spoken_status);
    assert(ra_controller_reclaim_status(&controller, released, 1) == 1);
    assert(released[0] == next_spoken_status);

    /* Courtesy media uses the same delayed, hardware-paced RF playback path. */
    controller.status_speed_wpm = 20;
    controller.status_frequency_hz = 800;
    controller.status_level_db = -6;
    controller.full_duplex = false;
    controller.courtesy_delay_ms = 250;
    controller.courtesy[RA_COURTESY_RECEIVER].settings = (struct ra_identifier_settings){
        .morse_text = "R", .morse_speed_wpm = 20, .morse_frequency_hz = 800, .morse_level_db = -6};
    controller.courtesy[RA_COURTESY_LINK].settings = (struct ra_identifier_settings){
        .morse_text = "L", .morse_speed_wpm = 20, .morse_frequency_hz = 800, .morse_level_db = -6};
    assert(ra_controller_start(&controller, 0));
    assert(!ra_controller_process(&controller, true, NULL, 0, 100));
    assert(!ra_controller_process(&controller, false, NULL, 0, 349));
    assert(ra_controller_process(&controller, false, audio, 1, 600));
    assert(controller.courtesy_playing && controller.courtesy_pending_count == 0);
    (void)ra_controller_process(&controller, true, NULL, 0, 601);
    (void)ra_controller_process(&controller, false, NULL, 0, 602);
    assert(controller.courtesy_pending_count == 1);
    assert(ra_controller_start(&controller, 0));
    controller.link_active = true;
    assert(ra_controller_process(&controller, false, NULL, 0, 100));
    controller.link_active = false;
    assert(!ra_controller_process(&controller, false, NULL, 0, 349));
    assert(ra_controller_process(&controller, false, audio, 1, 600));
    assert(controller.courtesy_playing && controller.courtesy_pending_count == 0);
    controller.courtesy[RA_COURTESY_RECEIVER].settings.morse_text = "";
    assert(ra_controller_start(&controller, 0));
    assert(!ra_controller_process(&controller, true, NULL, 0, 100));
    assert(!ra_controller_process(&controller, false, NULL, 0, 600));
    assert(!controller.courtesy_pending_count);
    controller.status_speed_wpm = 25;
    controller.status_frequency_hz = 1200;
    controller.status_level_db = -12;
    controller.full_duplex = true;

    /* Two deferred sources retain their order, reject a third, and keep PTT through playback. */
    int16_t courtesy_tick[] = {1};
    struct ra_controller courtesy_controller = {
        .rate = 8000, .full_duplex = true, .courtesy_delay_ms = 100};
    for (size_t index = 0; index < RA_CONTROLLER_COURTESY_QUEUE_DEPTH; ++index) {
        courtesy_controller.courtesy[index] =
            (struct ra_controller_id){.settings = {.morse_text = "E",
                                                   .morse_speed_wpm = 20,
                                                   .morse_frequency_hz = 800,
                                                   .morse_level_db = -6},
                                      .audio = courtesy_tick,
                                      .samples = 1};
    }
    assert(ra_controller_start(&courtesy_controller, 0));
    assert(ra_controller_process(&courtesy_controller, true, NULL, 0, 1));
    assert(!ra_controller_process(&courtesy_controller, false, NULL, 0, 2));
    courtesy_controller.link_active = true;
    assert(ra_controller_process(&courtesy_controller, false, NULL, 0, 3));
    courtesy_controller.link_active = false;
    assert(!ra_controller_process(&courtesy_controller, false, NULL, 0, 4));
    assert(courtesy_controller.courtesy_pending_count == 2);
    assert(ra_controller_process(&courtesy_controller, true, NULL, 0, 5));
    assert(!ra_controller_process(&courtesy_controller, false, NULL, 0, 6));
    assert(courtesy_controller.courtesy_pending_count == 2);
    assert(ra_controller_process(&courtesy_controller, false, audio, 1, 102));
    assert(courtesy_controller.courtesy_playing && courtesy_controller.courtesy_pending_count == 1);
    assert(ra_controller_process(&courtesy_controller, false, audio, 1, 103));
    assert(!courtesy_controller.courtesy_playing &&
           courtesy_controller.courtesy_pending_count == 1);
    assert(ra_controller_process(&courtesy_controller, false, audio, 1, 104));
    assert(courtesy_controller.courtesy_playing && !courtesy_controller.courtesy_pending_count);
    assert(!ra_controller_process(&courtesy_controller, false, audio, 1, 105));
    struct ra_controller invalid_courtesy = {.rate = 8000};
    invalid_courtesy.courtesy[RA_COURTESY_RECEIVER].settings = (struct ra_identifier_settings){
        .morse_text = "E", .morse_speed_wpm = 20, .morse_frequency_hz = 4000, .morse_level_db = -6};
    assert(!ra_controller_start(&invalid_courtesy, 0));
    struct ra_controller link_courtesy = {
        .rate = 8000, .full_duplex = true, .courtesy_delay_ms = 10};
    link_courtesy.courtesy[RA_COURTESY_RECEIVER] =
        (struct ra_controller_id){.settings = {.morse_text = "E",
                                               .morse_speed_wpm = 20,
                                               .morse_frequency_hz = 800,
                                               .morse_level_db = -6},
                                  .audio = courtesy_tick,
                                  .samples = 1};
    assert(ra_controller_start(&link_courtesy, 0));
    assert(ra_controller_process(&link_courtesy, true, NULL, 0, 1));
    assert(!ra_controller_process(&link_courtesy, false, NULL, 0, 2));
    link_courtesy.link_active = true;
    assert(ra_controller_process(&link_courtesy, false, audio, 1, 12));
    assert(link_courtesy.courtesy_playing && !link_courtesy.courtesy_playback.prepared);

    /* Status and courtesy arbitration keeps the active announcement intact. */
    struct ra_controller arbitration = {.rate = 8000,
                                        .courtesy_delay_ms = 1,
                                        .courtesy = {{.settings = {.morse_text = "R",
                                                                   .morse_speed_wpm = 20,
                                                                   .morse_frequency_hz = 800,
                                                                   .morse_level_db = -6}}}};
    assert(ra_controller_start(&arbitration, 0));
    arbitration.courtesy_pending[0] = RA_COURTESY_RECEIVER;
    arbitration.courtesy_pending_count = 1;
    arbitration.courtesy_due_ms = 0;
    arbitration.status_playing = true;
    assert(ra_controller_process(&arbitration, false, NULL, 0, 1));
    arbitration.status_playing = false;
    arbitration.courtesy_playing = true;
    arbitration.courtesy_pending_count = 0;
    assert(ra_controller_queue_status(&arbitration, "E", NULL, 0));
    assert(ra_controller_process(&arbitration, false, NULL, 0, 250));
    assert(arbitration.courtesy_playing && !arbitration.status_playing);
    arbitration.courtesy_playing = false;
    assert(!ra_controller_process(&arbitration, true, NULL, 0, 251));

    /* Link activity selects the Morse fallback and smoothly ducks its output. */
    int16_t ducked_status[80];
    for (size_t index = 0; index < sizeof(ducked_status) / sizeof(*ducked_status); ++index) {
        ducked_status[index] = 10000;
    }
    controller.telemetry_duck_db = -20;
    assert(ra_controller_start(&controller, 0));
    controller.link_active = true;
    assert(ra_controller_queue_status(&controller, "E", ducked_status,
                                      sizeof(ducked_status) / sizeof(*ducked_status)));
    assert(ra_controller_process(&controller, false, audio,
                                 sizeof(ducked_status) / sizeof(*ducked_status), 250));
    int peak = 0;
    for (size_t index = 0; index < sizeof(ducked_status) / sizeof(*ducked_status); ++index) {
        int value = audio[index] < 0 ? -audio[index] : audio[index];
        peak = value > peak ? value : peak;
    }
    assert(peak >= 7000 && peak <= 8000 && controller.telemetry_gain > 0.099 &&
           controller.telemetry_gain < 0.101);
    controller.link_active = false;

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
    assert(ra_controller_queue_status(&controller, "E", NULL, 0));
    assert(ra_controller_process(&controller, false, audio, 960, 250));
    assert(controller.playing == SIZE_MAX && states[0].satisfied_ms == 0);
    assert(ra_controller_process(&controller, false, audio, 2, 251));
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
