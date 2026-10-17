/** @file
 * @brief Hardware-free composition of the installed radio and GPIO DSOs.
 *
 * Only normalized device I/O is simulated. Radio timing, DSP, qualification,
 * output admission and epoch handling come from the real providers.
 */
#include <assert.h>
#include <math.h>
#include <stddef.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>

#include <rptadv_gpio_adapter/rptadv_gpio_adapter.h>
#include <rptadvradio/rptadvradio.h>

enum { PORTS = 7, MAX_FRAMES = 960, MUTES = 12, HEALTH = 31 };
static const struct rptadv_gpio_adapter_descriptor *gpio;
static const struct rptadv_radio_descriptor *radio;

/** Independent hardware gate: software cannot override a tripped interlock. */
struct hardware {
    uint32_t health, arm_epoch, feedback, inputs;
    uint32_t outputs[PORTS];
};

/** Report fresh inputs; deliberately provide no output readback. */
static int32_t sample(void *opaque, uint64_t now, struct rptadv_gpio_safety_input *safety,
                      struct rptadv_gpio_port_input *ports, size_t count) {
    const struct hardware *hw = opaque;
    *safety = (struct rptadv_gpio_safety_input){sizeof(*safety), now,          1,
                                                hw->health,      hw->feedback, hw->arm_epoch};
    for (size_t i = 0; i < count; ++i)
        ports[i] = (struct rptadv_gpio_port_input){sizeof(*ports), 1, hw->inputs, 3, 0, 0};
    return 0;
}

/** Request does not imply physical feedback. */
static int32_t arm(void *opaque, uint32_t epoch) {
    ((struct hardware *)opaque)->arm_epoch = epoch;
    return 0;
}

/** Gate every request through independently simulated physical health. */
static int32_t apply(void *opaque, const struct rptadv_gpio_port_output *ports, size_t count) {
    struct hardware *hw = opaque;
    for (size_t i = 0; i < count; ++i)
        hw->outputs[i] = hw->health == HEALTH ? ports[i].outputs : MUTES;
    return 0;
}

/** Release all covered actuators, including when the software service stalls. */
static int32_t safe(void *opaque) {
    struct hardware *hw = opaque;
    hw->feedback = 0;
    for (size_t i = 0; i < PORTS; ++i)
        hw->outputs[i] = MUTES;
    return 0;
}

/** Seven is a test size, not a provider limit. */
static struct rptadv_gpio_context *context(struct hardware *hw, uint32_t appliance) {
    struct rptadv_gpio_port_config ports[PORTS];
    for (size_t i = 0; i < PORTS; ++i)
        ports[i] = (struct rptadv_gpio_port_config){
            .struct_size = sizeof(ports[i]),
            .port_id = 100 + i,
            .capabilities = {sizeof(ports[i].capabilities), 3, 31, 0, 0, 0, appliance ? 31 : 0},
            .required_inputs = 3,
            .required_outputs = 3};
    const struct rptadv_gpio_context_config config = {sizeof(config), appliance, ports,
                                                      PORTS,          100,       10};
    const struct rptadv_gpio_transport transport = {
        sizeof(transport), hw, sample, arm, apply, safe};
    struct rptadv_gpio_context *result = NULL;
    assert(gpio->context_create(&config, &transport, &result) == 0);
    return result;
}

/** Snapshot carries the epoch required by every prepared action. */
static struct rptadv_gpio_safety_snapshot state(struct rptadv_gpio_context *ctx) {
    struct rptadv_gpio_safety_snapshot result = {.struct_size = sizeof(result)};
    assert(gpio->context_snapshot(ctx, &result) == 0);
    return result;
}

/** Borrow an endpoint whose lifetime is bounded by its hardware context. */
static const struct rptadv_gpio_port *port(struct rptadv_gpio_context *ctx, size_t index) {
    const struct rptadv_gpio_port *result = NULL;
    assert(gpio->context_get_port(ctx, 100 + index, &result) == 0);
    return result;
}

/** Use the existing native generator/decoder, never a fixture DSP substitute. */
static struct rptadv_radio_session *session(uint32_t encode, uint32_t decode, int dcs) {
    struct rptadv_radio_session_config config = {
        .struct_size = sizeof(config),
        .abi_version = RPTADV_RADIO_ABI_VERSION,
        .native_sample_rate_hz = 48000,
        .interleaved_channels = 2,
        .maximum_receive_frame_count = MAX_FRAMES,
        .maximum_transmit_frame_count = MAX_FRAMES,
        .publication_interval_ms = 50,
        .receive_input_gain = 1.0f,
        .receive = {.ctcss_decoder_gain = 1.0f,
                    .ctcss_enabled = decode == 1,
                    .ctcss_tone_mask = decode == 1,
                    .dcs_enabled = dcs,
                    .dcs_code = dcs ? 23 : 0},
        .qualification = {.radio_duplex = 1},
        .transmit = {.ctcss_transmit_enabled = encode == 1,
                     .default_ctcss_frequency_tenths_hz = encode == 1 ? 1148 : 0,
                     .ctcss_peak = encode == 1 ? 0.1f : 0,
                     .dcs_transmit_enabled = dcs,
                     .dcs_code = dcs ? 23 : 0,
                     .dcs_peak = dcs ? 0.1f : 0,
                     .output_a_route = 2,
                     .output_a_tone_gain = 1.0f}};
    const struct rptadv_radio_session_ports ports = {.struct_size = sizeof(ports)};
    const struct rptadv_radio_profile profile = {sizeof(profile), 1, 0, encode, decode};
    /* Unused external facilities are genuinely absent. */
    const struct rptadv_radio_profile_capabilities caps = {sizeof(caps), 1, decode == 2,
                                                           encode == 2, encode == 1 || dcs};
    struct rptadv_radio_session *result = NULL;
    assert(radio->session_create_profiled(&config, &ports, &profile, &caps, &result) == 0);
    assert(radio->session_warm(result) == 0);
    return result;
}

/** Deliver same-call radio intent to the GPIO publisher without device I/O. */
static void transmit(struct rptadv_radio_session *session, const struct rptadv_gpio_port *endpoint,
                     uint32_t epoch, uint32_t frames, uint32_t encode, uint32_t inhibit, int dcs) {
    float pcm[MAX_FRAMES * 2];
    for (size_t i = 0; i < MAX_FRAMES * 2; ++i)
        pcm[i] = 9.0f;
    const struct rptadv_radio_transmit_input input = {.external_ptt_request = 1,
                                                      .physical_ptt_applied = 1,
                                                      .render_admitted = 1,
                                                      .ctcss_inhibit = inhibit};
    struct rptadv_radio_profile_transmit_result result = {.struct_size = sizeof(result)};
    assert(radio->session_transmit_profiled(session, pcm, frames, &input, &result) == 0);
    assert(result.result.frame_count == frames);
    assert(result.outputs.logical_ptt == 1);
    assert(result.outputs.external_ctcss_enable == (uint32_t)(encode == 2 && !inhibit));
    if (frames == MAX_FRAMES && ((encode == 1 && !inhibit) || dcs))
        assert(result.result.output_peak > 0.01f);
    if (encode != 1 && !dcs)
        for (size_t i = 0; i < frames * 2; ++i)
            assert(pcm[i] == 0.0f);
    if (frames < MAX_FRAMES)
        assert(pcm[frames * 2] == 9.0f);
    const struct rptadv_gpio_output_intent action = {
        sizeof(action), epoch,
        result.outputs.logical_ptt | (result.outputs.external_ctcss_enable << 1)};
    assert(gpio->port_publish(endpoint, &action) == 0);
}

/** External decode is independent of DSP encode and closes immediately on loss. */
static void receive(struct rptadv_radio_session *session, const struct rptadv_gpio_port *endpoint,
                    uint32_t expected, uint32_t frames) {
    float input[MAX_FRAMES * 2] = {0}, output[MAX_FRAMES];
    struct rptadv_gpio_port_snapshot snapshot = {.struct_size = sizeof(snapshot)};
    assert(gpio->port_snapshot(endpoint, &snapshot) == 0);
    const struct rptadv_radio_profile_receive_input controls = {
        .struct_size = sizeof(controls),
        .hardware_cor = snapshot.inputs & 1,
        .hardware_cor_valid = snapshot.valid_inputs & 1,
        .external_ctcss = (snapshot.inputs >> 1) & 1,
        .external_ctcss_valid = (snapshot.valid_inputs >> 1) & 1};
    struct rptadv_radio_profile_receive_result result = {.struct_size = sizeof(result)};
    assert(radio->session_receive_profiled(session, input, output, frames, &controls, &result) ==
           0);
    assert(result.result.frame_count == frames);
    assert(result.result.receiver_keyed == expected);
}

/** No descriptor-tail absence may silently skip a required contract test. */
int main(void) {
    gpio = rptadv_gpio_adapter_descriptor();
    radio = rptadv_radio_descriptor();
    assert(gpio && gpio->abi_version == 1 &&
           gpio->struct_size >= offsetof(struct rptadv_gpio_adapter_descriptor, port_retire) +
                                    sizeof(gpio->port_retire));
    assert(radio && radio->abi_version == 4 &&
           radio->struct_size >=
               offsetof(struct rptadv_radio_descriptor, session_transmit_profiled) +
                   sizeof(radio->session_transmit_profiled));
    struct hardware hw = {.health = HEALTH, .inputs = 3},
                    ordinary_hw = {.health = HEALTH, .inputs = 3};
    struct rptadv_gpio_context *ctx = context(&hw, 1), *ordinary = context(&ordinary_hw, 0);
    struct rptadv_radio_session *sessions[PORTS];
    const uint32_t encode[PORTS] = {2, 1, 0, 0, 2, 1, 0};
    const uint32_t decode[PORTS] = {1, 2, 0, 0, 0, 0, 2};
    assert(gpio->context_initialize(ctx, 100) == 0);
    assert(gpio->context_initialize(ordinary, 100) == 0);
    uint32_t epoch = state(ctx).epoch;
    assert(gpio->context_request_arm(ctx, epoch, 100) != 0);
    for (size_t i = 0; i < PORTS; ++i) {
        sessions[i] = session(encode[i], decode[i], i == 3);
        assert(gpio->port_ready(port(ctx, i), epoch) == 0);
        assert(gpio->port_ready(port(ordinary, i), state(ordinary).epoch) == 0);
    }
    assert(gpio->context_request_arm(ctx, epoch, 101) == 0);
    assert(state(ctx).state == 1);
    hw.feedback = 1;
    assert(gpio->context_service(ctx, 102) == 0);
    assert(gpio->context_service(ordinary, 102) == 0);
    epoch = state(ctx).epoch;
    assert(state(ctx).state == 2 && state(ordinary).state == 2);
    assert(state(ordinary).arm_feedback == 0);
    const uint32_t partitions[] = {1, 17, 480, 960};
    for (size_t part = 0; part < sizeof(partitions) / sizeof(*partitions); ++part)
        for (size_t i = 0; i < PORTS; ++i) {
            transmit(sessions[i], port(ctx, i), epoch, partitions[part], encode[i], 0, i == 3);
            receive(sessions[i], port(ctx, i), decode[i] != 1 && i != 3, partitions[part]);
        }
    assert(gpio->context_service(ctx, 103) == 0);
    assert(hw.outputs[0] == 3 && hw.outputs[1] == 1);
    transmit(sessions[0], port(ctx, 0), epoch, 17, 2, 1, 0);
    hw.inputs = 1;
    assert(gpio->context_service(ctx, 104) == 0);
    assert(hw.outputs[0] == 1);
    receive(sessions[1], port(ctx, 1), 0, 17);
    struct rptadv_gpio_port_snapshot observed = {.struct_size = sizeof(observed)};
    assert(gpio->port_snapshot(port(ctx, 1), &observed) == 0);
    assert(observed.requested_outputs == 1 && observed.confirmed_valid == 0);

    /* A physical watchdog acts even with software service paused. */
    hw.health &= ~8U;
    safe(&hw);
    transmit(sessions[0], port(ctx, 0), epoch, 17, 2, 0, 0);
    assert(hw.outputs[0] == MUTES);
    assert(gpio->context_service(ctx, 105) != 0);
    assert(state(ctx).state == 3 && state(ordinary).state == 2);
    receive(sessions[2], port(ctx, 2), 0, 17);
    const struct rptadv_gpio_output_intent stale = {sizeof(stale), epoch, 3};
    assert(gpio->port_publish(port(ctx, 0), &stale) != 0);
    hw.health = HEALTH;
    hw.inputs = 3;
    assert(gpio->context_service(ctx, 106) != 0);
    assert(gpio->context_recover(ctx, 107) == 0);
    receive(sessions[1], port(ctx, 1), 1, 17);
    assert(state(ctx).state == 0);
    assert(gpio->context_request_arm(ctx, state(ctx).epoch, 108) != 0);
    for (size_t i = 0; i < PORTS; ++i)
        assert(gpio->port_ready(port(ctx, i), state(ctx).epoch) == 0);
    assert(gpio->context_request_arm(ctx, state(ctx).epoch, 108) == 0);
    uint32_t fresh_arm = hw.arm_epoch;
    hw.arm_epoch = epoch;
    hw.feedback = 1;
    assert(gpio->context_service(ctx, 109) == 0 && state(ctx).state == 1);
    assert(hw.arm_epoch == epoch);
    hw.arm_epoch = fresh_arm;
    assert(gpio->context_service(ctx, 110) == 0 && state(ctx).state == 2);
    for (size_t i = 0; i < PORTS; ++i)
        assert(hw.outputs[i] == MUTES);
    transmit(sessions[0], port(ctx, 0), state(ctx).epoch, 960, 2, 0, 0);
    assert(gpio->context_service(ctx, 111) == 0 && hw.outputs[0] == 3);
    assert(gpio->context_recover(ctx, 112) != 0);
    assert(gpio->context_disarm(ctx) == 0);
    for (size_t i = 0; i < PORTS; ++i) {
        assert(hw.outputs[i] == MUTES);
        radio->session_destroy(sessions[i]);
    }
    gpio->context_destroy(ctx);
    gpio->context_destroy(ordinary);
    puts("radio/interlock dynamic composition: seven ports passed");
    return 0;
}
