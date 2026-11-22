use std::{
    error::Error,
    fs,
    path::{Path, PathBuf},
};

// Keep the pinned source available for comparisons. These build-only changes
// stop the shared dispatcher from selecting excluded optional services.
pub fn source(vendor: &Path, out: &Path) -> Result<PathBuf, Box<dyn Error>> {
    let mut source = fs::read_to_string(vendor.join("src/hassmic/main.c"))?;
    for (upstream, satellite) in [
        ("static const struct proto *proto = &proto_esphome;", REGISTRATION),
        (
            "core_duplex && proto != &proto_wyoming",
            "core_duplex && proto != &echo_hassmic_wyoming",
        ),
        (
            "case 'P': proto = !strcmp(optarg, \"wyoming\") ? &proto_wyoming : &proto_esphome; break;",
            "case 'P': if (strcmp(optarg, \"wyoming\")) { fputs(\"hassmic: only Wyoming is available\\n\", stderr); return 2; } break;",
        ),
        (
            "case 'T': { char tok[160]; sendspin_init(); sendspin_pairing_token(tok, sizeof tok); puts(tok); return 0; }",
            "case 'T': fputs(\"hassmic: Sendspin is unavailable\\n\", stderr); return 2;",
        ),
        ("static int ota_port = 28929;", "static int ota_port = 0;"),
        ("int core_web_port = 28931;", "int core_web_port = 0;"),
        ("core_sendspin_port = 28928;", "core_sendspin_port = 0;"),
        (
            "    signal(SIGPIPE, SIG_IGN);",
            "    int unix_listener = echo_hassmic_prepare_listener();\n    if (unix_listener == -1) return 1;\n    signal(SIGPIPE, SIG_IGN);",
        ),
        (
            "    int ls = net_listen(core_port);",
            "#ifdef __ANDROID__\n    int ls = unix_listener;\n#else\n    int ls = unix_listener >= 0 ? unix_listener : net_listen(core_port);\n#endif",
        ),
        (
            "    fprintf(stderr, \"hassmic \" VERSION \" (\" BUILD \") %s on %d, wake=%s\\n\", proto->id, core_port, core_local_wake ? \"local\" : \"remote\");",
            "    if (unix_listener >= 0) echo_hassmic_unix_ready();\n    else fprintf(stderr, \"hassmic \" VERSION \" (\" BUILD \") %s on %d, wake=%s\\n\", proto->id, core_port, core_local_wake ? \"local\" : \"remote\");",
        ),
        (
            "        int c = net_accept(ls);",
            "        int c = echo_hassmic_accept(ls, unix_listener >= 0);",
        ),
        (
            "          setsockopt(c, SOL_SOCKET, SO_SNDTIMEO, &tv, sizeof tv); setsockopt(c, SOL_SOCKET, SO_KEEPALIVE, &on, sizeof on);",
            "          setsockopt(c, SOL_SOCKET, SO_SNDTIMEO, &tv, sizeof tv);\n          if (unix_listener < 0) { setsockopt(c, SOL_SOCKET, SO_KEEPALIVE, &on, sizeof on);",
        ),
        (
            "setsockopt(c, IPPROTO_TCP, TCP_USER_TIMEOUT, &user_ms, sizeof user_ms); }",
            "setsockopt(c, IPPROTO_TCP, TCP_USER_TIMEOUT, &user_ms, sizeof user_ms); } }",
        ),
    ] {
        if source.matches(upstream).count() != 1 {
            return Err(format!("hassmic main no longer contains exactly one `{upstream}`").into());
        }
        source = source.replacen(upstream, satellite, 1);
    }
    source.push_str(CORE_ACCESS);
    let main = out.join("hassmic_main.c");
    fs::write(&main, source)?;
    Ok(main)
}

const REGISTRATION: &str = r#"
#include <sys/un.h>
extern int echo_hassmic_prepare_listener(void);
extern void echo_hassmic_unix_ready(void);

static int echo_hassmic_accept(int fd, int local)
{
#ifndef __ANDROID__
    if (!local) return net_accept(fd);
#else
    (void)local;
#endif
    int client;
    do client = accept4(fd, NULL, NULL, SOCK_CLOEXEC); while (client < 0 && errno == EINTR);
    return client;
}

int echo_hassmic_probe_unix(const char *path)
{
    struct sockaddr_un addr = { .sun_family = AF_UNIX };
    size_t len = strlen(path);
    if (len >= sizeof addr.sun_path) return ENAMETOOLONG;
    memcpy(addr.sun_path, path, len + 1);
    int fd = socket(AF_UNIX, SOCK_STREAM | SOCK_NONBLOCK | SOCK_CLOEXEC, 0);
    if (fd < 0) return errno;
    int status = connect(fd, (struct sockaddr *)&addr, sizeof addr) < 0 ? errno : 0;
    close(fd);
    return status;
}

int echo_hassmic_open_lease(const char *path)
{
    return open(path, O_RDWR | O_CREAT | O_CLOEXEC | O_NOFOLLOW | O_NONBLOCK, 0600);
}

int echo_hassmic_on_stop(void (*notify)(int))
{
    return signal(SIGTERM, notify) == SIG_ERR || signal(SIGINT, notify) == SIG_ERR ? -1 : 0;
}

static struct proto echo_hassmic_wyoming = {
    .id = "wyoming", .port = 16700, .threaded = 0,
};
static const struct proto *proto = &echo_hassmic_wyoming;

void echo_hassmic_register(
    void (*serve)(int), void (*start)(void), void (*audio)(const void *, size_t),
    void (*played)(void), void (*volume_changed)(int), void (*mute_changed)(int),
    void (*print_mdns)(void))
{
    echo_hassmic_wyoming.serve = serve;
    echo_hassmic_wyoming.start = start;
    echo_hassmic_wyoming.audio = audio;
    echo_hassmic_wyoming.played = played;
    echo_hassmic_wyoming.volume_changed = volume_changed;
    echo_hassmic_wyoming.mute_changed = mute_changed;
    echo_hassmic_wyoming.print_mdns = print_mdns;
}
"#;

// Expose platform types and constants through the native headers.
const CORE_ACCESS: &str = r#"
void echo_hassmic_lock(void) { pthread_mutex_lock(&core_lock); }
void echo_hassmic_unlock(void) { pthread_mutex_unlock(&core_lock); }
int echo_hassmic_listening(void) { return core_state() == LISTENING; }
void echo_hassmic_detection(void) { core_set_state(LISTENING); }
void echo_hassmic_thinking(void) { core_set_state(THINKING); }
void echo_hassmic_heard(const char *text) { screen_words(SCREEN_WORDS_HEARD, text); }
const char *echo_hassmic_model(void) { return board.model; }
long long echo_hassmic_millis(void) { return mono_ms(); }
const char *echo_hassmic_wake_word(void)
{
    const struct core_wake_word *list;
    int count = core_wake_words(&list), index = core_wake_word(-1);
    return index >= 0 && index < count ? list[index].id : "alexa";
}
void echo_hassmic_shutdown(int fd) { shutdown(fd, SHUT_RDWR); }
"#;
