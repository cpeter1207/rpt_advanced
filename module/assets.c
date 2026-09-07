/* SPDX-License-Identifier: GPL-2.0-only */
/** @file
 * @brief File/Piper fallback and checked loading of rate-matched identifier PCM.
 */
#include "assets.h"
#include "speech.h"
#include <asterisk.h>
#include <asterisk/utils.h>
#include <stdio.h>
#include <time.h>
#include <unistd.h>

/** @brief Reap preparation outside real-time processing, cancelling a stuck child.
 * @param process Owned child.
 * @return True only for successful completion within the bounded wait.
 */
static bool completed(struct ra_speech *process) {
    const struct timespec pause = {.tv_nsec = 100000000};
    for (unsigned int attempt = 0; attempt < 300; ++attempt) {
        enum ra_speech_status status = ra_speech_poll(process);
        if (status != RA_SPEECH_RUNNING) {
            return status == RA_SPEECH_COMPLETE;
        }
        nanosleep(&pause, NULL);
    }
    ra_speech_cancel(process);
    return false;
}

/** @brief Read nonempty, whole-sample little-endian PCM into native signed samples.
 * @param path Temporary FFmpeg output.
 * @param samples Receives sample count only on success.
 * @return Owned PCM or null on input/allocation failure.
 */
static int16_t *read_pcm(const char *path, size_t *samples) {
    FILE *stream = fopen(path, "rb");
    if (!stream) {
        return NULL;
    }
    int16_t *audio = NULL;
    if (!fseek(stream, 0, SEEK_END)) {
        long bytes = ftell(stream);
        if (bytes > 0 && bytes % 2 == 0 && !fseek(stream, 0, SEEK_SET)) {
            audio = ast_malloc((size_t)bytes);
            if (audio) {
                if (fread(audio, 1, (size_t)bytes, stream) != (size_t)bytes) {
                    ast_free(audio);
                    audio = NULL;
                } else {
                    const unsigned char *raw = (const unsigned char *)audio;
                    *samples = (size_t)bytes / 2;
                    for (size_t i = 0; i < *samples; ++i) {
                        unsigned int value = raw[i * 2] | (unsigned int)raw[i * 2 + 1] << 8;
                        audio[i] = (int16_t)(value < 32768 ? (int)value : (int)value - 65536);
                    }
                }
            }
        }
    }
    fclose(stream);
    return audio;
}

/** @brief Convert an opened source using an exclusively created output file.
 * @param input Opened file, owned by caller.
 * @param rate Controller rate.
 * @param samples Receives length on success.
 * @return Owned PCM or null.
 */
static int16_t *convert(FILE *input, unsigned int rate, size_t *samples) {
    char path[] = "/tmp/rpt-advanced-pcm-XXXXXX";
    int descriptor = mkstemp(path);
    if (descriptor < 0) {
        return NULL;
    }
    close(descriptor);
    struct ra_speech process = {0};
    int16_t *audio = NULL;
    if (!ra_audio_prepare(&process, fileno(input), path, rate) && completed(&process)) {
        audio = read_pcm(path, samples);
    }
    unlink(path);
    return audio;
}

void ra_identifier_prepare(const struct ra_identifier_settings *settings, unsigned int rate,
                           int16_t **audio, size_t *samples) {
    *audio = NULL;
    *samples = 0;
    if (*settings->file) {
        FILE *input = fopen(settings->file, "rb");
        if (input) {
            *audio = convert(input, rate, samples);
            fclose(input);
        }
    }
    if (*audio || !*settings->speech_text) {
        return;
    }
    FILE *text = tmpfile();
    if (!text) {
        return;
    }
    if (fputs(settings->speech_text, text) < 0 || fflush(text) || fseek(text, 0, SEEK_SET)) {
        fclose(text);
        return;
    }
    char path[] = "/tmp/rpt-advanced-speech-XXXXXX";
    int descriptor = mkstemp(path);
    if (descriptor >= 0) {
        close(descriptor);
        struct ra_speech process = {0};
        if (!ra_piper_engine.start(&process, fileno(text), settings->speech_model, path,
                                   settings->speech_speed_percent) &&
            completed(&process)) {
            FILE *wave = fopen(path, "rb");
            if (wave) {
                *audio = convert(wave, rate, samples);
                fclose(wave);
            }
        }
        unlink(path);
    }
    fclose(text);
}
