/* SPDX-License-Identifier: GPL-2.0-only */
/** @file
 * @brief Piper process failure injection and Asterisk reaper ownership tests.
 */
#include <asterisk.h>

#include "speech.h"
#include <assert.h>
#include <errno.h>
#include <signal.h>
#include <spawn.h>
#include <stdio.h>
#include <string.h>
#include <sys/wait.h>

/** @brief Startup step selected for failure injection; zero means success. */
static int failure;
/** @brief Balanced Asterisk reaper suspension count. */
static int reaper;
/** @brief Mock child completion returned by waitpid. */
static pid_t completion = 42;
/** @brief Mock child's wait status. */
static int exit_status;
/** @brief Number of EINTR responses before a wait completes. */
static int interruptions;
/** @brief Expected locale-independent Piper length scale. */
static const char *expected_scale = "001.000000";

/** @brief Suspend the mock Asterisk child reaper. */
void ast_replace_sigchld(void) { ++reaper; }
/** @brief Verify reaper restoration is balanced. */
void ast_unreplace_sigchld(void) { assert(reaper-- == 1); }

/** @brief Inject file-action allocation failure.
 * @param actions Unused mock action storage.
 * @return POSIX result.
 */
int __wrap_posix_spawn_file_actions_init(posix_spawn_file_actions_t *actions) {
    (void)actions;
    return failure == 1 ? ENOMEM : 0;
}
/** @brief Verify stdin duplication and inject failure.
 * @param actions Mock actions.
 * @param source Text descriptor.
 * @param target Child stdin descriptor.
 * @return POSIX result.
 */
int __wrap_posix_spawn_file_actions_adddup2(posix_spawn_file_actions_t *actions, int source,
                                            int target) {
    (void)actions;
    assert(source == 3 && target == 0);
    return failure == 2 ? ENOMEM : 0;
}
/** @brief Verify closure of the duplicated descriptor and inject failure.
 * @param actions Mock actions.
 * @param descriptor Original text descriptor.
 * @return POSIX result.
 */
int __wrap_posix_spawn_file_actions_addclose(posix_spawn_file_actions_t *actions, int descriptor) {
    (void)actions;
    assert(descriptor == 3);
    return failure == 3 ? ENOMEM : 0;
}
/** @brief Release mock file actions.
 * @param actions Mock actions.
 * @return Zero.
 */
int __wrap_posix_spawn_file_actions_destroy(posix_spawn_file_actions_t *actions) {
    (void)actions;
    return 0;
}
/** @brief Inspect direct execution arguments without starting a process.
 * @param pid Receives mock child PID.
 * @param executable Program name.
 * @param actions Mock descriptor actions.
 * @param attributes Spawn attributes.
 * @param arguments Argument vector.
 * @param environment Environment vector.
 * @return Zero or missing-executable error.
 */
int __wrap_posix_spawnp(pid_t *pid, const char *executable,
                        const posix_spawn_file_actions_t *actions,
                        const posix_spawnattr_t *attributes, char *const arguments[],
                        char *const environment[]) {
    (void)actions;
    (void)attributes;
    (void)environment;
    assert(reaper == 1 && strcmp(executable, "piper") == 0);
    assert(strcmp(arguments[1], "--model") == 0);
    assert(strcmp(arguments[2], "model;not-a-shell-command") == 0);
    assert(strcmp(arguments[3], "--output_file") == 0);
    assert(strcmp(arguments[4], "output.wav") == 0);
    assert(strcmp(arguments[5], "--length_scale") == 0);
    assert(strcmp(arguments[6], expected_scale) == 0 && arguments[7] == NULL);
    *pid = 42;
    return failure == 4 ? ENOENT : 0;
}
/** @brief Simulate child state and interrupted waits.
 * @param pid Owned child.
 * @param status Optional exit-status output.
 * @param options Wait options.
 * @return Scripted wait result.
 */
pid_t __wrap_waitpid(pid_t pid, int *status, int options) {
    (void)options;
    assert(pid == 42 && reaper == 1);
    if (interruptions) {
        --interruptions;
        errno = EINTR;
        return -1;
    }
    errno = ECHILD;
    if (status) {
        *status = exit_status;
    }
    return completion;
}
/** @brief Verify cancellation only signals the owned child.
 * @param pid Owned child PID.
 * @param signal Requested termination signal.
 * @return Zero.
 */
int __wrap_kill(pid_t pid, int signal) {
    assert(pid == 42 && signal == SIGKILL && reaper == 1);
    return 0;
}

/** @brief Start a normal fixture using the engine adapter.
 * @param state Process record.
 * @param speed Requested speed.
 */
static void start(struct ra_speech *state, unsigned int speed) {
    assert(ra_piper_engine.start(state, 3, "model;not-a-shell-command", "output.wav", speed) == 0);
    assert(state->pid == 42 && state->status == RA_SPEECH_RUNNING);
}

/** @brief Exercise every startup, poll, cancellation, and reaper-cleanup outcome.
 * @return Zero after all checks.
 */
int main(void) {
    struct ra_speech state = {0};
    assert(ra_speech_poll(&state) == RA_SPEECH_IDLE);
    ra_speech_cancel(&state);
    assert(ra_piper_engine.start(&state, 0, "", "", 100) == EINVAL);
    assert(ra_piper_engine.start(&state, 3, "", "", 0) == EINVAL);
    assert(ra_piper_engine.start(&state, 3, "", "", 1001) == EINVAL);
    for (failure = 1; failure <= 4; ++failure) {
        assert(ra_piper_engine.start(&state, 3, "model;not-a-shell-command", "output.wav", 100));
        assert(!state.pid && !reaper);
    }
    failure = 0;
    start(&state, 100);
    assert(ra_piper_engine.start(&state, 3, "", "", 100) == EBUSY);
    completion = 0;
    assert(ra_speech_poll(&state) == RA_SPEECH_RUNNING);
    interruptions = 1;
    assert(ra_speech_poll(&state) == RA_SPEECH_RUNNING);
    completion = 42;
    assert(ra_speech_poll(&state) == RA_SPEECH_COMPLETE);
    assert(!state.pid && !reaper);
    start(&state, 100);
    exit_status = 1 << 8;
    assert(ra_speech_poll(&state) == RA_SPEECH_FAILED);
    start(&state, 100);
    exit_status = SIGKILL;
    assert(ra_speech_poll(&state) == RA_SPEECH_FAILED);
    start(&state, 100);
    completion = -1;
    assert(ra_speech_poll(&state) == RA_SPEECH_FAILED);
    expected_scale = "100.000000";
    start(&state, 1);
    completion = 42;
    interruptions = 1;
    ra_speech_cancel(&state);
    expected_scale = "000.100000";
    start(&state, 1000);
    completion = -1;
    ra_speech_cancel(&state);
    assert(!state.pid && !reaper && state.status == RA_SPEECH_FAILED);
    puts("Piper process and Asterisk reaper lifecycle tests passed");
    return 0;
}
