/* SPDX-License-Identifier: GPL-2.0-only */
/** @file
 * @brief Runtime Asterisk codec selection against a radio's native format.
 */
#ifndef RPT_ADVANCED_MEDIA_H
#define RPT_ADVANCED_MEDIA_H
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
/** @brief Offer registered audio formats with working conversions to and from local PCM.
 * @param radio Local signed-linear format.
 * @return Owned capability set or null on allocation/append failure.
 * The channel technology further limits this set to its supported wire formats.
 */
struct ast_format_cap *ra_media_offer(struct ast_format *radio);
#endif
