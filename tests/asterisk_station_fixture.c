/* SPDX-License-Identifier: GPL-2.0-only */
/** @file Test-only typed station binding for the real Asterisk channel fixture. */
#include <stdlib.h>
#include <string.h>
#include <usbradioplus_product.h>

struct plan {
    const char *address;
};

/* Real typed CM119 device selectors address deterministic channels in radio_fixture.c. */
static int select_address(void *context, const UrpNativeStationConfig *config) {
    static const char *const names[] = {"full",         "half",         "media",
                                        "network-a",    "network-b",    "network-dtmf",
                                        "network-echo", "network-full", "network-replacement"};
    if (!config || config->struct_size < sizeof(*config) || config->device_identifier_length != 3 ||
        !config->device_identifier) {
        return -1;
    }
    const uint8_t *name = config->device_identifier;
    if (name[0] != '3' || name[1] != '-' || name[2] < '1' || name[2] > '9') {
        return -1;
    }
    ((struct plan *)context)->address = names[name[2] - '1'];
    return 0;
}

static int prepare(const UrpBoundStationRequest *request, void **out) {
    if (!out) {
        return -1;
    }
    *out = NULL;
    if (!request || request->struct_size != sizeof(*request) || request->abi_version != 1 ||
        !request->candidate_identity || !request->prepared ||
        request->product != usbradioplus_product_descriptor_v1()) {
        return -1;
    }
    const UrpStationOwnershipDescriptor *owner = usbradioplus_station_ownership_descriptor();
    if (!owner || owner->struct_size < sizeof(*owner) || owner->abi_version != 1 ||
        !owner->copy_prepared) {
        return -1;
    }
    void *configuration = NULL;
    if (owner->copy_prepared(request->product, request->prepared, &configuration) != 0) {
        return -1;
    }
    struct plan *plan = calloc(1, sizeof(*plan));
    int code =
        plan ? request->product->native_configuration_visit(configuration, select_address, plan)
             : -1;
    request->product->native_configuration_destroy(configuration);
    if (code || !plan || !plan->address) {
        free(plan);
        return -1;
    }
    *out = plan;
    return 0;
}

static int with_request(const void *raw, void *context, UrpStationRequestChannel request) {
    const struct plan *plan = raw;
    return (plan && request) ? request(context, plan->address) : -1;
}

/* The fixture has no live reconfiguration. A new channel must be acquired and observed. */
static int stage(void *channel, const void *raw) { return channel && raw ? 1 : -1; }
static int finish(void *channel, uint32_t commit) {
    (void)channel;
    (void)commit;
    return -1;
}

const UrpAstStationBindingDescriptor *usbradioplus_asterisk_station_binding_descriptor(void) {
    static const UrpAstStationBindingDescriptor ops = {.struct_size = sizeof(ops),
                                                       .abi_version = 1,
                                                       .capability_name =
                                                           "usbradioplus.asterisk-station1",
                                                       .prepare = prepare,
                                                       .with_request = with_request,
                                                       .stage = stage,
                                                       .finish = finish,
                                                       .release = free};
    return &ops;
}
