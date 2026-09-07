/* SPDX-License-Identifier: GPL-2.0-only */
/** @file
 * @brief Asterisk module lifecycle and validated configuration ownership.
 */
#include <asterisk.h>

#include "document.h"
#include "schema.h"
#include <asterisk/buildopts.h>
#include <asterisk/logger.h>
#include <asterisk/module.h>
#include <asterisk/paths.h>
#include <stdio.h>
#include <string.h>

/** @brief Configuration owned by the loaded module. */
static struct ra_document configuration;

/** @brief Load a complete replacement without discarding working settings on failure.
 * @return Zero on success or minus one on file, syntax, or schema errors.
 */
static int read_configuration(void) {
    char *path = NULL;
    if (ast_asprintf(&path, "%s/rpt_advanced.conf", ast_config_AST_CONFIG_DIR) < 0) {
        ast_log(LOG_ERROR, "rpt_advanced: cannot allocate configuration path\n");
        return -1;
    }
    FILE *stream = fopen(path, "r");
    if (!stream) {
        ast_log(LOG_ERROR, "rpt_advanced: cannot open %s\n", path);
        ast_free(path);
        return -1;
    }
    struct ra_document replacement = {0};
    size_t line;
    const char *error = ra_document_read(stream, &replacement, &line);
    fclose(stream);
    if (error) {
        ast_log(LOG_ERROR, "rpt_advanced: %s:%zu: %s\n", path, line, error);
        ast_free(path);
        return -1;
    }
    const char *section;
    const char *key;
    error = ra_document_validate(&replacement, &section, &key);
    if (error) {
        ast_log(LOG_ERROR, "rpt_advanced: %s [%s] %s: %s\n", path, section, key ? key : "", error);
        ra_document_destroy(&replacement);
        ast_free(path);
        return -1;
    }
    ast_free(path);
    ra_document_destroy(&configuration);
    configuration = replacement;
    return 0;
}

/** @brief Validate configuration when Asterisk loads the module.
 * @return Asterisk success or decline status.
 */
static int load_module(void) {
    return read_configuration() ? AST_MODULE_LOAD_DECLINE : AST_MODULE_LOAD_SUCCESS;
}

/** @brief Replace validated configuration on an Asterisk module reload.
 * @return Zero on success or minus one while retaining the previous configuration.
 */
static int reload_module(void) { return read_configuration(); }

/** @brief Release module-owned configuration.
 * @return Zero after cleanup.
 */
static int unload_module(void) {
    ra_document_destroy(&configuration);
    return 0;
}

/** @brief Public Asterisk module descriptor; lifecycle callbacks own configuration. */
static struct ast_module_info descriptor = {
    .name = "app_rpt_advanced",
    .description = "rpt_advanced radio controller",
    .key = ASTERISK_GPL_KEY,
    .buildopt_sum = AST_BUILDOPT_SUM,
    .flags = AST_MODFLAG_LOAD_ORDER,
    .load = load_module,
    .unload = unload_module,
    .reload = reload_module,
    .load_pri = AST_MODPRI_DEFAULT,
    .support_level = AST_MODULE_SUPPORT_EXTENDED,
};

/** @brief Register the descriptor when the shared library is opened. */
static void __attribute__((constructor)) register_module(void) { ast_module_register(&descriptor); }

/** @brief Remove the descriptor when the shared library is closed. */
static void __attribute__((destructor)) unregister_module(void) {
    ast_module_unregister(&descriptor);
}
