/* SPDX-License-Identifier: GPL-2.0-only */
/* Public product/header compatibility probe; never starts workers or hardware. */
#include <assert.h>
#include <dlfcn.h>
#include <rptadv_product.h>
#include <string.h>

_Static_assert(RPTADV_PRODUCT_ABI_VERSION == 4, "update product ABI probe deliberately");
_Static_assert(RPTADV_HOST_ABI_VERSION == 5, "update host ABI probe deliberately");

static int node(void *context, const struct rptadv_node_host_configuration *record) {
    assert(record);
    assert(record->struct_size == sizeof(*record));
    assert(record->node_length == 4 && memcmp(record->node, "1000", 4) == 0);
    assert(record->enabled == 0);
    assert(record->radio);
    ++*(unsigned *)context;
    return 0;
}

int main(int argc, char **argv) {
    assert(argc == 2);
    void *library = dlopen(argv[1], RTLD_NOW | RTLD_LOCAL);
    assert(library);
    const struct rptadv_product_descriptor_v1 *(*descriptor)(void) =
        dlsym(library, "rptadv_product_descriptor_v1");
    assert(descriptor);
    const struct rptadv_product_descriptor_v1 *product = descriptor();
    assert(product);
    assert(product->struct_size == sizeof(*product));
    assert(product->abi_version == RPTADV_PRODUCT_ABI_VERSION);
    const unsigned char capability[16] = RPTADV_PRODUCT_CAPABILITY;
    assert(memcmp(product->capability, capability, sizeof(capability)) == 0);
    assert(strcmp(RPTADV_HOST_CAPABILITY, "rptadv.hst5") == 0);
    assert(product->start && product->reload && product->stop && product->authorize_incoming);
    assert(product->incoming && product->link_command && product->link_status && product->digit);
    assert(product->inspect_configuration && product->inspect_secrets);
    const char configuration[] = "[1000]\nnode_enabled=no\n";
    unsigned count = 0;
    assert(product->inspect_configuration(configuration, sizeof(configuration) - 1, node, &count,
                                          NULL, NULL) == 0);
    assert(count == 1);
    assert(dlclose(library) == 0);
    return 0;
}
