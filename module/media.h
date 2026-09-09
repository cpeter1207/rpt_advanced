/* SPDX-License-Identifier: GPL-2.0-only */
/** @file
 * @brief Runtime Asterisk codec selection against a radio's native format.
 */
#ifndef RPT_ADVANCED_MEDIA_H
#define RPT_ADVANCED_MEDIA_H
#include <stddef.h>

struct ast_format;
struct ast_format_cap;

/** @brief Find the highest usable rate without a compiled-in codec/rate list.
 * @param radio Detected hardware-native signed-linear format, borrowed.
 * @param rate Explicit codec sample rate, or zero for hardware-bounded automatic selection.
 * @param name Requested codec name; empty selects signed linear.
 * @return Owned Asterisk format reference, or null when no bidirectional path exists.
 * The caller releases the reference with ao2_cleanup. Both radio transport and
 * signed-linear identifier generation must be convertible in both directions.
 */
struct ast_format *ra_media_select(struct ast_format *radio, unsigned int rate, const char *name);
/** @brief Collect ordered wire formats usable by the rate-aware link boundary.
 * @param radio Local signed-linear format that limits the preferred wire sample rate.
 * @param formats Written owned format-reference array on success.
 * @param count Written number of entries in formats on success.
 * @return Zero on success or minus one on allocation, registry, or compatibility failure.
 *
 * Candidates are ordered from the local native rate downward, with signed-linear PCM preferred
 * over a compressed format at an equal rate. The caller releases the array with
 * ra_media_candidates_release(). Each candidate converts bidirectionally to matching-rate PCM;
 * link_hub independently converts that PCM to the local radio rate with libsamplerate.
 */
int ra_media_candidates_collect(struct ast_format *radio, struct ast_format ***formats,
                                size_t *count);

/** @brief Release candidates returned by ra_media_candidates_collect().
 * @param formats Owned candidate array, or null.
 * @param count Number of owned format references in formats.
 */
void ra_media_candidates_release(struct ast_format **formats, size_t count);

/** @brief Build one single-format IAX request capability.
 * @param format Owned elsewhere candidate format.
 * @return Owned one-format capability, or null on allocation or append failure.
 *
 * Asterisk's generic request path selects one best format from a capability before IAX sees it.
 * Calling it once per candidate preserves actual IAX negotiation instead of forcing a legacy peer
 * through an arbitrary high-rate Asterisk translator.
 */
struct ast_format_cap *ra_media_offer_create(struct ast_format *format);
#endif
