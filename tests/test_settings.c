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
    struct ra_announcement_settings announcement;
    struct ra_time_settings time;
    assert(!ra_node_settings_resolve(NULL, 0, "usb", &node));
    assert(node.enabled && node.full_duplex && node.dtmf_muting && node.hang_ms == 0 &&
           node.telemetry_duck_db == -20 && node.sample_rate == 0);
    assert(node.courtesy_delay_ms == 250);
    assert(!strcmp(node.channel, "usb") && !*node.codec);
    assert(!*node.link_allow_nodes && !*node.link_deny_nodes);
    assert(!*node.link_static_directory_file && !*node.link_directory_file &&
           node.link_lookup_method == RA_LINK_LOOKUP_BOTH);
    assert(!ra_identifier_settings_resolve(NULL, 0, NULL, NULL, &id));
    assert(id.interval_ms == 600000 && id.priority == 0);
    assert(!id.first_key_only && !id.regardless_of_activity && !id.polite &&
           id.polite_maximum_wait_ms == 60000);
    assert(!*id.file && !*id.speech_text && !*id.morse_text);
    assert(!strcmp(id.speech_model, "en_US-lessac-medium.onnx"));
    assert(id.speech_speed_percent == 100 && id.morse_speed_wpm == 20 &&
           id.morse_frequency_hz == 800 && id.speech_level_db == 0 && id.morse_level_db == -6);
    assert(!ra_announcement_settings_resolve(NULL, 0, NULL, NULL, &announcement));
    assert(!announcement.interval_ms && !*announcement.media.file &&
           !*announcement.media.speech_text && !*announcement.media.morse_text &&
           !strcmp(announcement.media.speech_model, "en_US-lessac-medium.onnx") &&
           announcement.media.speech_speed_percent == 100 &&
           announcement.media.morse_speed_wpm == 20 &&
           announcement.media.morse_frequency_hz == 800 && !announcement.media.speech_level_db &&
           announcement.media.morse_level_db == -6);
    assert(!ra_time_settings_resolve(NULL, 0, "usb", &time) && time.format == 12);
}

/** @brief Exercise every public option and scoped overrides independent of file order. */
static void configured(void) {
    const struct ra_config_entry entries[] = {
        {"usb", "transmit_hang_ms", "500"},
        {"general", "transmit_hang_ms", "100"},
        {"general", "node_enabled", "no"},
        {"general", "full_duplex", "no"},
        {"general", "dtmf_muting", "no"},
        {"usb", "dtmf_muting", "yes"},
        {"general", "telemetry_duck_db", "-18"},
        {"general", "courtesy_delay_ms", "300"},
        {"usb", "sample_rate_hz", "48000"},
        {"usb", "radio_channel", "radio"},
        {"usb", "codec", "slin48"},
        {"general", "link_allow_nodes", "508422"},
        {"usb", "link_allow_nodes", ""},
        {"general", "link_deny_nodes", "1234, 5678"},
        {"general", "link_static_directory_file", "static.conf"},
        {"general", "link_directory_file", "external.conf"},
        {"usb", "link_directory_file", "node.conf"},
        {"usb", "link_lookup_method", "file"},
        {"identifier", "interval_ms", "300000"},
        {"identifier", "priority", "2"},
        {"identifier", "first_key_only", "yes"},
        {"identifier", "regardless_of_activity", "yes"},
        {"identifier", "polite", "yes"},
        {"identifier", "polite_maximum_wait_ms", "70000"},
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
        {"identifier usb", "polite_maximum_wait_ms", "60000"},
        {"speech", "voice", "shared.onnx"},
        {"speech usb", "voice", "node.onnx"},
        {"speech usb", "speed_percent", "80"},
        {"speech usb", "level_db", "-4"},
        {"morse", "frequency_hz", "600"},
        {"morse usb", "frequency_hz", "750"},
        {"morse usb", "speed_wpm", "25"},
        {"morse usb", "level_db", "-8"},
        {"time", "format", "24"},
        {"time usb", "format", "12"},
        {"identifier usb welcome", "speech_text", ""},
        {"identifier usb welcome", "polite", "no"},
        {"identifier usb welcome", "speech_level_db", "-2"},
        {"identifier usb welcome", "morse_level_db", "-7"},
    };
    size_t count = sizeof(entries) / sizeof(entries[0]);
    struct ra_node_settings node;
    struct ra_identifier_settings id;
    struct ra_time_settings time;
    assert(!ra_node_settings_resolve(entries, count, "usb", &node));
    assert(!node.enabled && !node.full_duplex && node.dtmf_muting && node.hang_ms == 500 &&
           node.telemetry_duck_db == -18 && node.sample_rate == 48000);
    assert(node.courtesy_delay_ms == 300);
    assert(!strcmp(node.channel, "radio") && !strcmp(node.codec, "slin48"));
    assert(!*node.link_allow_nodes && !strcmp(node.link_deny_nodes, "1234, 5678"));
    assert(!strcmp(node.link_static_directory_file, "static.conf") &&
           !strcmp(node.link_directory_file, "node.conf") &&
           node.link_lookup_method == RA_LINK_LOOKUP_FILE);
    assert(!ra_identifier_settings_resolve(entries, count, "usb", "identifier usb welcome", &id));
    assert(id.interval_ms == 200000 && id.priority == 2 && id.first_key_only &&
           id.regardless_of_activity && !id.polite && id.polite_maximum_wait_ms == 60000);
    assert(!strcmp(id.file, "/tmp/id.wav") && !*id.speech_text);
    assert(!strcmp(id.speech_model, "node.onnx"));
    assert(id.speech_speed_percent == 80 && id.speech_level_db == -2 &&
           !strcmp(id.morse_text, "KG0BP"));
    assert(id.morse_speed_wpm == 25 && id.morse_frequency_hz == 750 && id.morse_level_db == -7);
    assert(!ra_time_settings_resolve(entries, count, "usb", &time) && time.format == 12);
}

/** @brief Courtesy media inherits its own defaults and node media defaults before its assignment.
 */
static void courtesy_settings(void) {
    const struct ra_config_entry entries[] = {
        {"courtesy", "sound_file", "/tmp/shared.wav"},
        {"courtesy", "speech_text", "Shared courtesy"},
        {"courtesy", "morse_text", "C"},
        {"courtesy", "tone_sequence", "700+900@-12/80, 0/40, 500/80"},
        {"courtesy", "level_db", "-18"},
        {"courtesy usb", "morse_text", "NODE"},
        {"courtesy usb", "level_db", "-20"},
        {"speech", "voice", "shared.onnx"},
        {"speech usb", "voice", "node.onnx"},
        {"speech usb", "speed_percent", "85"},
        {"morse", "frequency_hz", "600"},
        {"morse usb", "frequency_hz", "750"},
        {"morse usb", "speed_wpm", "25"},
        {"courtesy usb receiver", "input", "receiver"},
        {"courtesy usb receiver", "sound_file", ""},
        {"courtesy usb receiver", "speech_text", ""},
        {"courtesy usb receiver", "morse_text", "R"},
        {"courtesy usb receiver", "level_db", "-16"},
        {"courtesy usb north", "input", "link"},
        {"courtesy usb north", "remote_node", "123456"},
        {"courtesy usb north", "tone_sequence", "1200+1500/75,0/25,800/100"},
    };
    struct ra_courtesy_settings receiver;
    struct ra_courtesy_settings north;
    assert(!ra_courtesy_settings_resolve(entries, sizeof(entries) / sizeof(entries[0]), "usb",
                                         "courtesy usb receiver", &receiver));
    assert(receiver.input == RA_COURTESY_INPUT_RECEIVER && !*receiver.remote_node &&
           !*receiver.media.file && !*receiver.media.speech_text &&
           !strcmp(receiver.media.morse_text, "R") &&
           !strcmp(receiver.tone_sequence, "700+900@-12/80, 0/40, 500/80") &&
           receiver.level_db == -16 && !strcmp(receiver.media.speech_model, "node.onnx") &&
           receiver.media.speech_speed_percent == 85 && receiver.media.speech_level_db == 0 &&
           receiver.media.morse_speed_wpm == 25 && receiver.media.morse_frequency_hz == 750 &&
           receiver.media.morse_level_db == -16);
    assert(!ra_courtesy_settings_resolve(entries, sizeof(entries) / sizeof(entries[0]), "usb",
                                         "courtesy usb north", &north));
    assert(north.input == RA_COURTESY_INPUT_LINK && !strcmp(north.remote_node, "123456") &&
           !strcmp(north.media.file, "/tmp/shared.wav") &&
           !strcmp(north.media.speech_text, "Shared courtesy") &&
           !strcmp(north.media.morse_text, "NODE") &&
           !strcmp(north.tone_sequence, "1200+1500/75,0/25,800/100") && north.level_db == -20 &&
           north.media.morse_level_db == -20);
    struct ra_config_entry missing_input = {"courtesy usb missing", "morse_text", "M"};
    struct ra_courtesy_settings unchanged = {.level_db = 7};
    assert(!strcmp(
        ra_courtesy_settings_resolve(&missing_input, 1, "usb", "courtesy usb missing", &unchanged),
        "courtesy input is required"));
    assert(unchanged.level_db == 7);
    struct ra_config_entry receiver_remote[] = {
        {"courtesy usb bad", "input", "receiver"},
        {"courtesy usb bad", "remote_node", "123"},
    };
    assert(!strcmp(ra_courtesy_settings_resolve(
                       receiver_remote, sizeof(receiver_remote) / sizeof(receiver_remote[0]), "usb",
                       "courtesy usb bad", &unchanged),
                   "courtesy remote node requires link input"));
    assert(!strcmp(ra_courtesy_settings_resolve(NULL, 0, "usb", NULL, &unchanged),
                   "courtesy set is required"));
    assert(!ra_settings_validate_kind(RA_SETTINGS_COURTESY, "tone_sequence", "800/50"));
    assert(!ra_settings_validate_kind(RA_SETTINGS_COURTESY_SET, "input", "link"));
    assert(ra_settings_validate_kind(RA_SETTINGS_COURTESY_SET, "input", "wrong"));
    assert(ra_settings_validate_kind(RA_SETTINGS_COURTESY_SET, "remote_node", "12x"));

    const struct ra_config_entry empty_remote_entries[] = {
        {"courtesy usb link", "input", "link"},
        {"courtesy usb link", "remote_node", ""},
    };
    assert(!ra_courtesy_settings_resolve(
        empty_remote_entries, sizeof(empty_remote_entries) / sizeof(empty_remote_entries[0]), "usb",
        "courtesy usb link", &unchanged));
    assert(!*unchanged.remote_node);
}

/** @brief Courtesy resolution preserves inherited-scope errors and caller output. */
static void courtesy_default_errors(void) {
    struct ra_courtesy_settings unchanged = {.level_db = 7};
    const struct ra_config_entry courtesy_entries[] = {
        {"courtesy", "level_db", "invalid"},
        {"courtesy usb receiver", "input", "receiver"},
    };
    assert(!strcmp(ra_courtesy_settings_resolve(
                       courtesy_entries, sizeof(courtesy_entries) / sizeof(courtesy_entries[0]),
                       "usb", "courtesy usb receiver", &unchanged),
                   "level_db"));
    assert(unchanged.level_db == 7);

    const struct ra_config_entry speech_entries[] = {
        {"speech", "speed_percent", "invalid"},
        {"courtesy usb receiver", "input", "receiver"},
    };
    assert(!strcmp(ra_courtesy_settings_resolve(speech_entries,
                                                sizeof(speech_entries) / sizeof(speech_entries[0]),
                                                "usb", "courtesy usb receiver", &unchanged),
                   "speed_percent"));
    assert(unchanged.level_db == 7);

    const struct ra_config_entry morse_entries[] = {
        {"morse", "frequency_hz", "0"},
        {"courtesy usb receiver", "input", "receiver"},
    };
    assert(!strcmp(ra_courtesy_settings_resolve(morse_entries,
                                                sizeof(morse_entries) / sizeof(morse_entries[0]),
                                                "usb", "courtesy usb receiver", &unchanged),
                   "frequency_hz"));
    assert(unchanged.level_db == 7);
}

/** @brief Resolve flat, node, and set defaults by scope rather than file order. */
static void scoped_default_precedence(void) {
    const struct ra_config_entry entries[] = {
        {"identifier usb welcome", "interval_ms", "100000"},
        {"identifier usb welcome", "speech_level_db", "-2"},
        {"identifier usb welcome", "morse_level_db", "-7"},
        {"identifier usb", "interval_ms", "200000"},
        {"identifier usb", "speech_speed_percent", "80"},
        {"identifier usb", "morse_speed_wpm", "25"},
        {"speech usb", "voice", "node.onnx"},
        {"speech usb", "level_db", "-4"},
        {"morse usb", "frequency_hz", "750"},
        {"morse usb", "level_db", "-8"},
        {"identifier", "interval_ms", "300000"},
        {"identifier", "speech_speed_percent", "90"},
        {"identifier", "morse_speed_wpm", "18"},
        {"speech", "voice", "flat.onnx"},
        {"speech", "level_db", "-3"},
        {"morse", "frequency_hz", "600"},
        {"morse", "level_db", "-5"},
    };
    struct ra_identifier_settings id;
    assert(!ra_identifier_settings_resolve(entries, sizeof(entries) / sizeof(entries[0]), "usb",
                                           "identifier usb welcome", &id));
    assert(id.interval_ms == 100000 && !strcmp(id.speech_model, "node.onnx"));
    assert(id.speech_speed_percent == 80 && id.speech_level_db == -2);
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

/** @brief Announcements inherit media defaults but permit an every-release zero interval. */
static void announcement_settings(void) {
    const struct ra_config_entry entries[] = {
        {"announcement", "interval_ms", "1800000"},
        {"announcement", "sound_file", "/tmp/shared.wav"},
        {"announcement", "speech_text", "Shared announcement"},
        {"announcement", "morse_text", "DE SHARED"},
        {"announcement usb", "morse_text", "DE USB"},
        {"speech", "voice", "shared.onnx"},
        {"speech usb", "voice", "node.onnx"},
        {"speech usb", "level_db", "-4"},
        {"morse", "frequency_hz", "600"},
        {"morse usb", "frequency_hz", "750"},
        {"morse usb", "speed_wpm", "25"},
        {"announcement usb release", "interval_ms", "0"},
        {"announcement usb release", "sound_file", ""},
        {"announcement usb release", "speech_text", "Release announcement"},
        {"announcement usb release", "morse_level_db", "-20"},
    };
    struct ra_announcement_settings announcement;
    assert(!ra_announcement_settings_resolve(entries, sizeof(entries) / sizeof(entries[0]), "usb",
                                             "announcement usb release", &announcement));
    assert(!announcement.interval_ms && !*announcement.media.file &&
           !strcmp(announcement.media.speech_text, "Release announcement") &&
           !strcmp(announcement.media.morse_text, "DE USB") &&
           !strcmp(announcement.media.speech_model, "node.onnx") &&
           announcement.media.speech_level_db == -4 && announcement.media.morse_speed_wpm == 25 &&
           announcement.media.morse_frequency_hz == 750 &&
           announcement.media.morse_level_db == -20);
    assert(!ra_announcement_settings_resolve(entries, sizeof(entries) / sizeof(entries[0]), "usb",
                                             NULL, &announcement));
    assert(announcement.interval_ms == 1800000 &&
           !strcmp(announcement.media.file, "/tmp/shared.wav") &&
           !strcmp(announcement.media.speech_text, "Shared announcement"));
    struct ra_config_entry invalid = {"announcement usb release", "interval_ms", "invalid"};
    struct ra_announcement_settings unchanged = {.interval_ms = 7};
    assert(!strcmp(ra_announcement_settings_resolve(&invalid, 1, "usb", "announcement usb release",
                                                    &unchanged),
                   "interval_ms"));
    assert(unchanged.interval_ms == 7);
    invalid = (struct ra_config_entry){"announcement", "interval_ms", "invalid"};
    assert(!strcmp(ra_announcement_settings_resolve(&invalid, 1, "usb", NULL, &unchanged),
                   "interval_ms"));
    assert(unchanged.interval_ms == 7);
    assert(!ra_settings_validate_kind(RA_SETTINGS_ANNOUNCEMENT, "interval_ms", "0"));
    assert(ra_settings_validate_kind(RA_SETTINGS_ANNOUNCEMENT, "priority", "1"));
}

/** @brief Verify node DTMF muting inherits the global value and permits an override. */
static void dtmf_muting_inherits(void) {
    const struct ra_config_entry shared[] = {{"general", "dtmf_muting", "no"}};
    const struct ra_config_entry overridden[] = {{"general", "dtmf_muting", "no"},
                                                 {"usb", "dtmf_muting", "yes"}};
    struct ra_node_settings node;
    assert(!ra_node_settings_resolve(shared, sizeof(shared) / sizeof(shared[0]), "usb", &node));
    assert(!node.dtmf_muting);
    assert(!ra_node_settings_resolve(overridden, sizeof(overridden) / sizeof(overridden[0]), "usb",
                                     &node));
    assert(node.dtmf_muting);
}

/** @brief Time-format defaults inherit from the flat section and accept only 12 or 24 hours. */
static void time_settings(void) {
    const struct ra_config_entry inherited[] = {{"time", "format", "24"}};
    const struct ra_config_entry overridden[] = {{"time", "format", "24"},
                                                 {"time usb", "format", "12"}};
    struct ra_time_settings time;
    assert(!ra_time_settings_resolve(inherited, 1, "usb", &time) && time.format == 24);
    assert(!ra_time_settings_resolve(overridden, 2, "usb", &time) && time.format == 12);
    struct ra_config_entry invalid = {"time usb", "format", "13"};
    assert(!strcmp(ra_time_settings_resolve(&invalid, 1, "usb", &time), "format"));
    assert(!ra_settings_validate_kind(RA_SETTINGS_TIME, "format", "12"));
    assert(ra_settings_validate_kind(RA_SETTINGS_TIME, "format", "13"));
}

/** @brief Every typed setting rejects invalid text without committing earlier fields. */
static void invalid(void) {
    const char *node_keys[] = {"node_enabled",      "full_duplex",       "dtmf_muting",
                               "transmit_hang_ms",  "telemetry_duck_db", "courtesy_delay_ms",
                               "sample_rate_hz",    "link_allow_nodes",  "link_deny_nodes",
                               "link_lookup_method"};
    const char *id_keys[] = {"interval_ms",
                             "priority",
                             "first_key_only",
                             "regardless_of_activity",
                             "polite",
                             "polite_maximum_wait_ms",
                             "speech_speed_percent",
                             "speech_level_db",
                             "morse_speed_wpm",
                             "morse_frequency_hz",
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
    struct ra_config_entry courtesy = {"courtesy usb receiver", "level_db", "invalid"};
    struct ra_courtesy_settings courtesy_result = {0};
    assert(!strcmp(ra_courtesy_settings_resolve(&courtesy, 1, "usb", "courtesy usb receiver",
                                                &courtesy_result),
                   "level_db"));
    assert(!courtesy_result.media.speech_model);
    courtesy = (struct ra_config_entry){"courtesy usb receiver", "remote_node", "invalid"};
    assert(!strcmp(ra_courtesy_settings_resolve(&courtesy, 1, "usb", "courtesy usb receiver",
                                                &courtesy_result),
                   "remote_node"));
}

/** @brief Parse every documented post-static directory lookup selection. */
static void directory_settings(void) {
    const char *methods[] = {"both", "dns", "file"};
    const enum ra_link_lookup_method expected[] = {RA_LINK_LOOKUP_BOTH, RA_LINK_LOOKUP_DNS,
                                                   RA_LINK_LOOKUP_FILE};
    for (size_t index = 0; index < sizeof(methods) / sizeof(*methods); ++index) {
        struct ra_config_entry entry = {"usb", "link_lookup_method", methods[index]};
        struct ra_node_settings node;
        assert(!ra_settings_validate(false, entry.key, entry.value));
        assert(!ra_node_settings_resolve(&entry, 1, "usb", &node));
        assert(node.link_lookup_method == expected[index]);
    }
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
    for (size_t i = 0; i < sizeof(keys) / sizeof(*keys); ++i) {
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
    scoped_default_precedence();
    scoped_default_matching();
    announcement_settings();
    courtesy_settings();
    courtesy_default_errors();
    dtmf_muting_inherits();
    time_settings();
    invalid();
    directory_settings();
    command_settings();
    puts("settings resolution tests passed");
    return 0;
}
