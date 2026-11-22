/* Which of Android's audio sources the OpenSL ES capture records (audio_android.c), and the speaker volume index Amazon's
 * front end wants with one of them.  See capsrc.c.  Pure C but for capsrc_load and capsrc_run: the PC's unit test is
 * tests/unit/capsrc_test.c. */
#ifndef CAPSRC_H
#define CAPSRC_H
#include "afe_parcel.h"
#include <stddef.h>

enum capsrc { CAPSRC_VOICE_COMMUNICATION, CAPSRC_VOICE_RECOGNITION };

#define CAPSRC_DEFAULT        CAPSRC_VOICE_COMMUNICATION
#define ASP_INDEX_DEFAULT     80        /* playback as loud as with VOICE_RECOGNITION (capsrc.c) */
#define ASP_INDEX_MIN         1
#define ASP_INDEX_MAX         100
#define ASP_SEND_TIMEOUT_MS   2000      /* the service tool takes ~0.1 s; one that hangs is killed */
#define ASP_SEND_EVERY_S      60        /* again, should the audio HAL have restarted and lost it */

struct capsrc_conf { enum capsrc source; int asp_index; };

void capsrc_defaults(struct capsrc_conf *c);
/* "name=value" lines (capture_source, asp_volume_index; # comments and blank lines skipped) over what c holds.  A value
 * it cannot take leaves that setting at its default; each such line, and each name it does not know, gets a line in
 * notes (cut at cap - 1), for the log. */
void capsrc_parse(const char *text, struct capsrc_conf *c, char *notes, size_t cap);
/* The defaults, then state/capture over them where there is one; what it refused goes to the log. */
void capsrc_load(struct capsrc_conf *c);
const char *capsrc_name(enum capsrc s);         /* "VOICE_COMMUNICATION": Android's name, for the log */
const char *capsrc_value(enum capsrc s);        /* "voice_communication": state/capture's */

/* argv of "/system/bin/service call audiosignalprocessor 3 i32 5 i32 4 i32 <index> i32 16": 0, or -1 */
int capsrc_asp_argv(struct afe_call *call, int index);

/* Runs argv[0] with argv, no shell, its stdout and stderr to /dev/null, for timeout_ms at most: then it is killed.
 * Its exit status (128 + the signal that ended it), or one of these.  Works with SIGCHLD ignored (hassmic) or not. */
#define CAPSRC_RUN_FAILED  (-1)         /* no pipe, no fork */
#define CAPSRC_RUN_TIMEOUT (-2)
int capsrc_run(char *const argv[], int timeout_ms);

/* Whether a send that ended with status gets its log line: the first one, every failure, and a status other than
 * the last one's.  *last: CAPSRC_RUN_NEVER before the first; set to status. */
#define CAPSRC_RUN_NEVER   (-1000)
int capsrc_asp_say(int *last, int status);
/* "asp: volume index 80 sent (exit 0)": the line, without a newline */
void capsrc_asp_line(char *out, size_t cap, int index, int status);
#endif
