/* SPDX-License-Identifier: GPL-2.0-only */
/** @file
 * @brief Static, DNS, and external-directory ordering plus numeric identity tests.
 */
#include "link_directory.h"
#include <assert.h>
#include <asterisk.h>
#include <asterisk/config.h>
#include <asterisk/netsock2.h>
#include <asterisk/srv.h>
#include <stdarg.h>
#include <stdlib.h>
#include <string.h>

/** @brief Config-load states modeled by the directory fixture. */
enum config_state {
    CONFIG_MISSING,   /**< Asterisk could not open the file. */
    CONFIG_INVALID,   /**< Asterisk rejected the file. */
    CONFIG_UNCHANGED, /**< Asterisk reported an unchanged configuration. */
    CONFIG_VALID      /**< Asterisk supplied a readable `[extnodes]` record. */
};

/** @brief One independently loadable local or external directory source. */
struct directory_fixture {
    const char *path;        /**< Exact configured path. */
    int configuration;       /**< Opaque unique Asterisk configuration identity. */
    enum config_state state; /**< Selected file-load outcome. */
    const char *record;      /**< Optional entry for node 123. */
    unsigned int loads;      /**< Source-load count for lookup-order assertions. */
};

/** @brief Fixture for a local-priority static override source. */
static struct directory_fixture static_directory = {.path = "static.conf"};
/** @brief Fixture for the selected ASL external source. */
static struct directory_fixture external_directory = {.path = "external.conf"};
/** @brief Formatting call count. */
static unsigned int formatted;
/** @brief Selected formatting allocation failure. */
static unsigned int fail_format;
/** @brief SRV failure selects address-only fallback. */
static bool fail_srv;
/** @brief Number of fixture DNS addresses to return. */
static int address_count;
/** @brief DNS lookup count for source-order assertions. */
static unsigned int dns_calls;

/** @brief Map a configured filename to its fixture state.
 * @param filename Requested Asterisk configuration path.
 * @return Matching fixture, or null for an unexpected path.
 */
static struct directory_fixture *directory_for(const char *filename) {
    if (!strcmp(filename, static_directory.path)) {
        return &static_directory;
    }
    if (!strcmp(filename, external_directory.path)) {
        return &external_directory;
    }
    return NULL;
}

/** @brief Reset all source and resolver observations before one independent request. */
static void reset(void) {
    static_directory.state = CONFIG_VALID;
    static_directory.record = NULL;
    static_directory.loads = 0;
    external_directory.state = CONFIG_VALID;
    external_directory.record = NULL;
    external_directory.loads = 0;
    formatted = 0;
    fail_format = 0;
    fail_srv = false;
    address_count = 2;
    dns_calls = 0;
}

/** @brief Model optional Asterisk configuration loading.
 * @param filename Configured path.
 * @param who_asked Module identity.
 * @param flags Load flags.
 * @return Selected source configuration or its requested failure indicator.
 */
struct ast_config *ast_config_load2(const char *filename, const char *who_asked,
                                    struct ast_flags flags) {
    (void)flags;
    assert(!strcmp(who_asked, "rpt_advanced"));
    struct directory_fixture *fixture = directory_for(filename);
    assert(fixture);
    ++fixture->loads;
    switch (fixture->state) {
    case CONFIG_MISSING:
        return NULL;
    case CONFIG_INVALID:
        return CONFIG_STATUS_FILEINVALID;
    case CONFIG_UNCHANGED:
        return CONFIG_STATUS_FILEUNCHANGED;
    case CONFIG_VALID:
        return (struct ast_config *)&fixture->configuration;
    }
    assert(false);
    return NULL;
}

/** @brief Return the selected source's optional record.
 * @param config Fixture configuration identity.
 * @param category ASL external-node section.
 * @param variable Requested node identity.
 * @return Borrowed record or null.
 */
const char *ast_variable_retrieve(struct ast_config *config, const char *category,
                                  const char *variable) {
    assert(!strcmp(category, "extnodes") && !strcmp(variable, "123"));
    if (config == (struct ast_config *)&static_directory.configuration) {
        return static_directory.record;
    }
    assert(config == (struct ast_config *)&external_directory.configuration);
    return external_directory.record;
}

/** @brief Verify configuration ownership cleanup.
 * @param config Fixture configuration.
 */
void ast_config_destroy(struct ast_config *config) {
    assert(config == (struct ast_config *)&static_directory.configuration ||
           config == (struct ast_config *)&external_directory.configuration);
}

/** @brief Represent numeric addresses by distinct opaque fixture values.
 * @param address Receives fixture identity.
 * @param text Numeric address.
 * @param flags Port-forbidden policy.
 * @return One for recognized numeric addresses, zero otherwise.
 */
int ast_sockaddr_parse(struct ast_sockaddr *address, const char *text, int flags) {
    assert(flags == PARSE_PORT_FORBID);
    if (strcmp(text, "192.0.2.1") && strcmp(text, "192.0.2.2")) {
        return 0;
    }
    address->len = !strcmp(text, "192.0.2.1") ? 1 : 2;
    return 1;
}

/** @brief Compare fixture identities without considering a transport port.
 * @param first First address.
 * @param second Second address.
 * @return Zero for equal addresses.
 */
int ast_sockaddr_cmp_addr(const struct ast_sockaddr *first, const struct ast_sockaddr *second) {
    return first->len != second->len;
}

/** @brief Return a fixture SRV target or request address-only fallback.
 * @param context Receives resolver ownership marker.
 * @param service Exact ASL query.
 * @param host Receives target.
 * @param port Receives custom transport port.
 * @return Zero on success, minus one on failure.
 */
int ast_srv_lookup(struct srv_context **context, const char *service, const char **host,
                   unsigned short *port) {
    assert(!strcmp(service, "_iax._udp.123.nodes.allstarlink.org"));
    *context = (struct srv_context *)&static_directory.configuration;
    *host = "node.example";
    *port = 4570;
    ++dns_calls;
    return fail_srv ? -1 : 0;
}

/** @brief Verify resolver ownership cleanup.
 * @param context Owned resolver marker.
 */
void ast_srv_cleanup(struct srv_context **context) {
    assert(*context == (struct srv_context *)&static_directory.configuration);
    *context = NULL;
}

/** @brief Return configured candidate DNS addresses.
 * @param addresses Receives owned address array.
 * @param host SRV target or default ASL address name.
 * @param flags Port-forbidden parsing.
 * @param family Both IP families allowed.
 * @return Configured address count.
 */
int ast_sockaddr_resolve(struct ast_sockaddr **addresses, const char *host, int flags, int family) {
    assert(!strcmp(host, fail_srv ? "123.nodes.allstarlink.org" : "node.example"));
    assert(flags == PARSE_PORT_FORBID && family == AST_AF_UNSPEC);
    *addresses = calloc(2, sizeof(**addresses));
    assert(*addresses);
    (*addresses)[0].len = 1;
    (*addresses)[1].len = 2;
    return address_count;
}

/** @brief Validate the selected SRV or default port.
 * @param address Selected address.
 * @param port Transport port.
 * @param file Caller file.
 * @param line Caller line.
 * @param func Caller function.
 */
void _ast_sockaddr_set_port(struct ast_sockaddr *address, uint16_t port, const char *file, int line,
                            const char *func) {
    (void)file;
    (void)line;
    (void)func;
    assert(address && port == (fail_srv ? 4569 : 4570));
}

/** @brief Supply canonical address/port syntax.
 * @param address Selected address.
 * @param format Required combined representation.
 * @return Stable fixture string.
 */
char *ast_sockaddr_stringify_fmt(const struct ast_sockaddr *address, int format) {
    assert(format == AST_SOCKADDR_STR_DEFAULT);
    return address->len == 1 ? "192.0.2.1:4570" : "192.0.2.2:4570";
}

/** @brief Inject failure at each allocating-format boundary.
 * @param file Caller file.
 * @param line Caller line.
 * @param function Caller function.
 * @param result Receives owned string.
 * @param format Formatting string.
 * @param ... Formatting arguments.
 * @return Length or minus one.
 */
int __ast_asprintf(const char *file, int line, const char *function, char **result,
                   const char *format, ...) {
    (void)file;
    (void)line;
    (void)function;
    if (++formatted == fail_format) {
        return -1;
    }
    va_list arguments;
    va_start(arguments, format);
    int length = vasprintf(result, format, arguments);
    va_end(arguments);
    return length;
}

/** @brief Release resolver memory.
 * @param pointer Owned allocation.
 * @param file Caller file.
 * @param line Caller line.
 * @param function Caller function.
 */
void __ast_free(void *pointer, const char *file, int line, const char *function) {
    (void)file;
    (void)line;
    (void)function;
    free(pointer);
}

/** @brief Resolve one node with a selected static/external policy.
 * @param method DNS/file selection after static lookup.
 * @param static_file Optional local-priority source.
 * @param external_file Optional external source.
 * @param ip Optional incoming numeric address.
 * @return True only when a source produces a valid destination.
 */
static bool lookup(enum ra_link_lookup_method method, const char *static_file,
                   const char *external_file, const char *ip) {
    const struct ra_link_directory_policy policy = {
        .static_file = static_file, .external_file = external_file, .method = method};
    char *result = ra_link_directory_lookup("123", ip, &policy);
    bool found = result != NULL;
    free(result);
    return found;
}

/** @brief Verify argument validation before any directory source is contacted. */
static void invalid_arguments(void) {
    reset();
    const struct ra_link_directory_policy policy = {
        .static_file = NULL, .external_file = NULL, .method = RA_LINK_LOOKUP_BOTH};
    assert(!ra_link_directory_lookup("123", NULL, NULL));
    struct ra_link_directory_policy invalid = policy;
    invalid.method = (enum ra_link_lookup_method)99;
    assert(!ra_link_directory_lookup("123", NULL, &invalid));
    assert(!ra_link_directory_lookup("", NULL, &policy));
    assert(!ra_link_directory_lookup("not-a-node", NULL, &policy));
    assert(!ra_link_directory_lookup(
        "1234567890123456789012345678901234567890123456789012345678901234", NULL, &policy));
    assert(!ra_link_directory_lookup("123", "hostname", &policy));
    assert(!static_directory.loads && !external_directory.loads && !dns_calls);
}

/** @brief Verify DNS selection, matching, mismatch rejection, and address-only fallback. */
static void dns_resolution(void) {
    reset();
    assert(lookup(RA_LINK_LOOKUP_DNS, NULL, NULL, NULL));
    assert(!static_directory.loads && !external_directory.loads && dns_calls == 1);
    reset();
    assert(lookup(RA_LINK_LOOKUP_DNS, "", "", "192.0.2.2"));
    reset();
    address_count = 1;
    assert(!lookup(RA_LINK_LOOKUP_DNS, "", "", "192.0.2.2"));
    reset();
    address_count = 0;
    assert(!lookup(RA_LINK_LOOKUP_DNS, "", "", NULL));
    reset();
    address_count = 0;
    assert(!lookup(RA_LINK_LOOKUP_DNS, "", "", "192.0.2.2"));
    reset();
    fail_srv = true;
    assert(lookup(RA_LINK_LOOKUP_DNS, "", "", NULL));
    reset();
    fail_format = 1;
    assert(!lookup(RA_LINK_LOOKUP_DNS, "", "", NULL));
    reset();
    fail_format = 2;
    assert(!lookup(RA_LINK_LOOKUP_DNS, "", "", NULL));
}

/** @brief Verify file-only lookup and every unavailable external-file state. */
static void external_file_resolution(void) {
    reset();
    external_directory.record = "radio@192.0.2.1:4569/123,192.0.2.1";
    assert(lookup(RA_LINK_LOOKUP_FILE, "", external_directory.path, NULL));
    assert(!static_directory.loads && external_directory.loads == 1 && !dns_calls);
    reset();
    external_directory.record = "radio@192.0.2.1:4569/123,192.0.2.1";
    assert(lookup(RA_LINK_LOOKUP_FILE, "", external_directory.path, "192.0.2.1"));
    reset();
    external_directory.record = "radio@192.0.2.1:4569/123,192.0.2.1";
    assert(!lookup(RA_LINK_LOOKUP_FILE, "", external_directory.path, "192.0.2.2"));
    assert(!dns_calls);
    reset();
    external_directory.record = "radio@192.0.2.1:4569/124,192.0.2.1";
    assert(!lookup(RA_LINK_LOOKUP_FILE, "", external_directory.path, NULL));
    assert(external_directory.loads == 1 && !dns_calls);
    for (enum config_state state = CONFIG_MISSING; state <= CONFIG_UNCHANGED; ++state) {
        reset();
        external_directory.state = state;
        assert(!lookup(RA_LINK_LOOKUP_FILE, "", external_directory.path, NULL));
        assert(external_directory.loads == 1 && !dns_calls);
    }
    reset();
    assert(!lookup(RA_LINK_LOOKUP_FILE, "", external_directory.path, NULL));
    assert(external_directory.loads == 1 && !dns_calls);
    reset();
    assert(!lookup(RA_LINK_LOOKUP_FILE, "", NULL, NULL));
    reset();
    external_directory.record = "radio@192.0.2.1:4569/123,192.0.2.1";
    fail_format = 1;
    assert(!lookup(RA_LINK_LOOKUP_FILE, "", external_directory.path, NULL));
}

/** @brief Verify a static entry wins and malformed or mismatched entries fail closed. */
static void static_resolution(void) {
    reset();
    static_directory.record = "radio@192.0.2.1:4569/123,192.0.2.1";
    external_directory.record = "radio@192.0.2.2:4569/123,192.0.2.2";
    assert(lookup(RA_LINK_LOOKUP_BOTH, static_directory.path, external_directory.path, NULL));
    assert(static_directory.loads == 1 && !external_directory.loads && !dns_calls);
    const char *invalid[] = {"radio@missing",
                             "other@host,192.0.2.1",
                             "radio@host,192.0.2.1 ",
                             "radio@ho st/123,192.0.2.1",
                             "radio@host/123,hostname",
                             "radio@192.0.2.1:4569/124,192.0.2.1",
                             "radio@hostname123,192.0.2.1",
                             "radio@/123,192.0.2.1"};
    for (size_t index = 0; index < sizeof(invalid) / sizeof(*invalid); ++index) {
        reset();
        static_directory.record = invalid[index];
        assert(!lookup(RA_LINK_LOOKUP_BOTH, static_directory.path, external_directory.path, NULL));
        assert(static_directory.loads == 1 && !external_directory.loads && !dns_calls);
    }
    reset();
    static_directory.record = "radio@192.0.2.1:4569/123,192.0.2.1";
    assert(
        !lookup(RA_LINK_LOOKUP_BOTH, static_directory.path, external_directory.path, "192.0.2.2"));
    assert(static_directory.loads == 1 && !external_directory.loads && !dns_calls);
    reset();
    static_directory.record = "radio@192.0.2.1:4569/123,192.0.2.1";
    fail_format = 1;
    assert(!lookup(RA_LINK_LOOKUP_BOTH, static_directory.path, external_directory.path, NULL));
    assert(static_directory.loads == 1 && !external_directory.loads && !dns_calls);
    for (enum config_state state = CONFIG_MISSING; state <= CONFIG_UNCHANGED; ++state) {
        reset();
        static_directory.state = state;
        assert(lookup(RA_LINK_LOOKUP_DNS, static_directory.path, NULL, NULL));
        assert(static_directory.loads == 1 && dns_calls == 1);
    }
}

/** @brief Verify `both` uses DNS before the external file and cannot bypass a DNS mismatch. */
static void combined_resolution(void) {
    reset();
    external_directory.record = "radio@192.0.2.2:4569/123,192.0.2.2";
    assert(lookup(RA_LINK_LOOKUP_BOTH, "", external_directory.path, NULL));
    assert(dns_calls == 1 && !external_directory.loads);
    reset();
    address_count = 0;
    external_directory.record = "radio@192.0.2.2:4569/123,192.0.2.2";
    assert(lookup(RA_LINK_LOOKUP_BOTH, "", external_directory.path, NULL));
    assert(dns_calls == 1 && external_directory.loads == 1);
    reset();
    address_count = 1;
    external_directory.record = "radio@192.0.2.2:4569/123,192.0.2.2";
    assert(!lookup(RA_LINK_LOOKUP_BOTH, "", external_directory.path, "192.0.2.2"));
    assert(dns_calls == 1 && !external_directory.loads);
}

/** @brief Run every static/DNS/external lookup-policy test.
 * @return Zero after all assertions.
 */
int main(void) {
    invalid_arguments();
    dns_resolution();
    external_file_resolution();
    static_resolution();
    combined_resolution();
    return 0;
}
