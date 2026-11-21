/* SPDX-License-Identifier: GPL-2.0-only */
/** @file rptadv_asterisk_adapter.h
 * @brief Versioned public-Asterisk product lifecycle boundary.
 * The loader retains the product and all adapter libraries through successful unload.
 * No callback, task, channel owner or descriptor may outlive its code.
 */
#ifndef RPTADV_ASTERISK_ADAPTER_H
#define RPTADV_ASTERISK_ADAPTER_H
#include <stdint.h>
struct rptadv_product_descriptor_v1;
struct rptadv_product_management_v1;
struct rptadv_control_descriptor_v1;
struct rptadv_file_descriptor;
struct rptadv_speech_descriptor;
/** Initial-alpha incompatible artifact discriminator. */
#define RPTADV_ASTERISK_ABI_VERSION 1U
/** Exact fixed-width capability identity, including its terminator. */
#define RPTADV_ASTERISK_CAPABILITY "rptadv.asterisk"
/** Immutable exact-version/exact-size lifecycle table. */
struct rptadv_asterisk_descriptor_v1 {
    uint32_t struct_size;   /**< Complete table size. */
    uint32_t abi_version;   /**< Exact incompatible ABI version. */
    uint8_t capability[16]; /**< Exact capability name and NUL terminator. */
    int (*load)(void *module, const struct rptadv_product_descriptor_v1 *product,
                const struct rptadv_control_descriptor_v1 *control,
                const struct rptadv_file_descriptor *file,
                const struct rptadv_speech_descriptor *speech); /**< Prepare and register. */
    int (*reload)(void); /**< Replace configuration, retaining old state on failure. */
    int (*unload)(void); /**< Stop/join/drain; zero alone permits code unload. */
};
/** Return process-lifetime immutable lifecycle metadata.
 * @return Immutable descriptor retained through every lifecycle call.
 */
const struct rptadv_asterisk_descriptor_v1 *rptadv_asterisk_descriptor_v1(void);
/** Start the authenticated managed service. The linked product and all adapter
 * libraries remain loaded until the selected lifecycle confirms unload.
 * @param[in] module Current Asterisk module handle, borrowed through unload.
 * @param[in] product Immutable lifecycle descriptor for the selected product.
 * @param[in] management Immutable management descriptor from the same product
 * DSO, with a readable size prefix and complete table for version 1.
 * @param[in] control Immutable Asterisk control provider descriptor.
 * @param[in] file Immutable file provider descriptor.
 * @param[in] speech Immutable speech provider descriptor.
 * @return 0 if the module must stay loaded, including when startup or registration
 * failed and cleanup remains pending. Returns 1 to decline the load.
 */
int rptadv_asterisk_load_management_v1(void *module,
                                       const struct rptadv_product_descriptor_v1 *product,
                                       const struct rptadv_product_management_v1 *management,
                                       const struct rptadv_control_descriptor_v1 *control,
                                       const struct rptadv_file_descriptor *file,
                                       const struct rptadv_speech_descriptor *speech);
#endif
