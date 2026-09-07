/* SPDX-License-Identifier: GPL-2.0-only */
/** @file
 * @brief Validate resolved defaults, complete schemas, inheritance, and atomic failure.
 */
#include "settings.h"
#include <assert.h>
#include <stdio.h>
#include <string.h>

/** @brief Default settings require no identifier media or fixed hardware rate. */
static void defaults(void) {
    struct ra_node_settings node;
    struct ra_identifier_settings id;
    assert(!ra_node_settings_resolve(NULL, 0, "usb", &node));
    assert(node.enabled && node.full_duplex && node.hang_ms == 0 && node.sample_rate == 0);
    assert(!strcmp(node.channel, "usb") && !*node.codec);
    assert(!*node.link_allow_nodes && !*node.link_deny_nodes);
    assert(!ra_identifier_settings_resolve(NULL, 0, NULL, NULL, &id));
    assert(id.interval_ms == 600000 && id.priority == 0);
    assert(!id.first_key_only && !id.regardless_of_activity);
    assert(!*id.file && !*id.speech_text && !*id.morse_text);
    assert(!strcmp(id.speech_model, "en_US-lessac-medium.onnx"));
    assert(id.speech_speed_percent == 100 && id.morse_speed_wpm == 20 &&
           id.morse_frequency_hz == 800 && id.speech_level_db == 0 && id.morse_level_db == -6);
}

/** @brief Exercise every public option and scoped overrides independent of file order. */
static void configured(void) {
    const struct ra_config_entry entries[] = {
        {"usb", "transmit_hang_ms", "500"},
        {"general", "transmit_hang_ms", "100"},
        {"general", "node_enabled", "no"},
        {"general", "full_duplex", "no"},
        {"usb", "sample_rate_hz", "48000"},
        {"usb", "radio_channel", "radio"},
        {"usb", "codec", "slin48"},
        {"general", "link_allow_nodes", "508422"},
        {"usb", "link_allow_nodes", ""},
        {"general", "link_deny_nodes", "1234, 5678"},
        {"identifier", "interval_ms", "300000"},
        {"identifier", "priority", "2"},
        {"identifier", "first_key_only", "yes"},
        {"identifier", "regardless_of_activity", "yes"},
        {"identifier", "sound_file", "/tmp/id.wav"},
        {"identifier", "speech_text", "Welcome"},
        {"identifier", "speech_model", "/usr/lib/piper-tts/voices/en_US-amy-low.onnx"},
        {"identifier", "speech_speed_percent", "90"},
        {"identifier", "speech_level_db", "-3"},
        {"identifier", "morse_text", "KG0BP"},
        {"identifier", "morse_speed_wpm", "18"},
        {"identifier", "morse_frequency_hz", "700"},
        {"identifier", "morse_level_db", "-5"},
        {"identifier usb", "interval_ms", "200000"},
        {"speech", "voice", "shared.onnx"},
        {"speech usb", "voice", "node.onnx"},
        {"speech usb", "speed_percent", "80"},
        {"speech usb", "level_db", "-4"},
        {"morse", "frequency_hz", "600"},
        {"morse usb", "frequency_hz", "750"},
        {"morse usb", "speed_wpm", "25"},
        {"morse usb", "level_db", "-8"},
        {"identifier usb welcome", "speech_text", ""},
        {"identifier usb welcome", "speech_level_db", "-2"},
        {"identifier usb welcome", "morse_level_db", "-7"},
    };
    size_t count = sizeof(entries) / sizeof(entries[0]);
    struct ra_node_settings node;
    struct ra_identifier_settings id;
    assert(!ra_node_settings_resolve(entries, count, "usb", &node));
    assert(!node.enabled && !node.full_duplex && node.hang_ms == 500 && node.sample_rate == 48000);
    assert(!strcmp(node.channel, "radio") && !strcmp(node.codec, "slin48"));
    assert(!*node.link_allow_nodes && !strcmp(node.link_deny_nodes, "1234, 5678"));
    assert(!ra_identifier_settings_resolve(entries, count, "usb", "identifier usb welcome", &id));
    assert(id.interval_ms == 200000 && id.priority == 2 && id.first_key_only &&
           id.regardless_of_activity);
    assert(!strcmp(id.file, "/tmp/id.wav") && !*id.speech_text);
    assert(!strcmp(id.speech_model, "node.onnx"));
    assert(id.speech_speed_percent == 80 && id.speech_level_db == -2 &&
           !strcmp(id.morse_text, "KG0BP"));
    assert(id.morse_speed_wpm == 25 && id.morse_frequency_hz == 750 && id.morse_level_db == -7);
}

/** @brief Ignore malformed near-matches when resolving flat and node-scoped defaults. */
static void scoped_default_matching(void) {
    const struct ra_config_entry entries[] = {
        {"speech", "voice", "flat.onnx"},
        {"speechx", "voice", "wrong-prefix.onnx"},
        {"speech ", "voice", "missing-node.onnx"},
        {"speech other", "voice", "wrong-node.onnx"},
        {"speech usb extra", "voice", "trailing-name.onnx"},
        {"speech usb", "voice", "node.onnx"},
    };
    struct ra_identifier_settings id;
    assert(!ra_identifier_settings_resolve(entries, sizeof(entries) / sizeof(entries[0]), NULL,
                                           NULL, &id));
    assert(!strcmp(id.speech_model, "flat.onnx"));
    assert(!ra_identifier_settings_resolve(entries, sizeof(entries) / sizeof(entries[0]), "usb",
                                           NULL, &id));
    assert(!strcmp(id.speech_model, "node.onnx"));
}

/** @brief Every typed setting rejects invalid text without committing earlier fields. */
static void invalid(void) {
    const char *node_keys[] = {"node_enabled",   "full_duplex",      "transmit_hang_ms",
                               "sample_rate_hz", "link_allow_nodes", "link_deny_nodes"};
    const char *id_keys[] = {
        "interval_ms",          "priority",        "first_key_only",  "regardless_of_activity",
        "speech_speed_percent", "speech_level_db", "morse_speed_wpm", "morse_frequency_hz",
        "morse_level_db"};
    struct ra_node_settings node = {0};
    struct ra_identifier_settings id = {0};
    for (size_t i = 0; i < sizeof(node_keys) / sizeof(node_keys[0]); ++i) {
        struct ra_config_entry entry = {"usb", node_keys[i], "invalid"};
        assert(!strcmp(ra_node_settings_resolve(&entry, 1, "usb", &node), node_keys[i]));
        assert(!node.enabled && !node.channel);
    }
    for (size_t i = 0; i < sizeof(id_keys) / sizeof(id_keys[0]); ++i) {
        struct ra_config_entry entry = {"identifier", id_keys[i], "invalid"};
        assert(!strcmp(ra_identifier_settings_resolve(&entry, 1, NULL, NULL, &id), id_keys[i]));
        assert(id.interval_ms == 0 && !id.speech_model);
    }
    struct ra_config_entry speech = {"speech usb", "level_db", "invalid"};
    assert(!strcmp(ra_identifier_settings_resolve(&speech, 1, "usb", NULL, &id), "level_db"));
    assert(id.interval_ms == 0 && !id.speech_model);
    struct ra_config_entry morse = {"morse usb", "level_db", "invalid"};
    assert(!strcmp(ra_identifier_settings_resolve(&morse, 1, "usb", NULL, &id), "level_db"));
    assert(id.interval_ms == 0 && !id.speech_model);
}

/** @brief Every command prefix inherits, can be disabled, and rejects ambiguous mappings. */
static void command_settings(void) {
    const char *keys[] = {"link_command_disconnect",
                          "link_command_monitor",
                          "link_command_transceive",
                          "link_command_remote",
                          "link_command_status",
                          "link_command_disconnect_all",
                          "link_command_last_keyed",
                          "link_command_local_monitor",
                          "link_command_disconnect_permanent",
                          "link_command_permanent_monitor",
                          "link_command_permanent_transceive",
                          "link_command_full_status",
                          "link_command_reconnect_all",
                          "link_command_permanent_local_monitor"};
    for (size_t i = 0; i < RA_LINK_ACTION_COUNT; ++i) {
        struct ra_config_entry entries[] = {{"general", keys[i], "A"}, {"usb", keys[i], ""}};
        struct ra_node_settings node;
        assert(!ra_settings_validate(false, keys[i], "A"));
        assert(ra_settings_validate(false, keys[i], "invalid"));
        assert(!ra_node_settings_resolve(entries, 1, "usb", &node));
        assert(!strcmp(node.link_commands[i].digits, "A"));
        assert(!ra_node_settings_resolve(entries, 2, "usb", &node));
        assert(!*node.link_commands[i].digits);
    }
    struct ra_config_entry overlap = {"usb", "link_command_disconnect", "3"};
    struct ra_node_settings node = {0};
    assert(ra_node_settings_resolve(&overlap, 1, "usb", &node));
    assert(!node.enabled);
}

/** @brief Execute all settings tests.
 * @return Zero after successful assertions.
 */
int main(void) {
    defaults();
    configured();
    scoped_default_matching();
    invalid();
    command_settings();
    puts("settings resolution tests passed");
    return 0;
}
