/* SPDX-License-Identifier: GPL-2.0-only */
/** @file
 * @brief Test-only hardware-paced Asterisk radio; never installed on a node.
 */
#include <asterisk.h>
#include <asterisk/astobj2.h>
#include <asterisk/buildopts.h>
#include <asterisk/channel.h>
#include <asterisk/format.h>
#include <asterisk/format_cache.h>
#include <asterisk/format_cap.h>
#include <asterisk/frame.h>
#include <asterisk/logger.h>
#include <asterisk/module.h>
#include <fcntl.h>
#include <pthread.h>
#include <stdatomic.h>
#include <stdbool.h>
#include <time.h>
#include <unistd.h>

/** @brief Test-device state, retained until the producer thread has joined. */
struct fixture {
    int pipe[2];               /**< Readiness events representing hardware intervals. */
    pthread_t thread;          /**< Clock producer. */
    atomic_bool stop;          /**< Stop producer before closing its descriptors. */
    bool started;              /**< Thread was created successfully. */
    bool carrier;              /**< Current synthetic receiver indication. */
    bool media;                /**< Delay reception so a prepared ID starts first. */
    unsigned int file_blocks;  /**< Recognizable prepared-file blocks before reception. */
    unsigned int morse_blocks; /**< Negative Morse samples mixed with positive receive PCM. */
    unsigned int ticks;        /**< Voice frames consumed by Asterisk. */
    unsigned int writes;       /**< Transmit blocks received. */
    unsigned int nonzero;      /**< Blocks with nonzero output. */
    unsigned int early;        /**< Nonzero output during initial local reception. */
    unsigned int keys;         /**< PTT assertions. */
    unsigned int unkeys;       /**< PTT releases. */
    struct ast_frame frame;    /**< Borrowed read result. */
    int16_t audio[AST_FRIENDLY_OFFSET / 2 + 960]; /**< Native-rate PCM and headroom. */
};
/** @brief One native signed-linear capability. */
static struct ast_format_cap *capabilities;
/** @brief Reserved channels prevent fixture unload. */
static atomic_uint active;
/** @brief Registered technology, declared before the allocator. */
static struct ast_channel_tech technology;

/** @brief Produce finite hardware-like ticks without using the controller's clock.
 * @param context Owned device.
 * @return Null after stop or the test interval.
 */
static void *clock_run(void *context) {
    struct fixture *device = context;
    const struct timespec interval = {.tv_nsec = 20000000};
    for (unsigned int tick = 0; tick < 100 && !atomic_load(&device->stop); ++tick) {
        if (write(device->pipe[1], "x", 1) != 1) {
            break;
        }
        nanosleep(&interval, NULL);
    }
    return NULL;
}

/** @brief Reserve a synthetic device through the real Asterisk channel allocator.
 * @param type Requested technology.
 * @param cap Requested formats.
 * @param ids Assigned channel identifiers.
 * @param requestor Calling channel.
 * @param data Device name.
 * @param cause Optional failure reason.
 * @return Unlocked channel or null.
 */
static struct ast_channel *request(const char *type, struct ast_format_cap *cap,
                                   const struct ast_assigned_ids *ids,
                                   const struct ast_channel *requestor, const char *data,
                                   int *cause) {
    (void)type;
    (void)cap;
    (void)cause;
    struct fixture *device = ast_calloc(1, sizeof(*device));
    if (!device) {
        return NULL;
    }
    if (pipe2(device->pipe, O_CLOEXEC)) {
        ast_free(device);
        return NULL;
    }
    atomic_init(&device->stop, false);
    device->media = !strcmp(data, "media");
    struct ast_channel *channel = ast_channel_alloc(1, AST_STATE_DOWN, NULL, NULL, "", "", "", ids,
                                                    requestor, 0, "RadioPlusAdvanced/%s", data);
    if (!channel) {
        close(device->pipe[0]);
        close(device->pipe[1]);
        ast_free(device);
        return NULL;
    }
    ast_channel_tech_set(channel, &technology);
    ast_channel_tech_pvt_set(channel, device);
    ast_channel_nativeformats_set(channel, capabilities);
    ast_channel_set_readformat(channel, ast_format_cache_get_slin_by_rate(48000));
    ast_channel_set_writeformat(channel, ast_format_cache_get_slin_by_rate(48000));
    ast_channel_set_fd(channel, 0, device->pipe[0]);
    atomic_fetch_add(&active, 1);
    ast_channel_unlock(channel);
    return channel;
}

/** @brief Start the fixture clock without a PBX dialplan.
 * @param channel Reserved channel.
 * @param destination Unused device name.
 * @param timeout Unused call timeout.
 * @return Thread creation status.
 */
static int call(struct ast_channel *channel, const char *destination, int timeout) {
    (void)destination;
    (void)timeout;
    struct fixture *device = ast_channel_tech_pvt(channel);
    int result = pthread_create(&device->thread, NULL, clock_run, device);
    device->started = !result;
    ast_setstate(channel, AST_STATE_UP);
    return result;
}

/** @brief Return carrier transitions without consuming a voice clock tick.
 * @param channel Ready test device.
 * @return Borrowed control or native voice frame.
 */
static struct ast_frame *read_frame(struct ast_channel *channel) {
    struct fixture *device = ast_channel_tech_pvt(channel);
    device->frame = (struct ast_frame){.src = "rpt-test-radio"};
    unsigned int begin = device->media ? 10 : 0;
    if ((device->ticks == begin && !device->carrier) ||
        (device->ticks == begin + 10 && device->carrier)) {
        device->carrier = !device->carrier;
        device->frame.frametype = AST_FRAME_CONTROL;
        device->frame.subclass.integer =
            device->carrier ? AST_CONTROL_RADIO_KEY : AST_CONTROL_RADIO_UNKEY;
    } else {
        char event;
        if (read(device->pipe[0], &event, 1) != 1) {
            return NULL;
        }
        ++device->ticks;
        int16_t *audio = device->audio + AST_FRIENDLY_OFFSET / 2;
        for (size_t i = 0; i < 960; ++i) {
            audio[i] = device->carrier ? 1000 : 0;
        }
        device->frame.frametype = AST_FRAME_VOICE;
        device->frame.subclass.format = ast_format_cache_get_slin_by_rate(48000);
        device->frame.offset = AST_FRIENDLY_OFFSET;
        device->frame.samples = 960;
        device->frame.datalen = 1920;
        device->frame.data.ptr = audio;
    }
    return &device->frame;
}

/** @brief Inspect actual PCM returned through Asterisk's write path.
 * @param channel Device.
 * @param frame Output frame after Asterisk conversion.
 * @return Zero, or minus one for an invalid native format.
 */
static int write_frame(struct ast_channel *channel, struct ast_frame *frame) {
    struct fixture *device = ast_channel_tech_pvt(channel);
    if (frame->frametype != AST_FRAME_VOICE || frame->samples <= 0 ||
        frame->datalen != frame->samples * 2 ||
        ast_format_get_sample_rate(frame->subclass.format) != 48000) {
        return -1;
    }
    ++device->writes;
    if (device->writes == 30) {
        ast_log(LOG_NOTICE, "rpt_fixture ready %s\n", ast_channel_name(channel));
    }
    const int16_t *samples = frame->data.ptr;
    bool file = false;
    bool morse = false;
    for (int i = 0; i < frame->samples; ++i) {
        file |= !device->carrier && samples[i] == 2345;
        morse |= device->carrier && samples[i] < 0;
    }
    device->file_blocks += file;
    device->morse_blocks += morse;
    for (int i = 0; i < frame->samples; ++i) {
        if (samples[i]) {
            ++device->nonzero;
            device->early += device->carrier;
            break;
        }
    }
    return 0;
}

/** @brief Observe PTT through Asterisk's real indication interface.
 * @param channel Device.
 * @param condition Radio control.
 * @param data Unused payload.
 * @param size Unused payload size.
 * @return Zero.
 */
static int indicate(struct ast_channel *channel, int condition, const void *data, size_t size) {
    (void)data;
    (void)size;
    struct fixture *device = ast_channel_tech_pvt(channel);
    device->keys += condition == AST_CONTROL_RADIO_KEY;
    device->unkeys += condition == AST_CONTROL_RADIO_UNKEY;
    return 0;
}

/** @brief Join producer, report observable audio, and release channel-owned resources.
 * @param channel Device being released.
 * @return Zero.
 */
static int hangup(struct ast_channel *channel) {
    struct fixture *device = ast_channel_tech_pvt(channel);
    atomic_store(&device->stop, true);
    if (device->started) {
        pthread_join(device->thread, NULL);
    }
    ast_log(LOG_NOTICE, "rpt_fixture %s ticks=%u writes=%u nonzero=%u early=%u keys=%u unkeys=%u\n",
            ast_channel_name(channel), device->ticks, device->writes, device->nonzero,
            device->early, device->keys, device->unkeys);
    if (device->media) {
        ast_log(LOG_NOTICE, "rpt_fixture media file=%u morse=%u\n", device->file_blocks,
                device->morse_blocks);
    }
    close(device->pipe[0]);
    close(device->pipe[1]);
    ast_channel_tech_pvt_set(channel, NULL);
    ast_free(device);
    atomic_fetch_sub(&active, 1);
    return 0;
}

/** @brief Synthetic interface implementing only the required radio callbacks. */
static struct ast_channel_tech technology = {.type = "RadioPlusAdvanced",
                                             .description = "Test radio",
                                             .requester = request,
                                             .call = call,
                                             .read = read_frame,
                                             .write = write_frame,
                                             .indicate = indicate,
                                             .hangup = hangup};

/** @brief Register the test-only radio.
 * @return Module load status.
 */
static int load_module(void) {
    capabilities = ast_format_cap_alloc(AST_FORMAT_CAP_FLAG_DEFAULT);
    if (!capabilities) {
        return AST_MODULE_LOAD_DECLINE;
    }
    if (ast_format_cap_append(capabilities, ast_format_cache_get_slin_by_rate(48000), 0)) {
        ao2_cleanup(capabilities);
        return AST_MODULE_LOAD_DECLINE;
    }
    technology.capabilities = capabilities;
    if (ast_channel_register(&technology)) {
        ao2_cleanup(capabilities);
        return AST_MODULE_LOAD_DECLINE;
    }
    return AST_MODULE_LOAD_SUCCESS;
}
/** @brief Refuse unload while a test channel still owns resources.
 * @return Zero once unregistered, minus one if busy.
 */
static int unload_module(void) {
    if (atomic_load(&active)) {
        return -1;
    }
    ast_channel_unregister(&technology);
    ao2_cleanup(capabilities);
    return 0;
}
/** @brief Descriptor for the isolated test process only. */
static struct ast_module_info descriptor = {.name = "chan_rpt_fixture",
                                            .description = "Test radio",
                                            .key = ASTERISK_GPL_KEY,
                                            .buildopt_sum = AST_BUILDOPT_SUM,
                                            .flags = AST_MODFLAG_LOAD_ORDER,
                                            .load = load_module,
                                            .unload = unload_module,
                                            .load_pri = AST_MODPRI_CHANNEL_DRIVER,
                                            .support_level = AST_MODULE_SUPPORT_EXTENDED};
/** @brief Register on library open. */
static void __attribute__((constructor)) register_module(void) { ast_module_register(&descriptor); }
/** @brief Unregister on library close. */
static void __attribute__((destructor)) unregister_module(void) {
    ast_module_unregister(&descriptor);
}
