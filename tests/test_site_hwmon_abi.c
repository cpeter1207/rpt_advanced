/** @file test_site_hwmon_abi.c
 * @brief Validate the real sensor descriptor without acquiring host sensors.
 */
#include "rptadv_hwmon_adapter.h"
#include <assert.h>
#include <dlfcn.h>
#include <string.h>

/** An absent exact endpoint cannot fall through to any other installed device. */
int main(int argc, char **argv) {
    assert(argc == 2);
    void *library = dlopen(argv[1], RTLD_NOW | RTLD_LOCAL);
    assert(library);
    const rptadv_hwmon_descriptor_v1 *(*entry)(void) =
        dlsym(library, "rptadv_hwmon_adapter_descriptor_v1");
    assert(entry);
    const rptadv_hwmon_descriptor_v1 *api = entry();
    assert(api && api->struct_size == sizeof(*api) && api->abi_version == 1);
    assert(memcmp(api->capability, "rptadv.hwmon", sizeof("rptadv.hwmon")) == 0);
    assert(api->open && api->read && api->write && api->capabilities && api->destroy);
    const uint8_t path[] = "/dev/rptadv-no-such-hwmon-fixture";
    const uint8_t channel[] = "temp1_input";
    rptadv_hwmon_endpoint_v1 endpoint = {
        .struct_size = sizeof(endpoint),
        .abi_version = 1,
        .device = path,
        .device_length = sizeof(path) - 1,
        .channel = channel,
        .channel_length = sizeof(channel) - 1,
    };
    void *context = NULL;
    assert(api->open(&endpoint, &context) == 1 && !context);
    endpoint.abi_version = 2;
    assert(api->open(&endpoint, &context) == -1 && !context);
    const uint32_t short_record = sizeof(uint32_t);
    assert(api->open((const void *)&short_record, &context) == -1 && !context);
    assert(api->open(NULL, &context) == -1);
    assert(api->open(&endpoint, NULL) == -1);
    assert(api->read(NULL, NULL) == -1);
    assert(api->write(NULL, NULL) == -1);
    assert(api->capabilities(NULL, NULL) == -1);
    api->destroy(NULL);
    assert(dlclose(library) == 0);
    return 0;
}
