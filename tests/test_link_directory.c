/* SPDX-License-Identifier: GPL-2.0-only */
/** @file
 * @brief Directory precedence, numeric identity checks, and resolver failure cleanup.
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

/** @brief Opaque configuration identity. */
static int configuration;
/** @brief Configuration load status: missing, invalid, unchanged, or valid. */
static unsigned int config_status;
/** @brief Optional static directory record. */
static const char *record;
/** @brief Formatting call count. */
static unsigned int formatted;
/** @brief Selected formatting allocation failure. */
static unsigned int fail_format;
/** @brief SRV failure selects address-only fallback. */
static bool fail_srv;
/** @brief Number of fixture DNS addresses to return. */
static int address_count = 2;
/** @brief Track resolver invocation, proving invalid static entries fail closed. */
static unsigned int dns_calls;

/** @brief Model optional directory loading.
 * @param filename Configured path.
 * @param who_asked Module identity.
 * @param flags Load flags.
 * @return Selected status or fixture.
 */
struct ast_config *ast_config_load2(const char *filename, const char *who_asked,
                                    struct ast_flags flags) {
    (void)flags;
    assert(!strcmp(filename, "nodes.conf") && !strcmp(who_asked, "rpt_advanced"));
    if (!config_status) {
        return NULL;
    }
    if (config_status == 1) {
        return CONFIG_STATUS_FILEINVALID;
    }
    if (config_status == 2) {
        return CONFIG_STATUS_FILEUNCHANGED;
    }
    return (struct ast_config *)&configuration;
}

/** @brief Return a selected directory record.
 * @param config Fixture configuration.
 * @param category ASL external-node section.
 * @param variable Requested node.
 * @return Borrowed record or null.
 */
const char *ast_variable_retrieve(struct ast_config *config, const char *category,
                                  const char *variable) {
    assert(config == (struct ast_config *)&configuration);
    assert(!strcmp(category, "extnodes") && !strcmp(variable, "123"));
    return record;
}

/** @brief Verify configuration ownership cleanup.
 * @param config Fixture configuration.
 */
void ast_config_destroy(struct ast_config *config) {
    assert(config == (struct ast_config *)&configuration);
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
    *context = (struct srv_context *)&configuration;
    *host = "node.example";
    *port = 4570;
    ++dns_calls;
    return fail_srv ? -1 : 0;
}

/** @brief Verify resolver ownership cleanup.
 * @param context Owned resolver marker.
 */
void ast_srv_cleanup(struct srv_context **context) {
    assert(*context == (struct srv_context *)&configuration);
    *context = NULL;
}

/** @brief Return two candidate source addresses or no resolution.
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
    assert(address && format == AST_SOCKADDR_STR_DEFAULT);
    return "192.0.2.1:4570";
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
    va_list args;
    va_start(args, format);
    int length = vasprintf(result, format, args);
    va_end(args);
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

/** @brief Check lookup outcome and release successful destinations.
 * @param ip Optional source address.
 * @param file Optional directory filename.
 * @param success Expected lookup result.
 */
static void expect(const char *ip, const char *file, bool success) {
    formatted = 0;
    char *result = ra_link_directory_lookup("123", ip, file);
    assert((result != NULL) == success);
    free(result);
}

/** @brief Exercise lookup precedence, malformed identities, and every failure boundary.
 * @return Zero after assertions.
 */
int main(void) {
    assert(!ra_link_directory_lookup("", NULL, ""));
    assert(!ra_link_directory_lookup("not-a-node", NULL, ""));
    assert(!ra_link_directory_lookup(
        "1234567890123456789012345678901234567890123456789012345678901234", NULL, ""));
    expect("hostname", "", false);
    expect(NULL, "", true);
    expect("192.0.2.2", "", true);
    address_count = 1;
    expect("192.0.2.2", "", false);
    address_count = 0;
    expect(NULL, "", false);
    address_count = 2;
    fail_srv = true;
    expect(NULL, "", true);
    fail_srv = false;
    for (fail_format = 1; fail_format <= 2; ++fail_format) {
        expect(NULL, "", false);
    }
    fail_format = 0;
    for (config_status = 0; config_status <= 3; ++config_status) {
        expect(NULL, "nodes.conf", true);
    }
    config_status = 3;
    const char *invalid[] = {"radio@missing", "other@host,192.0.2.1", "radio@host,192.0.2.1 ",
                             "radio@host,hostname"};
    unsigned int before = dns_calls;
    for (size_t i = 0; i < sizeof(invalid) / sizeof(*invalid); ++i) {
        record = invalid[i];
        expect(NULL, "nodes.conf", false);
    }
    record = "radio@192.0.2.1:4569/123,192.0.2.1";
    expect(NULL, "nodes.conf", true);
    expect("192.0.2.1", "nodes.conf", true);
    expect("192.0.2.2", "nodes.conf", false);
    fail_format = 1;
    expect(NULL, "nodes.conf", false);
    assert(dns_calls == before);
    return 0;
}
