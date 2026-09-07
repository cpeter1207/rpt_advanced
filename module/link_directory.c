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

/** @brief Read a static ASL directory entry; an invalid present entry blocks DNS fallback.
 * @param path Directory path.
 * @param node Remote node identity.
 * @param source Optional numeric address to verify.
 * @param present Set when the directory defines this node, even if invalid.
 * @return Owned IAX destination or null.
 */
static char *from_file(const char *path, const char *node, const struct ast_sockaddr *source,
                       bool *present) {
    struct ast_config *config = ast_config_load2(path, "rpt_advanced", (struct ast_flags){0});
    if (!config || config == CONFIG_STATUS_FILEINVALID || config == CONFIG_STATUS_FILEUNCHANGED) {
        return NULL;
    }
    const char *value = ast_variable_retrieve(config, "extnodes", node);
    char *destination = NULL;
    if (value) {
        *present = true;
        const char *comma = strchr(value, ',');
        if (comma && !strncmp(value, "radio@", 6) && !strpbrk(value, " \t\r\n")) {
            struct ast_sockaddr expected;
            if (ast_sockaddr_parse(&expected, comma + 1, PARSE_PORT_FORBID) &&
                (!source || !ast_sockaddr_cmp_addr(source, &expected))) {
                if (ast_asprintf(&destination, "%.*s", (int)(comma - value), value) < 0) {
                    destination = NULL;
                }
            }
        }
    }
    ast_config_destroy(config);
    return destination;
}

char *ra_link_directory_lookup(const char *node, const char *peer_ip, const char *directory_file) {
    size_t length = strlen(node);
    if (!length || length > 63 || strspn(node, "0123456789") != length) {
        return NULL;
    }
    struct ast_sockaddr source;
    if (peer_ip && !ast_sockaddr_parse(&source, peer_ip, PARSE_PORT_FORBID)) {
        return NULL;
    }
    if (*directory_file) {
        bool present = false;
        char *destination = from_file(directory_file, node, peer_ip ? &source : NULL, &present);
        if (present) {
            return destination;
        }
    }
    char *domain = NULL;
    if (ast_asprintf(&domain, "_iax._udp.%s.nodes.allstarlink.org", node) < 0) {
        return NULL;
    }
    struct srv_context *context = NULL;
    const char *host = NULL;
    unsigned short port = 4569;
    int found = ast_srv_lookup(&context, domain, &host, &port);
    /* Older directory entries may publish only an address and use the standard port. */
    if (found) {
        host = domain + strlen("_iax._udp.");
        port = 4569;
    }
    struct ast_sockaddr *addresses = NULL;
    int count = ast_sockaddr_resolve(&addresses, host, PARSE_PORT_FORBID, AST_AF_UNSPEC);
    char *destination = NULL;
    for (int i = 0; i < count; ++i) {
        if (!peer_ip || !ast_sockaddr_cmp_addr(&source, &addresses[i])) {
            ast_sockaddr_set_port(&addresses[i], port);
            if (ast_asprintf(&destination, "radio@%s/%s", ast_sockaddr_stringify(&addresses[i]),
                             node) < 0) {
                destination = NULL;
            }
            break;
        }
    }
    ast_free(addresses);
    ast_srv_cleanup(&context);
    ast_free(domain);
    return destination;
}
