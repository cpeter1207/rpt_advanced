/* SPDX-License-Identifier: GPL-2.0-only */
/* Real shared-object trace: reorder opaque PCM fixtures, retain a lost interval,
 * then compare native output with a directly ordered reference ring. Codec
 * decoding is separately exercised by the standalone loopback-IAX tests. */
#include "rptadv_jitter_adapter.h"
#include <assert.h>
#include <rate_adjusting_pcm_ring3/rate_adjusting_pcm_ring3.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>

static const struct rpcr3_descriptor *ring_api;

static void publish(struct rpcr3_ring *ring, const float *pcm, uint64_t count) {
    uint64_t accepted = 0;
    int status = pcm ? ring_api->ring_producer_push(ring, pcm, count, &accepted)
                     : ring_api->ring_producer_gap(ring, count, &accepted);
    assert(status == RPCR3_OK && accepted == count);
}

int main(void) {
    const rptadv_jitter_descriptor_v1 *jitter = rptadv_jitter_adapter_descriptor_v1();
    const rptadv_jitter_config_v1 timing = {sizeof(timing), 1,  8000, 320, 480,
                                            4000,           80, 80,   16,  640};
    const struct rpcr3_config buffering = {.struct_size = sizeof(buffering),
                                           .abi_version = RPCR3_ABI_VERSION,
                                           .capacity_samples = 6176,
                                           .input_rate_hz = 8000,
                                           .output_rate_hz = 48000,
                                           .reserve_samples = 685,
                                           .target_samples = 2080,
                                           .max_producer_samples = 4096,
                                           .max_output_samples = 4096,
                                           .plc_mode = RPCR3_PLC_G711_APPENDIX_I};
    ring_api = rpcr3_descriptor();
    assert(ring_api->struct_size >= RPCR3_DESCRIPTOR_GAP_MIN_SIZE);
    void *context = NULL;
    struct rpcr3_ring *actual = NULL, *expected = NULL;
    assert(jitter->create(&timing, &context) == 0);
    assert(ring_api->ring_create(&buffering, &actual) == RPCR3_OK);
    assert(ring_api->ring_create(&buffering, &expected) == RPCR3_OK);

    float packets[8][160];
    for (size_t packet = 0; packet < 8; ++packet) {
        for (size_t sample = 0; sample < 160; ++sample) {
            packets[packet][sample] = packet < 2 ? 0.25F : -0.25F;
        }
        publish(expected, packet == 2 ? NULL : packets[packet], 160);
    }
    /* Packet 2 is absent; 3 arrives before 1, well before its deadline. */
    const unsigned arrival_order[] = {0, 3, 1, 4, 5, 6, 7};
    for (size_t position = 0; position < 7; ++position) {
        unsigned packet = arrival_order[position];
        const rptadv_jitter_packet_v1 input = {
            .struct_size = sizeof(input),
            .timestamp = packet * 160,
            .span = 160,
            .arrival_ns = position * 10000000ULL,
            .payload = (const uint8_t *)packets[packet],
            .length = sizeof(packets[packet]),
        };
        assert(jitter->put(context, &input) == 0);
    }
    uint64_t cursor = 0;
    while (cursor < 1280) {
        float payload[160];
        rptadv_jitter_result_v1 result = {.struct_size = sizeof(result)};
        assert(jitter->get(context, 200000000ULL, 160, (uint8_t *)payload, sizeof(payload),
                           &result) == 0);
        assert(result.kind != RPTADV_JITTER_NOT_DUE);
        assert(result.start_offset >= 0);
        if (result.start_offset) {
            publish(actual, NULL, (uint64_t)result.start_offset);
            cursor += (uint64_t)result.start_offset;
        }
        if (result.kind == RPTADV_JITTER_PACKET) {
            assert(result.timestamp == cursor && result.length == sizeof(payload));
            publish(actual, payload, result.span);
        } else {
            assert(result.kind == RPTADV_JITTER_MISSING);
            publish(actual, NULL, result.span);
        }
        cursor += result.span;
        assert(jitter->advance(context, 200000000ULL, 0) == 0);
    }
    assert(cursor == 1280);
    /* Comparing both rings also checks PLC position through FIR-held input. */
    for (unsigned block = 0; block < 7; ++block) {
        float output[960], reference[960];
        uint64_t produced = 0, reference_produced = 0;
        assert(ring_api->ring_consumer_render(actual, output, 960, &produced) == RPCR3_OK);
        assert(ring_api->ring_consumer_render(expected, reference, 960, &reference_produced) ==
               RPCR3_OK);
        assert(produced == reference_produced);
        assert(memcmp(output, reference, sizeof(output)) == 0);
    }
    struct rpcr3_observation observation = {.struct_size = sizeof(observation)};
    assert(ring_api->ring_observe(actual, &observation) == RPCR3_OK);
    assert(observation.missing_samples >= 950 && observation.missing_samples <= 970);
    jitter->destroy(context);
    ring_api->ring_destroy(actual);
    ring_api->ring_destroy(expected);
    puts("Jitter reordering and ordered-gap PLC shared-object trace passed");
    return 0;
}
