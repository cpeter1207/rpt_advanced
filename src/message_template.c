/* SPDX-License-Identifier: GPL-2.0-only */
/**
 * @file
 * @brief Strict bounded scheduled-message template rendering.
 */
#include "message_template.h"
#include "settings.h"
#include <string.h>

/** @brief Longest documented local weekday name. */
#define RA_MESSAGE_TEMPLATE_WEEKDAY_MAX 9U
/** @brief Fixed ISO local-date width. */
#define RA_MESSAGE_TEMPLATE_DATE_MAX 10U
/** @brief Longest 12-hour local clock text, such as `12:00 AM`. */
#define RA_MESSAGE_TEMPLATE_TIME_MAX 8U
/** @brief Longest time-of-day greeting, `Good Afternoon`. */
#define RA_MESSAGE_TEMPLATE_GREETING_MAX 14U
/** @brief Longest bounded direct-peer status summary, including a conservative payload margin. */
#define RA_MESSAGE_TEMPLATE_LINK_STATUS_MAX 106U

/** @brief Return the conservative maximum rendered bytes for one approved substitution.
 * @param name Candidate substitution name without delimiters.
 * @param length Bytes in @p name.
 * @return Nonzero maximum byte count, or zero for an unapproved name.
 */
static size_t maximum_value_length(const char *name, size_t length) {
    if (length == 11 && !memcmp(name, "day_of_week", length))
        return RA_MESSAGE_TEMPLATE_WEEKDAY_MAX;
    if (length == 4 && !memcmp(name, "date", length))
        return RA_MESSAGE_TEMPLATE_DATE_MAX;
    if (length == 4 && !memcmp(name, "time", length))
        return RA_MESSAGE_TEMPLATE_TIME_MAX;
    if (length == 8 && !memcmp(name, "greeting", length))
        return RA_MESSAGE_TEMPLATE_GREETING_MAX;
    if (length == 11 && !memcmp(name, "link_status", length))
        return RA_MESSAGE_TEMPLATE_LINK_STATUS_MAX;
    if (length == 4 && !memcmp(name, "node", length))
        return RA_NODE_NAME_MAX - 1U;
    if (length == 8 && !memcmp(name, "callsign", length))
        return RA_NODE_NAME_MAX - 1U;
    return 0;
}

/** @brief Check one exact approved substitution name without accepting aliases.
 * @param name Candidate substitution name without delimiters.
 * @param length Bytes in @p name.
 * @return True only for one documented substitution name.
 */
static bool valid_name(const char *name, size_t length) {
    return maximum_value_length(name, length) != 0;
}

/** @brief Resolve one approved substitution to its current non-null value.
 * @param name Validated substitution name without delimiters.
 * @param length Bytes in @p name.
 * @param values Current non-null replacement values.
 * @return Borrowed replacement text.
 */
static const char *value(const char *name, size_t length,
                         const struct ra_message_template_values *values) {
    /* The renderer's syntax check guarantees an approved name; only date/day share an initial. */
    if (*name == 'd')
        return length == 4 ? values->date : values->day_of_week;
    if (*name == 't')
        return values->time;
    if (*name == 'g')
        return values->greeting;
    if (*name == 'l')
        return values->link_status;
    if (*name == 'n')
        return values->node;
    return values->callsign;
}

bool ra_message_template_validate(const char *template_text) {
    if (!template_text)
        return false;
    while (*template_text) {
        if (template_text[0] != '$' || template_text[1] != '{') {
            ++template_text;
            continue;
        }
        const char *end = strchr(template_text + 2, '}');
        if (!end || end == template_text + 2 ||
            !valid_name(template_text + 2, (size_t)(end - template_text - 2)))
            return false;
        template_text = end + 1;
    }
    return true;
}

/** @brief Add one replacement or literal width while reserving the output terminator.
 * @param used Bytes already proven to fit.
 * @param added Additional payload bytes.
 * @return True when the combined text remains strictly smaller than the output capacity.
 */
static bool add_bounded_length(size_t *used, size_t added) {
    if (added >= RA_MESSAGE_TEMPLATE_OUTPUT_MAX - *used)
        return false;
    *used += added;
    return true;
}

bool ra_message_template_validate_output(const char *template_text) {
    if (!ra_message_template_validate(template_text))
        return false;
    size_t used = 0;
    while (*template_text) {
        if (template_text[0] != '$' || template_text[1] != '{') {
            if (!add_bounded_length(&used, 1))
                return false;
            ++template_text;
            continue;
        }
        /* Syntax validation above guarantees the matching terminator. */
        const char *end = strchr(template_text + 2, '}');
        size_t maximum = maximum_value_length(template_text + 2, (size_t)(end - template_text - 2));
        if (!add_bounded_length(&used, maximum))
            return false;
        template_text = end + 1;
    }
    return true;
}

/** @brief Append one complete non-null string, retaining an output terminator.
 * @param output Destination buffer.
 * @param capacity Destination capacity including its terminator.
 * @param used Current written payload bytes, updated only after a successful append.
 * @param text Non-null text to append.
 * @return True only when the complete text and terminator fit.
 */
static bool append(char *output, size_t capacity, size_t *used, const char *text) {
    size_t length = strlen(text);
    if (length >= capacity - *used)
        return false;
    for (size_t index = 0; index < length; ++index)
        output[*used + index] = text[index];
    *used += length;
    output[*used] = '\0';
    return true;
}

bool ra_message_template_render(const char *template_text,
                                const struct ra_message_template_values *values, char *output,
                                size_t capacity) {
    if (!ra_message_template_validate(template_text) || !values || !output || !capacity)
        return false;
    size_t used = 0;
    output[0] = '\0';
    while (*template_text) {
        if (template_text[0] != '$' || template_text[1] != '{') {
            if (used + 1 >= capacity)
                return false;
            output[used++] = *template_text++;
            output[used] = '\0';
            continue;
        }
        const char *end = strchr(template_text + 2, '}');
        const char *replacement =
            value(template_text + 2, (size_t)(end - template_text - 2), values);
        if (!append(output, capacity, &used, replacement))
            return false;
        template_text = end + 1;
    }
    return true;
}
