/* SPDX-License-Identifier: GPL-2.0-only */
/** @file Public Asterisk declarations used only by the Rust boundary. */
#include <asterisk.h>
#include <asterisk/app.h>
#include "include/rptadv_asterisk_adapter.h"
#include "../product/include/rptadv_product.h"
#include "../control-asterisk-adapter/include/rptadv_control_asterisk_adapter.h"
#include "../file-adapter/include/rptadv_file_adapter.h"
#include "../speech-adapter/include/rptadv_speech_adapter.h"
#include <asterisk/astobj2.h>
#include <asterisk/channel.h>
#include <asterisk/codec.h>
#include <asterisk/format.h>
#include <asterisk/format_cache.h>
#include <asterisk/format_cap.h>
#include <asterisk/frame.h>
#include <asterisk/translate.h>
#include <asterisk/config.h>
#include <asterisk/netsock2.h>
#include <asterisk/srv.h>
#include <asterisk/module.h>
#include <asterisk/cli.h>
#include <asterisk/pbx.h>
#include <asterisk/paths.h>
#include <asterisk/logger.h>
#include <time.h>
