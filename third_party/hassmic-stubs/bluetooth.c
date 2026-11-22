/* SPDX-License-Identifier: MIT */
/* The device role has no Bluetooth controller or Bluetooth audio route. */
#include "a2dp.h"
#include "ble.h"
#include "btout.h"
#include "improv.h"

void a2dp_start(void (*changed)(void)) { (void)changed; }
void a2dp_pair(int on) { (void)on; }
void a2dp_volume_changed(int percent) { (void)percent; }
void a2dp_speaker_volume(int percent) { (void)percent; }
int a2dp_button(int resume) { (void)resume; return 0; }
int a2dp_skip(int dir) { (void)dir; return 0; }
void a2dp_pause(void) {}
int a2dp_stop(void) { return 0; }
void a2dp_unyield(void) {}
void a2dp_out_enable(int on) { (void)on; }
int a2dp_out_enabled(void) { return 0; }
int a2dp_out_stay(int set) { (void)set; return 0; }
int a2dp_out_delay(int set) { (void)set; return 0; }
int btout_moving(void) { return 0; }
int btout_keep_awake(int set) { (void)set; return 0; }

int ble_present(void) { return 0; }
void ble_quiet(int ms) { (void)ms; }
void improv_start(const struct improv_hooks *hooks) { (void)hooks; }
int improv_authorize(void) { return 0; }
