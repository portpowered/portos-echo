/* SPDX-License-Identifier: MIT */
/* Satisfy the satellite's optional-service callbacks without starting those services. */
#include "davs.h"
#include "dropin.h"
#include "https.h"
#include "mass.h"
#include "netaddr.h"
#include "ota.h"
#include "sendspin.h"
#include "update.h"
#include "web.h"
#include "wifimotion.h"

int sendspin_start(int port) { (void)port; return -1; }
int sendspin_button(void) { return 0; }
int sendspin_skip(int dir) { (void)dir; return 0; }
int sendspin_stop(void) { return 0; }
void sendspin_pause(void) {}
void sendspin_volume_changed(int percent) { (void)percent; }
int sendspin_unpaired(int set) { (void)set; return 0; }
int sendspin_linked(void) { return 0; }
int sendspin_expected(void) { return 0; }
int sendspin_control(const char *cmd, long long arg) { (void)cmd; (void)arg; return -1; }
void mass_start(void) {}
void mass_ui(const char *line) { (void)line; }
void mass_replay(void) {}
void mass_stop(void) {}

const char *const dropin_states[4] = { "idle", "unavailable", "unavailable", "unavailable" };
const char *const dropin_results[DROPIN_NRESULTS] = { [DROPIN_R_FAILED] = "unavailable" };
int dropin_start(int port) { (void)port; return -1; }
int dropin_running(void) { return 0; }
int dropin_enable(int set) { (void)set; return 0; }
int dropin_ask(int set) { (void)set; return 0; }
void dropin_hangup(const char *why) { (void)why; }
int dropin_button(void) { return 0; }
int dropin_answer(void) { return 0; }
int dropin_status(char *peer, size_t cap)
{
    if (cap) peer[0] = '\0';
    return DROPIN_IDLE;
}
void dropin_mic(const int16_t *pcm, size_t n, int own) { (void)pcm; (void)n; (void)own; }
float dropin_far_level(void) { return -100; }
int dropin_far_talking(void) { return 0; }
void dropin_message(const unsigned char from[8], const char *node, unsigned ip, const unsigned char *p, size_t n)
{
    (void)from; (void)node; (void)ip; (void)p; (void)n;
}

int web_start(int port, const struct web_hooks *hooks) { (void)port; (void)hooks; return -1; }
int web_approve(void) { return 0; }
void netaddr_start(const struct netaddr_hooks *hooks) { (void)hooks; }
void netaddr_link(int fd, int on) { (void)fd; (void)on; }
int wifimotion_present(void) { return 0; }
int wifimotion_enable(int set) { (void)set; return 0; }
int wifimotion_sensitivity(int set) { (void)set; return WIFIMOTION_SENS_DEFAULT; }
void davs_start(void) {}
int ota_start(int port) { (void)port; return -1; }
void ota_healthy(void) {}
int https_verify(int set) { (void)set; return 1; }
const char *const update_channels[3] = { "off", "beta", "release" };
int update_channel(int ch) { (void)ch; return UPDATE_OFF; }
