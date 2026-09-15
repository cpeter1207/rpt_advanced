/* SPDX-License-Identifier: GPL-2.0-only */
/** @file
 * @brief Runtime Asterisk peer-media selection against a radio's native format.
 */
#ifndef RPT_ADVANCED_MEDIA_H
#define RPT_ADVANCED_MEDIA_H
#include <stddef.h>

struct ast_format;
struct ast_format_cap;

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
