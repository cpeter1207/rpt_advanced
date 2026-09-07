/* SPDX-License-Identifier: GPL-2.0-only */
/** @file
 * @brief Offline speech-engine process ownership outside the audio loop.
 */
#ifndef RPT_ADVANCED_SPEECH_H
#define RPT_ADVANCED_SPEECH_H
#include <sys/types.h>

/** @brief Synthesis lifecycle result. */
enum ra_speech_status {
    RA_SPEECH_FAILED = -1, /**< Start, synthesis, or cancellation failed to produce speech. */
    RA_SPEECH_IDLE = 0,    /**< No synthesis has been started. */
    RA_SPEECH_RUNNING = 1, /**< Child process is still producing the WAV file. */
    RA_SPEECH_COMPLETE = 2 /**< Child exited successfully; caller must validate its output. */
};

/** @brief Child process owned exclusively by one identifier worker. */
struct ra_speech {
    pid_t pid;                    /**< Positive child PID, or zero when no child is owned. */
    enum ra_speech_status status; /**< Most recent lifecycle status. */
};

/** @brief Replaceable offline engine; Piper is the only implementation. */
struct ra_speech_engine {
    /** @brief Start synthesis without blocking for completion.
     * @param state Zero-initialized or completed process state.
     * @param input Readable, rewound text-file descriptor greater than STDERR_FILENO.
     * @param model Local voice model path.
     * @param output Caller-owned temporary WAV path.
     * @param speed Speaking-rate percent, from 1 through 1000.
     * @return Zero on start, or a POSIX error number; an active child is never replaced.
     */
    int (*start)(struct ra_speech *state, int input, const char *model, const char *output,
                 unsigned int speed);
};

/** @brief Default Piper command-line engine, executed directly without a shell. */
extern const struct ra_speech_engine ra_piper_engine;

/** @brief Prepare an opened audio file as mono signed 16-bit little-endian PCM.
 * @param state Child ownership record shared with speech synthesis.
 * @param input Rewound audio-file descriptor greater than STDERR_FILENO.
 * @param output Caller-owned temporary output path; existing content is replaced.
 * @param rate Positive target samples per second selected for playback.
 * @return Zero on start or a POSIX error number. Poll/cancel with the same lifecycle API.
 * FFmpeg reads the supplied descriptor without reopening the configured input path.
 */
int ra_audio_prepare(struct ra_speech *state, int input, const char *output, unsigned int rate);

/** @brief Poll and reap completed synthesis without waiting for a running child.
 * @param state Owned synthesis process.
 * @return Current status; failed synthesis selects the caller's Morse fallback.
 */
enum ra_speech_status ra_speech_poll(struct ra_speech *state);

/** @brief Kill and reap owned synthesis during interruption or module cleanup.
 * @param state Owned synthesis process; idle/completed states are unchanged.
 * Call from the identifier worker, not the hardware audio callback.
 */
void ra_speech_cancel(struct ra_speech *state);
#endif
