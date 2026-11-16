/* SPDX-License-Identifier: GPL-2.0-only */
#include "include/rptadv_product.h"
#include "../control-asterisk-adapter/include/rptadv_control_asterisk_adapter.h"
#include "../file-adapter/include/rptadv_file_adapter.h"
#include "../speech-adapter/include/rptadv_speech_adapter.h"
#include "../gpiod-adapter/include/rptadv_gpiod_adapter.h"
#include "../hwmon-adapter/include/rptadv_hwmon_adapter.h"
#include <rate_adjusting_pcm_ring3/rate_adjusting_pcm_ring3.h>
#include <rptadv_samplerate_adapter/rptadv_samplerate_adapter.h>
#include <rptadvradio/rptadvradio.h>
#include <usbradioplus_product.h>

/* Bindgen cannot evaluate the provider's UINT64_C function-like macros.
 * This internal enum binds their exact mask and rejects older headers. */
enum {
    RPTADV_PHONE_SPEECH_REQUIRED_CAPABILITIES =
        RPTADV_RADIO_SPEECH_PREPROCESSING_CAPABILITY | RPTADV_RADIO_SPEECH_ANALYSIS_RESET_CAPABILITY
};
