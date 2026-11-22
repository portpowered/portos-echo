/* The capture's audio source on the Android models (audio_android.c), kept in state/capture ("name=value" lines, as
 * state/config; not in it: settings.c writes that file whole from its table, and this is no setting of the page's or
 * of Home Assistant's):
 *
 *   capture_source=voice_communication | voice_recognition      (default voice_communication)
 *   asp_volume_index=1..100                                     (default 80)
 *
 * VOICE_RECOGNITION (6) is the HAL's ASR pipeline, what stock's wake word listens to.  Measured on an Echo Show 8 2nd
 * gen (athena, MT8183, Android 9), 2026-11-19: it takes none of our own playback out, a reply reaches the microphone
 * stream at about -42 dBFS, so a duplex client (-D) hears itself.  VOICE_COMMUNICATION (7) is the HAL's Voice
 * pipeline, the one Amazon's calls use, with libasp's echo canceller, noise suppressor and gain control: the same
 * playback is at -73 to -81 dBFS there (31 to 36 dB less).  It records at 8 or 16 kHz only; CAP_RATE is 16 kHz.
 *
 * What comes with it: while such a capture is open, libasp's VoIP speaker chain multiplies all playback by a volume
 * index that only Amazon's calling app sets.  Nobody sets it here, it stays 0 and the speaker is silent.  So we set
 * it, as the front end is reached everywhere else (afe_parcel.h): command 5, "current volume", a 4-byte index in and
 * 16 bytes back.  The audio log then says "Current volume <index>".  At 80 the amplifier gets what it got with
 * VOICE_RECOGNITION (-27 to -30 dBFS rms against -28 to -30; peaks about 3.5 dB higher).  The index lives in the
 * audio HAL's process: it outlasts the capture being closed and opened, before or after makes no difference, and a
 * restart of the HAL very likely loses it.  Hence when it is sent (audio_android.c): before the first open, after
 * every reopen, and once a minute.
 *
 * Also measured: the first sound after a fresh open starts about 26 dB low and is at full level within about 6 s,
 * then stays there while the capture is open (through pauses of 30 s too); the room's noise floor is about 5.5 dB
 * higher and less steady than with VOICE_RECOGNITION.  The wake word on this source is not measured yet. */
#define _GNU_SOURCE                     /* pipe2 */
#include "capsrc.h"
#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

#define ASP_CMD_CURRENT_VOLUME 5
#define ASP_REPLY_BYTES        16

void capsrc_defaults(struct capsrc_conf *c) { c->source = CAPSRC_DEFAULT; c->asp_index = ASP_INDEX_DEFAULT; }

const char *capsrc_name(enum capsrc s) { return s == CAPSRC_VOICE_RECOGNITION ? "VOICE_RECOGNITION" : "VOICE_COMMUNICATION"; }
const char *capsrc_value(enum capsrc s) { return s == CAPSRC_VOICE_RECOGNITION ? "voice_recognition" : "voice_communication"; }

static void note(char *notes, size_t cap, const char *fmt, const char *a, const char *b)
{
    size_t l;
    if (!notes || !cap || (l = strlen(notes)) + 1 >= cap) return;
    snprintf(notes + l, cap - l, fmt, a, b);
}

static char *trim(char *s)
{
    while (*s == ' ' || *s == '\t') s++;
    size_t l = strlen(s);
    while (l && (s[l - 1] == ' ' || s[l - 1] == '\t' || s[l - 1] == '\r')) s[--l] = 0;
    return s;
}

void capsrc_parse(const char *text, struct capsrc_conf *c, char *notes, size_t cap)
{
    char line[200], num[12];
    if (notes && cap) notes[0] = 0;
    snprintf(num, sizeof num, "%d", ASP_INDEX_DEFAULT);
    for (const char *p = text; p && *p;) {
        size_t n = strcspn(p, "\n");
        int whole = n < sizeof line;                /* a longer line is skipped whole, as settings.c does */
        if (whole) { memcpy(line, p, n); line[n] = 0; }
        p += n; if (*p) p++;
        if (!whole) continue;
        char *k = trim(line), *eq = strchr(k, '=');
        if (!*k || *k == '#') continue;
        if (!eq) { note(notes, cap, "capture: \"%s\" is not name=value, ignored%s\n", k, ""); continue; }
        *eq = 0;
        char *v = trim(eq + 1); k = trim(k);
        if (!strcmp(k, "capture_source")) {
            if (!strcmp(v, capsrc_value(CAPSRC_VOICE_COMMUNICATION))) c->source = CAPSRC_VOICE_COMMUNICATION;
            else if (!strcmp(v, capsrc_value(CAPSRC_VOICE_RECOGNITION))) c->source = CAPSRC_VOICE_RECOGNITION;
            else {
                c->source = CAPSRC_DEFAULT;
                note(notes, cap, "capture: capture_source=%s is neither voice_communication nor voice_recognition, using %s\n",
                     v, capsrc_value(CAPSRC_DEFAULT));
            }
        } else if (!strcmp(k, "asp_volume_index")) {
            char *end; errno = 0;
            long x = strtol(v, &end, 10);
            if (!*v || *end || errno || x < ASP_INDEX_MIN || x > ASP_INDEX_MAX) {
                c->asp_index = ASP_INDEX_DEFAULT;
                note(notes, cap, "capture: asp_volume_index=%s is not a number from 1 to 100, using %s\n", v, num);
            } else c->asp_index = (int)x;
        } else note(notes, cap, "capture: no setting \"%s\", ignored%s\n", k, "");
    }
}

void capsrc_load(struct capsrc_conf *c)
{
    char p[300], text[2048], notes[1024]; const char *d = getenv("HASSMIC_STATE");
    capsrc_defaults(c);
    snprintf(p, sizeof p, "%s/capture", d ? d : "/data/local/hassmic/state");
    FILE *f = fopen(p, "r");
    if (!f) return;                                 /* no file: the defaults, nothing to say */
    size_t n = fread(text, 1, sizeof text - 1, f);
    fclose(f);
    text[n] = 0;
    capsrc_parse(text, c, notes, sizeof notes);
    if (notes[0]) fputs(notes, stderr);
}

int capsrc_asp_argv(struct afe_call *call, int index)
{
    unsigned char in[4] = { (unsigned char)index, (unsigned char)(index >> 8), (unsigned char)(index >> 16), (unsigned char)(index >> 24) };
    return afe_call_build(call, ASP_CMD_CURRENT_VOLUME, in, sizeof in, ASP_REPLY_BYTES);
}

static int ms_since(const struct timespec *t0)
{
    struct timespec t; clock_gettime(CLOCK_MONOTONIC, &t);
    return (int)((t.tv_sec - t0->tv_sec) * 1000 + (t.tv_nsec - t0->tv_nsec) / 1000000);
}

/* hassmic ignores SIGCHLD, so a child of its own is reaped unseen and has no exit status to ask for.  As artifacts.c
 * does: a child in between, which takes SIGCHLD back, waits for the tool and writes its status into a pipe.  That pipe
 * is close-on-exec, so the tool never holds it: the end of the pipe without a byte is the child in between gone.
 * Both are one process group, for the kill.  After fork() only async-signal-safe calls: other threads hold locks. */
int capsrc_run(char *const argv[], int timeout_ms)
{
    int p[2];
    if (pipe2(p, O_CLOEXEC)) return CAPSRC_RUN_FAILED;
    pid_t pid = fork();
    if (pid == 0) {
        setpgid(0, 0);
        signal(SIGCHLD, SIG_DFL);
        int nul = open("/dev/null", O_RDWR | O_CLOEXEC);
        if (nul >= 0) { dup2(nul, 1); dup2(nul, 2); }
        unsigned char code = 126; int st = 0;
        pid_t tool = fork();
        if (tool == 0) { execv(argv[0], argv); _exit(127); }
        if (tool > 0) {
            pid_t w;
            while ((w = waitpid(tool, &st, 0)) < 0 && errno == EINTR) {}
            if (w == tool) code = (unsigned char)(WIFEXITED(st) ? WEXITSTATUS(st) : 128 + WTERMSIG(st));
        }
        ssize_t k = write(p[1], &code, 1); (void)k;
        _exit(0);
    }
    close(p[1]);
    if (pid < 0) { close(p[0]); return CAPSRC_RUN_FAILED; }
    struct timespec t0; clock_gettime(CLOCK_MONOTONIC, &t0);
    int status = CAPSRC_RUN_FAILED, gone = 1;
    for (;;) {
        int left = timeout_ms - ms_since(&t0);
        struct pollfd pf = { p[0], POLLIN, 0 };
        int k = left > 0 ? poll(&pf, 1, left) : 0;
        if (k < 0 && errno == EINTR) continue;
        if (k < 0) break;
        if (k == 0) {
            /* by group, as main.c's run_output: while it has a member its number is nobody else's.  Where setpgid
             * was refused, the child alone, and only while waitpid says it is ours and running */
            int st;
            gone = kill(-pid, SIGKILL) == 0;
            if (!gone && waitpid(pid, &st, WNOHANG) == 0) gone = kill(pid, SIGKILL) == 0;
            status = CAPSRC_RUN_TIMEOUT;
            break;
        }
        unsigned char code;
        ssize_t r = read(p[0], &code, 1);
        if (r < 0 && errno == EINTR) continue;
        if (r == 1) status = code;
        break;
    }
    close(p[0]);
    /* no zombie where SIGCHLD is not ignored (the unit test).  Where it is (hassmic), the child is reaped unseen and
     * its pid free for anyone, a later child of ours too: nothing to wait for, and nothing safe to wait on */
    struct sigaction sa;
    if (gone && !sigaction(SIGCHLD, NULL, &sa) && sa.sa_handler != SIG_IGN) { int st; while (waitpid(pid, &st, 0) < 0 && errno == EINTR) {} }
    return status;
}

int capsrc_asp_say(int *last, int status)
{
    int say = *last == CAPSRC_RUN_NEVER || status != 0 || status != *last;
    *last = status;
    return say;
}

void capsrc_asp_line(char *out, size_t cap, int index, int status)
{
    if (status == CAPSRC_RUN_TIMEOUT)
        snprintf(out, cap, "asp: volume index %d sent (exit none: killed after %d ms)", index, ASP_SEND_TIMEOUT_MS);
    else if (status < 0) snprintf(out, cap, "asp: volume index %d sent (exit none: the service tool could not be started)", index);
    else snprintf(out, cap, "asp: volume index %d sent (exit %d)", index, status);
}
