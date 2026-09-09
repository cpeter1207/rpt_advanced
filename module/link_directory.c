/* SPDX-License-Identifier: GPL-2.0-only */
/** @file
 * @brief Resolve registered ASL nodes without treating caller ID as authentication.
 */
#include "link_directory.h"
#include <asterisk.h>
#include <asterisk/channel.h>
#include <asterisk/config.h>
#include <asterisk/netsock2.h>
#include <asterisk/srv.h>
#include <string.h>

/** @brief Distinguish an unavailable source from a record that must fail closed. */
enum directory_result {
    DIRECTORY_ABSENT,   /**< The source has no usable record for this node. */
    DIRECTORY_RESOLVED, /**< A verified record produced an owned destination. */
    DIRECTORY_REJECTED  /**< A present record is malformed or conflicts with the source address. */
};

/** @brief Verify that an external-node dial target belongs to the requested node identity.
 * @param value Complete untrusted `radio@.../node,address` record.
 * @param comma Delimiter between the dial target and numeric address.
 * @param node Requested decimal node identity.
 * @return True only when the target has a nonempty host and ends exactly in `/node`.
 *
 * An `[extnodes]` key is not sufficient identity proof: a stale or incorrectly copied record
 * could otherwise route one requested node to another node's dial target.
 */
static bool target_matches_node(const char *value, const char *comma, const char *node) {
    size_t target_length = (size_t)(comma - value);
    size_t node_length = strlen(node);
    if (target_length <= sizeof("radio@/") - 1 + node_length) {
        return false;
    }
    const char *suffix = comma - node_length;
    return suffix[-1] == '/' && !memcmp(suffix, node, node_length);
}

/** @brief Resolve one `[extnodes]` source, preserving an explicit record's authority.
 * @param path Optional Asterisk configuration path.
 * @param node Requested remote identity.
 * @param source Incoming numeric address to verify, or null for an outbound lookup.
 * @param destination Receives an owned IAX destination only on success.
 * @return Whether the source was absent, resolved, or must reject the request.
 *
 * Both local overrides and ASL external files use the documented `number=radio@host:port/number,
 * numeric-address` record syntax. Once a source declares a node, accepting a later source after
 * malformed data or an address mismatch could turn a stale or hostile record into an authorization
 * bypass, so that case is deliberately terminal.
 */
static enum directory_result resolve_file(const char *path, const char *node,
                                          const struct ast_sockaddr *source, char **destination) {
    *destination = NULL;
    if (!path || !*path) {
        return DIRECTORY_ABSENT;
    }
    struct ast_config *config = ast_config_load2(path, "rpt_advanced", (struct ast_flags){0});
    if (!config || config == CONFIG_STATUS_FILEINVALID || config == CONFIG_STATUS_FILEUNCHANGED) {
        return DIRECTORY_ABSENT;
    }
    const char *value = ast_variable_retrieve(config, "extnodes", node);
    enum directory_result result = DIRECTORY_ABSENT;
    if (value) {
        const char *comma = strchr(value, ',');
        struct ast_sockaddr expected;
        if (!comma || strncmp(value, "radio@", 6) || !target_matches_node(value, comma, node) ||
            strpbrk(value, " \t\r\n") ||
            !ast_sockaddr_parse(&expected, comma + 1, PARSE_PORT_FORBID) ||
            (source && ast_sockaddr_cmp_addr(source, &expected))) {
            result = DIRECTORY_REJECTED;
        } else if (ast_asprintf(destination, "%.*s", (int)(comma - value), value) < 0) {
            *destination = NULL;
            result = DIRECTORY_REJECTED;
        } else {
            result = DIRECTORY_RESOLVED;
        }
    }
    ast_config_destroy(config);
    return result;
}

/** @brief Resolve one ASL DNS record while distinguishing no record from an address mismatch.
 * @param node Requested remote identity.
 * @param source Incoming numeric address to verify, or null for an outbound lookup.
 * @param destination Receives an owned IAX destination only on success.
 * @return Whether DNS was absent, resolved, or contradicted the claimed source.
 */
static enum directory_result resolve_dns(const char *node, const struct ast_sockaddr *source,
                                         char **destination) {
    *destination = NULL;
    char *domain = NULL;
    if (ast_asprintf(&domain, "_iax._udp.%s.nodes.allstarlink.org", node) < 0) {
        return DIRECTORY_REJECTED;
    }
    struct srv_context *context = NULL;
    const char *host = NULL;
    unsigned short port = 4569;
    int srv_result = ast_srv_lookup(&context, domain, &host, &port);
    if (srv_result) {
        host = domain + strlen("_iax._udp.");
        port = 4569;
    }
    struct ast_sockaddr *addresses = NULL;
    int count = ast_sockaddr_resolve(&addresses, host, PARSE_PORT_FORBID, AST_AF_UNSPEC);
    enum directory_result result = DIRECTORY_ABSENT;
    for (int index = 0; index < count; ++index) {
        if (source && ast_sockaddr_cmp_addr(source, &addresses[index])) {
            continue;
        }
        ast_sockaddr_set_port(&addresses[index], port);
        if (ast_asprintf(destination, "radio@%s/%s", ast_sockaddr_stringify(&addresses[index]),
                         node) < 0) {
            *destination = NULL;
            result = DIRECTORY_REJECTED;
        } else {
            result = DIRECTORY_RESOLVED;
        }
        break;
    }
    if (result == DIRECTORY_ABSENT && source && count > 0) {
        result = DIRECTORY_REJECTED;
    }
    ast_free(addresses);
    ast_srv_cleanup(&context);
    ast_free(domain);
    return result;
}

/** @brief Return an owned resolution only when a source positively matched it.
 * @param result Completed source lookup result.
 * @param destination Owned source destination, if any.
 * @return Destination on success, otherwise null after preserving fail-closed behavior.
 */
static char *resolved_destination(enum directory_result result, char *destination) {
    if (result == DIRECTORY_RESOLVED) {
        return destination;
    }
    ast_free(destination);
    return NULL;
}

char *ra_link_directory_lookup(const char *node, const char *peer_ip,
                               const struct ra_link_directory_policy *policy) {
    if (!policy || (policy->method != RA_LINK_LOOKUP_BOTH && policy->method != RA_LINK_LOOKUP_DNS &&
                    policy->method != RA_LINK_LOOKUP_FILE)) {
        return NULL;
    }
    size_t length = strlen(node);
    if (!length || length > 63 || strspn(node, "0123456789") != length) {
        return NULL;
    }
    struct ast_sockaddr source;
    const struct ast_sockaddr *expected = NULL;
    if (peer_ip) {
        if (!ast_sockaddr_parse(&source, peer_ip, PARSE_PORT_FORBID)) {
            return NULL;
        }
        expected = &source;
    }
    char *destination = NULL;
    enum directory_result result = resolve_file(policy->static_file, node, expected, &destination);
    if (result != DIRECTORY_ABSENT) {
        return resolved_destination(result, destination);
    }
    if (policy->method != RA_LINK_LOOKUP_FILE) {
        result = resolve_dns(node, expected, &destination);
        if (result != DIRECTORY_ABSENT) {
            return resolved_destination(result, destination);
        }
    }
    if (policy->method != RA_LINK_LOOKUP_DNS) {
        result = resolve_file(policy->external_file, node, expected, &destination);
        return resolved_destination(result, destination);
    }
    return NULL;
}
