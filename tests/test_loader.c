/* SPDX-License-Identifier: GPL-2.0-only */
/* Public Asterisk registration/descriptor ABI fixture; never installed. */
#include <assert.h>
#include <asterisk.h>
#include <asterisk/module.h>
#include <rptadv_asterisk_adapter.h>
#include <rptadv_control_asterisk_adapter.h>
#include <rptadv_file_adapter.h>
#include <rptadv_product.h>
#include <rptadv_speech_adapter.h>
#include <string.h>

static const struct ast_module_info *metadata;
static struct rptadv_asterisk_descriptor_v1 descriptor;
static const struct rptadv_asterisk_descriptor_v1 *selected;
static const struct rptadv_product_descriptor_v1 product;
static const struct rptadv_control_descriptor_v1 control;
static const struct rptadv_file_descriptor file;
static const struct rptadv_speech_descriptor speech;
static int load_result, unload_result, reload_calls, load_calls;

void ast_module_register(const struct ast_module_info *info) { metadata = info; }
void ast_module_unregister(const struct ast_module_info *info) { assert(info == metadata); }

const struct rptadv_asterisk_descriptor_v1 *rptadv_asterisk_descriptor_v1(void) { return selected; }
const struct rptadv_product_descriptor_v1 *rptadv_product_descriptor_v1(void) { return &product; }
const struct rptadv_control_descriptor_v1 *rptadv_control_descriptor_v1(void) { return &control; }
const struct rptadv_file_descriptor *rptadv_file_adapter_descriptor(void) { return &file; }
const struct rptadv_speech_descriptor *rptadv_speech_adapter_descriptor(void) { return &speech; }

static int load(void *module, const struct rptadv_product_descriptor_v1 *owner,
                const struct rptadv_control_descriptor_v1 *executor,
                const struct rptadv_file_descriptor *files,
                const struct rptadv_speech_descriptor *voices) {
    assert(module == metadata->self);
    assert(owner == &product);
    assert(executor == &control);
    assert(files == &file);
    assert(voices == &speech);
    ++load_calls;
    return load_result;
}
static int reload(void) { return ++reload_calls; }
static int unload(void) { return unload_result; }

static void valid(void) {
    descriptor = (struct rptadv_asterisk_descriptor_v1){
        .struct_size = sizeof(descriptor),
        .abi_version = RPTADV_ASTERISK_ABI_VERSION,
        .capability = RPTADV_ASTERISK_CAPABILITY,
        .load = load,
        .reload = reload,
        .unload = unload,
    };
    selected = &descriptor;
}

static void rejected(void) {
    int before = load_calls;
    assert(metadata->load() == AST_MODULE_LOAD_DECLINE);
    assert(load_calls == before);
    valid();
}

int main(void) {
    assert(metadata);
    assert(strcmp(metadata->name, "app_rpt_advanced") == 0);
    assert(metadata->reload() == -1);
    assert(metadata->unload() == 0);
    rejected(); /* No descriptor. */
    descriptor.struct_size--;
    rejected();
    descriptor.abi_version++;
    rejected();
    descriptor.capability[0] = '!';
    rejected();
    descriptor.load = NULL;
    rejected();
    descriptor.reload = NULL;
    rejected();
    descriptor.unload = NULL;
    rejected();

    load_result = -1;
    assert(metadata->load() == AST_MODULE_LOAD_DECLINE);
    assert(metadata->reload() == -1);
    load_result = 0;
    assert(metadata->load() == AST_MODULE_LOAD_SUCCESS);
    assert(metadata->reload() == 1);
    unload_result = -1;
    assert(metadata->unload() == -1);
    assert(metadata->reload() == 2); /* Failed unload retains the selected descriptor. */
    unload_result = 0;
    assert(metadata->unload() == 0);
    assert(metadata->reload() == -1);
    assert(metadata->unload() == 0);
    return 0;
}
