/* SPDX-License-Identifier: GPL-2.0-only */
/** @file
 * @brief Exercise real POSIX process execution through the Piper adapter.
 */
#define _GNU_SOURCE
#include "speech.h"
#include <assert.h>
#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <unistd.h>

/** @brief Mock host coordination count; process operations themselves are real. */
static int reaper;
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
    assert(fputs("test output", output) >= 0);
    assert(fclose(output) == 0);
    return 0;
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
    assert(unlink(output_path) == 0);
    assert(rmdir(directory) == 0);
    free(executable);
    free(program);
    free(output_path);
    puts("real speech subprocess integration tests passed");
    return 0;
}
