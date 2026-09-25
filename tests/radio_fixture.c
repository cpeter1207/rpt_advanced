/* SPDX-License-Identifier: GPL-2.0-only */
/** @file
 * @brief Test-only hardware-paced Asterisk radio; never installed on a node.
 */
#include "../rust/product/include/rptadv_product.h"
#include <asterisk.h>
#include <asterisk/astobj2.h>
#include <asterisk/buildopts.h>
#include <asterisk/channel.h>
#include <asterisk/format_cache.h>
#include <asterisk/format_cap.h>
#include <asterisk/logger.h>
#include <asterisk/module.h>
#include <math.h>
#include <pthread.h>
#include <stdatomic.h>
#include <stdbool.h>
#include <time.h>

/** @brief Test-device state, retained until the producer thread has joined. */
struct fixture {
    pthread_t thread;   /**< Clock producer. */
    atomic_bool stop;   /**< Stop callbacks before releasing their borrowed contexts. */
    bool started;       /**< Thread was created successfully. */
    bool keyed;         /**< Last physical PTT state. */
    bool carrier;       /**< Current synthetic receiver indication. */
    bool media;         /**< Delay reception so a prepared ID starts first. */
    bool network;       /**< Repeat phased receive bursts during network tests. */
    bool dtmf;          /**< Generate an on-air connect command, then a disconnect command. */
    unsigned int phase; /**< Receive phase in a 100-frame network test cycle. */
    unsigned int remote_blocks; /**< Nonzero transmit blocks while local reception is inactive. */
    unsigned int file_blocks;   /**< Recognizable prepared-file blocks before reception. */
    unsigned int morse_blocks;  /**< Negative Morse samples mixed with positive receive PCM. */
    unsigned int ticks;         /**< Hardware-paced direct receive calls. */
    unsigned int writes;        /**< Transmit blocks received. */
    unsigned int nonzero;       /**< Blocks with nonzero output. */
    unsigned int early;         /**< Nonzero output during initial local reception. */
    unsigned int keys;          /**< PTT assertions. */
    unsigned int unkeys;        /**< PTT releases observed in callback output. */
    struct urp_ast_direct_callbacks callbacks; /**< Retained ABI2 endpoints. */
    struct ast_channel *channel; /**< Borrowed until synchronous hangup joins the clock. */
    float audio[960];            /**< Normalized native-rate PCM. */
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
 * Duplex cases finish at an observed unkey; streaming media/network cases stay live.
 * A stuck key never produces finite readiness, and teardown adds no counted edge.
 */
static void *clock_run(void *context);

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
    atomic_init(&device->stop, false);
    device->media = !strcmp(data, "media");
    device->network = !strncmp(data, "network-", 8);
    device->dtmf = !strcmp(data, "network-dtmf");
    device->phase = !strcmp(data, "network-b") ? 50 : 0;
    struct ast_channel *channel = ast_channel_alloc(1, AST_STATE_DOWN, NULL, NULL, "", "", "", ids,
                                                    requestor, 0, "RadioPlusAdvanced/%s", data);
    if (!channel) {
        ast_free(device);
        return NULL;
    }
    ast_channel_tech_set(channel, &technology);
    ast_channel_tech_pvt_set(channel, device);
    ast_channel_nativeformats_set(channel, capabilities);
    ast_channel_set_readformat(channel, ast_format_cache_get_slin_by_rate(48000));
    ast_channel_set_writeformat(channel, ast_format_cache_get_slin_by_rate(48000));
    device->channel = channel;
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
    if (!device->callbacks.receive || device->started) {
        return -1;
    }
    int result = pthread_create(&device->thread, NULL, clock_run, device);
    device->started = !result;
    ast_setstate(channel, AST_STATE_UP);
    return result;
}

/** @brief Produce one native receive interval with its carrier and optional DTMF.
 * @param device Clock-owned fixture.
 */
static void receive_audio(struct fixture *device) {
    unsigned int begin = device->media ? 10 : 0;
    device->carrier = device->network ? device->ticks % 100 >= device->phase &&
                                            device->ticks % 100 < device->phase + 30
                                      : device->ticks >= begin && device->ticks < begin + 10;
    if (device->dtmf) {
        device->carrier = true;
    }
    ++device->ticks;
    float *audio = device->audio;
    for (size_t i = 0; i < 960; ++i) {
        audio[i] = device->carrier ? 1000.0F / 32768.0F : 0;
        if (device->dtmf) {
            audio[i] = 0;
            unsigned int start = device->ticks < 400 ? 100 : 400;
            const char *sequence = device->ticks < 400 ? "*3508422#" : "*1508422";
            unsigned int elapsed = device->ticks >= start ? device->ticks - start : 1000;
            if (elapsed / 10 < strlen(sequence) && elapsed % 10 < 6) {
                const char *keypad = "123A456B789C*0#D";
                size_t key = (size_t)(strchr(keypad, sequence[elapsed / 10]) - keypad);
                const double rows[] = {697, 770, 852, 941};
                const double columns[] = {1209, 1336, 1477, 1633};
                double phase = 2.0 * 3.14159265358979323846 * ((elapsed % 10) * 960 + i) / 48000;
                audio[i] = (float)(4000.0 / 32768.0 *
                                   (sin(phase * rows[key / 4]) + sin(phase * columns[key % 4])));
            }
        }
    }
}

/** @brief Inspect canonical PCM returned directly into the hardware buffer.
 * @param device Clock-owned fixture.
 */
static void inspect_audio(struct fixture *device) {
    ++device->writes;
    const float *samples = device->audio;
    bool file = false;
    bool morse = false;
    for (size_t i = 0; i < 960; ++i) {
        file |= !device->carrier && fabsf(samples[i] - 2345.0F / 32768.0F) < 1.0F / 32768.0F;
        morse |= device->carrier && samples[i] < 0;
    }
    device->file_blocks += file;
    device->morse_blocks += morse;
    for (size_t i = 0; i < 960; ++i) {
        if (samples[i]) {
            ++device->nonzero;
            device->early += device->carrier;
            device->remote_blocks += !device->carrier;
            break;
        }
    }
}

/** @brief Acknowledge supported direct endpoints and borrowed link attachments.
 * @param channel Reserved test device.
 * @param option Supported attachment identifier.
 * @param data Mutable descriptor.
 * @param size Exact descriptor size.
 * @return Zero after acknowledgment, minus one for an invalid attachment.
 */
static int setoption(struct ast_channel *channel, int option, void *data, int size) {
    struct fixture *device = ast_channel_tech_pvt(channel);
    if (option == URP_AST_OPTION_LINK_ATTACH) {
        if (!data || size != sizeof(struct urp_ast_link_attach)) {
            return -1;
        }
        struct urp_ast_link_attach *attachment = data;
        attachment->accepted_abi_version = 0;
        if (attachment->struct_size != sizeof(*attachment) ||
            attachment->abi_version != URP_AST_LINK_ATTACH_ABI_VERSION ||
            !attachment->peer_channel) {
            return -1;
        }
        attachment->accepted_abi_version = URP_AST_LINK_ATTACH_ABI_VERSION;
        return 0;
    }
    if (option != URP_AST_OPTION_DIRECT_CALLBACKS || !data ||
        size != sizeof(struct urp_ast_direct_callbacks) || device->callbacks.receive) {
        return -1;
    }
    struct urp_ast_direct_callbacks *callbacks = data;
    callbacks->accepted_abi_version = 0;
    if (callbacks->struct_size != sizeof(*callbacks) ||
        callbacks->abi_version != URP_AST_DIRECT_CALLBACKS_ABI_VERSION || !callbacks->receive ||
        !callbacks->transmit) {
        return -1;
    }
    device->callbacks = *callbacks;
    callbacks->accepted_abi_version = URP_AST_DIRECT_CALLBACKS_ABI_VERSION;
    return 0;
}

static void *clock_run(void *context) {
    struct fixture *device = context;
    const struct timespec interval = {.tv_nsec = 20000000};
    for (unsigned int tick = 0;
         tick < (device->network ? 2000U : 100U) && !atomic_load(&device->stop); ++tick) {
        receive_audio(device);
        uint32_t keyed = 0;
        if (device->callbacks.receive(device->callbacks.receive_context, device->carrier,
                                      device->audio, 960) ||
            device->callbacks.transmit(device->callbacks.transmit_context, device->audio, 960,
                                       &keyed)) {
            break;
        }
        device->keys += keyed && !device->keyed;
        device->unkeys += !keyed && device->keyed;
        device->keyed = keyed != 0;
        inspect_audio(device);
        if (device->network || device->media ? device->writes == 30
                                             : device->writes >= 30 && !device->keyed) {
            ast_log(LOG_NOTICE, "rpt_fixture ready %s\n", ast_channel_name(device->channel));
            if (!device->network && !device->media) {
                break;
            }
        }
        nanosleep(&interval, NULL);
    }
    device->keyed = false;
    return NULL;
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
    if (device->network) {
        ast_log(LOG_NOTICE, "rpt_fixture network remote=%u\n", device->remote_blocks);
    }
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
                                             .setoption = setoption,
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
