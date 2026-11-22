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
