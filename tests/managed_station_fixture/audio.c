/* SPDX-License-Identifier: GPL-2.0-only */
/** @file
 * @brief Private foreground-test port of USBRadioPlus provider_support.rs AUDIO.
 */
#define _POSIX_C_SOURCE 200809L
#include <rptadv_portaudio_alsa_adapter/rptadv_portaudio_alsa_adapter.h>

#include <errno.h>
#include <pthread.h>
#include <stdatomic.h>
#include <stdbool.h>
#include <stddef.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

enum { FRAMES = 960, SAMPLE_COUNT = FRAMES * RPTADV_AUDIO_CANONICAL_CHANNELS };
static const char topology[] = "3-1";
static const char interface[] = "3-1:1.0";
static const char serial[] = "ABC";

/** @brief Stream owns one callback thread while the control owner retains its contexts. */
struct rptadv_audio_stream {
    struct rptadv_audio_stream_config config;
    pthread_t worker;
    atomic_bool stop;
    bool started;
    _Atomic uint64_t capture_count;
    _Atomic uint64_t playback_count;
    _Atomic uint64_t failure_count;
    FILE *phone_witness;
    FILE *progress_pcm;
};

/** @brief The SIP fixture injects 500 Hz while this station captures 1 kHz. */
static bool phone_programme(const float *samples)
{
    float in_phase = 0.0f;
    float quadrature = 0.0f;
    for (unsigned int frame = 0; frame < FRAMES; ++frame) {
        const float sample = samples[2 * frame];
        in_phase += frame % 96 < 48 ? sample : -sample;
        quadrature += (frame + 24) % 96 < 48 ? sample : -sample;
    }
    return in_phase * in_phase + quadrature * quadrature > 16.0f;
}

/** @brief Mixer handle with process-local volume and switch state. */
struct rptadv_audio_mixer {
    _Atomic uint32_t *volume;
    uint32_t enabled;
};

/** @brief Physical mixer levels persist across handle replacement in the foreground. */
static struct {
    const char *element;
    _Atomic uint32_t volume;
} mixer_levels[] = {
    {"rx", 500}, {"tx-a", 500}, {"tx-b", 500}, {"side", 500}, {"compat", 500},
};

/** @brief Run the canonical 20 ms buffers on one monotonic clock. */
static void *run_audio(void *context)
{
    struct rptadv_audio_stream *stream = context;
    const struct rptadv_audio_stream_config *config = &stream->config;
    struct timespec deadline;
    float input[SAMPLE_COUNT];
    float output[SAMPLE_COUNT] = {0};
    unsigned int phone_blocks = 0;

    /* 1 kHz, 0.25 peak, with an integral number of cycles per stereo block. */
    for (unsigned int frame = 0; frame < FRAMES; ++frame) {
        int phase = frame % 48;
        float sample = (12 - abs(phase - 24)) / 48.0f;
        input[2 * frame] = sample;
        input[2 * frame + 1] = sample;
    }
    if (clock_gettime(CLOCK_MONOTONIC, &deadline) != 0) {
        atomic_fetch_add(&stream->failure_count, 1);
        return NULL;
    }
    while (!atomic_load(&stream->stop)) {
        atomic_fetch_add(&stream->capture_count, 1);
        int receive = config->receive_worker(config->receive_worker_context, input, FRAMES);
        atomic_fetch_add(&stream->playback_count, 1);
        int transmit = config->transmit_worker(config->transmit_worker_context, output, FRAMES);
        if (receive != 0 || transmit != 0) {
            atomic_fetch_add(&stream->failure_count, 1);
            break;
        }
        if (stream->progress_pcm) {
            /* The opt-in test reads the left output channel at 8 kHz. */
            float mono[FRAMES / 6];
            for (size_t i = 0; i < FRAMES / 6; ++i) {
                mono[i] = output[12 * i];
            }
            if (fwrite(mono, sizeof(mono), 1, stream->progress_pcm) != 1
                || fflush(stream->progress_pcm) != 0) {
                atomic_fetch_add(&stream->failure_count, 1);
                break;
            }
        }
        if (stream->phone_witness && phone_blocks < 3) {
            phone_blocks = phone_programme(output) ? phone_blocks + 1 : 0;
            if (phone_blocks == 3) {
                fputs("500Hz\n", stream->phone_witness);
                fflush(stream->phone_witness);
            }
        }
        deadline.tv_nsec += 20000000;
        if (deadline.tv_nsec >= 1000000000) {
            deadline.tv_nsec -= 1000000000;
            ++deadline.tv_sec;
        }
        int result;
        do {
            result = clock_nanosleep(CLOCK_MONOTONIC, TIMER_ABSTIME, &deadline, NULL);
        } while (result == EINTR && !atomic_load(&stream->stop));
        if (result != 0 && result != EINTR) {
            atomic_fetch_add(&stream->failure_count, 1);
            break;
        }
    }
    return NULL;
}

static enum rptadv_audio_result stream_create(const struct rptadv_audio_stream_config *config,
                                              struct rptadv_audio_stream **output)
{
    if (config->struct_size < sizeof(*config)
        || config->abi_version != RPTADV_AUDIO_ADAPTER_ABI_VERSION
        || config->native_sample_rate_hz != RPTADV_AUDIO_NATIVE_SAMPLE_RATE_HZ
        || config->maximum_receive_frame_count < FRAMES || config->maximum_transmit_frame_count < FRAMES
        || !config->receive_worker || !config->transmit_worker) {
        return RPTADV_AUDIO_INVALID_ARGUMENT;
    }
    struct rptadv_audio_stream *stream = calloc(1, sizeof(*stream));
    if (!stream) {
        return RPTADV_AUDIO_NO_MEMORY;
    }
    const char *phone_witness = getenv("RPTADV_TEST_PHONE_AUDIO");
    if (phone_witness) {
        stream->phone_witness = fopen(phone_witness, "w");
        if (!stream->phone_witness) {
            free(stream);
            return RPTADV_AUDIO_INVALID_ARGUMENT;
        }
    }
    const char *capture = getenv("RPTADV_TEST_SIP_PROGRESS_PCM");
    if (capture && !(stream->progress_pcm = fopen(capture, "wb"))) {
        if (stream->phone_witness) {
            fclose(stream->phone_witness);
        }
        free(stream);
        return RPTADV_AUDIO_INVALID_ARGUMENT;
    }
    stream->config = *config;
    atomic_init(&stream->stop, false);
    atomic_init(&stream->capture_count, 0);
    atomic_init(&stream->playback_count, 0);
    atomic_init(&stream->failure_count, 0);
    *output = stream;
    return RPTADV_AUDIO_OK;
}

static enum rptadv_audio_result stream_start(struct rptadv_audio_stream *stream)
{
    if (!stream->started) {
        atomic_store(&stream->stop, false);
        if (pthread_create(&stream->worker, NULL, run_audio, stream) != 0) {
            return RPTADV_AUDIO_PORTAUDIO_ERROR;
        }
        stream->started = true;
    }
    return RPTADV_AUDIO_OK;
}

/** @brief Synchronously quiesce callbacks, including on restart or destruction. */
static enum rptadv_audio_result stream_stop(struct rptadv_audio_stream *stream)
{
    if (stream->started) {
        atomic_store(&stream->stop, true);
        if (pthread_join(stream->worker, NULL) != 0) {
            return RPTADV_AUDIO_PORTAUDIO_ERROR;
        }
        stream->started = false;
    }
    return RPTADV_AUDIO_OK;
}

static void stream_destroy(struct rptadv_audio_stream *stream)
{
    if (stream_stop(stream) != RPTADV_AUDIO_OK) {
        abort(); /* The ABI's destructor cannot return with active borrowers. */
    }
    if (stream->phone_witness) {
        fclose(stream->phone_witness);
    }
    if (stream->progress_pcm) {
        fclose(stream->progress_pcm);
    }
    free(stream);
}

static enum rptadv_audio_result stream_stats(const struct rptadv_audio_stream *stream,
                                             struct rptadv_audio_stream_stats *output)
{
    /* USBRadioPlus supplies the prefix through capture_callback_count. */
    size_t extent = output->struct_size;
    const size_t prefix = offsetof(struct rptadv_audio_stream_stats, capture_scheduling_policy);
    if (extent < prefix) {
        return RPTADV_AUDIO_INVALID_ARGUMENT;
    }
    if (extent > sizeof(*output)) {
        extent = sizeof(*output);
    }
    uint64_t playback_count = atomic_load(&stream->playback_count);
    const struct rptadv_audio_stream_stats stats = {
        .struct_size = extent,
        .abi_version = RPTADV_AUDIO_ADAPTER_ABI_VERSION,
        .callback_count = playback_count,
        .callback_frame_count = playback_count * FRAMES,
        .worker_failure_count = atomic_load(&stream->failure_count),
        .capture_callback_count = atomic_load(&stream->capture_count),
    };
    memcpy(output, &stats, extent);
    return RPTADV_AUDIO_OK;
}

static enum rptadv_audio_result stream_timing(const struct rptadv_audio_stream *stream,
                                              struct rptadv_audio_stream_timing *output)
{
    (void)stream;
    *output = (struct rptadv_audio_stream_timing){
        .struct_size = sizeof(*output),
        .abi_version = RPTADV_AUDIO_ADAPTER_ABI_VERSION,
        .input_latency_seconds = 0.01,
        .output_latency_seconds = 0.02,
        .sample_rate_hz = RPTADV_AUDIO_NATIVE_SAMPLE_RATE_HZ,
    };
    return RPTADV_AUDIO_OK;
}

static enum rptadv_audio_result unsupported_mixer(const struct rptadv_audio_mixer_config *config,
                                                  struct rptadv_audio_mixer **output)
{
    (void)config;
    (void)output;
    return RPTADV_AUDIO_UNSUPPORTED;
}

static enum rptadv_audio_result mixer_create(const struct rptadv_audio_usb_mixer_config *config,
                                             struct rptadv_audio_mixer **output)
{
    if (strcmp(config->usb_interface_path, interface) != 0) {
        return RPTADV_AUDIO_UNSUPPORTED;
    }
    for (size_t index = 0; index < sizeof(mixer_levels) / sizeof(mixer_levels[0]); ++index) {
        if (strcmp(config->element, mixer_levels[index].element) == 0) {
            struct rptadv_audio_mixer *mixer = calloc(1, sizeof(*mixer));
            if (!mixer) {
                return RPTADV_AUDIO_NO_MEMORY;
            }
            mixer->volume = &mixer_levels[index].volume;
            *output = mixer;
            return RPTADV_AUDIO_OK;
        }
    }
    return RPTADV_AUDIO_UNSUPPORTED;
}

static enum rptadv_audio_result mixer_range(const struct rptadv_audio_mixer *mixer, int64_t *minimum,
                                            int64_t *maximum)
{
    (void)mixer;
    *minimum = 0;
    *maximum = 2000;
    return RPTADV_AUDIO_OK;
}

static enum rptadv_audio_result mixer_get_steps(const struct rptadv_audio_mixer *mixer, int64_t *value)
{
    *value = (int64_t)atomic_load(mixer->volume) * 2;
    return RPTADV_AUDIO_OK;
}

static enum rptadv_audio_result mixer_set_steps(struct rptadv_audio_mixer *mixer, int64_t value)
{
    atomic_store(mixer->volume, value / 2);
    return RPTADV_AUDIO_OK;
}

static enum rptadv_audio_result mixer_get_normalized(const struct rptadv_audio_mixer *mixer,
                                                     uint32_t *value)
{
    *value = atomic_load(mixer->volume);
    return RPTADV_AUDIO_OK;
}

static enum rptadv_audio_result mixer_set_normalized(struct rptadv_audio_mixer *mixer, uint32_t value)
{
    atomic_store(mixer->volume, value);
    return RPTADV_AUDIO_OK;
}

static enum rptadv_audio_result mixer_get_switch(const struct rptadv_audio_mixer *mixer,
                                                 uint32_t *value)
{
    *value = mixer->enabled;
    return RPTADV_AUDIO_OK;
}

static enum rptadv_audio_result mixer_set_switch(struct rptadv_audio_mixer *mixer, uint32_t value)
{
    mixer->enabled = value;
    return RPTADV_AUDIO_OK;
}

static void mixer_destroy(struct rptadv_audio_mixer *mixer)
{
    free(mixer);
}

static enum rptadv_audio_result unsupported_resolve(
    const struct rptadv_audio_usb_device_identity *identity,
    struct rptadv_audio_usb_device_selection *output)
{
    (void)identity;
    (void)output;
    return RPTADV_AUDIO_UNSUPPORTED;
}

static enum rptadv_audio_result device_select(const struct rptadv_audio_usb_device_selector *selector,
                                              struct rptadv_audio_usb_device_match *output)
{
    const char *id = selector->device_identifier;
    if ((id && strcmp(id, topology) != 0 && strcmp(id, interface) != 0)
        || (selector->usb_serial && strcmp(selector->usb_serial, serial) != 0)) {
        return RPTADV_AUDIO_UNSUPPORTED;
    }
    *output = (struct rptadv_audio_usb_device_match){
        .struct_size = sizeof(*output),
        .abi_version = RPTADV_AUDIO_ADAPTER_ABI_VERSION,
        .selection = {
            .struct_size = sizeof(output->selection),
            .abi_version = RPTADV_AUDIO_ADAPTER_ABI_VERSION,
            .alsa_card_index = 1,
            .input_device_index = 2,
            .output_device_index = 3,
        },
    };
    memcpy(output->usb_interface_path, interface, sizeof(interface));
    memcpy(output->usb_serial, serial, sizeof(serial));
    return RPTADV_AUDIO_OK;
}

static enum rptadv_audio_result mixer_paths(const char *path,
                                            struct rptadv_audio_cm119_mixer_paths *output)
{
    if (strcmp(path, interface) != 0) {
        return RPTADV_AUDIO_UNSUPPORTED;
    }
    const uint32_t volume_switch =
        RPTADV_AUDIO_CM119_MIXER_PATH_VOLUME | RPTADV_AUDIO_CM119_MIXER_PATH_SWITCH;
    *output = (struct rptadv_audio_cm119_mixer_paths){
        .struct_size = sizeof(*output),
        .abi_version = RPTADV_AUDIO_ADAPTER_ABI_VERSION,
        .rx_capture_path_count = 1,
        .tx_playback_path_count = 2,
        .sidetone_path_count = 1,
        .rx_compatibility_switch_path_count = 1,
        .rx_capture_paths = {
            {.element = "rx", .direction = RPTADV_AUDIO_MIXER_CAPTURE, .capabilities = volume_switch},
        },
        .tx_playback_paths = {
            {.element = "tx-a", .direction = RPTADV_AUDIO_MIXER_PLAYBACK, .capabilities = volume_switch},
            {.element = "tx-b", .direction = RPTADV_AUDIO_MIXER_PLAYBACK, .capabilities = volume_switch},
        },
        .sidetone_paths = {
            {.element = "side", .direction = RPTADV_AUDIO_MIXER_PLAYBACK, .capabilities = volume_switch},
        },
        .rx_compatibility_switch_paths = {
            {.element = "compat", .direction = RPTADV_AUDIO_MIXER_PLAYBACK,
             .capabilities = RPTADV_AUDIO_CM119_MIXER_PATH_SWITCH},
        },
    };
    return RPTADV_AUDIO_OK;
}

_Static_assert(RPTADV_AUDIO_ADAPTER_ABI_VERSION == 2, "expected audio ABI 2");
_Static_assert(sizeof(struct rptadv_audio_adapter_descriptor) == 208, "expected the output-timing tail");

static const struct rptadv_audio_adapter_descriptor AUDIO = {
    .struct_size = sizeof(AUDIO),
    .abi_version = RPTADV_AUDIO_ADAPTER_ABI_VERSION,
    .capability_name = "rptadv.portaudio-alsa-audio",
    .stream_create = stream_create,
    .stream_start = stream_start,
    .stream_stop = stream_stop,
    .stream_get_stats = stream_stats,
    .stream_destroy = stream_destroy,
    .mixer_create = unsupported_mixer,
    .mixer_get_range_centibels = mixer_range,
    .mixer_get_centibels = mixer_get_steps,
    .mixer_set_centibels = mixer_set_steps,
    .mixer_destroy = mixer_destroy,
    .mixer_create_for_usb_interface = mixer_create,
    .mixer_get_range_steps = mixer_range,
    .mixer_get_steps = mixer_get_steps,
    .mixer_set_steps = mixer_set_steps,
    .mixer_get_normalized = mixer_get_normalized,
    .mixer_set_normalized = mixer_set_normalized,
    .mixer_get_switch = mixer_get_switch,
    .mixer_set_switch = mixer_set_switch,
    .usb_device_resolve = unsupported_resolve,
    .usb_device_select = device_select,
    .stream_get_timing = stream_timing,
    .cm119_mixer_paths_resolve = mixer_paths,
};

/** @brief Immutable fixture descriptor for the loaded object's lifetime. */
__attribute__((visibility("default")))
const struct rptadv_audio_adapter_descriptor *rptadv_portaudio_alsa_adapter_descriptor(void)
{
    return &AUDIO;
}
