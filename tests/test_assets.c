/* SPDX-License-Identifier: GPL-2.0-only */
/** @file
 * @brief Identifier preparation fallback, PCM validation, and failure cleanup.
 */
#include "assets.h"
#include "speech.h"
#include <assert.h>
#include <asterisk.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <unistd.h>

/** @brief Selected preparation failure. */
enum failure {
    NONE,
    TEMP,
    TEXT,
    FLUSH,
    SEEK_TEXT,
    TEMP_NAME,
    CONVERT,
    SYNTHESIS,
    CHILD,
    HANG,
    OPEN_PCM,
    OPEN_WAVE,
    FAIL_SEEK_END,
    TELL,
    SEEK_START,
    ALLOCATE,
    READ_SHORT
};
/** @brief Current injected failure. */
static enum failure failure;
/** @brief Generated PCM byte count, including deliberately malformed outputs. */
static size_t output_bytes = 4;
/** @brief Count of fake sleeps in a hung-child case. */
static unsigned int sleeps;
/** @brief Count of explicit child cancellations. */
static unsigned int cancelled;
/** @brief Count of completed speech invocations. */
static unsigned int speeches;
/** @brief Identify the file-backed text stream in I/O wrappers. */
static FILE *text_stream;

/** @brief Unwrapped libc file open.
 * @param path Filename.
 * @param mode Access mode.
 * @return Opened stream.
 */
FILE *__real_fopen(const char *path, const char *mode);
/** @brief Unwrapped temporary stream creation.
 * @return Temporary stream.
 */
FILE *__real_tmpfile(void);
/** @brief Unwrapped exclusive temporary file creation.
 * @param path Mutable template.
 * @return Descriptor.
 */
int __real_mkstemp(char *path);
/** @brief Unwrapped seek.
 * @param stream Stream.
 * @param offset Offset.
 * @param origin Origin.
 * @return Status.
 */
int __real_fseek(FILE *stream, long offset, int origin);
/** @brief Unwrapped stream position.
 * @param stream Stream.
 * @return Position.
 */
long __real_ftell(FILE *stream);
/** @brief Unwrapped text output.
 * @param value Text.
 * @param stream Stream.
 * @return Status.
 */
int __real_fputs(const char *value, FILE *stream);
/** @brief Unwrapped flush.
 * @param stream Stream.
 * @return Status.
 */
int __real_fflush(FILE *stream);
/** @brief Unwrapped binary read.
 * @param data Destination.
 * @param size Item size.
 * @param count Item count.
 * @param stream Stream.
 * @return Read items.
 */
size_t __real_fread(void *data, size_t size, size_t count, FILE *stream);

/** @brief Inject output-open failures.
 * @param path Filename.
 * @param mode Mode.
 * @return Stream or null.
 */
FILE *__wrap_fopen(const char *path, const char *mode) {
    if ((failure == OPEN_PCM && strstr(path, "-pcm-")) ||
        (failure == OPEN_WAVE && strstr(path, "-speech-"))) {
        return NULL;
    }
    return __real_fopen(path, mode);
}
/** @brief Inject temporary stream failure.
 * @return Stream or null.
 */
FILE *__wrap_tmpfile(void) {
    text_stream = failure == TEMP ? NULL : __real_tmpfile();
    return text_stream;
}
/** @brief Inject exclusive file creation failure.
 * @param path Template.
 * @return Descriptor or minus one.
 */
int __wrap_mkstemp(char *path) { return failure == TEMP_NAME ? -1 : __real_mkstemp(path); }
/** @brief Inject each distinct seek failure.
 * @param stream Stream.
 * @param offset Offset.
 * @param origin Origin.
 * @return Status.
 */
int __wrap_fseek(FILE *stream, long offset, int origin) {
    if ((failure == SEEK_TEXT && stream == text_stream) ||
        (failure == FAIL_SEEK_END && origin == SEEK_END) ||
        (failure == SEEK_START && stream != text_stream && origin == SEEK_SET)) {
        return -1;
    }
    return __real_fseek(stream, offset, origin);
}
/** @brief Inject position failure.
 * @param stream Stream.
 * @return Position or minus one.
 */
long __wrap_ftell(FILE *stream) { return failure == TELL ? -1 : __real_ftell(stream); }
/** @brief Inject text-write failure.
 * @param value Text.
 * @param stream Stream.
 * @return Status.
 */
int __wrap_fputs(const char *value, FILE *stream) {
    return failure == TEXT ? -1 : __real_fputs(value, stream);
}
/** @brief Inject text-flush failure.
 * @param stream Stream.
 * @return Status.
 */
int __wrap_fflush(FILE *stream) { return failure == FLUSH ? -1 : __real_fflush(stream); }
/** @brief Inject short PCM read.
 * @param data Destination.
 * @param size Item size.
 * @param count Item count.
 * @param stream Stream.
 * @return Number of items read.
 */
size_t __wrap_fread(void *data, size_t size, size_t count, FILE *stream) {
    return failure == READ_SHORT ? 0 : __real_fread(data, size, count, stream);
}
/** @brief Advance timeout tests without wall-clock delays.
 * @param request Requested pause.
 * @param remaining Unused remainder.
 * @return Zero.
 */
int __wrap_nanosleep(const struct timespec *request, struct timespec *remaining) {
    assert(request->tv_nsec == 100000000 && !remaining);
    ++sleeps;
    return 0;
}
/** @brief Asterisk allocation fixture.
 * @param size Bytes.
 * @param file Caller file.
 * @param line Caller line.
 * @param function Caller function.
 * @return Memory or null.
 */
void *__ast_malloc(size_t size, const char *file, int line, const char *function) {
    (void)file;
    (void)line;
    (void)function;
    return failure == ALLOCATE ? NULL : malloc(size);
}
/** @brief Asterisk memory release fixture.
 * @param pointer Owned memory.
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

/** @brief Generate exact PCM fixture bytes without calling wrapped file APIs.
 * @param path Output path.
 */
static void output(const char *path) {
    const unsigned char bytes[] = {0xff, 0x7f, 0x00, 0x80};
    FILE *stream = __real_fopen(path, "wb");
    assert(stream && fwrite(bytes, 1, output_bytes, stream) == output_bytes);
    assert(!fclose(stream));
}
int ra_audio_prepare(struct ra_speech *state, int input, const char *path, unsigned int rate) {
    assert(input > 2 && rate == 16000);
    (void)state;
    if (failure == CONVERT) {
        return -1;
    }
    output(path);
    return 0;
}
/** @brief Offline-engine fixture preserving the public speech adapter contract.
 * @param state Process record.
 * @param input Text descriptor.
 * @param model Configured model.
 * @param path Wave output.
 * @param speed Configured speed.
 * @return Start status.
 */
static int synthesize(struct ra_speech *state, int input, const char *model, const char *path,
                      unsigned int speed) {
    (void)state;
    assert(input > 2 && !strcmp(model, "existing.onnx") && speed == 100);
    ++speeches;
    if (failure == SYNTHESIS) {
        return -1;
    }
    output(path);
    return 0;
}
const struct ra_speech_engine ra_piper_engine = {.start = synthesize};
enum ra_speech_status ra_speech_poll(struct ra_speech *state) {
    (void)state;
    return failure == HANG    ? RA_SPEECH_RUNNING
           : failure == CHILD ? RA_SPEECH_FAILED
                              : RA_SPEECH_COMPLETE;
}
void ra_speech_cancel(struct ra_speech *state) {
    (void)state;
    ++cancelled;
}

/** @brief Check output validity and release successful preparation.
 * @param settings Resolved fixture settings.
 * @param success Expected PCM availability.
 */
static void check(const struct ra_identifier_settings *settings, bool success) {
    int16_t *audio;
    size_t samples;
    text_stream = NULL;
    ra_identifier_prepare(settings, 16000, &audio, &samples);
    assert((audio != NULL) == success);
    if (success) {
        assert(samples == 2 && audio[0] == 32767 && audio[1] == -32768);
    } else {
        assert(samples == 0);
    }
    free(audio);
}

/** @brief Test source priority, terminal fallback, timeout, and all I/O failures.
 * @return Zero after assertions and fixture cleanup.
 */
int main(void) {
    char source[] = "/tmp/rpt-advanced-source-XXXXXX";
    int descriptor = __real_mkstemp(source);
    assert(descriptor >= 0 && !close(descriptor));
    struct ra_identifier_settings settings = {.file = source,
                                              .speech_text = "",
                                              .speech_model = "existing.onnx",
                                              .speech_speed_percent = 100};
    check(&settings, true);
    for (failure = TEMP_NAME; failure <= READ_SHORT; ++failure) {
        if (failure != SYNTHESIS && failure != OPEN_WAVE && failure != SEEK_TEXT) {
            check(&settings, false);
        }
    }
    failure = NONE;
    output_bytes = 0;
    check(&settings, false);
    output_bytes = 1;
    check(&settings, false);
    output_bytes = 4;
    settings.speech_text = "TEST";
    check(&settings, true);
    assert(!speeches);
    settings.file = "/no/such/rpt-advanced-source";
    check(&settings, true);
    assert(speeches == 1);
    settings.speech_level_db = -6;
    int16_t *audio;
    size_t samples;
    ra_identifier_prepare(&settings, 16000, &audio, &samples);
    assert(audio && samples == 2 && audio[0] >= 16420 && audio[0] <= 16422 && audio[1] <= -16421 &&
           audio[1] >= -16423);
    free(audio);
    settings.speech_level_db = 0;
    settings.file = "";
    check(&settings, true);
    for (failure = TEMP; failure <= READ_SHORT; ++failure) {
        check(&settings, false);
    }
    assert(cancelled == 2 && sleeps == 600);
    failure = NONE;
    settings.speech_text = "";
    check(&settings, false);
    assert(!unlink(source));
    puts("file/speech preparation, PCM validation, and fallback tests passed");
    return 0;
}
