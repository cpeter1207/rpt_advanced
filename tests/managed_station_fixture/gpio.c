/* SPDX-License-Identifier: GPL-2.0-only */
/** @file
 * @brief Private foreground-test port of USBRadioPlus provider_support.rs GPIO.
 */
#include <rptadv_gpio_adapter/rptadv_gpio_adapter.h>

#include <stdatomic.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

#define GPIO_ABI RPTADV_GPIO_ADAPTER_ABI_VERSION

/** @brief Selected fixture HID identity, shared with the fixture audio table. */
static const struct rptadv_gpio_device_info identity = {
    .struct_size = sizeof(identity),
    .abi_version = GPIO_ABI,
    .present = 1,
    .vendor_id = RPTADV_GPIO_CM119_DEFAULT_VENDOR_ID,
    .product_id = 0x000c,
    .usb_bus = 3,
    .usb_port_number_count = 1,
    .usb_port_numbers = {1},
    .serial = "ABC",
};

/** @brief Memory-backed HID, allocated and closed by the real station owner. */
struct rptadv_gpio_device {
    _Atomic uint32_t ptt;
    FILE *progress_ptt;
    _Atomic uint32_t outputs;
    _Atomic uint64_t service_count;
    struct rptadv_gpio_eeprom_image eeprom;
};

/** @brief Memory-backed parallel register for the existing fixture's parallel operations. */
struct rptadv_gpio_parallel_device {
    _Atomic uint32_t outputs;
};

static enum rptadv_gpio_result device_probe(const struct rptadv_gpio_device_config *config,
                                           struct rptadv_gpio_device_info *output)
{
    *output = identity;
    const char *path = config->usb_port_path;
    output->present = (strcmp(path, "3-1") == 0 || strcmp(path, "3-1:1.0") == 0)
        && (config->vendor_id == 0 || config->vendor_id == identity.vendor_id)
        && (config->product_id == 0 || config->product_id == identity.product_id);
    return RPTADV_GPIO_OK;
}

static enum rptadv_gpio_result device_discover(struct rptadv_gpio_device_list *output)
{
    *output = (struct rptadv_gpio_device_list){
        .struct_size = sizeof(*output),
        .abi_version = GPIO_ABI,
        .matching_device_count = 1,
        .returned_device_count = 1,
        .devices = {identity},
    };
    return RPTADV_GPIO_OK;
}

static enum rptadv_gpio_result device_open(const struct rptadv_gpio_device_config *config,
                                          struct rptadv_gpio_device **output)
{
    struct rptadv_gpio_device_info info;
    device_probe(config, &info);
    if (!info.present) {
        return RPTADV_GPIO_UNSUPPORTED;
    }
    struct rptadv_gpio_device *device = calloc(1, sizeof(*device));
    if (!device) {
        return RPTADV_GPIO_NO_MEMORY;
    }
    const char *trace = getenv("RPTADV_TEST_SIP_PROGRESS_PTT");
    if (trace) {
        device->progress_ptt = fopen(trace, "w");
        if (!device->progress_ptt) {
            free(device);
            return RPTADV_GPIO_UNSUPPORTED;
        }
        fputc('0', device->progress_ptt);
        fflush(device->progress_ptt);
    }
    atomic_init(&device->ptt, 0);
    atomic_init(&device->outputs, config->gpio_output_initial_mask);
    atomic_init(&device->service_count, 0);
    device->eeprom = (struct rptadv_gpio_eeprom_image){
        .struct_size = sizeof(device->eeprom),
        .abi_version = GPIO_ABI,
        .checksum_valid = 1,
        .magic_valid = 1,
        .words = {[52] = 321, [53] = 411, [54] = 512, [59] = 63, [60] = 654},
    };
    *output = device;
    return RPTADV_GPIO_OK;
}

static enum rptadv_gpio_result device_publish(struct rptadv_gpio_device *device,
                                             const struct rptadv_gpio_output_action *action)
{
    uint32_t prior = atomic_exchange(&device->ptt, action->ptt_asserted);
    if (device->progress_ptt && prior != action->ptt_asserted) {
        fputc(action->ptt_asserted ? '1' : '0', device->progress_ptt);
        fflush(device->progress_ptt);
    }
    atomic_store(&device->outputs, action->gpio_output_mask);
    return RPTADV_GPIO_OK;
}

static enum rptadv_gpio_result device_service(struct rptadv_gpio_device *device)
{
    atomic_fetch_add(&device->service_count, 1);
    return RPTADV_GPIO_OK;
}

static enum rptadv_gpio_result device_inputs(const struct rptadv_gpio_device *device,
                                            struct rptadv_gpio_input_snapshot *output)
{
    (void)device;
    /* Keep the qualified default fixture behavior outside the idle RTP case. */
    const char *edge = getenv("RPTADV_TEST_SIP_CARRIER_EDGE");
    const uint32_t carrier = getenv("RPTADV_TEST_SIP_IDLE_RTP") == NULL
                             || (edge != NULL && access(edge, F_OK) == 0);
    *output = (struct rptadv_gpio_input_snapshot){
        .struct_size = sizeof(*output),
        .abi_version = GPIO_ABI,
        .online = 1,
        .cor_active = carrier,
        .ctcss_active = carrier,
    };
    return RPTADV_GPIO_OK;
}

static enum rptadv_gpio_result device_stats(const struct rptadv_gpio_device *device,
                                           struct rptadv_gpio_device_stats *output)
{
    uint64_t count = atomic_load(&device->service_count);
    *output = (struct rptadv_gpio_device_stats){
        .struct_size = sizeof(*output),
        .abi_version = GPIO_ABI,
        .input_read_count = count,
        .output_apply_count = count,
        .ptt_applied = atomic_load(&device->ptt),
        .online = 1,
    };
    return RPTADV_GPIO_OK;
}

static void device_close(struct rptadv_gpio_device *device)
{
    if (device->progress_ptt) {
        fclose(device->progress_ptt);
    }
    free(device);
}

static enum rptadv_gpio_result device_read_eeprom(struct rptadv_gpio_device *device,
                                                 struct rptadv_gpio_eeprom_image *image)
{
    *image = device->eeprom;
    return RPTADV_GPIO_OK;
}

static enum rptadv_gpio_result device_write_eeprom(struct rptadv_gpio_device *device,
                                                  struct rptadv_gpio_eeprom_image *image)
{
    image->checksum_valid = 1;
    image->magic_valid = 1;
    device->eeprom = *image;
    return RPTADV_GPIO_OK;
}

static enum rptadv_gpio_result parallel_open(const struct rptadv_gpio_parallel_config *config,
                                            struct rptadv_gpio_parallel_device **output)
{
    struct rptadv_gpio_parallel_device *device = calloc(1, sizeof(*device));
    if (!device) {
        return RPTADV_GPIO_NO_MEMORY;
    }
    atomic_init(&device->outputs, config->output_initial_mask);
    *output = device;
    return RPTADV_GPIO_OK;
}

static enum rptadv_gpio_result parallel_write(struct rptadv_gpio_parallel_device *device,
                                             uint32_t data)
{
    atomic_store(&device->outputs, data);
    return RPTADV_GPIO_OK;
}

static enum rptadv_gpio_result parallel_publish(
    struct rptadv_gpio_parallel_device *device, const struct rptadv_gpio_parallel_output_action *action)
{
    return parallel_write(device, action->output_mask);
}

static enum rptadv_gpio_result parallel_service(struct rptadv_gpio_parallel_device *device)
{
    (void)device;
    return RPTADV_GPIO_OK;
}

static enum rptadv_gpio_result parallel_inputs(const struct rptadv_gpio_parallel_device *device,
                                              struct rptadv_gpio_parallel_input_snapshot *output)
{
    (void)device;
    *output = (struct rptadv_gpio_parallel_input_snapshot){
        .struct_size = sizeof(*output),
        .abi_version = GPIO_ABI,
        .online = 1,
    };
    return RPTADV_GPIO_OK;
}

static enum rptadv_gpio_result parallel_stats(const struct rptadv_gpio_parallel_device *device,
                                             struct rptadv_gpio_parallel_stats *output)
{
    *output = (struct rptadv_gpio_parallel_stats){
        .struct_size = sizeof(*output),
        .abi_version = GPIO_ABI,
        .online = 1,
        .applied_output_mask = atomic_load(&device->outputs),
    };
    return RPTADV_GPIO_OK;
}

static void parallel_close(struct rptadv_gpio_parallel_device *device)
{
    free(device);
}

static int32_t parallel_close_outputs(struct rptadv_gpio_parallel_device *device, uint32_t outputs)
{
    parallel_write(device, outputs);
    parallel_close(device);
    return RPTADV_GPIO_OK;
}

/* Preserve the test table's accepted GPIO/parallel pulse and RTX operations. */
static enum rptadv_gpio_result device_pulse(
    struct rptadv_gpio_device *device, const struct rptadv_gpio_cm119_inverting_pulse_action *action)
{
    (void)device;
    (void)action;
    return RPTADV_GPIO_OK;
}

static enum rptadv_gpio_result device_schedule(
    struct rptadv_gpio_device *device,
    const struct rptadv_gpio_cm119_scheduled_inverting_pulse_action *action)
{
    (void)device;
    (void)action;
    return RPTADV_GPIO_OK;
}

static enum rptadv_gpio_result parallel_pulse(
    struct rptadv_gpio_parallel_device *device,
    const struct rptadv_gpio_parallel_inverting_pulse_action *action)
{
    (void)device;
    (void)action;
    return RPTADV_GPIO_OK;
}

static enum rptadv_gpio_result parallel_schedule(
    struct rptadv_gpio_parallel_device *device,
    const struct rptadv_gpio_parallel_scheduled_inverting_pulse_action *action)
{
    (void)device;
    (void)action;
    return RPTADV_GPIO_OK;
}

static enum rptadv_gpio_result parallel_channel(struct rptadv_gpio_parallel_device *device,
                                               uint8_t channel)
{
    (void)device;
    (void)channel;
    return RPTADV_GPIO_OK;
}

static enum rptadv_gpio_result parallel_program(
    struct rptadv_gpio_parallel_device *device, uint32_t receive_hz, uint32_t transmit_hz,
    uint32_t transmitting, uint32_t high_power)
{
    (void)device;
    (void)receive_hz;
    (void)transmit_hz;
    (void)transmitting;
    (void)high_power;
    return RPTADV_GPIO_OK;
}

static const struct rptadv_gpio_port_capabilities capabilities = {
    .struct_size = sizeof(capabilities),
    .input_pin_mask = 0xfb,
    .output_pin_mask = 0xfb,
};

static int32_t device_capabilities(const struct rptadv_gpio_device *device,
                                   struct rptadv_gpio_port_capabilities *output)
{
    (void)device;
    *output = capabilities;
    return RPTADV_GPIO_OK;
}

static int32_t parallel_capabilities(const struct rptadv_gpio_parallel_device *device,
                                     struct rptadv_gpio_port_capabilities *output)
{
    (void)device;
    *output = capabilities;
    return RPTADV_GPIO_OK;
}

_Static_assert(GPIO_ABI == 1, "expected GPIO ABI 1");
_Static_assert(sizeof(struct rptadv_gpio_adapter_descriptor) == 360, "expected full SDK53 table");

static const struct rptadv_gpio_adapter_descriptor GPIO = {
    .struct_size = sizeof(GPIO),
    .abi_version = GPIO_ABI,
    .capability_name = "rptadv.cm119-hid-gpio",
    .device_probe = device_probe,
    .device_open = device_open,
    .device_publish_outputs = device_publish,
    .device_service = device_service,
    .device_get_inputs = device_inputs,
    .device_get_stats = device_stats,
    .device_close = device_close,
    .device_discover = device_discover,
    .device_read_eeprom = device_read_eeprom,
    .device_write_eeprom = device_write_eeprom,
    .parallel_open = parallel_open,
    .parallel_publish_outputs = parallel_publish,
    .parallel_service = parallel_service,
    .parallel_control_write_data = parallel_write,
    .parallel_get_inputs = parallel_inputs,
    .parallel_get_stats = parallel_stats,
    .parallel_close = parallel_close,
    .device_publish_inverting_pulse = device_pulse,
    .parallel_publish_inverting_pulse = parallel_pulse,
    .device_schedule_inverting_pulse = device_schedule,
    .parallel_schedule_inverting_pulse = parallel_schedule,
    .parallel_set_binary_channel = parallel_channel,
    .parallel_program_rtx = parallel_program,
    .parallel_clear_rtx_transmit = parallel_service,
    .device_capabilities = device_capabilities,
    .parallel_capabilities = parallel_capabilities,
    .parallel_close_outputs = parallel_close_outputs,
};

/** @brief Immutable fixture descriptor for the loaded object's lifetime. */
__attribute__((visibility("default")))
const struct rptadv_gpio_adapter_descriptor *rptadv_gpio_adapter_descriptor(void)
{
    return &GPIO;
}
