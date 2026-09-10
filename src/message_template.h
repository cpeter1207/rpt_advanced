/* SPDX-License-Identifier: GPL-2.0-only */
/**
 * @file
 * @brief Strict expansion of scheduled-message substitutions.
 */
#ifndef RPT_ADVANCED_MESSAGE_TEMPLATE_H
#define RPT_ADVANCED_MESSAGE_TEMPLATE_H
#include <stdbool.h>
#include <stddef.h>

/** @brief Maximum rendered scheduled-message bytes, including the terminating null byte. */
#define RA_MESSAGE_TEMPLATE_OUTPUT_MAX 128U

/** @brief Values available to one scheduled message expansion. */
struct ra_message_template_values {
    const char *day_of_week; /**< Local weekday name. */
    const char *date;        /**< Local calendar date. */
    const char *time;        /**< Local clock time. */
    const char *greeting;    /**< Local time-of-day greeting. */
    const char *link_status; /**< Current bounded link summary. */
    const char *node;        /**< Local node identity. */
    const char *callsign;    /**< Configured local callsign. */
};

/** @brief Validate the syntax and substitution names of one message template.
 * @param template_text Input using only documented `${name}` substitutions.
 * @return True when every substitution is complete and approved.
 *
 * This does not render or bound an output. It is therefore suitable for configuration validation
 * before runtime values, such as a link-status summary, are available.
 */
bool ra_message_template_validate(const char *template_text);

/** @brief Validate syntax and prove every permitted expansion fits the scheduled buffer.
 * @param template_text Input using only documented `${name}` substitutions.
 * @return True only when the source is valid and its worst-case rendered text fits in
 *         RA_MESSAGE_TEMPLATE_OUTPUT_MAX bytes including the terminator.
 *
 * The proof uses fixed maximum values for the documented local date/time/greeting fields,
 * bounded 63-byte node and callsign settings, and the longest possible direct-peer status
 * summary. Configuration uses this before runtime state exists, so a valid reload cannot fail
 * merely because a current link summary is long.
 */
bool ra_message_template_validate_output(const char *template_text);

/** @brief Validate and expand a fixed scheduled-message template.
 * @param template_text Input using only documented `${name}` substitutions.
 * @param values Non-null substitutions.
 * @param output Destination including terminator.
 * @param capacity Destination bytes.
 * @return True only when every substitution is valid and the complete result fits.
 */
bool ra_message_template_render(const char *template_text,
                                const struct ra_message_template_values *values, char *output,
                                size_t capacity);
#endif
