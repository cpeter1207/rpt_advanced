/* SPDX-License-Identifier: GPL-2.0-only */
/** @file
 * @brief Exercise the real shared module through Asterisk's public lifecycle ABI.
 */
#include <asterisk.h>

#include "runtime.h"
#include <assert.h>
#include <asterisk/module.h>
#include <dlfcn.h>
#include <stdarg.h>
#include <stdbool.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>

/** @brief Number of runtime starts to reject in sequence. */
static unsigned int runtime_failures;

const char *ra_runtime_start(struct ra_runtime *runtime, const struct ra_document *document) {
    (void)runtime;
    (void)document;
    if (runtime_failures) {
        --runtime_failures;
        return "fixture radio unavailable";
    }
    return NULL;
}

void ra_runtime_stop(struct ra_runtime *runtime) { (void)runtime; }

/** @brief Asterisk configuration-directory symbol supplied by this test host. */
const char *ast_config_AST_CONFIG_DIR;
/** @brief Module registered by its shared-library constructor. */
static const struct ast_module_info *registered;
/** @brief Number of diagnostics observed. */
static unsigned int errors;
/** @brief Inject one configuration-path allocation failure. */
static bool fail_allocation;

/** @brief Supply Asterisk's allocating formatter and a deterministic failure case.
 * @param file Caller source file.
 * @param line Caller source line.
 * @param function Caller function.
 * @param result Receives allocated text.
 * @param format Formatting string.
 * @param ... Formatting arguments.
 * @return Formatted length or minus one on allocation failure.
 */
int __ast_asprintf(const char *file, int line, const char *function, char **result,
                   const char *format, ...) {
    (void)file;
    (void)line;
    (void)function;
    if (fail_allocation) {
        return -1;
    }
    va_list arguments;
    va_start(arguments, format);
    int length = vasprintf(result, format, arguments);
    va_end(arguments);
    return length;
}

/** @brief Supply Asterisk's deallocator to the standalone test host.
 * @param pointer Allocated memory.
 * @param file Caller file.
 * @param line Caller line.
 * @param function Caller function.
 */
void __ast_free(void *pointer, const char *file, int line, const char *function) {
    (void)file;
    (void)line;
    (void)function;
    free(pointer);
}

/** @brief Capture module registration as Asterisk would.
 * @param info Module descriptor.
 */
void ast_module_register(const struct ast_module_info *info) {
    assert(!registered);
    registered = info;
}

/** @brief Observe destructor registration cleanup.
 * @param info Registered module descriptor.
 */
void ast_module_unregister(const struct ast_module_info *info) {
    assert(registered == info);
    registered = NULL;
}

/** @brief Count diagnostics without relying on localized output.
 * @param level Asterisk logging level.
 * @param file Source file.
 * @param line Source line.
 * @param function Source function.
 * @param format Message format.
 * @param ... Message arguments.
 */
void ast_log(int level, const char *file, int line, const char *function, const char *format, ...) {
    (void)level;
    (void)file;
    (void)line;
    (void)function;
    (void)format;
    ++errors;
}

/** @brief Write a temporary configuration fixture.
 * @param path File in the test-owned temporary directory.
 * @param content Complete configuration text.
 */
static void write_config(const char *path, const char *content) {
    FILE *stream = fopen(path, "w");
    assert(stream);
    assert(fputs(content, stream) >= 0);
    assert(fclose(stream) == 0);
}

/** @brief Test loading, failure paths, reload, and unloading through the real descriptor.
 * @return Zero after assertions and temporary-file cleanup.
 */
int main(void) {
    char directory[] = "/tmp/rpt-advanced-module-XXXXXX";
    assert(mkdtemp(directory));
    ast_config_AST_CONFIG_DIR = directory;
    char path[PATH_MAX];
    assert(snprintf(path, sizeof(path), "%s/rpt_advanced.conf", directory) > 0);
    void *handle = dlopen("build/module-coverage/app_rpt_advanced.so", RTLD_NOW);
    if (!handle) {
        fprintf(stderr, "%s\n", dlerror());
    }
    assert(handle && registered);
    assert(registered->optional_modules &&
           !strcmp(registered->optional_modules, "chan_usbradioplus"));
    assert(registered->load() == AST_MODULE_LOAD_DECLINE);
    fail_allocation = true;
    assert(registered->load() == AST_MODULE_LOAD_DECLINE);
    fail_allocation = false;
    write_config(path, "[broken\n");
    assert(registered->load() == AST_MODULE_LOAD_DECLINE);
    write_config(path, "[identifier missing welcome]\n");
    assert(registered->load() == AST_MODULE_LOAD_DECLINE);
    write_config(path, "[usb]\nfull_duplex=maybe\n");
    assert(registered->load() == AST_MODULE_LOAD_DECLINE);
    write_config(path, "[usb]\nfull_duplex=yes\n[identifier usb welcome]\nmorse_text=KG0BP\n");
    assert(registered->load() == AST_MODULE_LOAD_SUCCESS);
    write_config(path, "[usb]\nunknown=yes\n");
    assert(registered->reload() == -1);
    write_config(path, "[usb]\n");
    runtime_failures = 1;
    assert(registered->reload() == -1);
    runtime_failures = 2;
    assert(registered->reload() == -1);
    write_config(path, "");
    assert(registered->reload() == 0);
    assert(errors == 9);
    assert(registered->unload() == 0);
    assert(dlclose(handle) == 0 && !registered);
    assert(unlink(path) == 0);
    assert(rmdir(directory) == 0);
    puts("Asterisk shared-module lifecycle tests passed");
    return 0;
}
