/* SPDX-License-Identifier: GPL-2.0-only */
/** @file
 * @brief Asterisk metadata and validated lifecycle forwarding only.
 */
#include <asterisk.h>
#include <asterisk/buildopts.h>
#include <asterisk/module.h>
#include <rptadv_asterisk_adapter.h>
#include <rptadv_control_asterisk_adapter.h>
#include <rptadv_file_adapter.h>
#include <rptadv_product.h>
#include <rptadv_speech_adapter.h>
#include <string.h>

/** Selected immutable descriptor; its library remains a loader dependency. */
static const struct rptadv_asterisk_descriptor_v1 *adapter;

/** Validate mixed artifacts before invoking Rust product lifecycle.
 * @return Asterisk load success or decline on incompatible artifacts or preparation failure.
 */
static int load_module(void) {
    const struct rptadv_asterisk_descriptor_v1 *candidate = rptadv_asterisk_descriptor_v1();
    if (!candidate || candidate->struct_size != sizeof(*candidate) ||
        candidate->abi_version != RPTADV_ASTERISK_ABI_VERSION ||
        memcmp(candidate->capability, RPTADV_ASTERISK_CAPABILITY, sizeof(candidate->capability)) ||
        !candidate->load || !candidate->reload || !candidate->unload) {
        return AST_MODULE_LOAD_DECLINE;
    }
    int result = candidate->load(AST_MODULE_SELF, rptadv_product_descriptor_v1(),
                                 rptadv_control_descriptor_v1(), rptadv_file_adapter_descriptor(),
                                 rptadv_speech_adapter_descriptor());
    if (!result) {
        adapter = candidate;
    }
    return result ? AST_MODULE_LOAD_DECLINE : AST_MODULE_LOAD_SUCCESS;
}
/** Forward replacement to the selected product owner.
 * @return Zero on replacement, minus one on rejection or absent owner.
 */
static int reload_module(void) { return adapter ? adapter->reload() : -1; }
/** Clear selection only after successful quiescent unload.
 * @return Zero after quiescence, or the owner's nonzero refusal to unload.
 */
static int unload_module(void) {
    int result = adapter ? adapter->unload() : 0;
    if (!result) {
        adapter = NULL;
    }
    return result;
}

/** Public Asterisk metadata; all product lifecycle behavior belongs to the Rust adapter. */
AST_MODULE_INFO(ASTERISK_GPL_KEY, AST_MODFLAG_LOAD_ORDER, "rpt_advanced radio controller",
                .load = load_module, .reload = reload_module, .unload = unload_module,
                .load_pri = AST_MODPRI_DEFAULT, .support_level = AST_MODULE_SUPPORT_EXTENDED,
                .optional_modules = "chan_usbradioplus");
