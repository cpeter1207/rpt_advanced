/** @file
 * @brief Private Sofia-SIP 1.13.19 tags and synchronous callback translation.
 */
#define _GNU_SOURCE
#include "native.h"
#include <inttypes.h>
#include <pthread.h>
#include <signal.h>
#include <sofia-sip/bnf.h>
#include <sofia-sip/msg_addr.h>
#include <sofia-sip/nta_tag.h>
#include <sofia-sip/nua.h>
#include <sofia-sip/nua_tag.h>
#include <sofia-sip/sip_header.h>
#include <sofia-sip/sip_tag.h>
#include <sofia-sip/su.h>
#include <sofia-sip/su_string.h>
#include <sofia-sip/su_wait.h>
#include <sofia-sip/tport_tag.h>
#include <stdatomic.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/queue.h>
#include <time.h>
#include <unistd.h>

/** Immutable first outgoing answer envelope and its Sofia-parsed early-dialog tag. */
struct early_answer {
    rptadv_sip_call_v2 observed;
    char tag[];
};

/** One owned call handle and its authoritative, coalesced observation. */
struct call {
    struct native *owner;        /**< Serial owner of both intrusive lists. */
    TAILQ_ENTRY(call) pending;   /**< Coalesced observation ready for delivery. */
    TAILQ_ENTRY(call) entries;   /**< Remains owned through terminal observation and release. */
    nua_handle_t *handle;        /**< Native dialog owner, NULL after termination. */
    rptadv_sip_call_v2 observed; /**< Stable initial offer and current lifecycle. */
    struct early_answer *first;  /**< Allocated only for an outgoing provisional answer. */
    int changed;                 /**< Host has not consumed the latest observation. */
    uint64_t diagnostic_token;   /**< Process-local opaque log identity, never a SIP Call-ID. */
    uint64_t admitted_ms;        /**< Monotonic origin; zero means unavailable. */
    uint32_t notices;            /**< At most one record per fixed transition category. */
    char realm[256];             /**< One pending bounded native Digest realm. */
    size_t realm_length;         /**< Nonzero until the serial owner supplies credentials. */
    unsigned challenges;         /**< Bounded outgoing authentication attempts. */
    int cancel_pending;          /**< A late accepted offer still requires one local BYE. */
};

/** Private native state; account policy and event retention live in Rust. */
struct native {
    su_home_t home[1];                /**< Contact allocation owner. */
    su_root_t *root;                  /**< Synchronous owner-thread event loop. */
    nua_t *nua;                       /**< Native account agent. */
    nua_handle_t *account;            /**< Single activated registration handle. */
    sip_contact_t *contact;           /**< Copied generated local Contact. */
    int contact_query_done;           /**< Initial parameter query completed. */
    pthread_t owner;                  /**< Creation and service thread identity. */
    sip_native_sink sink;             /**< Rust observation callback. */
    void *context;                    /**< Stable Rust callback allocation. */
    TAILQ_HEAD(, call) calls;          /**< Account-owned incoming and outgoing dialogs. */
    TAILQ_HEAD(, call) changed;        /**< FIFO, at most one pending observation per call. */
    struct call *last_observed;  /**< Cached current FIFO result; cleared before release. */
    uint64_t next_call;               /**< Last issued call token; exhaustion refuses admission. */
    int admission;                    /**< Explicit host permission to accept a new offer. */
};

/** Fixed lifecycle notices; no foreign text, account identity or media is logged. */
enum call_notice {
    REMOTE_BYE,
    REMOTE_CANCEL,
    NATIVE_TERMINAL,
    LOCAL_CLOSE,
    LOCAL_REJECT,
    ACCOUNT_SHUTDOWN
};
/** Process-wide diagnostic serial, independent of account/provider identities. */
static atomic_uint_least64_t diagnostic_sequence;

/** Allocate a nonreused log token, or disable diagnostics after exhaustion.
 * @return Opaque nonzero token, or zero.
 */
static uint64_t diagnostic_token(void) {
    uint_least64_t previous = atomic_load_explicit(&diagnostic_sequence, memory_order_relaxed);
    while (previous != UINT64_MAX) {
        if (atomic_compare_exchange_weak_explicit(&diagnostic_sequence, &previous, previous + 1,
                                                  memory_order_relaxed, memory_order_relaxed)) {
            return previous + 1;
        }
    }
    return 0;
}

/** Obtain elapsed-time evidence without using wall clock or wire timestamps.
 * @return Monotonic milliseconds, or zero when unavailable.
 */
static uint64_t monotonic_ms(void) {
    struct timespec now;
    if (clock_gettime(CLOCK_MONOTONIC, &now) != 0 || now.tv_sec < 0 ||
        (uint64_t)now.tv_sec > (UINT64_MAX - 999) / 1000) {
        return 0;
    }
    return (uint64_t)now.tv_sec * 1000 + (uint64_t)now.tv_nsec / 1000000;
}

/** Record only a factual transition or explicit local API request once.
 * @param call Owned admitted slot.
 * @param notice Fixed internal category, never a native reason string.
 * @param status Numeric native event status; zero means no status is supplied.
 */
static void call_notice(struct call *call, enum call_notice notice, int status) {
    static const char *const names[] = {"remote-bye",  "remote-cancel", "native-terminal",
                                        "local-close", "local-reject",  "account-shutdown"};
    uint32_t bit = UINT32_C(1) << notice;
    if (!call->diagnostic_token || (call->notices & bit)) {
        return;
    }
    call->notices |= bit;
    uint64_t now = monotonic_ms();
    int64_t elapsed = -1;
    if (call->admitted_ms && now >= call->admitted_ms && now - call->admitted_ms <= INT64_MAX) {
        elapsed = (int64_t)(now - call->admitted_ms);
    }
    (void)dprintf(STDERR_FILENO,
                  "phone SIP: call=%" PRIu64 " event=%s status=%d elapsed_ms=%" PRId64 "\n",
                  call->diagnostic_token, names[notice], status, elapsed);
}

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

/** Find one live token, never accepting a released or reused slot.
 * @param state Native owner.
 * @param token Exact call token.
 * @return Owned slot or NULL.
 */
static struct call *find_call(struct native *state, uint64_t token) {
    struct call *call;
    TAILQ_FOREACH(call, &state->calls, entries) {
        if (call->observed.call == token) {
            return call;
        }
    }
    return NULL;
}

/** Publish one complete dialog record on its serial account owner. */
static void retain_call(struct native *state, struct call *call, nua_handle_t *handle) {
    call->owner = state;
    call->handle = handle;
    call->diagnostic_token = diagnostic_token();
    call->admitted_ms = monotonic_ms();
    call->observed.struct_size = sizeof(call->observed);
    call->observed.call = ++state->next_call;
    TAILQ_INSERT_TAIL(&state->calls, call, entries);
}

/** Queue each owned record once. Later changes update its current observation in place. */
static void mark_changed(struct call *call) {
    if (!call->changed) {
        TAILQ_INSERT_TAIL(&call->owner->changed, call, pending);
        call->changed = 1;
    }
}

/** Drop a terminal or quiescent record from both account-owned lists. */
static void free_call(struct call *call) {
    if (call->changed) {
        TAILQ_REMOVE(&call->owner->changed, call, pending);
    }
    TAILQ_REMOVE(&call->owner->calls, call, entries);
    if (call->owner->last_observed == call) {
        call->owner->last_observed = NULL;
    }
    free(call->first);
    free(call);
}

/** Coalesce state without discarding the terminal observation.
 * @param call Owned slot.
 * @param value Neutral lifecycle state.
 * @param status Native numeric status; zero preserves the previous value.
 */
static void call_state(struct call *call, uint32_t value, int status) {
    if (value >= call->observed.state &&
        (call->observed.state != value ||
         (status && call->observed.sip_status != (uint32_t)status))) {
        call->observed.state = value;
        if (status) {
            call->observed.sip_status = (uint32_t)status;
        }
        mark_changed(call);
    }
}

/** Retain one received final refusal independently of later lifecycle status. */
static void refuse_invite(struct call *call, const sip_t *sip, int status) {
    if (call->observed.state == 1 && !call->observed.invite_failure_status &&
        status >= 300 && status <= 699 && sip && sip->sip_status &&
        sip->sip_status->st_status == status) {
        call->observed.invite_failure_status = (uint32_t)status;
    }
    call_state(call, 4, status);
}

/** Copy one bounded remote SDP and its actual message source, never header routing.
 * @param state Native event owner.
 * @param call Exact dialog slot.
 * @param sip Current parsed message.
 * @return Nonzero only for a complete supported envelope; SDP policy remains with the host.
 */
static int remote_media(struct native *state, struct call *call, const sip_t *sip) {
    if (!sip || !sip->sip_content_type ||
        !su_casematch(sip->sip_content_type->c_type, "application/sdp") || !sip->sip_payload ||
        sip->sip_payload->pl_next || !sip->sip_payload->pl_len ||
        sip->sip_payload->pl_len > sizeof(call->observed.offer)) {
        return 0;
    }
    msg_t *message = nua_current_request(state->nua);
    su_sockaddr_t *source = message ? msg_addr(message) : NULL;
    if (!source || (source->su_family != AF_INET && source->su_family != AF_INET6)) {
        return 0;
    }
    const uint8_t *bytes;
    size_t length;
    if (source->su_family == AF_INET) {
        call->observed.source_family = 4;
        call->observed.source_port = ntohs(source->su_sin.sin_port);
        bytes = (const uint8_t *)&source->su_sin.sin_addr;
        length = 4;
    } else {
        call->observed.source_family = 6;
        call->observed.source_port = ntohs(source->su_sin6.sin6_port);
        bytes = (const uint8_t *)&source->su_sin6.sin6_addr;
        length = 16;
    }
    memset(call->observed.source, 0, sizeof(call->observed.source));
    memcpy(call->observed.source, bytes, length);
    call->observed.offer_length = (uint32_t)sip->sip_payload->pl_len;
    for (size_t i = 0; i < call->observed.offer_length; ++i) {
        call->observed.offer[i] = (uint8_t)sip->sip_payload->pl_data[i];
    }
    return 1;
}

/** Save the first bounded provisional envelope on the exact early dialog. */
static int retain_early_answer(struct call *call, const char *tag, int status) {
    if (call->first) {
        return 1;
    }
    size_t length = strlen(tag);
    if (length > SIZE_MAX - sizeof(struct early_answer) - 1) {
        return 0;
    }
    struct early_answer *first = malloc(sizeof(*first) + length + 1);
    if (!first) {
        return 0;
    }
    first->observed = call->observed;
    first->observed.sip_status = (uint32_t)status;
    memcpy(first->tag, tag, length + 1);
    call->first = first;
    return 1;
}

/** Translate only native call ownership and initial bounded SDP.
 * @param state Native owner.
 * @param kind Native event.
 * @param status Native numeric status.
 * @param handle Native operation handle.
 * @param sip Borrowed parsed message, possibly NULL.
 * @param tags Borrowed callback tags.
 */
static void call_event(struct native *state, nua_event_t kind, int status, nua_handle_t *handle,
                       const sip_t *sip, tagi_t tags[]) {
    struct call *call = NULL;
    if (!handle || handle == state->account) {
        return;
    }
    TAILQ_FOREACH(call, &state->calls, entries) {
        if (call->handle == handle) {
            break;
        }
    }
    if (kind == nua_i_invite && status < 200) {
        if (call) {
            /* Re-INVITE/media changes are not a supported profile in this milestone. */
            nua_respond(handle, 488, "Not Acceptable Here", TAG_END());
            return;
        }
        int refusal = 480;
        if (state->admission && state->next_call != UINT64_MAX) {
            refusal = 503;
            call = calloc(1, sizeof(*call));
        }
        if (call && !remote_media(state, call, sip)) {
            free(call);
            call = NULL;
            refusal = 488;
        }
        if (!call) {
            nua_respond(handle, refusal, "Call Unavailable", TAG_END());
            nua_handle_destroy(handle);
            return;
        }
        retain_call(state, call, handle);
        call_state(call, 1, status);
    }
    if (!call) {
        if (kind == nua_i_options && status >= 200) {
            nua_handle_destroy(handle);
        }
        return;
    }
    if (call->observed.direction == 1 && kind == nua_r_invite) {
        if (status == 401 || status == 407) {
            const char *realm = challenge_realm(sip, status);
            if (call->observed.state == 1 && realm && call->challenges < 2) {
                call->realm_length = strlen(realm);
                for (size_t i = 0; i < call->realm_length; ++i) {
                    call->realm[i] = realm[i];
                }
                ++call->challenges;
                call_state(call, 1, status);
            } else {
                refuse_invite(call, sip, status);
                call->cancel_pending = 1;
                nua_cancel(handle, TAG_END());
            }
        } else if (status >= 200 && status < 300) {
            if (call->observed.state == 1) {
                const char *tag = sip && sip->sip_to ? sip->sip_to->a_tag : NULL;
                if ((!call->first || (tag && strcmp(call->first->tag, tag) == 0)) &&
                    remote_media(state, call, sip)) {
                    call_state(call, 2, status);
                } else {
                    /* A final success still requires ACK before rejecting its media. */
                    nua_ack(handle, TAG_END());
                    call_state(call, 4, 488);
                    nua_bye(handle, TAG_END());
                }
            } else if (call->observed.state >= 3) {
                nua_ack(handle, TAG_END());
                if (call->cancel_pending) {
                    call->cancel_pending = 0;
                    nua_bye(handle, TAG_END());
                }
            }
        } else if (status >= 300) {
            refuse_invite(call, sip, status);
        } else if (call->observed.state == 1) {
            if (status > 100 && status < 200 && sip && sip->sip_status &&
                sip->sip_status->st_status == status) {
                if (status == 180 && !call->observed.alerting_seen) {
                    call->observed.alerting_seen = 1;
                    mark_changed(call);
                }
                const char *tag = sip->sip_to ? sip->sip_to->a_tag : NULL;
                if (tag && *tag && (!call->first || strcmp(call->first->tag, tag) == 0) &&
                    remote_media(state, call, sip)) {
                    if (!retain_early_answer(call, tag, status)) {
                        call_state(call, 4, 500);
                        call->cancel_pending = 1;
                        nua_cancel(handle, TAG_END());
                    } else {
                        mark_changed(call);
                    }
                }
            }
            call_state(call, 1, status);
        }
    }
    if (kind == nua_i_cancel || kind == nua_i_bye) {
        call_notice(call, kind == nua_i_bye ? REMOTE_BYE : REMOTE_CANCEL, status);
        call_state(call, 4, status);
    } else if (kind == nua_i_state) {
        int value = nua_callstate_init;
        tl_gets(tags, NUTAG_CALLSTATE_REF(value), TAG_END());
        if (value == nua_callstate_ready) {
            call_state(call, 3, status);
        } else if (value == nua_callstate_terminating) {
            call_state(call, 4, status);
        } else if (value == nua_callstate_terminated) {
            call_notice(call, NATIVE_TERMINAL, status);
            call_state(call, 5, status);
            call->handle = NULL;
            nua_handle_destroy(handle);
        }
    }
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
    (void)handle_magic;
    call_event(state, kind, status, handle, sip, tags);
    if (kind == nua_r_shutdown && status == 200) {
        /* NUA may remove a pending session without a call-state event.
         * Final shutdown proves every remaining handle quiescent. */
        struct call *call;
        TAILQ_FOREACH(call, &state->calls, entries) {
            if (call->handle) {
                call_notice(call, NATIVE_TERMINAL, 0);
                call_state(call, 5, 0);
                nua_handle_destroy(call->handle);
                call->handle = NULL;
            }
        }
    }
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
        kind == nua_i_outbound || (kind == nua_r_authenticate && handle == state->account)) {
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
    TAILQ_INIT(&state->calls);
    TAILQ_INIT(&state->changed);
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
    /* Match Sofia's 100 ms compensation so the stepped root keeps the configured interval. */
    if (state->root && su_root_set_max_defer(state->root, 100) == 0) {
        su_root_threading(state->root, 0);
        state->nua = nua_create(state->root, event, (nua_magic_t *)state, NUTAG_URL(bind),
                                NUTAG_SIPS_URL(NULL), NUTAG_MEDIA_ENABLE(0), NUTAG_AUTOANSWER(0),
                                NUTAG_AUTOACK(0), NUTAG_RETRY_COUNT(2), NUTAG_M_USERNAME("rptadv"),
                                NUTAG_OUTBOUND("natify use-rport no-validate options-keepalive"),
                                NUTAG_KEEPALIVE(15000),
                                NTATAG_MAXSIZE(8192), NTATAG_SIP_T1X64(32000), NTATAG_SIP_T4(5000),
                                NTATAG_SRV_503(0), TPTAG_QUEUESIZE(4), TPTAG_LOG(0),
                                TPTAG_DUMP(NULL), NUTAG_USER_AGENT("rpt_advanced"), TAG_END());
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
    struct call *call;
    TAILQ_FOREACH(call, &state->calls, entries) {
        if (call->handle) {
            call_notice(call, ACCOUNT_SHUTDOWN, 0);
        }
    }
    nua_shutdown(state->nua);
}

void sip_native_destroy(void *context) {
    struct native *state = context;
    sip_native_cancel(state);
    nua_destroy(state->nua);
    struct call *call;
    while ((call = TAILQ_FIRST(&state->calls)) != NULL) {
        free_call(call);
    }
    su_root_destroy(state->root);
    su_deinit();
    su_home_deinit(state->home);
    free(state);
}

void sip_native_admission(void *context, int enabled) {
    struct native *state = context;
    state->admission = enabled;
}

int sip_native_next_call(void *context, rptadv_sip_call_v2 *output) {
    struct native *state = context;
    struct call *call = TAILQ_FIRST(&state->changed);
    if (!call) {
        return 1;
    }
    *output = call->observed;
    state->last_observed = call;
    TAILQ_REMOVE(&state->changed, call, pending);
    call->changed = 0;
    return 0;
}

int sip_native_first_provisional(void *context, uint64_t token, rptadv_sip_call_v2 *output) {
    struct native *state = context;
    struct call *call = state->last_observed;
    if (!call || call->observed.call != token) {
        call = find_call(state, token);
    }
    if (!call) {
        return -2;
    }
    if (!call->first) {
        return 1;
    }
    *output = call->first->observed;
    return 0;
}

int sip_native_respond(void *context, uint64_t token, uint32_t status, const char *answer) {
    struct call *call = find_call(context, token);
    if (!call) {
        return -2;
    }
    if (call->observed.state != 1 || call->observed.direction != 0) {
        return -3;
    }
    if (status != 200) {
        call_notice(call, LOCAL_REJECT, (int)status);
    }
    call_state(call, status == 200 ? 2 : 4, (int)status);
    nua_respond(call->handle, (int)status, status == 200 ? "OK" : "Call Unavailable",
                TAG_IF(answer, SIPTAG_CONTENT_TYPE_STR("application/sdp")),
                TAG_IF(answer, SIPTAG_PAYLOAD_STR(answer)), TAG_END());
    return 0;
}

int sip_native_hangup(void *context, uint64_t token) {
    struct call *call = find_call(context, token);
    if (!call) {
        return -2;
    }
    if (call->observed.state < 4) {
        call_notice(call, LOCAL_CLOSE, 0);
    }
    if (call->observed.direction == 1 && call->observed.state == 1) {
        call_state(call, 4, 0);
        call->cancel_pending = 1;
        nua_cancel(call->handle, TAG_END());
        return 0;
    }
    if (call->observed.state == 1) {
        return sip_native_respond(context, token, 480, NULL);
    }
    if (call->observed.direction == 1 && call->observed.state == 2) {
        nua_ack(call->handle, TAG_END());
    }
    if (call->observed.state < 4) {
        call_state(call, 4, 0);
        nua_bye(call->handle, TAG_END());
    }
    return 0;
}

int sip_native_release(void *context, uint64_t token) {
    struct native *state = context;
    struct call *call = find_call(state, token);
    if (!call) {
        return -2;
    }
    if (call->observed.state != 5) {
        return 1;
    }
    free_call(call);
    return 0;
}

int sip_native_originate(void *context, const char *aor, const char *hop, const char *destination,
                         const char *offer, uint64_t *token) {
    struct native *state = context;
    if (!state->admission || state->next_call == UINT64_MAX) {
        return -3;
    }
    struct call *call = calloc(1, sizeof(*call));
    if (!call) {
        return -4;
    }
    nua_handle_t *handle =
        nua_handle(state->nua, NULL, SIPTAG_FROM_STR(aor), SIPTAG_TO_STR(destination), TAG_END());
    if (!handle) {
        free(call);
        return -4;
    }
    retain_call(state, call, handle);
    call->observed.direction = 1;
    call_state(call, 1, 0);
    *token = call->observed.call;
    nua_invite(handle, NUTAG_URL(destination), NUTAG_PROXY(hop), SIPTAG_CONTACT(state->contact),
               NUTAG_AUTOACK(0), SIPTAG_CONTENT_TYPE_STR("application/sdp"),
               SIPTAG_PAYLOAD_STR(offer), TAG_END());
    return 0;
}

int sip_native_acknowledge(void *context, uint64_t token) {
    struct call *call = find_call(context, token);
    if (!call) {
        return -2;
    }
    if (call->observed.direction != 1 || (call->observed.state != 2 && call->observed.state != 3)) {
        return -3;
    }
    if (call->observed.state == 2) {
        nua_ack(call->handle, TAG_END());
    }
    return 0;
}

size_t sip_native_next_authentication(void *context, uint64_t *token, char realm[256]) {
    struct native *state = context;
    struct call *call;
    TAILQ_FOREACH(call, &state->calls, entries) {
        if (call->observed.state == 1 && call->realm_length) {
            size_t length = call->realm_length;
            for (size_t j = 0; j < length; ++j) {
                realm[j] = call->realm[j];
            }
            call->realm_length = 0;
            *token = call->observed.call;
            return length;
        }
    }
    return 0;
}

int sip_native_authenticate_call(void *context, uint64_t token, const char *credentials) {
    struct call *call = find_call(context, token);
    if (!call) {
        return -2;
    }
    if (call->observed.state != 1 || call->observed.direction != 1) {
        return -3;
    }
    /* A Digest retry starts a fresh INVITE transaction after its challenged early dialog. */
    free(call->first);
    call->first = NULL;
    call->observed.offer_length = 0;
    call->observed.source_family = 0;
    call->observed.source_port = 0;
    memset(call->observed.source, 0, sizeof(call->observed.source));
    nua_authenticate(call->handle, NUTAG_AUTH(credentials), TAG_END());
    return 0;
}
