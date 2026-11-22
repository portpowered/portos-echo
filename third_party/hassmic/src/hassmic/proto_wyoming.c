/* Wyoming satellite protocol: Home Assistant connects to us (wyoming integration). */
#include <stdio.h>
#include <string.h>
#include <sys/socket.h>
#include <time.h>
#include "board.h"
#include "core.h"
#include "screen.h"
#include "wyoming.h"

#define TTS_RATE 22050      /* assumed when audio-start carries no rate */

static int client = -1;

static void send_event(const char *type, const char *data, const void *payload, size_t len)
{
    if (client >= 0 && wy_write(client, type, data, payload, len) < 0)
        shutdown(client, SHUT_RDWR);        /* reader thread notices and cleans up */
}

/* Lock held.  The active wake word's id ("computer-en-US", the firmware's "alexa"), as a JSON string body: ids are
 * model directory names, anything outside [A-Za-z0-9._-] becomes '_' */
static const char *active_word(char *out, size_t cap)
{
    const struct core_wake_word *list;
    int n = core_wake_words(&list), i = core_wake_word(-1);
    const char *id = i >= 0 && i < n ? list[i].id : "alexa";
    size_t k = 0;
    for (; id[k] && k + 1 < cap; k++) {
        char c = id[k];
        out[k] = (c >= 'a' && c <= 'z') || (c >= 'A' && c <= 'Z') || (c >= '0' && c <= '9') || c == '.' || c == '_' ||
                 c == '-' ? c : '_';
    }
    out[k] = 0;
    return out;
}

static void start(void)
{
    char data[256], word[64];
    snprintf(data, sizeof data,
             "{\"start_stage\":\"%s\",\"end_stage\":\"tts\",\"restart_on_end\":false%s%s%s}",
             core_local_wake ? "asr" : "wake", core_local_wake ? ",\"wake_word_name\":\"" : "",
             core_local_wake ? active_word(word, sizeof word) : "", core_local_wake ? "\"" : "");
    send_event("run-pipeline", data, NULL, 0);
}

static void audio(const void *pcm, size_t len)
{
    char data[96]; struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    snprintf(data, sizeof data, "{\"rate\":16000,\"width\":2,\"channels\":1,\"timestamp\":%lld}",
             (long long)ts.tv_sec * 1000 + ts.tv_nsec / 1000000);
    send_event("audio-chunk", data, pcm, len);
}

static void played(void) { send_event("played", NULL, NULL, 0); }

/* Lock held.  The effective mute (privacy latch or soft mute, core_muted()), which Wyoming has no event for: our own
 * "hassmic-mute" event on every change.  A connection starts unmuted as far as the client knows, so one made while
 * muted is told so first; an unmuted one hears nothing new, and clients that read events in a fixed order (Home
 * Assistant ignores types it does not know anyway) see the stream as before */
static void mute_changed(int muted) { send_event("hassmic-mute", muted ? "{\"muted\":true}" : "{\"muted\":false}", NULL, 0); }

/* Lock held.  The speaker, which Wyoming has no event for either: our own "hassmic-speaker" {"volume": 0..100,
 * "muted": bool} on connecting and on every change of either, whatever moved the volume (the keys, the settings page,
 * Sendspin, a Bluetooth speaker's own buttons, the client).  muted is the output mute (core_output_mute), never the
 * microphones' (hassmic-mute).  A client that sets either hears this as its answer, so one comes for a set that
 * changed nothing too */
static void volume_changed(int percent)
{
    char data[48];
    snprintf(data, sizeof data, "{\"volume\":%d,\"muted\":%s}", percent, core_output_mute(-1) ? "true" : "false");
    send_event("hassmic-speaker", data, NULL, 0);
}

static void send_info(void)
{
    char data[1024], word[64], active[80] = "";
    if (core_local_wake) snprintf(active, sizeof active, "\"%s\"", active_word(word, sizeof word));
    snprintf(data, sizeof data,
             "{\"asr\":[],\"tts\":[],\"handle\":[],\"intent\":[],\"wake\":[],\"mic\":[],\"snd\":[],"
             "\"satellite\":{\"name\":\"%s\",\"attribution\":{\"name\":\"hassmic\",\"url\":\"\"},"
             "\"installed\":true,\"description\":\"%s with Amazon audio front end\",\"version\":\"" VERSION "\","
             "\"area\":null,\"has_vad\":false,\"active_wake_words\":[%s],\"max_active_wake_words\":1,"
             "\"supports_trigger\":true}}",
             core_name, board.model, active);
    send_event("info", data, NULL, 0);
}

static void handle(const struct wy_event *ev)
{
    const char *t = ev->type;

    pthread_mutex_lock(&core_lock);
    if (!strcmp(t, "audio-chunk")) { core_duplex_progress(); core_tts_data(ev->payload, ev->payload_len); }
    else if (!strcmp(t, "ping")) send_event("pong", NULL, NULL, 0);
    else if (!strcmp(t, "describe")) send_info();
    else if (!strcmp(t, "run-satellite")) {
        if (core_duplex) {
            int seconds = wy_failsafe_seconds(ev->json);
            if (seconds > 0) core_duplex_failsafe(seconds);
            else fprintf(stderr, "invalid hassmic_failsafe_seconds, ignored\n");
        }
        fprintf(stderr, "satellite: running\n"); core_link(1, 1);
    }
    else if (!strcmp(t, "pause-satellite")) core_link_pause();
    else if (!strcmp(t, "detection")) core_set_state(LISTENING);
    else if (!strcmp(t, "voice-stopped") || !strcmp(t, "transcript")) {
        core_duplex_progress();
        if (!strcmp(t, "transcript")) {
            char text[256] = "";
            wy_json_str(strchr(ev->json, '\n') ? strchr(ev->json, '\n') : ev->json, "text", text, sizeof text);
            fprintf(stderr, "transcript: %s\n", text);
            screen_words(SCREEN_WORDS_HEARD, text);              /* Wyoming tells no reply text, only the words */
            if (!core_duplex) core_mic_off();
        }
        if (core_state() == LISTENING) core_set_state(THINKING);
    } else if (!strcmp(t, "audio-start")) {
        long rate = TTS_RATE, ch = 1, width = 2;
        wy_json_int(ev->json, "rate", &rate); wy_json_int(ev->json, "channels", &ch); wy_json_int(ev->json, "width", &width);
        if (width != 2) fprintf(stderr, "play: unsupported sample width %ld, expect noise\n", width);
        core_tts_begin(rate, ch, 0);
    } else if (!strcmp(t, "hassmic-continue")) {
        /* Compatibility hint for older duplex clients. Natural reply completion now always keeps capture;
         * hassmic-flush discards output and hassmic-end explicitly ends the run. Ignored without -D. */
        if (!core_continue()) fprintf(stderr, "hassmic-continue: no reply of a -D run to go on from, ignored\n");
    } else if (!strcmp(t, "hassmic-set-volume")) {
        /* a client's volume control, as Home Assistant's slider over ESPHome: core_set_volume answers (volume_changed) */
        int v = wy_speaker_volume(ev->json);
        if (v >= 0) core_set_volume(v); else fprintf(stderr, "hassmic-set-volume: no volume 0..100, ignored\n");
    } else if (!strcmp(t, "hassmic-set-output-mute")) {
        int m = wy_output_mute(ev->json);
        if (m >= 0) { core_output_mute(m); volume_changed(core_volume()); }
        else fprintf(stderr, "hassmic-set-output-mute: no mute true or false, ignored\n");
    } else if (!strcmp(t, "hassmic-flush")) core_duplex_flush();
    else if (!strcmp(t, "hassmic-end")) core_duplex_end();
    else if (!strcmp(t, "audio-stop")) { core_duplex_progress(); core_tts_end(); }
    else if (!strcmp(t, "error")) {
        char code[64] = "";
        fprintf(stderr, "server error: %s\n", ev->json);
        wy_json_str(ev->json, "code", code, sizeof code);
        if (!strcmp(code, "stt-no-text-recognized")) core_no_speech(); else core_error();     /* nothing said: quietly */
    }
    pthread_mutex_unlock(&core_lock);
}

static void serve(int fd)
{
    struct wy_reader rd; static struct wy_event ev;
    if (wy_reader_init(&rd, fd) < 0) return;
    pthread_mutex_lock(&core_lock);
    client = fd; core_duplex_connected(); core_duplex_failsafe(30); core_link(1, 0); if (core_muted()) mute_changed(1);
    /* the speaker as it is, on every connection: Home Assistant reads past event types it does not know (its describe
     * waits for info, its satellite loop drops the rest) */
    volume_changed(core_volume());
    pthread_mutex_unlock(&core_lock);
    fprintf(stderr, "client connected\n");

    while (wy_read(&rd, &ev) > 0) handle(&ev);

    pthread_mutex_lock(&core_lock);
    client = -1; core_link(0, 0);
    pthread_mutex_unlock(&core_lock);
    wy_reader_free(&rd);
    fprintf(stderr, "client disconnected\n");
}

static void print_mdns(void)
{
    printf("<?xml version=\"1.0\" standalone='no'?>\n<!DOCTYPE service-group SYSTEM \"avahi-service.dtd\">\n"
           "<service-group>\n  <name replace-wildcards=\"yes\">%s %%h</name>\n"
           "  <service><type>_wyoming._tcp</type><port>%d</port></service>\n</service-group>\n", core_name, core_port);
}

const struct proto proto_wyoming = { "wyoming", 16700, 0, serve, start, audio, NULL, NULL, played, volume_changed, mute_changed, print_mdns, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL };
