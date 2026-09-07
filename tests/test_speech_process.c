/* SPDX-License-Identifier: GPL-2.0-only */
/** @file
 * @brief Exercise real POSIX process execution through the Piper adapter.
 */
#define _GNU_SOURCE
#include "assets.h"
#include "speech.h"
#include <assert.h>
#include <errno.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <unistd.h>

/** @brief Mock host coordination count; process operations themselves are real. */
static int reaper;
static void wave_fixture(FILE *file);

/** @brief Supply Asterisk allocation for the standalone process fixture.
 * @param size Bytes.
 * @param file Caller file.
 * @param line Caller line.
 * @param function Caller function.
 * @return Allocated memory.
 */
void *__ast_malloc(size_t size, const char *file, int line, const char *function) {
    (void)file;
    (void)line;
    (void)function;
    return malloc(size);
}
/** @brief Supply Asterisk deallocation for the standalone process fixture.
 * @param pointer Memory.
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
/** @brief Enter host child ownership. */
void ast_replace_sigchld(void) { ++reaper; }
/** @brief Verify balanced exit from child ownership. */
void ast_unreplace_sigchld(void) { assert(reaper-- == 1); }

/** @brief Poll a real process with a bounded monotonic deadline.
 * @param state Owned child process.
 * @return Child's terminal status.
 */
static enum ra_speech_status finish(struct ra_speech *state) {
    struct timespec start, now;
    assert(clock_gettime(CLOCK_MONOTONIC, &start) == 0);
    while (ra_speech_poll(state) == RA_SPEECH_RUNNING) {
        assert(clock_gettime(CLOCK_MONOTONIC, &now) == 0);
        if (now.tv_sec - start.tv_sec >= 30) {
            ra_speech_cancel(state);
            assert(0 && "child did not finish within test deadline");
        }
        nanosleep(&(struct timespec){.tv_nsec = 1000000}, NULL);
    }
    assert(!reaper && !state->pid);
    return state->status;
}

/** @brief Act as a deterministic synthesizer when invoked through the piper symlink.
 * @param arguments Engine command-line arguments.
 * @return Selected child exit code.
 */
static int child(char **arguments) {
    assert(strcmp(arguments[1], "--model") == 0);
    assert(strcmp(arguments[3], "--output_file") == 0);
    assert(strcmp(arguments[5], "--length_scale") == 0);
    assert(strcmp(arguments[6], "002.000000") == 0);
    if (strcmp(arguments[2], "fail") == 0) {
        return 23;
    }
    if (strcmp(arguments[2], "wait") == 0) {
        for (;;) {
            pause();
        }
    }
    char input[64];
    assert(fgets(input, sizeof(input), stdin));
    assert(strcmp(input, "Identifier; $(not a command)\n") == 0);
    FILE *output = fopen(arguments[4], "w");
    assert(output);
    if (!strcmp(arguments[2], "wave")) {
        wave_fixture(output);
    } else {
        assert(fputs("test output", output) >= 0);
    }
    assert(fclose(output) == 0);
    return 0;
}

/** @brief Write a 100 ms, 22050 Hz PCM WAV, outside Asterisk's WAV reader rates.
 * @param file Empty temporary file positioned at its beginning.
 */
static void wave_fixture(FILE *file) {
    static const unsigned char header[] = {
        'R', 'I', 'F', 'F', 0x5e, 0x11, 0,   0,   'W', 'A',  'V',  'E',  'f', 'm',  't',
        ' ', 16,  0,   0,   0,    1,    0,   1,   0,   0x22, 0x56, 0,    0,   0x44, 0xac,
        0,   0,   2,   0,   16,   0,    'd', 'a', 't', 'a',  0x3a, 0x11, 0,   0};
    assert(fwrite(header, 1, sizeof(header), file) == sizeof(header));
    for (unsigned int sample = 0; sample < 2205; ++sample) {
        assert(fwrite("\xe8\x03", 1, 2, file) == 2);
    }
    rewind(file);
}

/** @brief Test process startup, stdin, arguments, exit failure, cancellation, and missing Piper.
 * @param argc Argument count; seven identifies the child invocation.
 * @param argv Command-line arguments.
 * @return Zero after assertions and cleanup.
 */
int main(int argc, char **argv) {
    if (argc == 7) {
        return child(argv);
    }
    char directory[] = "/tmp/rpt-advanced-speech-XXXXXX";
    assert(mkdtemp(directory));
    char *executable = realpath("/proc/self/exe", NULL);
    char *program, *output_path;
    assert(executable);
    assert(asprintf(&program, "%s/piper", directory) > 0);
    assert(asprintf(&output_path, "%s/output.wav", directory) > 0);
    assert(symlink(executable, program) == 0);
    assert(setenv("PATH", directory, 1) == 0);
    FILE *input = tmpfile();
    assert(input);
    assert(fputs("Identifier; $(not a command)\n", input) >= 0);
    rewind(input);
    struct ra_speech state = {0};
    assert(ra_piper_engine.start(&state, fileno(input), "model", output_path, 50) == 0);
    assert(finish(&state) == RA_SPEECH_COMPLETE);
    FILE *output = fopen(output_path, "r");
    assert(output);
    char data[32];
    assert(fgets(data, sizeof(data), output));
    assert(strcmp(data, "test output") == 0);
    assert(fclose(output) == 0);
    assert(ra_piper_engine.start(&state, fileno(input), "fail", output_path, 50) == 0);
    assert(finish(&state) == RA_SPEECH_FAILED);
    assert(ra_piper_engine.start(&state, fileno(input), "wait", output_path, 50) == 0);
    ra_speech_cancel(&state);
    assert(state.status == RA_SPEECH_FAILED && !state.pid && !reaper);
    assert(unlink(program) == 0);
    assert(ra_piper_engine.start(&state, fileno(input), "model", output_path, 50) == ENOENT);
    assert(!reaper && !state.pid);
    assert(fclose(input) == 0);
    assert(setenv("PATH", "/usr/bin:/bin", 1) == 0);
    input = tmpfile();
    assert(input);
    wave_fixture(input);
    assert(ra_audio_prepare(&state, fileno(input), output_path, 0) == EINVAL);
    assert(ra_audio_prepare(&state, fileno(input), output_path, 48000) == 0);
    assert(finish(&state) == RA_SPEECH_COMPLETE);
    output = fopen(output_path, "rb");
    assert(output);
    int16_t samples[4801];
    assert(fread(samples, sizeof(*samples), 4801, output) == 4800);
    assert(samples[2400] >= 999 && samples[2400] <= 1001);
    assert(fclose(output) == 0);
    assert(fclose(input) == 0);
    input = tmpfile();
    assert(input);
    assert(fputs("not an audio file", input) >= 0);
    rewind(input);
    assert(ra_audio_prepare(&state, fileno(input), output_path, 48000) == 0);
    assert(finish(&state) == RA_SPEECH_FAILED);
    assert(fclose(input) == 0);
    output = fopen(output_path, "wb");
    assert(output);
    wave_fixture(output);
    assert(!fclose(output));
    struct ra_identifier_settings settings = {
        .file = output_path, .speech_text = "", .speech_model = "wave", .speech_speed_percent = 50};
    int16_t *prepared;
    size_t prepared_count;
    ra_identifier_prepare(&settings, 16000, &prepared, &prepared_count);
    assert(prepared && prepared_count == 1600 && prepared[800] >= 999 && prepared[800] <= 1001);
    free(prepared);
    assert(!symlink(executable, program));
    char *search_path;
    assert(asprintf(&search_path, "%s:/usr/bin:/bin", directory) > 0);
    assert(!setenv("PATH", search_path, 1));
    free(search_path);
    settings.file = "/no/such/identifier.wav";
    settings.speech_text = "Identifier; $(not a command)\n";
    ra_identifier_prepare(&settings, 48000, &prepared, &prepared_count);
    assert(prepared && prepared_count == 4800 && prepared[2400] >= 999 && prepared[2400] <= 1001);
    free(prepared);
    settings.speech_model = "fail";
    ra_identifier_prepare(&settings, 48000, &prepared, &prepared_count);
    assert(!prepared && !prepared_count && !reaper);
    assert(!unlink(program));
    assert(unlink(output_path) == 0);
    assert(rmdir(directory) == 0);
    free(executable);
    free(program);
    free(output_path);
    puts("real speech subprocess integration tests passed");
    return 0;
}
