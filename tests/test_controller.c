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
    struct ra_controller_id receiver_courtesy = {
        .settings = (struct ra_identifier_settings){.morse_text = "R",
                                                    .morse_speed_wpm = 20,
                                                    .morse_frequency_hz = 800,
                                                    .morse_level_db = -6}};
    struct ra_controller_id link_courtesy_media = {
        .settings = (struct ra_identifier_settings){.morse_text = "L",
                                                    .morse_speed_wpm = 20,
                                                    .morse_frequency_hz = 800,
                                                    .morse_level_db = -6}};
    controller.receiver_courtesy = &receiver_courtesy;
    controller.link_courtesy = &link_courtesy_media;
    assert(ra_controller_start(&controller, 0));
    assert(!ra_controller_process(&controller, true, NULL, 0, 100));
    assert(ra_controller_process(&controller, false, NULL, 0, 349));
    assert(ra_controller_process(&controller, false, audio, 1, 600));
    assert(controller.courtesy_playing && controller.courtesy_pending_count == 0);
    (void)ra_controller_process(&controller, true, NULL, 0, 601);
    (void)ra_controller_process(&controller, false, NULL, 0, 602);
    assert(controller.courtesy_pending_count == 1);
    /* Flutter before a courtesy tone starts cancels that stale announcement. */
    (void)ra_controller_process(&controller, true, NULL, 0, 603);
    assert(!controller.courtesy_pending_count);
    assert(ra_controller_start(&controller, 0));
    controller.link_active = true;
    assert(ra_controller_process(&controller, false, NULL, 0, 100));
    controller.link_active = false;
    ra_controller_link_unkeyed(&controller, "link", false, 100);
    assert(ra_controller_process(&controller, false, NULL, 0, 349));
    assert(ra_controller_process(&controller, false, audio, 1, 600));
    assert(controller.courtesy_playing && controller.courtesy_pending_count == 0);
    receiver_courtesy.settings.morse_text = "";
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
    struct ra_controller_id courtesy_receiver = {.settings = {.morse_text = "E",
                                                              .morse_speed_wpm = 20,
                                                              .morse_frequency_hz = 800,
                                                              .morse_level_db = -6},
                                                 .audio = courtesy_tick,
                                                 .samples = 1};
    struct ra_controller_id courtesy_link = courtesy_receiver;
    struct ra_controller courtesy_controller = {.receiver_courtesy = &courtesy_receiver,
                                                .link_courtesy = &courtesy_link,
                                                .rate = 8000,
                                                .full_duplex = true,
                                                .courtesy_delay_ms = 100};
    assert(ra_controller_start(&courtesy_controller, 0));
    courtesy_controller.link_active = true;
    assert(ra_controller_process(&courtesy_controller, true, NULL, 0, 1));
    courtesy_controller.link_active = false;
    ra_controller_link_unkeyed(&courtesy_controller, "link", false, 2);
    assert(ra_controller_process(&courtesy_controller, false, NULL, 0, 2));
    assert(courtesy_controller.courtesy_pending_count == 2);
    assert(ra_controller_process(&courtesy_controller, true, NULL, 0, 3));
    assert(courtesy_controller.courtesy_pending_count == 1);
    assert(!courtesy_controller.courtesy_pending[0].receiver &&
           !strcmp(courtesy_controller.courtesy_pending[0].remote, "link"));
    assert(ra_controller_process(&courtesy_controller, false, NULL, 0, 4));
    assert(courtesy_controller.courtesy_pending_count == 2);
    assert(ra_controller_process(&courtesy_controller, false, audio, 1, 104));
    assert(courtesy_controller.courtesy_playing && courtesy_controller.courtesy_pending_count == 1);
    assert(ra_controller_process(&courtesy_controller, false, audio, 1, 105));
    assert(!courtesy_controller.courtesy_playing &&
           courtesy_controller.courtesy_pending_count == 1);
    assert(ra_controller_process(&courtesy_controller, false, audio, 1, 106));
    assert(courtesy_controller.courtesy_playing && !courtesy_controller.courtesy_pending_count);
    struct ra_controller invalid_courtesy = {.rate = 8000};
    struct ra_controller_id invalid_receiver_courtesy = {
        .settings = (struct ra_identifier_settings){.morse_text = "E",
                                                    .morse_speed_wpm = 20,
                                                    .morse_frequency_hz = 4000,
                                                    .morse_level_db = -6}};
    invalid_courtesy.receiver_courtesy = &invalid_receiver_courtesy;
    assert(!ra_controller_start(&invalid_courtesy, 0));
    struct ra_controller link_courtesy = {
        .rate = 8000, .full_duplex = true, .courtesy_delay_ms = 10};
    struct ra_controller_id link_receiver_courtesy =
        (struct ra_controller_id){.settings = {.morse_text = "E",
                                               .morse_speed_wpm = 20,
                                               .morse_frequency_hz = 800,
                                               .morse_level_db = -6},
                                  .audio = courtesy_tick,
                                  .samples = 1};
    struct ra_controller_id link_link_courtesy = link_receiver_courtesy;
    link_courtesy.receiver_courtesy = &link_receiver_courtesy;
    link_courtesy.link_courtesy = &link_link_courtesy;
    assert(ra_controller_start(&link_courtesy, 0));
    assert(ra_controller_process(&link_courtesy, true, NULL, 0, 1));
    assert(ra_controller_process(&link_courtesy, false, NULL, 0, 2));
    struct ra_controller link_only_courtesy = {.link_courtesy = &link_link_courtesy,
                                               .rate = 8000,
                                               .full_duplex = true,
                                               .courtesy_delay_ms = 10};
    assert(ra_controller_start(&link_only_courtesy, 0));
    link_only_courtesy.link_active = true;
    assert(ra_controller_process(&link_only_courtesy, false, audio, 1, 12));
    assert(!link_only_courtesy.courtesy_playing && !link_only_courtesy.courtesy_pending_count);
    link_only_courtesy.link_active = false;
    ra_controller_link_unkeyed(&link_only_courtesy, "link", false, 13);
    assert(ra_controller_process(&link_only_courtesy, false, NULL, 0, 13));
    assert(ra_controller_process(&link_only_courtesy, false, audio, 1, 24));
    assert(link_only_courtesy.courtesy_playing && link_only_courtesy.courtesy_playback.prepared);

    /* No configured courtesy media must not extend PTT past the ordinary receiver release. */
    struct ra_controller no_courtesy = {.rate = 8000, .full_duplex = true, .hang_ms = 0};
    assert(ra_controller_start(&no_courtesy, 0));
    assert(ra_controller_process(&no_courtesy, true, audio, 1, 1));
    assert(!ra_controller_process(&no_courtesy, false, audio, 1, 2));
    assert(!no_courtesy.courtesy_pending_count && !no_courtesy.courtesy_playing);

    /* A playable courtesy holds PTT through its delay and last sample, then releases normally. */
    const int16_t courtesy_contract_audio[] = {321, -654};
    struct ra_controller_id courtesy_contract_media = {
        .settings = {.morse_text = "R", .morse_speed_wpm = 20, .morse_frequency_hz = 800},
        .audio = courtesy_contract_audio,
        .samples = sizeof(courtesy_contract_audio) / sizeof(*courtesy_contract_audio)};
    struct ra_controller courtesy_contract = {.receiver_courtesy = &courtesy_contract_media,
                                              .rate = 8000,
                                              .full_duplex = true,
                                              .hang_ms = 0,
                                              .courtesy_delay_ms = 10};
    assert(ra_controller_start(&courtesy_contract, 0));
    assert(ra_controller_process(&courtesy_contract, true, audio, 1, 10));
    assert(ra_controller_process(&courtesy_contract, false, audio, 1, 11));
    assert(courtesy_contract.courtesy_pending_count == 1);
    assert(ra_controller_process(&courtesy_contract, false, audio, 1, 20));
    assert(audio[0] == 0 && courtesy_contract.courtesy_pending_count == 1);
    assert(ra_controller_process(&courtesy_contract, false, audio, 1, 21));
    assert(audio[0] == courtesy_contract_audio[0] && courtesy_contract.courtesy_playing);
    assert(ra_controller_process(&courtesy_contract, false, audio, 1, 22));
    assert(audio[0] == courtesy_contract_audio[1] && courtesy_contract.courtesy_playing);
    assert(!ra_controller_process(&courtesy_contract, false, audio, 1, 23));
    assert(audio[0] == 0 && !courtesy_contract.courtesy_playing &&
           !courtesy_contract.courtesy_pending_count);

    /* An empty queue ignores deliberately poisoned storage beyond its count. */
    struct ra_controller empty_courtesy_queue = {.rate = 8000, .full_duplex = true, .hang_ms = 0};
    assert(ra_controller_start(&empty_courtesy_queue, 0));
    empty_courtesy_queue.courtesy_pending[0] = (struct ra_controller_courtesy_pending){
        .media = &courtesy_contract_media, .receiver = true, .due_ms = 0};
    assert(!empty_courtesy_queue.courtesy_pending_count);
    assert(!ra_controller_process(&empty_courtesy_queue, false, audio, 1, 1));
    assert(!empty_courtesy_queue.courtesy_playing && !empty_courtesy_queue.courtesy_pending_count);

    /* A renewed receiver or link ducks active courtesy PCM without replacing it with Morse. */
    const int16_t ducked_courtesy_audio[] = {10000, 10000, 10000, 10000};
    struct ra_controller_id ducked_courtesy_media = {
        .settings = {.morse_text = "M", .morse_speed_wpm = 20, .morse_frequency_hz = 800},
        .audio = ducked_courtesy_audio,
        .samples = sizeof(ducked_courtesy_audio) / sizeof(*ducked_courtesy_audio)};
    struct ra_controller ducked_courtesy_controller = {
        .receiver_courtesy = &ducked_courtesy_media,
        .rate = 8000,
        .full_duplex = true,
        .telemetry_duck_db = -20,
    };
    assert(ra_controller_start(&ducked_courtesy_controller, 0));
    assert(ra_controller_process(&ducked_courtesy_controller, true, NULL, 0, 1));
    assert(ra_controller_process(&ducked_courtesy_controller, false, audio, 1, 2));
    assert(audio[0] == ducked_courtesy_audio[0] && ducked_courtesy_controller.courtesy_playing &&
           ducked_courtesy_controller.courtesy_playback.prepared);
    ducked_courtesy_controller.link_active = true;
    assert(ra_controller_process(&ducked_courtesy_controller, false, audio, 1, 3));
    assert(audio[0] > 0 && audio[0] < ducked_courtesy_audio[1] &&
           ducked_courtesy_controller.courtesy_playback.prepared);
    ducked_courtesy_controller.link_active = false;
    audio[0] = 0;
    assert(ra_controller_process(&ducked_courtesy_controller, true, audio, 1, 4));
    assert(audio[0] > 0 && audio[0] < ducked_courtesy_audio[2] &&
           ducked_courtesy_controller.courtesy_playback.prepared);

    /* A permanent peer may override generic link media; temporary and unmatched peers fall back. */
    const int16_t generic_link_audio[] = {101};
    const int16_t north_link_audio[] = {202};
    struct ra_controller_id generic_link_courtesy = {
        .settings = {.morse_text = "L", .morse_speed_wpm = 20, .morse_frequency_hz = 800},
        .audio = generic_link_audio,
        .samples = 1};
    struct ra_controller_id north_link_courtesy = {
        .settings = {.morse_text = "N", .morse_speed_wpm = 20, .morse_frequency_hz = 800},
        .audio = north_link_audio,
        .samples = 1};
    struct ra_controller_peer_courtesy peer_overrides[] = {{"north", &north_link_courtesy}};
    struct ra_controller per_peer_courtesy = {.link_courtesy = &generic_link_courtesy,
                                              .peer_courtesies = peer_overrides,
                                              .peer_courtesy_count = 1,
                                              .rate = 8000,
                                              .full_duplex = true};
    assert(ra_controller_start(&per_peer_courtesy, 0));
    ra_controller_link_unkeyed(&per_peer_courtesy, "north", false, 1);
    assert(ra_controller_process(&per_peer_courtesy, false, audio, 1, 1));
    assert(audio[0] == generic_link_audio[0]);
    assert(ra_controller_start(&per_peer_courtesy, 0));
    ra_controller_link_unkeyed(&per_peer_courtesy, "north", true, 1);
    assert(ra_controller_process(&per_peer_courtesy, false, audio, 1, 1));
    assert(audio[0] == north_link_audio[0]);
    assert(ra_controller_start(&per_peer_courtesy, 0));
    ra_controller_link_unkeyed(&per_peer_courtesy, "south", true, 1);
    assert(ra_controller_process(&per_peer_courtesy, false, audio, 1, 1));
    assert(audio[0] == generic_link_audio[0]);
    assert(ra_controller_start(&per_peer_courtesy, 0));
    ra_controller_link_unkeyed(&per_peer_courtesy, "north", true, 1);
    ra_controller_link_unkeyed(&per_peer_courtesy, "south", false, 1);
    assert(per_peer_courtesy.courtesy_pending_count == 2);
    ra_controller_link_keyed(&per_peer_courtesy, "north");
    assert(per_peer_courtesy.courtesy_pending_count == 1 &&
           !strcmp(per_peer_courtesy.courtesy_pending[0].remote, "south"));
    ra_controller_link_keyed(&per_peer_courtesy, NULL);
    assert(per_peer_courtesy.courtesy_pending_count == 1);
    assert(ra_controller_process(&per_peer_courtesy, false, audio, 1, 1));
    assert(audio[0] == generic_link_audio[0]);
    struct ra_controller_id unavailable_peer_courtesy = {0};
    struct ra_controller_peer_courtesy unavailable_override[] = {
        {"silent", &unavailable_peer_courtesy}};
    per_peer_courtesy.peer_courtesies = unavailable_override;
    assert(ra_controller_start(&per_peer_courtesy, 0));
    ra_controller_link_unkeyed(&per_peer_courtesy, "silent", true, 1);
    assert(ra_controller_process(&per_peer_courtesy, false, audio, 1, 1));
    assert(audio[0] == generic_link_audio[0]);
    assert(ra_controller_start(&per_peer_courtesy, 0));
    ra_controller_link_unkeyed(&per_peer_courtesy, NULL, true, 1);
    ra_controller_link_unkeyed(&per_peer_courtesy, "", true, 1);
    ra_controller_link_unkeyed(&per_peer_courtesy, "", false, 1);
    assert(!per_peer_courtesy.courtesy_pending_count);
    char oversized_remote[RA_CONTROLLER_COURTESY_REMOTE_MAX + 1];
    memset(oversized_remote, 'x', sizeof(oversized_remote) - 1);
    oversized_remote[sizeof(oversized_remote) - 1] = '\0';
    ra_controller_link_unkeyed(&per_peer_courtesy, oversized_remote, false, 1);
    assert(!per_peer_courtesy.courtesy_pending_count);
    struct ra_controller invalid_peer_courtesy = {.peer_courtesy_count = 1, .rate = 8000};
    assert(!ra_controller_start(&invalid_peer_courtesy, 0));
    struct ra_controller_peer_courtesy invalid_peer_overrides[] = {{NULL, &generic_link_courtesy}};
    invalid_peer_courtesy.peer_courtesies = invalid_peer_overrides;
    assert(!ra_controller_start(&invalid_peer_courtesy, 0));
    invalid_peer_overrides[0].remote = "";
    assert(!ra_controller_start(&invalid_peer_courtesy, 0));
    invalid_peer_overrides[0].remote = "north";
    invalid_peer_overrides[0].media = NULL;
    assert(!ra_controller_start(&invalid_peer_courtesy, 0));
    struct ra_controller_id invalid_peer_media = {
        .settings = {.morse_text = "E", .morse_speed_wpm = 20, .morse_frequency_hz = 4000}};
    invalid_peer_overrides[0].media = &invalid_peer_media;
    assert(!ra_controller_start(&invalid_peer_courtesy, 0));
    per_peer_courtesy.peer_courtesies = peer_overrides;

    /* Multiple pending courtesy sources play in order without stale queueing. */
    struct ra_controller ordered_courtesy = {
        .rate = 8000, .full_duplex = true, .courtesy_delay_ms = 1};
    struct ra_controller_id ordered_receiver_courtesy =
        (struct ra_controller_id){.settings = {.morse_text = "E",
                                               .morse_speed_wpm = 20,
                                               .morse_frequency_hz = 800,
                                               .morse_level_db = -6},
                                  .audio = courtesy_tick,
                                  .samples = 1};
    struct ra_controller_id ordered_link_courtesy = ordered_receiver_courtesy;
    ordered_courtesy.receiver_courtesy = &ordered_receiver_courtesy;
    ordered_courtesy.link_courtesy = &ordered_link_courtesy;
    assert(ra_controller_start(&ordered_courtesy, 0));
    ordered_courtesy.courtesy_pending[0] = (struct ra_controller_courtesy_pending){
        .media = &ordered_receiver_courtesy, .receiver = true, .due_ms = 0};
    ordered_courtesy.courtesy_pending[1] = (struct ra_controller_courtesy_pending){
        .media = &ordered_link_courtesy, .receiver = false, .due_ms = 0, .remote = "link"};
    ordered_courtesy.courtesy_pending_count = 2;
    assert(ra_controller_process(&ordered_courtesy, false, audio, 1, 1));
    assert(ordered_courtesy.courtesy_playing && ordered_courtesy.courtesy_pending_count == 1);
    assert(ra_controller_process(&ordered_courtesy, false, audio, 1, 2));
    assert(!ordered_courtesy.courtesy_playing && ordered_courtesy.courtesy_pending_count == 1);
    assert(ra_controller_start(&ordered_courtesy, 0));
    ordered_courtesy.courtesy_pending_count = RA_CONTROLLER_COURTESY_QUEUE_DEPTH;
    ra_controller_link_unkeyed(&ordered_courtesy, "overflow", false, 0);
    assert(ordered_courtesy.courtesy_pending_count == RA_CONTROLLER_COURTESY_QUEUE_DEPTH);
    assert(ra_controller_start(&ordered_courtesy, 0));
    ordered_courtesy.courtesy_delay_ms = UINT64_MAX;
    ra_controller_link_unkeyed(&ordered_courtesy, "saturated", false, 1);
    assert(ordered_courtesy.courtesy_pending_count == 1 &&
           ordered_courtesy.courtesy_pending[0].due_ms == UINT64_MAX);

    /* Each unkey retains its own delay even when another tone is already queued. */
    struct ra_controller staggered_courtesy = {.link_courtesy = &ordered_link_courtesy,
                                               .rate = 8000,
                                               .full_duplex = true,
                                               .courtesy_delay_ms = 100};
    assert(ra_controller_start(&staggered_courtesy, 0));
    ra_controller_link_unkeyed(&staggered_courtesy, "first", false, 0);
    ra_controller_link_unkeyed(&staggered_courtesy, "second", false, 50);
    ra_controller_link_unkeyed(&staggered_courtesy, "third", false, 75);
    assert(ra_controller_process(&staggered_courtesy, false, audio, 1, 99));
    assert(!audio[0] && staggered_courtesy.courtesy_pending_count == 3 &&
           !staggered_courtesy.courtesy_playing);
    assert(ra_controller_process(&staggered_courtesy, false, audio, 1, 100));
    assert(staggered_courtesy.courtesy_playing && staggered_courtesy.courtesy_pending_count == 2);
    assert(ra_controller_process(&staggered_courtesy, false, audio, 1, 101));
    assert(!staggered_courtesy.courtesy_playing && staggered_courtesy.courtesy_pending_count == 2);
    assert(ra_controller_process(&staggered_courtesy, false, audio, 1, 149));
    assert(!audio[0] && !staggered_courtesy.courtesy_playing);
    assert(ra_controller_process(&staggered_courtesy, false, audio, 1, 150));
    assert(staggered_courtesy.courtesy_playing && staggered_courtesy.courtesy_pending_count == 1);
    assert(ra_controller_process(&staggered_courtesy, false, audio, 1, 151));
    assert(!staggered_courtesy.courtesy_playing && staggered_courtesy.courtesy_pending_count == 1);
    assert(ra_controller_process(&staggered_courtesy, false, audio, 1, 175));
    assert(staggered_courtesy.courtesy_playing && !staggered_courtesy.courtesy_pending_count);
    struct ra_controller unavailable_courtesy = {.rate = 8000, .full_duplex = true};
    assert(ra_controller_start(&unavailable_courtesy, 0));
    (void)ra_controller_process(&unavailable_courtesy, true, NULL, 0, 1);
    (void)ra_controller_process(&unavailable_courtesy, false, NULL, 0, 2);

    /* Status and courtesy arbitration keeps the active announcement intact. */
    struct ra_controller_id arbitration_receiver_courtesy = {.settings = {.morse_text = "R",
                                                                          .morse_speed_wpm = 20,
                                                                          .morse_frequency_hz = 800,
                                                                          .morse_level_db = -6}};
    struct ra_controller arbitration = {
        .rate = 8000, .courtesy_delay_ms = 1, .receiver_courtesy = &arbitration_receiver_courtesy};
    assert(ra_controller_start(&arbitration, 0));
    arbitration.courtesy_pending[0] = (struct ra_controller_courtesy_pending){
        .media = &arbitration_receiver_courtesy, .receiver = true, .due_ms = 0};
    arbitration.courtesy_pending_count = 1;
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

    /* RF status waits until both local and linked receive are idle. */
    int16_t ducked_status[80];
    for (size_t index = 0; index < sizeof(ducked_status) / sizeof(*ducked_status); ++index) {
        ducked_status[index] = 10000;
    }
    controller.telemetry_duck_db = -20;
    link_courtesy_media.settings.morse_text = "";
    assert(ra_controller_start(&controller, 0));
    controller.link_active = true;
    assert(ra_controller_queue_status(&controller, "E", ducked_status,
                                      sizeof(ducked_status) / sizeof(*ducked_status)));
    assert(ra_controller_process(&controller, false, audio,
                                 sizeof(ducked_status) / sizeof(*ducked_status), 250));
    assert(!controller.status_playing && !audio[0]);
    controller.link_active = false;
    assert(ra_controller_process(&controller, false, audio,
                                 sizeof(ducked_status) / sizeof(*ducked_status), 251));
    assert(controller.status_playing && audio[0] > 0 && audio[0] < ducked_status[0] &&
           controller.telemetry_gain < 1.0);

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
    /* A queued status waits until the current identifier finishes. */
    assert(ra_controller_process(&controller, false, audio, 2, 100));
    assert(controller.playing == 0 && controller.playback.offset == 2);
    assert(ra_controller_queue_status(&controller, "E", NULL, 0));
    assert(ra_controller_process(&controller, false, audio, 960, 250));
    assert(controller.playing == SIZE_MAX && states[0].satisfied_ms == 250 &&
           !controller.status_playing);
    assert(ra_controller_process(&controller, false, audio, 2, 251));
    assert(controller.status_playing && controller.playing == SIZE_MAX);
    assert(ra_controller_start(&controller, 0));
    assert(ra_controller_process(&controller, false, audio, 960, 100));
    assert(audio[0] == 100 && audio[3] == -200 && audio[4] == 0);
    assert(states[0].satisfied_ms == 100 && states[1].satisfied_ms == 100);
    assert(!ra_controller_process(&controller, false, audio, 160, 150));
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

    /* Polite IDs wait for receive and telemetry, then honor their bounded deadline. */
    struct ra_controller_id polite_id = {.settings = {.interval_ms = 100,
                                                      .regardless_of_activity = true,
                                                      .polite = true,
                                                      .polite_maximum_wait_ms = 60,
                                                      .morse_text = "E",
                                                      .morse_speed_wpm = 20,
                                                      .morse_frequency_hz = 800},
                                         .audio = prepared,
                                         .samples = sizeof(prepared) / sizeof(*prepared)};
    struct ra_id_rule polite_rule;
    struct ra_id_state polite_state;
    struct ra_controller polite = {.ids = &polite_id,
                                   .rules = &polite_rule,
                                   .states = &polite_state,
                                   .count = 1,
                                   .rate = 8000,
                                   .full_duplex = true};
    assert(ra_controller_start(&polite, 0));
    polite.link_active = true;
    assert(ra_controller_process(&polite, false, NULL, 0, 100));
    assert(polite.playing == SIZE_MAX);
    polite.link_active = false;
    assert(ra_controller_process(&polite, false, audio, 1, 110));
    assert(polite.playing == 0);
    assert(ra_controller_start(&polite, 0));
    polite.link_active = true;
    assert(ra_controller_process(&polite, false, NULL, 0, 100));
    assert(ra_controller_process(&polite, false, audio, 1, 159));
    assert(polite.playing == SIZE_MAX);
    assert(ra_controller_process(&polite, false, audio, 1, 160));
    assert(polite.playing == 0);
    polite.link_active = false;
    assert(ra_controller_start(&polite, 0));
    int16_t polite_status[] = {100};
    assert(ra_controller_queue_status(&polite, "E", polite_status, 1));
    assert(ra_controller_process(&polite, false, audio, 1, 250));
    assert(polite.status_playing && polite.playing == SIZE_MAX);
    assert(ra_controller_process(&polite, false, audio, 1, 251));
    assert(!polite.status_playing && polite.playing == SIZE_MAX);
    assert(ra_controller_process(&polite, false, audio, 1, 252));
    assert(polite.playing == 0);

    /* First-key polite IDs measure their bounded wait from the qualifying key. */
    struct ra_controller_id welcome_polite_id = {.settings = {.interval_ms = 100,
                                                              .first_key_only = true,
                                                              .polite = true,
                                                              .polite_maximum_wait_ms = 60,
                                                              .morse_text = "E",
                                                              .morse_speed_wpm = 20,
                                                              .morse_frequency_hz = 800},
                                                 .audio = prepared,
                                                 .samples = sizeof(prepared) / sizeof(*prepared)};
    struct ra_id_rule welcome_polite_rule;
    struct ra_id_state welcome_polite_state;
    struct ra_controller welcome_polite = {.ids = &welcome_polite_id,
                                           .rules = &welcome_polite_rule,
                                           .states = &welcome_polite_state,
                                           .count = 1,
                                           .rate = 8000,
                                           .full_duplex = true};
    assert(ra_controller_start(&welcome_polite, 0));
    assert(ra_controller_process(&welcome_polite, true, NULL, 0, 100));
    assert(welcome_polite_state.first_key_pending && welcome_polite.playing == SIZE_MAX);
    assert(ra_controller_process(&welcome_polite, true, audio, 1, 159));
    assert(welcome_polite.playing == SIZE_MAX);
    assert(ra_controller_process(&welcome_polite, true, audio, 1, 160));
    assert(welcome_polite.playing == 0);

    /* A finished ID uses a short natural PTT tail instead of the configured audio hang. */
    struct ra_controller_id tail_id = {.settings = {.interval_ms = 100,
                                                    .regardless_of_activity = true,
                                                    .morse_text = "E",
                                                    .morse_speed_wpm = 20,
                                                    .morse_frequency_hz = 800},
                                       .audio = prepared,
                                       .samples = 1};
    struct ra_id_rule tail_rule;
    struct ra_id_state tail_state;
    struct ra_controller tail = {.ids = &tail_id,
                                 .rules = &tail_rule,
                                 .states = &tail_state,
                                 .count = 1,
                                 .rate = 8000,
                                 .full_duplex = true,
                                 .hang_ms = 1000};
    assert(ra_controller_start(&tail, 0));
    assert(ra_controller_process(&tail, false, audio, 1, 100));
    assert(tail.duplex.release_hang_ms == 50);
    assert(ra_controller_process(&tail, false, NULL, 0, 149));
    assert(!ra_controller_process(&tail, false, NULL, 0, 150));

    /* An every-release announcement follows ordinary hang once and never self-schedules. */
    const int16_t every_release_audio[] = {321};
    struct ra_controller_announcement every_release_media[] = {
        {.media = {.settings = {.morse_text = "E",
                                .morse_speed_wpm = 20,
                                .morse_frequency_hz = 800},
                   .audio = every_release_audio,
                   .samples = 1},
         .interval_ms = 0}};
    struct ra_controller_announcement_state every_release_state[1];
    struct ra_controller every_release = {.announcements = every_release_media,
                                          .announcement_states = every_release_state,
                                          .announcement_count = 1,
                                          .rate = 8000,
                                          .full_duplex = true,
                                          .hang_ms = 10};
    assert(ra_controller_start(&every_release, 0));
    assert(!ra_controller_process(&every_release, false, audio, 1, 9));
    assert(!every_release_state[0].release_pending);
    assert(ra_controller_process(&every_release, true, audio, 1, 10));
    assert(every_release_state[0].release_pending);
    assert(ra_controller_process(&every_release, false, audio, 1, 19));
    assert(every_release.announcement_playing == SIZE_MAX && audio[0] == 0);
    assert(ra_controller_process(&every_release, false, audio, 1, 20));
    assert(every_release.announcement_playing == SIZE_MAX && audio[0] > 0 &&
           audio[0] <= every_release_audio[0]);
    assert(!every_release_state[0].release_pending);
    assert(ra_controller_process(&every_release, false, NULL, 0, 69));
    assert(!ra_controller_process(&every_release, false, NULL, 0, 70));
    assert(!ra_controller_process(&every_release, false, audio, 1, 80));
    assert(!audio[0]);
    /* A zero interval only follows an actual keyed release; it never keys from idle. */
    every_release_state[0].release_pending = true;
    assert(!ra_controller_process(&every_release, false, NULL, 0, 81));
    assert(every_release_state[0].release_pending && !every_release.announcement_release_pending);
    every_release_state[0].release_pending = false;

    /* Half-duplex receive cannot arm a release-only set or conceal a later periodic set. */
    const int16_t half_duplex_periodic_audio[] = {654};
    struct ra_controller_announcement mixed_announcement_media[] = {
        {.media = {.settings = {.morse_text = "E",
                                .morse_speed_wpm = 20,
                                .morse_frequency_hz = 800},
                   .audio = every_release_audio,
                   .samples = 1},
         .interval_ms = 0},
        {.media = {.settings = {.morse_text = "E",
                                .morse_speed_wpm = 20,
                                .morse_frequency_hz = 800},
                   .audio = half_duplex_periodic_audio,
                   .samples = 1},
         .interval_ms = 100}};
    struct ra_controller_announcement_state mixed_announcement_state[2];
    struct ra_controller mixed_announcement = {
        .announcements = mixed_announcement_media,
        .announcement_states = mixed_announcement_state,
        .announcement_count = 2,
        .rate = 8000,
        .full_duplex = false,
    };
    assert(ra_controller_start(&mixed_announcement, 0));
    assert(!ra_controller_process(&mixed_announcement, true, audio, 1, 10));
    assert(!mixed_announcement_state[0].release_pending);
    assert(ra_controller_process(&mixed_announcement, false, audio, 1, 100));
    assert(audio[0] == half_duplex_periodic_audio[0]);

    /* A tail waits for ordinary hang, then follows a pending polite ID without another hang. */
    const int16_t tail_order_id_audio[] = {111};
    const int16_t tail_order_announcement_audio[] = {222};
    struct ra_controller_id tail_order_id[] = {{.settings = {.interval_ms = 1,
                                                             .regardless_of_activity = true,
                                                             .polite = true,
                                                             .polite_maximum_wait_ms = 1000,
                                                             .morse_text = "E",
                                                             .morse_speed_wpm = 20,
                                                             .morse_frequency_hz = 800},
                                                .audio = tail_order_id_audio,
                                                .samples = 1}};
    struct ra_id_rule tail_order_rule[1];
    struct ra_id_state tail_order_id_state[1];
    struct ra_controller_announcement tail_order_announcement[] = {
        {.media = {.settings = {.morse_text = "E",
                                .morse_speed_wpm = 20,
                                .morse_frequency_hz = 800},
                   .audio = tail_order_announcement_audio,
                   .samples = 1},
         .interval_ms = 0}};
    struct ra_controller_announcement_state tail_order_announcement_state[1];
    struct ra_controller tail_order = {.ids = tail_order_id,
                                       .rules = tail_order_rule,
                                       .states = tail_order_id_state,
                                       .count = 1,
                                       .announcements = tail_order_announcement,
                                       .announcement_states = tail_order_announcement_state,
                                       .announcement_count = 1,
                                       .rate = 8000,
                                       .full_duplex = true,
                                       .hang_ms = 100};
    assert(ra_controller_start(&tail_order, 0));
    assert(ra_controller_process(&tail_order, true, audio, 1, 1));
    assert(tail_order_announcement_state[0].release_pending);
    assert(ra_controller_process(&tail_order, false, audio, 1, 2));
    assert(audio[0] == 0 && tail_order.playing == SIZE_MAX &&
           tail_order.announcement_playing == SIZE_MAX);
    assert(ra_controller_process(&tail_order, false, audio, 1, 100));
    assert(audio[0] == 0 && tail_order.playing == SIZE_MAX);
    assert(ra_controller_process(&tail_order, false, audio, 1, 101));
    assert(audio[0] == tail_order_id_audio[0] && tail_order.playing == SIZE_MAX);
    assert(ra_controller_process(&tail_order, false, audio, 1, 102));
    assert(audio[0] == tail_order_announcement_audio[0] &&
           tail_order.announcement_playing == SIZE_MAX);
    assert(ra_controller_process(&tail_order, false, NULL, 0, 151));
    assert(!ra_controller_process(&tail_order, false, NULL, 0, 152));

    /* A periodic idle announcement keys, gives a due ID priority, then plays in set order. */
    const int16_t announcement_id_audio[] = {111};
    const int16_t periodic_first_audio[] = {222};
    const int16_t periodic_second_audio[] = {333};
    struct ra_controller_id announcement_id[] = {{.settings = {.interval_ms = 100,
                                                               .regardless_of_activity = true,
                                                               .morse_text = "E",
                                                               .morse_speed_wpm = 20,
                                                               .morse_frequency_hz = 800},
                                                  .audio = announcement_id_audio,
                                                  .samples = 1}};
    struct ra_id_rule announcement_rules[1];
    struct ra_id_state announcement_id_state[1];
    struct ra_controller_announcement periodic_media[] = {
        {.media = {.settings = {.morse_text = "E",
                                .morse_speed_wpm = 20,
                                .morse_frequency_hz = 800},
                   .audio = periodic_first_audio,
                   .samples = 1},
         .interval_ms = 100},
        {.media = {.settings = {.morse_text = "E",
                                .morse_speed_wpm = 20,
                                .morse_frequency_hz = 800},
                   .audio = periodic_second_audio,
                   .samples = 1},
         .interval_ms = 100}};
    struct ra_controller_announcement_state periodic_states[2];
    struct ra_controller periodic = {.ids = announcement_id,
                                     .rules = announcement_rules,
                                     .states = announcement_id_state,
                                     .count = 1,
                                     .announcements = periodic_media,
                                     .announcement_states = periodic_states,
                                     .announcement_count = 2,
                                     .rate = 8000,
                                     .full_duplex = true};
    assert(ra_controller_start(&periodic, 0));
    assert(ra_controller_process(&periodic, false, audio, 1, 100));
    assert(audio[0] == announcement_id_audio[0] && periodic.announcement_playing == SIZE_MAX);
    assert(ra_controller_process(&periodic, false, audio, 1, 101));
    assert(audio[0] == periodic_first_audio[0] && periodic.announcement_playing == SIZE_MAX);
    assert(periodic_states[0].satisfied_ms == 101 && periodic_states[1].satisfied_ms == 0);
    assert(ra_controller_process(&periodic, false, audio, 1, 102));
    assert(audio[0] == periodic_second_audio[0] && periodic.announcement_playing == SIZE_MAX);
    assert(periodic_states[1].satisfied_ms == 102);

    /* A due ID retains priority over a due announcement until all of its PCM has played. */
    const int16_t queued_id_audio[] = {111, 111};
    struct ra_controller_id queued_id[] = {{.settings = {.interval_ms = 100,
                                                         .regardless_of_activity = true,
                                                         .morse_text = "E",
                                                         .morse_speed_wpm = 20,
                                                         .morse_frequency_hz = 800},
                                            .audio = queued_id_audio,
                                            .samples = 2}};
    struct ra_id_rule queued_rules[1];
    struct ra_id_state queued_id_state[1];
    struct ra_controller_announcement queued_media[] = {
        {.media = {.settings = {.morse_text = "E",
                                .morse_speed_wpm = 20,
                                .morse_frequency_hz = 800},
                   .audio = periodic_first_audio,
                   .samples = 1},
         .interval_ms = 100}};
    struct ra_controller_announcement_state queued_announcement_state[1];
    struct ra_controller queued = {.ids = queued_id,
                                   .rules = queued_rules,
                                   .states = queued_id_state,
                                   .count = 1,
                                   .announcements = queued_media,
                                   .announcement_states = queued_announcement_state,
                                   .announcement_count = 1,
                                   .rate = 8000,
                                   .full_duplex = true};
    assert(ra_controller_start(&queued, 0));
    assert(ra_controller_process(&queued, false, audio, 1, 100));
    assert(audio[0] == queued_id_audio[0] && queued.playing == 0);
    assert(ra_controller_process(&queued, false, audio, 1, 101));
    assert(audio[0] == queued_id_audio[1] && queued.playing == SIZE_MAX &&
           queued.announcement_playing == SIZE_MAX && queued.announcement_release_pending);
    assert(ra_controller_process(&queued, false, audio, 1, 102));
    assert(audio[0] == periodic_first_audio[0] && queued.announcement_playing == SIZE_MAX);

    /* A status that becomes due while an ID finishes remains pending for the next block. */
    const int16_t status_blocking_id_audio[] = {111, 222};
    struct ra_controller_id status_blocking_id[] = {{.settings = {.interval_ms = 3,
                                                                  .regardless_of_activity = true,
                                                                  .morse_text = "E",
                                                                  .morse_speed_wpm = 20,
                                                                  .morse_frequency_hz = 800},
                                                     .audio = status_blocking_id_audio,
                                                     .samples = 2}};
    struct ra_id_rule status_blocking_rules[1];
    struct ra_id_state status_blocking_states[1];
    struct ra_controller status_blocking = {.ids = status_blocking_id,
                                            .rules = status_blocking_rules,
                                            .states = status_blocking_states,
                                            .count = 1,
                                            .rate = 8000,
                                            .full_duplex = true};
    assert(ra_controller_start(&status_blocking, 0));
    assert(ra_controller_process(&status_blocking, true, audio, 1, 1));
    assert(!ra_controller_process(&status_blocking, false, audio, 1, 2));
    assert(ra_controller_process(&status_blocking, false, audio, 1, 3));
    assert(status_blocking.playing == 0 && audio[0] == status_blocking_id_audio[0]);
    assert(ra_controller_queue_status(&status_blocking, "E", NULL, 0));
    assert(status_blocking.receiver_unkey_ms == 2 &&
           atomic_load(&status_blocking.status_write) == 1 &&
           atomic_load(&status_blocking.status_read) == 0);
    assert(ra_controller_process(&status_blocking, false, NULL, 0, 252));
    assert(status_blocking.playing == 0 && !status_blocking.status_playing &&
           atomic_load(&status_blocking.status_read) == 0);
    assert(ra_controller_process(&status_blocking, false, audio, 1, 253));
    assert(status_blocking.playing == SIZE_MAX && !status_blocking.status_playing);
    assert(ra_controller_process(&status_blocking, false, audio, 1, 254));
    assert(status_blocking.status_playing);

    /* A due periodic announcement waits through active receive and ordinary transmitter hang. */
    struct ra_controller_announcement delayed_media[] = {
        {.media = {.settings = {.morse_text = "E",
                                .morse_speed_wpm = 20,
                                .morse_frequency_hz = 800},
                   .audio = periodic_first_audio,
                   .samples = 1},
         .interval_ms = 100}};
    struct ra_controller_announcement_state delayed_state[1];
    struct ra_controller delayed = {.announcements = delayed_media,
                                    .announcement_states = delayed_state,
                                    .announcement_count = 1,
                                    .rate = 8000,
                                    .full_duplex = true,
                                    .hang_ms = 10};
    assert(ra_controller_start(&delayed, 0));
    assert(ra_controller_process(&delayed, true, audio, 1, 100));
    assert(delayed.announcement_playing == SIZE_MAX);
    assert(ra_controller_process(&delayed, false, audio, 1, 109));
    assert(delayed.announcement_playing == SIZE_MAX && audio[0] == 0);
    assert(ra_controller_process(&delayed, false, audio, 1, 110));
    assert(delayed.announcement_playing == SIZE_MAX && audio[0] > 0 &&
           audio[0] <= periodic_first_audio[0]);

    /* Receive activity ducks an active announcement without interrupting or restarting it. */
    const int16_t ducked_announcement_audio[] = {10000, 10000, 10000};
    struct ra_controller_announcement ducked_announcement[] = {
        {.media = {.settings = {.morse_text = "E",
                                .morse_speed_wpm = 20,
                                .morse_frequency_hz = 800},
                   .audio = ducked_announcement_audio,
                   .samples = 3},
         .interval_ms = 100}};
    struct ra_controller_announcement_state ducked_announcement_state[1];
    struct ra_controller ducked_announcement_controller = {.announcements = ducked_announcement,
                                                           .announcement_states =
                                                               ducked_announcement_state,
                                                           .announcement_count = 1,
                                                           .rate = 8000,
                                                           .full_duplex = true,
                                                           .telemetry_duck_db = -20};
    assert(ra_controller_start(&ducked_announcement_controller, 0));
    assert(ra_controller_process(&ducked_announcement_controller, false, audio, 1, 100));
    assert(ducked_announcement_controller.announcement_playing == 0 && audio[0] == 10000);
    assert(ra_controller_process(&ducked_announcement_controller, false, audio, 1, 101));
    assert(ducked_announcement_controller.announcement_playing == 0 && audio[0] == 10000);
    audio[0] = 0;
    assert(ra_controller_process(&ducked_announcement_controller, true, audio, 1, 102));
    assert(ducked_announcement_controller.announcement_playing == SIZE_MAX && audio[0] > 0 &&
           audio[0] < ducked_announcement_audio[2]);

    /* Nonempty announcement schedules require both immutable media and writable state arrays. */
    struct ra_controller invalid_announcement = {.announcement_count = 1, .rate = 8000};
    assert(!ra_controller_start(&invalid_announcement, 0));
    struct ra_controller_announcement invalid_announcement_media[] = {
        {.media = {.settings = {.morse_text = "E",
                                .morse_speed_wpm = 20,
                                .morse_frequency_hz = 4000,
                                .morse_level_db = -6}},
         .interval_ms = 1}};
    struct ra_controller_announcement_state invalid_announcement_state[1];
    invalid_announcement = (struct ra_controller){.announcements = invalid_announcement_media,
                                                  .announcement_states = invalid_announcement_state,
                                                  .announcement_count = 1,
                                                  .rate = 8000};
    assert(!ra_controller_start(&invalid_announcement, 0));
    invalid_announcement = (struct ra_controller){
        .announcements = invalid_announcement_media, .announcement_count = 1, .rate = 8000};
    assert(!ra_controller_start(&invalid_announcement, 0));

    /* The watchdog releases PTT immediately, then requires both source clear and lockout expiry. */
    struct ra_controller watchdog = {.rate = 8000,
                                     .full_duplex = true,
                                     .transmit_timeout_ms = 100,
                                     .timeout_lockout_ms = 30,
                                     .link_active = true};
    assert(ra_controller_start(&watchdog, 0));
    assert(ra_controller_process(&watchdog, false, audio, 1, 1));
    assert(!ra_controller_process(&watchdog, false, audio, 1, 101));
    assert(watchdog.timeout_wait_unkey && watchdog.timeout_until_ms == 131);
    watchdog.link_active = false;
    assert(!ra_controller_process(&watchdog, false, audio, 1, 132));
    watchdog.link_active = true;
    assert(ra_controller_process(&watchdog, false, audio, 1, 133));

    /* A source-specific kerchunk never queues the link courtesy tone. */
    struct ra_controller kerchunk = {
        .link_courtesy = &link_link_courtesy, .rate = 8000, .kerchunk_max_ms = 500};
    assert(ra_controller_start(&kerchunk, 0));
    ra_controller_link_unkeyed_kerchunk(&kerchunk, "link", false, true, 2);
    assert(!kerchunk.courtesy_pending_count && kerchunk.suppress_release);

    /* Local kerchunks suppress release handling, while a longer carrier is ordinary traffic. */
    struct ra_controller local_kerchunk = {
        .rate = 8000, .full_duplex = true, .kerchunk_max_ms = 10};
    assert(ra_controller_start(&local_kerchunk, 0));
    assert(ra_controller_process(&local_kerchunk, true, NULL, 0, 1));
    assert(!ra_controller_process(&local_kerchunk, false, NULL, 0, 2));
    assert(local_kerchunk.suppress_release);
    assert(ra_controller_start(&local_kerchunk, 0));
    assert(ra_controller_process(&local_kerchunk, true, NULL, 0, 10));
    assert(!ra_controller_process(&local_kerchunk, false, NULL, 0, 21));
    assert(!local_kerchunk.suppress_release);

    /* The watchdog stays locked while its source remains active or its timer has not expired. */
    struct ra_controller active_timeout = {.rate = 8000,
                                           .full_duplex = true,
                                           .transmit_timeout_ms = 100,
                                           .timeout_lockout_ms = 30,
                                           .link_active = true};
    assert(ra_controller_start(&active_timeout, 0));
    assert(ra_controller_process(&active_timeout, false, audio, 1, 1));
    assert(ra_controller_process(&active_timeout, false, audio, 1, 2));
    assert(!ra_controller_process(&active_timeout, false, audio, 1, 101));
    assert(active_timeout.timeout_wait_unkey);
    assert(!ra_controller_process(&active_timeout, false, audio, 1, 102));
    assert(active_timeout.timeout_wait_unkey && !active_timeout.duplex.keyed);
    active_timeout.link_active = false;
    assert(!ra_controller_process(&active_timeout, false, audio, 1, 110));
    assert(active_timeout.timeout_wait_unkey && !active_timeout.duplex.keyed);

    /* A suppressed short transmission cannot arm an every-release announcement. */
    struct ra_controller suppressed_release = {.rate = 8000, .full_duplex = true};
    assert(ra_controller_start(&suppressed_release, 0));
    suppressed_release.link_active = true;
    suppressed_release.link_was_active = true;
    suppressed_release.suppress_release = true;
    assert(ra_controller_process(&suppressed_release, false, audio, 1, 1));
    puts("node controller audio and identification sequences passed");
    return 0;
}
