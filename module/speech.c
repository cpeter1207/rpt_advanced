/* SPDX-License-Identifier: GPL-2.0-only */
/** @file
 * @brief Offline identifier preparation with shared child-process ownership.
 */
#include <asterisk.h>

#include "speech.h"
#include <asterisk/app.h>
#include <errno.h>
#include <limits.h>
#include <signal.h>
#include <spawn.h>
#include <sys/wait.h>
#include <unistd.h>

/** @brief Environment passed to the separately executed offline synthesizer. */
extern char **environ;

/** @brief Start an offline preparation command with file-backed stdin.
 * @param state Child ownership record.
 * @param input Rewound input descriptor, greater than standard error.
 * @param arguments Null-terminated command arguments, starting with executable name.
 * @return Zero on success, or a POSIX error number.
 */
static int preparation_start(struct ra_speech *state, int input, char *const arguments[]) {
    if (state->pid) {
        return EBUSY;
    }
    if (input <= STDERR_FILENO) {
        return EINVAL;
    }
    posix_spawn_file_actions_t actions;
    int error = posix_spawn_file_actions_init(&actions);
    if (error) {
        return error;
    }
    error = posix_spawn_file_actions_adddup2(&actions, input, STDIN_FILENO);
    if (!error) {
        error = posix_spawn_file_actions_addclose(&actions, input);
    }
    if (!error) {
        ast_replace_sigchld();
        error = posix_spawnp(&state->pid, arguments[0], &actions, NULL, arguments, environ);
        if (error) {
            ast_unreplace_sigchld();
        }
    }
    posix_spawn_file_actions_destroy(&actions);
    if (error) {
        state->pid = 0;
        state->status = RA_SPEECH_FAILED;
    } else {
        state->status = RA_SPEECH_RUNNING;
    }
    return error;
}

/** @brief Start Piper without shell interpretation or locale-dependent numbers.
 * @param state Child ownership record.
 * @param input Rewound text descriptor.
 * @param model Local voice model path.
 * @param output Caller-owned WAV output path.
 * @param speed Speaking-rate percentage, 1 through 1000.
 * @return Zero on start or a POSIX error number.
 */
static int piper_start(struct ra_speech *state, int input, const char *model, const char *output,
                       unsigned int speed) {
    if (!speed || speed > 1000) {
        return EINVAL;
    }
    unsigned int scale = 100000000 / speed;
    char length[] = "000.000000";
    for (size_t index = sizeof(length) - 1; index > 0;) {
        --index;
        if (index == 3) {
            continue;
        }
        length[index] = (char)('0' + scale % 10);
        scale /= 10;
    }
    char *arguments[] = {"piper",        "--model",        (char *)model, "--output_file",
                         (char *)output, "--length_scale", length,        NULL};
    return preparation_start(state, input, arguments);
}

int ra_audio_prepare(struct ra_speech *state, int input, const char *output, unsigned int rate) {
    if (!rate) {
        return EINVAL;
    }
    char number[sizeof(rate) * CHAR_BIT / 3 + 2];
    char *digits = number + sizeof(number) - 1;
    *digits = '\0';
    do {
        *--digits = (char)('0' + rate % 10);
        rate /= 10;
    } while (rate);
    /* Restrict input protocols: a configured local file must not fetch network media. */
    char *arguments[] = {
        "ffmpeg",       "-nostdin", "-v",     "error", "-y",    "-protocol_whitelist",
        "file,pipe",    "-i",       "pipe:0", "-map",  "0:a:0", "-vn",
        "-ac",          "1",        "-ar",    digits,  "-f",    "s16le",
        (char *)output, NULL};
    return preparation_start(state, input, arguments);
}

const struct ra_speech_engine ra_piper_engine = {.start = piper_start};

enum ra_speech_status ra_speech_poll(struct ra_speech *state) {
    if (!state->pid) {
        return state->status;
    }
    int status;
    pid_t result = waitpid(state->pid, &status, WNOHANG);
    if (result == 0 || (result < 0 && errno == EINTR)) {
        return RA_SPEECH_RUNNING;
    }
    state->pid = 0;
    ast_unreplace_sigchld();
    state->status = result > 0 && WIFEXITED(status) && WEXITSTATUS(status) == 0 ? RA_SPEECH_COMPLETE
                                                                                : RA_SPEECH_FAILED;
    return state->status;
}

void ra_speech_cancel(struct ra_speech *state) {
    if (!state->pid) {
        return;
    }
    /* This PID is our unreaped child; it cannot be reused before waitpid. */
    kill(state->pid, SIGKILL);
    while (waitpid(state->pid, NULL, 0) < 0 && errno == EINTR) {
    }
    state->pid = 0;
    ast_unreplace_sigchld();
    state->status = RA_SPEECH_FAILED;
}
