/* SPDX-License-Identifier: GPL-2.0-only */
/**
 * @file
 * @brief Exercise strict scheduled-message substitution rendering.
 */
#include "message_template.h"
#include <assert.h>
#include <stdio.h>
#include <string.h>

/** @brief Exercise valid, invalid, bounded, and rendered template cases.
 * @return Zero after all assertions pass.
 */
int main(void) {
    const struct ra_message_template_values values = {
        "Wednesday", "2026-09-10", "8:15 PM", "Good Evening", "no links", "524950", "KG0BP"};
    char output[128];
    assert(ra_message_template_validate("${day_of_week} ${date} ${time}"));
    assert(ra_message_template_validate("plain text"));
    assert(ra_message_template_validate("$x"));
    assert(!ra_message_template_validate(NULL));
    assert(!ra_message_template_validate("${unknown}"));
    assert(!ra_message_template_validate("${xxxxxxxxxxx}"));
    assert(!ra_message_template_validate("${xxxx}"));
    assert(!ra_message_template_validate("${xxxxxxxx}"));
    assert(!ra_message_template_validate("${node"));
    assert(!ra_message_template_validate("${}"));
    assert(ra_message_template_validate_output(""));
    assert(ra_message_template_validate_output("${day_of_week}"));
    assert(ra_message_template_validate_output("${date}"));
    assert(ra_message_template_validate_output("${time}"));
    assert(ra_message_template_validate_output("${greeting}"));
    assert(ra_message_template_validate_output("$x"));
    assert(ra_message_template_validate_output("${link_status} ${date} ${time}x"));
    assert(!ra_message_template_validate_output("${link_status} ${date} ${time}xx"));
    assert(!ra_message_template_validate_output("${link_status}${greeting}${date}"));
    assert(ra_message_template_validate_output("${node}${callsign}x"));
    assert(!ra_message_template_validate_output("${node}${callsign}xx"));
    assert(!ra_message_template_validate_output(NULL));
    assert(!ra_message_template_validate_output("${unknown}"));
    char largest[RA_MESSAGE_TEMPLATE_OUTPUT_MAX];
    char too_large[RA_MESSAGE_TEMPLATE_OUTPUT_MAX + 1];
    memset(largest, 'x', sizeof(largest) - 1);
    largest[sizeof(largest) - 1] = '\0';
    memset(too_large, 'x', sizeof(too_large) - 1);
    too_large[sizeof(too_large) - 1] = '\0';
    assert(ra_message_template_validate_output(largest));
    assert(!ra_message_template_validate_output(too_large));
    assert(ra_message_template_render("${day_of_week} ${date} ${time}", &values, output,
                                      sizeof(output)));
    assert(!strcmp(output, "Wednesday 2026-09-10 8:15 PM"));
    assert(ra_message_template_render("${greeting}. ${node} ${callsign}: ${link_status}.", &values,
                                      output, sizeof(output)));
    assert(!strcmp(output, "Good Evening. 524950 KG0BP: no links."));
    assert(ra_message_template_render("plain text", &values, output, sizeof(output)));
    assert(!strcmp(output, "plain text"));
    assert(ra_message_template_render("$x", &values, output, sizeof(output)));
    assert(!strcmp(output, "$x"));
    assert(ra_message_template_render("", &values, output, sizeof(output)));
    assert(!strcmp(output, ""));
    struct ra_message_template_values empty_callsign = values;
    empty_callsign.callsign = "";
    assert(ra_message_template_render("${callsign}", &empty_callsign, output, sizeof(output)));
    assert(!strcmp(output, ""));
    assert(!ra_message_template_render(NULL, &values, output, sizeof(output)));
    assert(!ra_message_template_render("text", NULL, output, sizeof(output)));
    assert(!ra_message_template_render("text", &values, NULL, sizeof(output)));
    assert(!ra_message_template_render("text", &values, output, 0));
    assert(!ra_message_template_render("${unknown}", &values, output, sizeof(output)));
    assert(!ra_message_template_render("${node", &values, output, sizeof(output)));
    assert(!ra_message_template_render("${}", &values, output, sizeof(output)));
    assert(!ra_message_template_render("${callsign}", &values, output, 4));
    assert(!ra_message_template_render("text", &values, output, 4));
    puts("message template tests passed");
    return 0;
}
