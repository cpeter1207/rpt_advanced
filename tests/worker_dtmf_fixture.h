/* SPDX-License-Identifier: GPL-2.0-only */
/** @file
 * @brief Deterministic detector controls shared by worker lifecycle tests.
 */
#ifndef RPT_ADVANCED_WORKER_DTMF_FIXTURE_H
#define RPT_ADVANCED_WORKER_DTMF_FIXTURE_H
#include <stdbool.h>
extern bool ra_test_dtmf_fail;           /**< Fail detector creation. */
extern char ra_test_dtmf_digit;          /**< Next detected digit; zero represents no event. */
extern unsigned int ra_test_dtmf_closed; /**< Released detector count. */
#endif
