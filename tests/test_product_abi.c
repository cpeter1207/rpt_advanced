/* SPDX-License-Identifier: GPL-2.0-only */
/* Public product/header compatibility probe; never starts workers or hardware. */
#include <assert.h>
#include <dlfcn.h>
#include <rptadv_product.h>
#include <stddef.h>
#include <string.h>

_Static_assert(sizeof(struct rptadv_product_descriptor_v1) == 128,
               "preserve the original complete product table");
_Static_assert(sizeof(struct rptadv_store_maintenance_v1) == 32,
               "maintenance is a separate complete descriptor");
_Static_assert(RPTADV_STORE_MAINTENANCE_ABI_VERSION == 1,
               "update the optional maintenance probe deliberately");
_Static_assert(RPTADV_PRODUCT_ABI_VERSION == 7, "update product ABI probe deliberately");
_Static_assert(RPTADV_HOST_ABI_VERSION == 10, "update host ABI probe deliberately");
_Static_assert(RPTADV_PEER_PHONE == 3 && RPTADV_PEER_PRIVATE_CALL == 32,
               "phone identity requires an explicit private-call capability");
_Static_assert(RPTADV_PEER_INFO_PREFIX_SIZE == 80, "preserve the original peer metadata prefix");
_Static_assert(RPTADV_PEER_INFO_PHONE_SIZE == 96,
               "phone admission requires the complete account tail");
_Static_assert(URP_AST_DIRECT_CALLBACKS_ABI_VERSION == 6,
               "use the canonical metadata-aware SDK callbacks");
_Static_assert(sizeof(UrpNativeReceiveMetadata) == 72,
               "preserve the source-associated receive metadata record");
_Static_assert(offsetof(struct rptadv_host_services_v10, radio_metadata_bind) == 360,
               "preserve the complete pre-remote ABI-10 prefix");
_Static_assert(offsetof(struct rptadv_host_services_v10, radio_service) == 368,
               "preserve the complete metadata tail");
_Static_assert(offsetof(struct rptadv_host_services_v10, radio_quiesce) == 376,
               "preserve remote callback order");
_Static_assert(offsetof(struct rptadv_host_services_v10, radio_remote_submit) == 384,
               "preserve remote callback order");
_Static_assert(offsetof(struct rptadv_host_services_v10, radio_ready) == 392,
               "preserve the complete remote tail");
_Static_assert(offsetof(struct rptadv_host_services_v10, station_binding) == 400,
               "complete old host services prefix");
_Static_assert(sizeof(struct rptadv_station_request_v1) == 48, "station request");
_Static_assert(sizeof(struct rptadv_station_binding_v1) == 48, "station binding operations");
_Static_assert(offsetof(struct rptadv_host_services_v10, phone_provider) == 408, "complete station tail");
_Static_assert(offsetof(struct rptadv_host_services_v10, asl_provider) == 416, "complete phone tail");
_Static_assert(sizeof(struct rptadv_asl_admission_v1) == 24, "complete admission callbacks");
_Static_assert(sizeof(struct rptadv_asl_provider_v1) == 40, "complete managed ASL operations");
_Static_assert(offsetof(struct rptadv_host_services_v10, managed_echo_provider) == 424, "complete ASL tail");
_Static_assert(sizeof(struct rptadv_managed_echo_provider_v1) == 40, "complete managed Echo operations");
_Static_assert(sizeof(struct rptadv_host_services_v10) == 432,
               "update the complete ABI-10 size deliberately");

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
    _Static_assert(sizeof(struct rptadv_product_descriptor_v1) == 128,
                   "frozen legacy product table");
    _Static_assert(sizeof(struct rptadv_product_management_v1) == 32, "management startup table");
    _Static_assert(sizeof(struct rptadv_product_inspection_v1) == 32, "managed read-only table");
    const struct rptadv_product_inspection_v1 *(*inspection_getter)(void) =
        dlsym(library, "rptadv_product_inspection_descriptor_v1");
    assert(inspection_getter);
    const struct rptadv_product_inspection_v1 *inspection = inspection_getter();
    assert(inspection && inspection->abi_version == 1 &&
           inspection->struct_size == sizeof(*inspection));
    assert(!memcmp(inspection->capability, "rptadv.inspect1", sizeof(inspection->capability)));
    assert(inspection->inspect);

    const struct rptadv_product_management_v1 *(*management_getter)(void) =
        dlsym(library, "rptadv_product_management_descriptor_v1");
    assert(management_getter);
    const struct rptadv_product_management_v1 *management = management_getter();
    assert(management && management->struct_size == sizeof(*management));
    const unsigned char management_capability[16] = "rptadv.mgmt1";
    assert(management->abi_version == 1 && management->start);
    assert(!memcmp(management->capability, management_capability, sizeof(management_capability)));
    assert(product->struct_size == sizeof(*product));
    assert(product->abi_version == RPTADV_PRODUCT_ABI_VERSION);
    const unsigned char capability[16] = RPTADV_PRODUCT_CAPABILITY;
    assert(memcmp(product->capability, capability, sizeof(capability)) == 0);
    const struct rptadv_store_maintenance_v1 *(*maintenance_descriptor)(void) =
        dlsym(library, "rptadv_product_store_maintenance_descriptor_v1");
    assert(maintenance_descriptor);
    const struct rptadv_store_maintenance_v1 *maintenance = maintenance_descriptor();
    assert(maintenance && maintenance->struct_size == sizeof(*maintenance));
    assert(maintenance->abi_version == RPTADV_STORE_MAINTENANCE_ABI_VERSION);
    const unsigned char maintenance_capability[16] = RPTADV_STORE_MAINTENANCE_CAPABILITY;
    assert(memcmp(maintenance->capability, maintenance_capability,
                  sizeof(maintenance_capability)) == 0);
    assert(maintenance->run);
    assert(maintenance->run(999, NULL, 0, NULL, NULL) < 0);
    assert(strcmp(RPTADV_HOST_CAPABILITY, "rptadv.hst10") == 0);
    assert(product->start && product->reload && product->stop && product->authorize_incoming);
    assert(product->incoming && product->link_command && product->link_status && product->digit);
    assert(product->inspect_configuration && product->inspect_secrets);
    assert(product->site_command);
    const char configuration[] = "[1000]\nnode_enabled=no\n";
    unsigned count = 0;
    assert(product->inspect_configuration(configuration, sizeof(configuration) - 1, node, &count,
                                          NULL, NULL) == 0);
    assert(count == 1);
    assert(dlclose(library) == 0);
    return 0;
}
