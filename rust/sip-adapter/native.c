/** @file
 * @brief Private Sofia-SIP 1.13.19 tags and synchronous callback translation.
 */
#define _GNU_SOURCE
#include "native.h"
#include <pthread.h>
#include <signal.h>
#include <sofia-sip/bnf.h>
#include <sofia-sip/nta_tag.h>
#include <sofia-sip/nua.h>
#include <sofia-sip/nua_tag.h>
#include <sofia-sip/sip_header.h>
#include <sofia-sip/sip_tag.h>
#include <sofia-sip/su.h>
#include <sofia-sip/su_string.h>
#include <sofia-sip/su_wait.h>
#include <sofia-sip/tport_tag.h>
#include <stdlib.h>
#include <string.h>

/** Private native state; account policy and event retention live in Rust. */
struct native {
    su_home_t home[1];      /**< Contact allocation owner. */
    su_root_t *root;        /**< Synchronous owner-thread event loop. */
    nua_t *nua;             /**< Native account agent. */
    nua_handle_t *account;  /**< Single activated registration handle. */
    sip_contact_t *contact; /**< Copied generated local Contact. */
    int contact_query_done; /**< Initial parameter query completed. */
    pthread_t owner;        /**< Creation and service thread identity. */
    sip_native_sink sink;   /**< Rust observation callback. */
    void *context;          /**< Stable Rust callback allocation. */
};

/** Preserve the embedding process's meaningful signal disposition exactly.
 * @param a Original signal disposition.
 * @param b Observed signal disposition.
 * @return Nonzero if all meaningful fields agree.
 */
static int same_action(const struct sigaction *a, const struct sigaction *b) {
    if (a->sa_handler != b->sa_handler || a->sa_flags != b->sa_flags) {
        return 0;
    }
    for (int number = 1; number < NSIG; ++number) {
        if (sigismember(&a->sa_mask, number) != sigismember(&b->sa_mask, number)) {
            return 0;
        }
    }
    return 1;
}

/** Reuse Sofia's parsed Digest realm and quoted-string grammar without auth parsing.
 * @param sip Borrowed native response, possibly NULL.
 * @param status Numeric response status.
 * @return Borrowed native-validated quoted realm of at most 256 bytes, or NULL.
 */
static const char *challenge_realm(const sip_t *sip, int status) {
    if (!sip || (status != 401 && status != 407)) {
        return NULL;
    }
    const msg_auth_t *challenge =
        status == 401 ? sip->sip_www_authenticate : sip->sip_proxy_authenticate;
    for (; challenge; challenge = challenge->au_next) {
        const char *realm = msg_header_find_param(challenge->au_common, "realm=");
        if (challenge->au_scheme && su_casematch(challenge->au_scheme, "Digest") && realm) {
            size_t length = strnlen(realm, 257);
            if (length <= 256 && span_quoted(realm) == (isize_t)length) {
                return realm;
            }
        }
    }
    return NULL;
}

/** Native media is disabled. Only bounded observations cross privately to Rust.
 * @param kind Native event discriminator.
 * @param status Numeric native status.
 * @param phrase Unused native reason, never logged.
 * @param nua Unused native agent.
 * @param magic Stable native context.
 * @param handle Unused native handle.
 * @param handle_magic Unused handle context.
 * @param sip Borrowed native response, possibly NULL.
 * @param tags Borrowed native callback tags.
 */
static void event(nua_event_t kind, int status, const char *phrase, nua_t *nua, nua_magic_t *magic,
                  nua_handle_t *handle, nua_hmagic_t *handle_magic, const sip_t *sip,
                  tagi_t tags[]) {
    struct native *state = (struct native *)magic;
    uint32_t expiry = 0;
    (void)phrase;
    (void)nua;
    (void)handle;
    (void)handle_magic;
    if (kind == nua_r_get_params && status >= 200) {
        const sip_contact_t *contact = NULL;
        state->contact_query_done = 1;
        if (status == 200) {
            tl_gets(tags, NTATAG_CONTACT_REF(contact), TAG_END());
            state->contact = sip_contact_dup(state->home, contact);
        }
    }
    if (kind == nua_r_register && sip && state->contact && status >= 200 && status < 300) {
        url_t reflected = *state->contact->m_url;
        if (sip->sip_via) {
            if (sip->sip_via->v_received) {
                reflected.url_host = sip->sip_via->v_received;
            }
            if (sip->sip_via->v_rport && *sip->sip_via->v_rport) {
                reflected.url_port = sip->sip_via->v_rport;
            }
        }
        for (const sip_contact_t *m = sip->sip_contact; m; m = m->m_next) {
            if (url_cmp(m->m_url, state->contact->m_url) == 0 ||
                url_cmp(m->m_url, &reflected) == 0) {
                sip_time_t accepted =
                    sip_contact_expires(m, sip->sip_expires, sip->sip_date, 0, sip_now());
                expiry = accepted <= 86400 ? (uint32_t)accepted : 0;
                break;
            }
        }
    }
    if (kind == nua_r_register || kind == nua_r_unregister || kind == nua_r_shutdown ||
        kind == nua_i_outbound || kind == nua_r_authenticate) {
        uint32_t translated = kind == nua_r_register     ? 1
                              : kind == nua_r_unregister ? 2
                              : kind == nua_r_shutdown   ? 3
                              : kind == nua_i_outbound   ? 4
                                                         : 5;
        const char *realm = challenge_realm(sip, status);
        state->sink(state->context, translated, (uint32_t)status, expiry, realm,
                    realm ? strlen(realm) : 0);
    }
}

/** Refuse environment-driven protocol dumps, capture and verbose authentication logs.
 * @return Nonzero if no protocol logging override is present.
 */
static int safe_logging(void) {
    const char *const names[] = {"MSG_STREAM_LOG", "TPORT_LOG",    "MSG_DUMP",  "TPORT_DUMP",
                                 "TPORT_CAPT",     "SOFIA_DEBUG",  "NUA_DEBUG", "NTA_DEBUG",
                                 "TPORT_DEBUG",    "IPTSEC_DEBUG", "MSG_DEBUG", "SOA_DEBUG",
                                 "NEA_DEBUG",      "STUN_DEBUG",   NULL};
    for (const char *const *name = names; *name; ++name) {
        if (getenv(*name)) {
            return 0;
        }
    }
    return 1;
}

void *sip_native_create(const char *bind, sip_native_sink sink, void *context) {
    struct sigaction original, observed;
    if (!safe_logging() || sigaction(SIGPIPE, NULL, &original) || original.sa_handler != SIG_IGN) {
        return NULL;
    }
    struct native *state = calloc(1, sizeof(*state));
    if (!state) {
        return NULL;
    }
    su_home_init(state->home);
    state->owner = pthread_self();
    state->sink = sink;
    state->context = context;
    if (su_init() != 0) {
        su_home_deinit(state->home);
        free(state);
        return NULL;
    }
    if (sigaction(SIGPIPE, NULL, &observed) ||
        (!same_action(&original, &observed) && sigaction(SIGPIPE, &original, NULL))) {
        su_deinit();
        su_home_deinit(state->home);
        free(state);
        return NULL;
    }
    state->root = su_root_create(NULL);
    if (state->root) {
        su_root_threading(state->root, 0);
        state->nua = nua_create(state->root, event, (nua_magic_t *)state, NUTAG_URL(bind),
                                NUTAG_SIPS_URL(NULL), NUTAG_MEDIA_ENABLE(0), NUTAG_AUTOANSWER(0),
                                NUTAG_AUTOACK(0), NUTAG_RETRY_COUNT(2), NUTAG_M_USERNAME("rptadv"),
                                NUTAG_OUTBOUND("natify use-rport no-validate options-keepalive"),
                                NUTAG_KEEPALIVE(15000), NTATAG_STATELESS(1), NTATAG_MAXSIZE(8192),
                                NTATAG_SIP_T1X64(32000), NTATAG_SIP_T4(5000), NTATAG_SRV_503(0),
                                TPTAG_QUEUESIZE(4), TPTAG_LOG(0), TPTAG_DUMP(NULL),
                                NUTAG_USER_AGENT("rpt_advanced"), TAG_END());
    }
    if (!state->nua) {
        if (state->root) {
            su_root_destroy(state->root);
        }
        su_deinit();
        su_home_deinit(state->home);
        free(state);
        return NULL;
    }
    nua_get_params(state->nua, TAG_ANY(), TAG_END());
    return state;
}

int sip_native_owner(void *context) {
    struct native *state = context;
    return pthread_equal(state->owner, pthread_self());
}

void sip_native_step(void *context) {
    struct native *state = context;
    su_root_step(state->root, 0);
}

int sip_native_register(void *context, const char *registrar, const char *aor, const char *hop,
                        const char *expires) {
    struct native *state = context;
    if (!state->contact) {
        return state->contact_query_done ? -1 : 1;
    }
    state->account = nua_handle(state->nua, NULL, SIPTAG_FROM_STR(aor), SIPTAG_TO_STR(aor),
                                NUTAG_REGISTRAR(registrar), TAG_END());
    if (!state->account) {
        sip_native_cancel(state);
        return -1;
    }
    nua_register(state->account, NUTAG_PROXY(hop), SIPTAG_EXPIRES_STR(expires), TAG_END());
    return 0;
}

void sip_native_authenticate(void *context, const char *credentials) {
    struct native *state = context;
    nua_authenticate(state->account, NUTAG_AUTH(credentials), TAG_END());
}

void sip_native_unregister(void *context) {
    struct native *state = context;
    nua_unregister(state->account, TAG_END());
}

void sip_native_cancel(void *context) {
    struct native *state = context;
    if (state->account) {
        nua_handle_destroy(state->account);
        state->account = NULL;
    }
}

void sip_native_shutdown(void *context) {
    struct native *state = context;
    nua_shutdown(state->nua);
}

void sip_native_destroy(void *context) {
    struct native *state = context;
    sip_native_cancel(state);
    nua_destroy(state->nua);
    su_root_destroy(state->root);
    su_deinit();
    su_home_deinit(state->home);
    free(state);
}
