/** @file test_site_gpio_abi.c
 * @brief Exercise the installed GPIO descriptor without accessing real hardware.
 */
#include "rptadv_gpiod_adapter.h"
#include <assert.h>
#include <dlfcn.h>
#include <string.h>

/** Load the actual DSO; absent configured hardware must fail without substitution. */
int main(int argc, char **argv) {
    assert(argc == 2);
    void *library = dlopen(argv[1], RTLD_NOW | RTLD_LOCAL);
    assert(library);
    const rptadv_gpiod_descriptor_v1 *(*entry)(void) =
        dlsym(library, "rptadv_gpiod_adapter_descriptor_v1");
    assert(entry);
    const rptadv_gpiod_descriptor_v1 *api = entry();
    assert(api && api->struct_size == sizeof(*api) && api->abi_version == 1);
    assert(memcmp(api->capability, "rptadv.gpiod", sizeof("rptadv.gpiod")) == 0);
    assert(api->open && api->read && api->write && api->capabilities && api->destroy);
    const uint8_t path[] = "/dev/rptadv-no-such-gpio-fixture";
    const rptadv_gpiod_endpoint_v1 endpoint = {
        .struct_size = sizeof(endpoint),
        .abi_version = 1,
        .device = path,
        .device_length = sizeof(path) - 1,
    };
    void *context = NULL;
    assert(api->open(&endpoint, &context) == 1 && !context);
    assert(api->open(NULL, &context) == -1);
    assert(api->read(NULL, NULL) == -1);
    assert(api->write(NULL, NULL) == -1);
    assert(api->capabilities(NULL, NULL) == -1);
    api->destroy(NULL);
    assert(dlclose(library) == 0);
    return 0;
}
