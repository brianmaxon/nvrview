/*
 * A C client of the nvrview core: opens one tile on a (mock) device, plays
 * it with a fake decoder that answers every Decode from inside the
 * callback, switches it to the main stream (a second generation), renders
 * its audio from a separate (audio-device-like) thread with a tap attached
 * and reads its level, stores a layout, goes through the recordings
 * functions (index, incidents, a thumbnail, a download, a lock, the Trash),
 * plays two cameras' recordings back beside the live tile (open, play,
 * seek, 4x, close), records the live tile and a call with a fake AAC
 * encoder to finalized MP4 files, checks a smart-alert verification round
 * trip (a trigger, its pictures, boxes, the alert, the log), and checks the
 * shutdown guarantees. Built and run by scripts/test-c-ffi.sh
 * against a spawned mock-nvr with recordings.
 *
 * usage: smoke HOST PORT WORKDIR   (password from NVRVIEW_TEST_PASSWORD;
 * WORKDIR: an empty directory for the core's data and the downloads)
 */
/* nanosleep is POSIX; strict C11 on glibc hides it without this. */
#define _POSIX_C_SOURCE 200809L

#include "nvrview.h"

#include <math.h>
#include <pthread.h>
#include <stdatomic.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

static NvCore *_Atomic g_core;
static atomic_ulong g_configures, g_decodes, g_answered, g_events, g_callbacks;
static atomic_ulong g_problems, g_tile_closed;
static atomic_ulong g_taps, g_renders, g_loud;
/* Answered decodes and resets of generations 0 and 1. */
static atomic_ulong g_gen_answered[2], g_gen_resets[2];
static atomic_bool g_stop_render;
/* Recordings events seen. */
static atomic_ulong g_rec_changed, g_thumb_ready, g_download_done, g_download_trashed;
static atomic_ulong g_device_info_rec;
static atomic_ulong g_ptz_move, g_ptz_stop, g_ptz_preset, g_ptz_zoom_clamped;
/* nv_tile_open writes the ID here, and only then lets actions run, so the
 * callback may read it without further synchronization. */
static uint64_t g_tile;
/* Set while playback tiles (minor 6) may exist; set before
 * nv_playback_open, so their actions are never taken for unknown tiles. */
static atomic_bool g_pb_phase;
static atomic_ulong g_pb_decodes, g_pb_seeks_active, g_pb_rate4, g_pb_closed, g_pb_tile_closed;

static void sleep_ms(long ms) {
    struct timespec ts = {ms / 1000, (ms % 1000) * 1000000L};
    nanosleep(&ts, NULL);
}

/* NVRVIEW_SMOKE_TIMING=1 prints the time each part took (stderr). */
static void phase(const char *name) {
    static struct timespec last;
    struct timespec now;
    clock_gettime(CLOCK_MONOTONIC, &now);
    if (getenv("NVRVIEW_SMOKE_TIMING") != NULL && last.tv_sec != 0) {
        fprintf(stderr, "smoke-timing: %-12s %6ld ms\n", name,
                (long)((now.tv_sec - last.tv_sec) * 1000 + (now.tv_nsec - last.tv_nsec) / 1000000));
    }
    last = now;
}

static void problem(const char *what) {
    fprintf(stderr, "smoke: %s\n", what);
    atomic_fetch_add(&g_problems, 1);
}

static bool contains(const char *text, size_t len, const char *needle) {
    size_t n = strlen(needle);
    for (size_t i = 0; i + n <= len; i++) {
        if (memcmp(text + i, needle, n) == 0) {
            return true;
        }
    }
    return false;
}

static void on_action(void *context, uint64_t tile, const NvAction *action) {
    (void)context;
    atomic_fetch_add(&g_callbacks, 1);
    if (atomic_load(&g_pb_phase) && tile != g_tile) {
        /* A playback tile (the only other tiles): recorded video arrives
         * like live video. */
        if (action->kind == NV_ACTION_DECODE &&
            nv_tile_decoded(atomic_load(&g_core), tile, action->generation,
                            action->decode.pts_us) == NV_OK) {
            atomic_fetch_add(&g_pb_decodes, 1);
        }
        return;
    }
    if (tile != g_tile) {
        problem("action for a tile whose ID nv_tile_open had not returned");
    }
    if (action->generation > 1) {
        problem("unexpected generation");
    }
    unsigned gen = action->generation > 1 ? 1 : action->generation;
    if (action->kind == NV_ACTION_RESET) {
        atomic_fetch_add(&g_gen_resets[gen], 1);
    }
    switch (action->kind) {
    case NV_ACTION_CONFIGURE: {
        const NvConfigure *c = &action->configure;
        atomic_fetch_add(&g_configures, 1);
        if (c->parameter_set_count < 2 || c->parameter_sets == NULL ||
            c->coded_size.width == 0 || c->visible_rect.height == 0) {
            problem("incomplete Configure");
        }
        for (size_t i = 0; i < c->parameter_set_count; i++) {
            if (c->parameter_sets[i].data == NULL || c->parameter_sets[i].len == 0) {
                problem("empty parameter set");
            }
        }
        break;
    }
    case NV_ACTION_DECODE: {
        const NvDecode *d = &action->decode;
        atomic_fetch_add(&g_decodes, 1);
        /* A platform decoder copies the access unit; so does this one. */
        uint8_t *copy = malloc(d->access_unit.len);
        if (copy == NULL || d->access_unit.len < 4) {
            problem("bad access unit");
        } else {
            memcpy(copy, d->access_unit.data, d->access_unit.len);
            if (copy[0] != 0 || copy[1] != 0) {
                problem("access unit is not Annex-B");
            }
        }
        free(copy);
        if (nv_tile_decoded(atomic_load(&g_core), tile, action->generation, d->pts_us) == NV_OK) {
            atomic_fetch_add(&g_answered, 1);
            atomic_fetch_add(&g_gen_answered[gen], 1);
        }
        break;
    }
    case NV_ACTION_FLUSH:
    case NV_ACTION_RESET:
    case NV_ACTION_CLEAR:
    case NV_ACTION_STATUS:
        break;
    default:
        /* Unknown kinds need no answer. */
        break;
    }
}

static void note_alert_event(const char *json, size_t len);

static void on_event(void *context, const char *json, size_t len) {
    (void)context;
    atomic_fetch_add(&g_callbacks, 1);
    atomic_fetch_add(&g_events, 1);
    if (len < 10 || json[0] != '{' || json[len - 1] != '}') {
        problem("event is not a JSON object");
    }
    if (contains(json, len, "\"type\":\"tile_closed\"")) {
        atomic_fetch_add(&g_tile_closed, 1);
    }
    note_alert_event(json, len);
    if (contains(json, len, "\"type\":\"playback_seek\"") && contains(json, len, "\"state\":\"active\"")) {
        atomic_fetch_add(&g_pb_seeks_active, 1);
    }
    if (contains(json, len, "\"type\":\"playback_state\"") && contains(json, len, "\"rate\":4.0") &&
        contains(json, len, "\"state\":\"playing\"")) {
        atomic_fetch_add(&g_pb_rate4, 1);
    }
    if (contains(json, len, "\"type\":\"playback_closed\"")) {
        atomic_fetch_add(&g_pb_closed, 1);
    }
    if (contains(json, len, "\"type\":\"recordings_changed\"")) {
        atomic_fetch_add(&g_rec_changed, 1);
    }
    if (contains(json, len, "\"type\":\"thumbnail\"") && contains(json, len, "\"state\":\"ready\"")) {
        atomic_fetch_add(&g_thumb_ready, 1);
    }
    /* "download" with the closing quote, so not download_removed. */
    if (contains(json, len, "\"type\":\"download\"") && contains(json, len, "\"state\":\"done\"")) {
        atomic_fetch_add(&g_download_done, 1);
    }
    if (contains(json, len, "\"type\":\"download_removed\"") && contains(json, len, "\"trash\":true")) {
        atomic_fetch_add(&g_download_trashed, 1);
    }
    if (contains(json, len, "\"type\":\"ptz\"") && contains(json, len, "\"state\":\"sent\"")) {
        if (contains(json, len, "\"action\":\"move\"")) {
            atomic_fetch_add(&g_ptz_move, 1);
        }
        if (contains(json, len, "\"action\":\"stop\"")) {
            atomic_fetch_add(&g_ptz_stop, 1);
        }
        if (contains(json, len, "\"action\":\"preset\"")) {
            atomic_fetch_add(&g_ptz_preset, 1);
        }
        if (contains(json, len, "\"action\":\"lens\"") && contains(json, len, "\"position\":6000")) {
            atomic_fetch_add(&g_ptz_zoom_clamped, 1);
        }
    }
    if (contains(json, len, "\"type\":\"device_info\"") &&
        contains(json, len, "\"recordings\":{")) {
        atomic_fetch_add(&g_device_info_rec, 1);
    }
}

static void on_audio_tap(void *context, uint64_t tile, const NvAudioBlock *block) {
    (void)context;
    atomic_fetch_add(&g_callbacks, 1);
    atomic_fetch_add(&g_taps, 1);
    if (tile != g_tile || block->sample_rate != 16000 || block->samples == NULL ||
        block->sample_count == 0) {
        problem("bad tap block");
    }
}

/* An audio device's real-time thread: 10 ms of 48 kHz stereo every 10 ms,
 * each callback 10 ms ahead of when it is heard. */
static void *render_thread(void *arg) {
    NvCore *core = arg;
    static float buf[480 * 2];
    while (!atomic_load(&g_stop_render)) {
        uint64_t host = nv_clock_now_ns() + 10000000u;
        if (nv_audio_render(core, buf, 480, 2, host) != NV_OK) {
            problem("nv_audio_render failed");
        }
        int loud = 0;
        for (size_t i = 0; i < sizeof buf / sizeof buf[0]; i++) {
            if (!isfinite(buf[i]) || buf[i] > 1.0f || buf[i] < -1.0f) {
                problem("rendered sample out of range");
                break;
            }
            loud |= fabsf(buf[i]) > 0.1f;
        }
        atomic_fetch_add(&g_renders, 1);
        if (loud) {
            atomic_fetch_add(&g_loud, 1);
        }
        sleep_ms(10);
    }
    return NULL;
}

/* ---- Recordings (minor 5) ------------------------------------------- */

typedef NvStatus (*query_fn)(NvCore *, const char *, char *, size_t, size_t *);

/* A query's answer in a malloc'd, NUL-terminated buffer (NULL on failure),
 * with the out-buffer loop every binding uses: ask for the size, retry with
 * a larger buffer while the answer grows. */
static char *query_json(NvCore *core, query_fn f, const char *query) {
    size_t len = 0;
    char *buf = NULL;
    NvStatus s = f(core, query, NULL, 0, &len);
    while (s == NV_ERR_BUFFER_TOO_SMALL) {
        free(buf);
        buf = malloc(len + 1);
        if (buf == NULL) {
            return NULL;
        }
        s = f(core, query, buf, len + 1, &len);
    }
    if (s != NV_OK) {
        free(buf);
        return NULL;
    }
    return buf;
}

static NvStatus status_fn(NvCore *core, const char *query, char *out, size_t cap, size_t *len) {
    (void)query;
    return nv_recordings_status(core, out, cap, len);
}

static NvStatus downloads_fn(NvCore *core, const char *query, char *out, size_t cap, size_t *len) {
    (void)query;
    return nv_downloads_get(core, out, cap, len);
}

static NvStatus groups_fn(NvCore *core, const char *query, char *out, size_t cap, size_t *len) {
    (void)query;
    return nv_camera_groups_get(core, out, cap, len);
}

/* Copies the string value after `"name":"` in text into out (no escapes in
 * the values read here). */
static bool string_member(const char *text, const char *name, char *out, size_t cap) {
    char needle[64];
    snprintf(needle, sizeof needle, "\"%s\":\"", name);
    const char *p = strstr(text, needle);
    if (p == NULL) {
        return false;
    }
    p += strlen(needle);
    const char *end = strchr(p, '"');
    if (end == NULL || (size_t)(end - p) >= cap) {
        return false;
    }
    memcpy(out, p, (size_t)(end - p));
    out[end - p] = 0;
    return true;
}

static bool number_member(const char *text, const char *name, long long *out) {
    char needle[64];
    snprintf(needle, sizeof needle, "\"%s\":", name);
    const char *p = strstr(text, needle);
    if (p == NULL) {
        return false;
    }
    *out = strtoll(p + strlen(needle), NULL, 10);
    return true;
}

/* The first object of the array named `name`, as a malloc'd string. */
static char *first_object(const char *text, const char *name) {
    char needle[64];
    snprintf(needle, sizeof needle, "\"%s\":[{", name);
    const char *p = strstr(text, needle);
    if (p == NULL) {
        return NULL;
    }
    p += strlen(needle) - 1;
    int depth = 0;
    for (const char *q = p; *q; q++) {
        if (*q == '{') {
            depth++;
        } else if (*q == '}' && --depth == 0) {
            size_t n = (size_t)(q - p) + 1;
            char *o = malloc(n + 1);
            if (o != NULL) {
                memcpy(o, p, n);
                o[n] = 0;
            }
            return o;
        }
    }
    return NULL;
}

static bool settled(const char *downloads) {
    return strstr(downloads, "\"state\":\"done\"") != NULL &&
           strstr(downloads, "\"state\":\"running\"") == NULL &&
           strstr(downloads, "\"state\":\"queued\"") == NULL &&
           strstr(downloads, "\"state\":\"finalizing\"") == NULL &&
           strstr(downloads, "\"state\":\"waiting_device\"") == NULL &&
           strstr(downloads, "\"state\":\"failed\"") == NULL;
}

/* Edit lists and the rest of sound (minor 8) against the finished download
 * and the mock's cameras. */
static void edit_and_sound(NvCore *core, const char *key, const char *download, long long day) {
    char json[1024], small[8];
    size_t len = 0;
    char *plan = malloc(1 << 20);
    if (plan == NULL) {
        problem("edit_and_sound: out of memory");
        return;
    }
    snprintf(json, sizeof json, "{\"kind\":\"grid\",\"download\":\"%s\",\"layout\":\"2up\"}", download);
    if (nv_edit_compute(core, json, small, sizeof small, &len) != NV_ERR_BUFFER_TOO_SMALL ||
        len <= sizeof small || len >= (1 << 20) ||
        nv_edit_compute(core, json, plan, 1 << 20, &len) != NV_OK ||
        strstr(plan, "\"kind\":\"grid\"") == NULL || strstr(plan, "\"cells\"") == NULL) {
        problem("nv_edit_compute grid");
    }
    snprintf(json, sizeof json, "{\"kind\":\"follow\",\"download\":\"%s\"}", download);
    if (nv_edit_compute(core, json, plan, 1 << 20, &len) != NV_OK ||
        strstr(plan, "\"kind\":\"follow\"") == NULL || strstr(plan, "\"segments\"") == NULL) {
        problem("nv_edit_compute follow");
    }
    snprintf(json, sizeof json, "{\"kind\":\"follow\",\"download\":\"%s\",\"in_ms\":1}", download);
    if (nv_edit_compute(core, json, plan, 1 << 20, &len) != NV_OK ||
        nv_edit_compute(core, "{\"kind\":\"follow\",\"download\":\"nope\"}", plan, 1 << 20, &len) !=
            NV_ERR_UNKNOWN_ID ||
        nv_edit_compute(core, "{", plan, 1 << 20, &len) != NV_ERR_INVALID_ARGUMENT ||
        nv_edit_compute(core, json, plan, 1 << 20, NULL) != NV_ERR_INVALID_ARGUMENT) {
        problem("nv_edit_compute status codes");
    }

    char camera[256];
    snprintf(camera, sizeof camera, "{\"device\":\"%s\",\"channel\":0,\"lens\":0}", key);
    if (nv_sound_watch(core, camera, true) != NV_OK || nv_sound_watch(core, camera, false) != NV_OK ||
        nv_sound_watch(core, "{", true) != NV_ERR_INVALID_ARGUMENT) {
        problem("nv_sound_watch");
    }
    snprintf(json, sizeof json,
             "{\"start\":%lld,\"end\":%lld,\"bucket_s\":60,\"cameras\":[{\"device\":\"%s\",\"channel\":0}]}",
             day, day + 3599, key);
    if (nv_recordings_loudness(core, json, plan, 1 << 20, &len) != NV_OK ||
        strstr(plan, "\"lanes\"") == NULL || strstr(plan, "\"peak_db\"") == NULL ||
        nv_recordings_loudness(core, "{", plan, 1 << 20, &len) != NV_ERR_INVALID_ARGUMENT) {
        problem("nv_recordings_loudness");
    }
    snprintf(json, sizeof json,
             "{\"camera\":%s,\"analyzer\":\"soundanalysis-v1\",\"start\":%lld,\"end\":%lld,"
             "\"labels\":[{\"kind\":\"dog_bark\",\"start\":%lld,\"end\":%lld,\"confidence\":0.9,"
             "\"label\":\"dog_bark\"}]}",
             camera, day, day + 600, day + 100, day + 105);
    if (nv_sound_labels_put(core, json) != NV_OK ||
        nv_sound_labels_put(core, "{") != NV_ERR_INVALID_ARGUMENT) {
        problem("nv_sound_labels_put");
    }
    snprintf(json, sizeof json, "{\"camera\":%s,\"day\":%lld}", camera, day + 5);
    char analysis[256];
    if (nv_sound_analyze_day(core, json, analysis, sizeof analysis, &len) != NV_OK ||
        strstr(analysis, "\"queued\"") == NULL ||
        nv_sound_analyze_cancel(core, 987654321) != NV_ERR_UNKNOWN_ID) {
        problem("nv_sound_analyze_day");
    }
    uint8_t peak[4], rms[4];
    int64_t first = 0;
    if (nv_tile_loudness(core, 987654321, peak, rms, sizeof peak, &first, &len) != NV_ERR_UNKNOWN_ID ||
        nv_tile_loudness(core, 987654321, peak, rms, sizeof peak, NULL, &len) != NV_ERR_INVALID_ARGUMENT) {
        problem("nv_tile_loudness");
    }
    free(plan);
}

/* Export (minor 10) against the finished incident download and a range
 * download of the same camera: a trim plan on each, a still plan from a file,
 * and the refusals. `start` is a time (seconds) the camera recorded. */
static void export_plans(NvCore *core, const char *key, const char *download, long long start) {
    char json[1024], answer[4096], range[128];
    size_t len = 0;
    char *plan = malloc(1 << 20);
    if (plan == NULL) {
        problem("export_plans: out of memory");
        return;
    }
    /* Both files cover these points: they clip to the file. */
    snprintf(json, sizeof json,
             "{\"kind\":\"trim\",\"download\":\"%s\",\"in_ms\":%lld,\"out_ms\":%lld}", download,
             (start - 100000) * 1000, (start + 100000) * 1000);
    if (nv_edit_compute(core, json, plan, 1 << 20, &len) != NV_OK ||
        strstr(plan, "\"kind\":\"trim\"") == NULL || strstr(plan, "\"outputs\"") == NULL ||
        strstr(plan, "\"copy_possible\":true") == NULL) {
        problem("nv_edit_compute trim");
    }
    snprintf(json, sizeof json,
             "{\"kind\":\"trim\",\"download\":\"%s\",\"in_ms\":5,\"out_ms\":5}", download);
    if (nv_edit_compute(core, json, plan, 1 << 20, &len) != NV_ERR_INVALID_ARGUMENT) {
        problem("nv_edit_compute trim with an empty window");
    }

    snprintf(json, sizeof json,
             "{\"start\":%lld,\"end\":%lld,\"cameras\":[{\"camera\":{\"device\":\"%s\",\"channel\":0},"
             "\"tier\":\"sub\"}],\"share_safe\":true}",
             start, start + 8, key);
    if (nv_download_range(core, json, answer, sizeof answer, &len) != NV_OK ||
        !string_member(answer, "download", range, sizeof range) || strstr(answer, "\"folder\"") == NULL ||
        strstr(answer, "\"existing\":false") == NULL) {
        problem("nv_download_range");
        free(plan);
        return;
    }
    if (nv_download_range(core, json, answer, sizeof answer, &len) != NV_OK ||
        strstr(answer, "\"existing\":true") == NULL || strstr(answer, range) == NULL) {
        problem("nv_download_range again");
    }
    bool done = false;
    for (int i = 0; i < 3000 && !done; i++) {
        char *all = query_json(core, downloads_fn, NULL);
        done = all != NULL && strstr(all, range) != NULL && settled(all) &&
               strstr(all, "\"range\":{") != NULL;
        free(all);
        if (!done) {
            sleep_ms(10);
        }
    }
    if (!done) {
        problem("the range download did not finish");
    }
    snprintf(json, sizeof json,
             "{\"kind\":\"trim\",\"download\":\"%s\",\"in_ms\":%lld,\"out_ms\":%lld,\"cut\":\"exact\","
             "\"share_safe\":true}",
             range, (start + 2) * 1000, (start + 6) * 1000);
    if (nv_edit_compute(core, json, plan, 1 << 20, &len) != NV_OK ||
        strstr(plan, "\"Camera A\"") == NULL || strstr(plan, "\"copy_possible\":false") == NULL) {
        problem("nv_edit_compute trim of a range");
    }
    snprintf(json, sizeof json,
             "{\"download\":\"%s\",\"camera\":{\"device\":\"%s\",\"channel\":0},\"at_ms\":%lld,"
             "\"format\":\"jpeg\"}",
             range, key, (start + 3) * 1000);
    if (nv_still_plan(core, json, plan, 1 << 20, &len) != NV_OK || strstr(plan, "\"stills\"") == NULL ||
        strstr(plan, ".jpg") == NULL) {
        problem("nv_still_plan from a file");
    }
    if (nv_still_plan(core, "{\"playback\":987654321}", plan, 1 << 20, &len) != NV_ERR_UNKNOWN_ID ||
        nv_still_plan(core, "{", plan, 1 << 20, &len) != NV_ERR_INVALID_ARGUMENT ||
        nv_still_plan(core, json, plan, 1 << 20, NULL) != NV_ERR_INVALID_ARGUMENT) {
        problem("nv_still_plan status codes");
    }
    snprintf(json, sizeof json,
             "{\"start\":%lld,\"end\":%lld,\"cameras\":[{\"camera\":{\"device\":\"%s\",\"channel\":0}}],"
             "\"dest\":\"relative\"}",
             start, start + 8, key);
    if (nv_download_range(core, json, answer, sizeof answer, &len) != NV_ERR_INVALID_ARGUMENT ||
        nv_download_range(core, "{\"start\":5,\"end\":4,\"cameras\":[]}", answer, sizeof answer, &len) !=
            NV_ERR_INVALID_ARGUMENT) {
        problem("nv_download_range refusals");
    }
    free(plan);
}

/* Configures recordings, asks for yesterday's index, queries it, edits and
 * locks an incident, fetches a thumbnail from pending to ready, downloads
 * an incident to the end, and hands it to the Trash, with the events of
 * each step. */
static void recordings(NvCore *core, const char *key, const char *work) {
    char json[1024], answer[4096], incident[256], download[128];
    size_t len = 0;
    long long day = ((long long)time(NULL) / 86400 - 1) * 86400;
    char cams[512];
    snprintf(cams, sizeof cams, "[{\"device\":\"%s\",\"channel\":0},{\"device\":\"%s\",\"channel\":1}]",
             key, key);

    snprintf(json, sizeof json, "{\"channels\":[{\"channel\":0},{\"channel\":1}]}");
    if (nv_recordings_enable(core, key, json) != NV_ERR_NOT_CONFIGURED) {
        problem("nv_recordings_enable before nv_recordings_configure");
    }
    if (nv_recordings_configure(core, "{") != NV_ERR_INVALID_ARGUMENT) {
        problem("nv_recordings_configure accepted invalid JSON");
    }
    char config[600];
    snprintf(config, sizeof config, "{\"downloads_dir\":\"%s/downloads\"}", work);
    if (nv_recordings_configure(core, config) != NV_OK ||
        nv_recordings_enable(core, key, json) != NV_OK ||
        nv_sound_configure(core, "{\"alarms\":true}") != NV_OK) {
        problem("recordings configuration");
        return;
    }

    snprintf(json, sizeof json, "{\"cameras\":%s,\"days\":[%lld],\"force\":false}", cams, day);
    if (nv_recordings_want(core, json) != NV_OK) {
        problem("nv_recordings_want");
        return;
    }
    char query[128];
    snprintf(query, sizeof query, "{\"start\":%lld,\"end\":%lld}", day, day + 86399);
    char *events = NULL;
    for (int i = 0; i < 2000; i++) {
        free(events);
        events = query_json(core, nv_recordings_events, query);
        /* Done once the index is complete (both cameras' events, no day
         * pending, the lane idle), not at the first event: playback and
         * the incident list below rely on all of it. */
        char *status = query_json(core, status_fn, NULL);
        bool filled = status != NULL && strstr(status, "\"state\":\"idle\"") != NULL &&
                      strstr(status, "\"pending_days\":0") != NULL;
        free(status);
        if (filled && events != NULL && strstr(events, "\"kind\"") != NULL &&
            strstr(events, "\"channel\":0") != NULL && strstr(events, "\"channel\":1") != NULL &&
            atomic_load(&g_rec_changed) > 0) {
            break;
        }
        sleep_ms(10);
    }
    char *first = events ? first_object(events, "events") : NULL;
    long long channel = -1, start = 0, end = 0;
    if (first == NULL || !number_member(first, "channel", &channel) ||
        !number_member(first, "start", &start) || !number_member(first, "end", &end)) {
        problem("no events indexed, or no recordings_changed");
        free(first);
        free(events);
        return;
    }
    free(first);
    free(events);

    {
        char small[8];
        char *calendar = query_json(core, nv_recordings_calendar, query);
        char *spans = query_json(core, nv_recordings_spans, query);
        char *status = query_json(core, status_fn, NULL);
        if (nv_recordings_calendar(core, query, small, sizeof small, &len) != NV_ERR_BUFFER_TOO_SMALL ||
            len < sizeof small || calendar == NULL || strstr(calendar, "\"days\"") == NULL ||
            spans == NULL || strstr(spans, "\"causes\"") == NULL || status == NULL ||
            strstr(status, key) == NULL) {
            problem("calendar, spans, or status");
        }
        free(calendar);
        free(spans);
        free(status);
    }
    snprintf(json, sizeof json, "{\"cameras\":%s}", cams);
    if (nv_recordings_hold_today(core, 1, json) != NV_OK ||
        nv_recordings_hold_today(core, 1, NULL) != NV_OK ||
        nv_recordings_hold_today(core, 0, json) != NV_ERR_INVALID_ARGUMENT) {
        problem("nv_recordings_hold_today");
    }

    /* Incidents: the newest, renamed, then locked. */
    char *incidents = NULL;
    /* Incidents are grouped after the index settles and announced by an
     * event, so ask until the list has one. */
    for (int i = 0; i < 2000; i++) {
        free(incidents);
        incidents = query_json(core, nv_incidents_get, query);
        if (incidents != NULL && strstr(incidents, "\"key\"") != NULL) {
            break;
        }
        sleep_ms(10);
    }
    if (incidents == NULL || !string_member(incidents, "key", incident, sizeof incident)) {
        problem("nv_incidents_get");
        free(incidents);
        return;
    }
    free(incidents);
    if (nv_incident_get(core, "i-unknown", answer, sizeof answer, &len) != NV_ERR_UNKNOWN_ID ||
        nv_incident_get(core, incident, answer, sizeof answer, &len) != NV_OK ||
        strstr(answer, incident) == NULL) {
        problem("nv_incident_get");
    }
    snprintf(json, sizeof json, "{\"op\":\"rename\",\"key\":\"%s\",\"name\":\"Delivery\"}", incident);
    if (nv_incident_edit(core, json, answer, sizeof answer, &len) != NV_OK ||
        strstr(answer, "\"keys\"") == NULL) {
        problem("nv_incident_edit");
    }
    char *groups = query_json(core, groups_fn, NULL);
    if (groups == NULL || nv_camera_groups_put(core, "{\"groups\":[]}") != NV_OK) {
        problem("camera groups");
    }
    free(groups);

    /* A thumbnail of the first event: pending, then ready. */
    char thumb[512];
    snprintf(thumb, sizeof thumb,
             "{\"camera\":{\"device\":\"%s\",\"channel\":%lld},\"tier\":\"sub\",\"start\":%lld,"
             "\"end\":%lld}",
             key, channel, start, end);
    NvThumbnail info = {0};
    if (nv_thumbnail_get(core, thumb, &info, NULL, 0, &len) != NV_PENDING || len != 0) {
        problem("a thumbnail not cached yet was not pending");
    }
    for (int i = 0; i < 2000 && atomic_load(&g_thumb_ready) == 0; i++) {
        sleep_ms(10);
    }
    uint8_t *bytes = NULL;
    if (nv_thumbnail_get(core, thumb, &info, NULL, 0, &len) != NV_ERR_BUFFER_TOO_SMALL || len < 5 ||
        info.kind != NV_THUMB_H264_KEYFRAME || (bytes = malloc(len)) == NULL ||
        nv_thumbnail_get(core, thumb, &info, bytes, len, &len) != NV_OK || bytes[0] != 0 ||
        bytes[1] != 0) {
        problem("thumbnail from pending to ready");
    }
    free(bytes);
    {
        static const uint8_t image[4] = {0xff, 0xd8, 0xff, 0xd9};
        NvThumbnail small = {NV_THUMB_IMAGE, 64, 36, 0};
        uint8_t back[16];
        if (nv_thumbnail_put(core, thumb, &small, image, sizeof image) != NV_OK ||
            nv_thumbnail_get(core, thumb, &info, back, sizeof back, &len) != NV_OK ||
            info.kind != NV_THUMB_IMAGE || len != sizeof image || memcmp(back, image, len) != 0 ||
            nv_thumbnail_cancel(core, NULL) != NV_OK) {
            problem("nv_thumbnail_put");
        }
    }

    /* A download to the end, then locked (the same download comes back),
     * then handed to the Trash. */
    snprintf(json, sizeof json, "{\"incident\":\"%s\"}", incident);
    if (nv_download_incident(core, json, answer, sizeof answer, &len) != NV_OK ||
        !string_member(answer, "download", download, sizeof download) ||
        strstr(answer, work) == NULL) {
        problem("nv_download_incident");
        return;
    }
    bool done = false;
    for (int i = 0; i < 3000 && !done; i++) {
        char *all = query_json(core, downloads_fn, NULL);
        done = all != NULL && strstr(all, download) != NULL && settled(all) &&
               atomic_load(&g_download_done) > 0;
        free(all);
        if (!done) {
            sleep_ms(10);
        }
    }
    if (!done) {
        problem("the download did not finish");
    }
    edit_and_sound(core, key, download, day);
    export_plans(core, key, download, start);
    if (nv_incident_lock(core, incident, true, answer, sizeof answer, &len) != NV_OK ||
        strstr(answer, "\"existing\"") == NULL || strstr(answer, download) == NULL) {
        problem("nv_incident_lock");
    }
    if (nv_download_pause(core, "no-such-download", NULL, true) != NV_ERR_UNKNOWN_ID ||
        nv_download_retry(core, download, NULL) != NV_OK) {
        problem("download control");
    }
    if (nv_download_trash(core, download, answer, sizeof answer, &len) != NV_OK ||
        strstr(answer, "\"folder\"") == NULL) {
        problem("nv_download_trash");
    }
    for (int i = 0; i < 2000 && atomic_load(&g_download_trashed) == 0; i++) {
        sleep_ms(10);
    }
    char *after = query_json(core, downloads_fn, NULL);
    if (atomic_load(&g_download_trashed) == 0 || after == NULL || strstr(after, download) != NULL ||
        nv_download_forget(core, download, false) != NV_ERR_UNKNOWN_ID) {
        problem("download_removed after the Trash");
    }
    free(after);
    if (atomic_load(&g_device_info_rec) == 0) {
        problem("device_info without recordings flags");
    }
}

/* ---- Recorded playback (minor 6) ------------------------------------ */

static NvStatus playback_status_fn(NvCore *core, const char *query, char *out, size_t cap,
                                   size_t *len) {
    return nv_playback_status(core, (uint64_t)strtoull(query, NULL, 10), out, cap, len);
}

static bool wait_count(atomic_ulong *counter, unsigned long at_least) {
    for (int i = 0; i < 2000 && atomic_load(counter) < at_least; i++) {
        sleep_ms(10);
    }
    return atomic_load(counter) >= at_least;
}

/* Opens a playback of yesterday's recordings of both channels (indexed by
 * recordings()), plays it beside the live tile, seeks, plays at 4x, and
 * closes it, with its events and status codes. */
static void playback(NvCore *core, const char *key) {
    long long day = ((long long)time(NULL) / 86400 - 1) * 86400;
    long long start = (day + 10 * 3600) * 1000, p0 = start + 30000;
    char json[1024];
    snprintf(json, sizeof json,
             "{\"start_ms\":%lld,\"end_ms\":%lld,\"position_ms\":%lld,\"cameras\":["
             "{\"camera\":{\"device\":\"%s\",\"channel\":0},\"tier\":\"sub\"},"
             "{\"camera\":{\"device\":\"%s\",\"channel\":1},\"tier\":\"sub\"}]}",
             start, start + 600000, p0, key, key);
    uint64_t pb = 0, tiles[4] = {0};
    size_t count = 0;
    atomic_store(&g_pb_phase, true);
    unsigned long closed_before = atomic_load(&g_tile_closed);

    if (nv_playback_open(core, json, &pb, tiles, 1, &count) != NV_ERR_BUFFER_TOO_SMALL ||
        count != 2 ||
        nv_playback_open(core, "{\"start_ms\":0}", &pb, tiles, 4, &count) !=
            NV_ERR_INVALID_ARGUMENT ||
        nv_playback_open(core, json, NULL, tiles, 4, &count) != NV_ERR_INVALID_ARGUMENT) {
        problem("nv_playback_open refusals");
    }
    if (nv_playback_open(core, json, &pb, tiles, 4, &count) != NV_OK || count != 2 || pb == 0 ||
        tiles[0] == 0 || tiles[1] == 0) {
        problem("nv_playback_open");
        return;
    }
    if (!wait_count(&g_pb_seeks_active, 1) || !wait_count(&g_pb_decodes, 2)) {
        problem("the opening seek did not show pictures");
    }
    int64_t pos = 0;
    NvPlaybackState state = NV_PLAYBACK_FAILED;
    if (nv_playback_position(core, pb, &pos, &state) != NV_OK || pos != p0 ||
        state != NV_PLAYBACK_PAUSED) {
        problem("paused at the opening position");
    }
    if (nv_tile_close(core, tiles[0]) != NV_ERR_INVALID_ARGUMENT ||
        nv_playback_seek(core, pb, p0, (NvSeekMode)9) != NV_ERR_INVALID_ARGUMENT ||
        nv_playback_set_rate(core, pb, 3.0) != NV_ERR_INVALID_ARGUMENT ||
        nv_playback_play(core, pb + 1000) != NV_ERR_UNKNOWN_ID ||
        nv_playback_set_offset(core, pb, tiles[1], 0) != NV_OK) {
        problem("playback refusals");
    }

    /* Play: the position moves on from where it was paused. */
    if (nv_playback_play(core, pb) != NV_OK) {
        problem("nv_playback_play");
    }
    bool moving = false;
    for (int i = 0; i < 2000 && !moving; i++) {
        moving = nv_playback_position(core, pb, &pos, &state) == NV_OK && pos > p0 &&
                 state == NV_PLAYBACK_PLAYING;
        if (!moving) {
            sleep_ms(10);
        }
    }
    if (!moving) {
        problem("playing");
    }

    /* An exact seek half a minute on: the next generation shows it. */
    unsigned long seeks = atomic_load(&g_pb_seeks_active);
    long long target = p0 + 30000;
    if (nv_playback_seek(core, pb, target, NV_SEEK_EXACT) != NV_OK ||
        !wait_count(&g_pb_seeks_active, seeks + 1) ||
        nv_playback_position(core, pb, &pos, &state) != NV_OK || pos < target) {
        problem("nv_playback_seek");
    }

    /* 4x: the clock runs about four times as fast. */
    if (nv_playback_set_rate(core, pb, 4.0) != NV_OK || !wait_count(&g_pb_rate4, 1)) {
        problem("nv_playback_set_rate");
    }
    /* Measured over 1.5 s of media (about 0.4 s at 4x) against the host
     * clock read before the first position and after the last, so a slow
     * machine can only make it look slower. */
    int64_t before = 0, after = 0;
    struct timespec t0, t1;
    clock_gettime(CLOCK_MONOTONIC, &t0);
    nv_playback_position(core, pb, &before, &state);
    after = before;
    for (int i = 0; i < 1000 && after - before < 1500; i++) {
        sleep_ms(10);
        nv_playback_position(core, pb, &after, &state);
    }
    clock_gettime(CLOCK_MONOTONIC, &t1);
    long long wall_ms = (long long)(t1.tv_sec - t0.tv_sec) * 1000 + (t1.tv_nsec - t0.tv_nsec) / 1000000;
    if (after - before < 1500 || (after - before) * 10 < wall_ms * 25) {
        fprintf(stderr, "smoke: 4x moved %lld ms in %lld ms\n", (long long)(after - before), wall_ms);
        problem("4x did not run faster");
    }
    char id[32];
    snprintf(id, sizeof id, "%llu", (unsigned long long)pb);
    char *status = query_json(core, playback_status_fn, id);
    if (status == NULL || strstr(status, "\"rate\":4.0") == NULL ||
        strstr(status, "\"cameras\":[{") == NULL) {
        problem("nv_playback_status");
    }
    free(status);
    if (nv_playback_pause(core, pb) != NV_OK) {
        problem("nv_playback_pause");
    }

    /* Close: both tiles close, then playback_closed. */
    if (nv_playback_close(core, pb) != NV_OK || !wait_count(&g_pb_closed, 1) ||
        nv_playback_close(core, pb) != NV_ERR_UNKNOWN_ID ||
        nv_playback_position(core, pb, &pos, &state) != NV_ERR_UNKNOWN_ID) {
        problem("nv_playback_close");
    }
    if (atomic_load(&g_tile_closed) - closed_before != 2) {
        problem("expected a tile_closed per playback tile");
    }
    atomic_store(&g_pb_tile_closed, atomic_load(&g_tile_closed) - closed_before);
}

/* ---- Capture (minor 9) ---------------------------------------------- */

/* The fake encoder's counts. */
static atomic_ulong g_enc_opened, g_enc_frames, g_enc_closed_flushed, g_enc_refused;
static const uint8_t SILENT_AU[4] = {0, 0, 0, 7};

static void enc_answer(uint64_t recording, uint32_t track) {
    if (nv_record_audio_encoded(atomic_load(&g_core), recording, track, SILENT_AU,
                                sizeof SILENT_AU) != NV_OK) {
        atomic_fetch_add(&g_enc_refused, 1);
    }
}

static bool enc_open(void *context, uint64_t recording, uint32_t track, uint32_t sample_rate,
                     uint32_t channels, uint32_t bit_rate, uint8_t *asc, size_t asc_cap,
                     size_t *asc_len, uint32_t *priming_frames) {
    (void)context;
    (void)recording;
    (void)track;
    if (sample_rate != 48000 || channels != 1 || bit_rate == 0 || asc_cap < 2) {
        atomic_fetch_add(&g_enc_refused, 1);
        return false;
    }
    /* AAC-LC, 48 kHz, mono. */
    asc[0] = 0x11;
    asc[1] = 0x88;
    *asc_len = 2;
    *priming_frames = 2112;
    atomic_fetch_add(&g_enc_opened, 1);
    return true;
}

/* Answers each frame from inside the callback, as an encoder may. */
static void enc_encode(void *context, uint64_t recording, uint32_t track, const float *pcm,
                       size_t frames) {
    (void)context;
    if (pcm == NULL || frames != 1024) {
        atomic_fetch_add(&g_enc_refused, 1);
        return;
    }
    float sum = 0;
    for (size_t i = 0; i < frames; i++) {
        sum += pcm[i] * pcm[i];
    }
    if (sum != sum) {
        atomic_fetch_add(&g_enc_refused, 1);
    }
    atomic_fetch_add(&g_enc_frames, 1);
    enc_answer(recording, track);
}

static void enc_close(void *context, uint64_t recording, uint32_t track, bool flush) {
    (void)context;
    if (flush) {
        enc_answer(recording, track);
        enc_answer(recording, track);
        atomic_fetch_add(&g_enc_closed_flushed, 1);
    }
}

static NvStatus record_status_fn(NvCore *core, const char *query, char *out, size_t cap,
                                 size_t *len) {
    (void)query;
    return nv_record_status(core, out, cap, len);
}

/* Starts a recording; its ID and file (file NULL to skip), 0 on failure. */
static uint64_t record_start(NvCore *core, const char *request, char *file, size_t file_cap) {
    char out[4096];
    size_t len = 0;
    long long id = 0;
    if (nv_record_start(core, request, out, sizeof out, &len) != NV_OK ||
        !number_member(out, "recording", &id) || id <= 0 ||
        strstr(out, "\"state\":\"waiting_keyframe\"") == NULL ||
        (file != NULL && !string_member(out, "file", file, file_cap))) {
        return 0;
    }
    return (uint64_t)id;
}

/* Waits until the recording's status shows it recording for `ms`. */
static bool recorded_for(NvCore *core, uint64_t id, long long ms) {
    char needle[64];
    snprintf(needle, sizeof needle, "\"recording\":%llu,", (unsigned long long)id);
    for (int i = 0; i < 1000; i++) {
        char *status = query_json(core, record_status_fn, NULL);
        long long duration = 0;
        bool done = status != NULL && strstr(status, needle) != NULL &&
                    strstr(status, "\"state\":\"recording\"") != NULL &&
                    number_member(status, "duration_ms", &duration) && duration >= ms;
        free(status);
        if (done) {
            return true;
        }
        sleep_ms(10);
    }
    return false;
}

/* Waits until no recording is left running. */
static bool all_ended(NvCore *core) {
    for (int i = 0; i < 1000; i++) {
        char *status = query_json(core, record_status_fn, NULL);
        bool done = status != NULL && strcmp(status, "{\"recordings\":[]}") == 0;
        free(status);
        if (done) {
            return true;
        }
        sleep_ms(10);
    }
    return false;
}

/* A finalized (not fragmented) MP4: top-level ftyp first, then a moov and no
 * moof. */
static bool finalized_mp4(const char *path) {
    FILE *f = fopen(path, "rb");
    if (f == NULL) {
        return false;
    }
    bool ftyp = false, moov = false, moof = false, first = true;
    unsigned char h[16];
    long pos = 0;
    while (fseek(f, pos, SEEK_SET) == 0 && fread(h, 1, 8, f) == 8) {
        unsigned long long size = ((unsigned long long)h[0] << 24) | (h[1] << 16) |
                                  (h[2] << 8) | h[3];
        if (size == 1) {
            if (fread(h + 8, 1, 8, f) != 8) {
                break;
            }
            size = 0;
            for (int i = 8; i < 16; i++) {
                size = (size << 8) | h[i];
            }
        }
        if (size < 8) {
            break;
        }
        if (first) {
            ftyp = memcmp(h + 4, "ftyp", 4) == 0;
            first = false;
        }
        moov |= memcmp(h + 4, "moov", 4) == 0;
        moof |= memcmp(h + 4, "moof", 4) == 0;
        pos += (long)size;
    }
    fclose(f);
    return ftyp && moov && !moof;
}

/* Records the live tile to a finalized MP4, then a call recording through
 * the fake encoder, with the status codes and the limit answer. */
static void capture(NvCore *core, uint64_t tile, const char *work) {
    char json[2048], file[1024], out[256];
    size_t len = 0;
    snprintf(json, sizeof json,
             "{\"kind\":\"live\",\"tile\":%llu,\"name\":\"Smoke\",\"utc_offset_s\":0}",
             (unsigned long long)tile);
    if (record_start(core, json, NULL, 0) != 0 ||
        nv_record_start(core, json, out, sizeof out, &len) != NV_ERR_NOT_CONFIGURED ||
        nv_capture_configure(core, "{\"live_dir\":\"relative\"}") != NV_ERR_INVALID_ARGUMENT) {
        problem("capture before nv_capture_configure");
    }
    char config[1200];
    snprintf(config, sizeof config,
             "{\"live_dir\":\"%s/live\",\"min_free_bytes\":0,\"stop_free_bytes\":0}", work);
    if (nv_capture_configure(core, config) != NV_OK) {
        problem("nv_capture_configure");
        return;
    }
    NvAudioEncoder encoder = {NULL, enc_open, enc_encode, NULL};
    if (nv_capture_set_encoder(core, &encoder) != NV_ERR_INVALID_ARGUMENT) {
        problem("an encoder without close was accepted");
    }
    encoder.close = enc_close;
    if (nv_capture_set_encoder(core, &encoder) != NV_OK) {
        problem("nv_capture_set_encoder");
    }

    /* Live: about two seconds, stopped, finalized. */
    uint64_t live = record_start(core, json, file, sizeof file);
    if (live == 0) {
        problem("nv_record_start (live)");
        return;
    }
    if (!recorded_for(core, live, 2000)) {
        problem("the live recording did not record");
    }
    if (nv_record_stop(core, live) != NV_OK || !all_ended(core)) {
        problem("nv_record_stop (live)");
    }
    if (!finalized_mp4(file)) {
        problem("the live recording is not a finalized MP4");
    }

    /* A call: Conversation, Visitor (the camera sends no audio here), and
     * You all go through the encoder. */
    snprintf(json, sizeof json, "{\"kind\":\"call\",\"tile\":%llu,\"file\":\"%s/live/call.mp4\"}",
             (unsigned long long)tile, work);
    uint64_t call = record_start(core, json, file, sizeof file);
    if (call == 0) {
        problem("nv_record_start (call)");
        return;
    }
    if (!recorded_for(core, call, 1500)) {
        problem("the call recording did not record");
    }
    if (nv_record_stop(core, call) != NV_OK || !all_ended(core)) {
        problem("nv_record_stop (call)");
    }
    if (!finalized_mp4(file)) {
        problem("the call recording is not a finalized MP4");
    }
    if (atomic_load(&g_enc_opened) != 3 || atomic_load(&g_enc_closed_flushed) != 3 ||
        atomic_load(&g_enc_frames) < 3 * 40 || atomic_load(&g_enc_refused) != 0) {
        problem("the encoder's calls");
    }
    if (nv_capture_set_encoder(core, NULL) != NV_OK) {
        problem("removing the encoder");
    }

    /* Limits: no live recordings allowed answers which limit. */
    snprintf(config, sizeof config, "{\"live_dir\":\"%s/live\",\"max_live\":0,\"min_free_bytes\":0}",
             work);
    snprintf(json, sizeof json, "{\"kind\":\"live\",\"tile\":%llu}", (unsigned long long)tile);
    if (nv_capture_configure(core, config) != NV_OK ||
        nv_record_start(core, json, out, sizeof out, &len) != NV_ERR_LIMIT ||
        strcmp(out, "{\"limit\":\"live\"}") != 0 || len != strlen(out)) {
        problem("the live limit");
    }
    snprintf(json, sizeof json, "{\"kind\":\"live\",\"tile\":%llu}", (unsigned long long)tile + 100);
    if (nv_record_start(core, json, out, sizeof out, &len) != NV_ERR_UNKNOWN_ID ||
        nv_record_start(core, "{\"kind\":\"film\"}", out, sizeof out, &len) !=
            NV_ERR_INVALID_ARGUMENT ||
        nv_record_stop(core, 999999) != NV_ERR_UNKNOWN_ID ||
        nv_record_status(core, out, sizeof out, NULL) != NV_ERR_INVALID_ARGUMENT ||
        nv_record_audio_encoded(core, call, 0, NULL, 4) != NV_ERR_INVALID_ARGUMENT) {
        problem("capture refusals");
    }
}

/* ---- Smart alerts (minor 12) ---------------------------------------- */

/* verify_request keys seen by on_event, answered by the main thread. */
static pthread_mutex_t g_req_lock = PTHREAD_MUTEX_INITIALIZER;
static char g_req_keys[8][32];
static int g_req_count;
static atomic_ulong g_alerts_new, g_alerts_configured;

static void note_alert_event(const char *json, size_t len) {
    if (contains(json, len, "\"type\":\"alerts_configured\"")) {
        atomic_fetch_add(&g_alerts_configured, 1);
    }
    if (contains(json, len, "\"type\":\"alert\"") && contains(json, len, "\"state\":\"new\"")) {
        atomic_fetch_add(&g_alerts_new, 1);
    }
    if (!contains(json, len, "\"type\":\"verify_request\"")) {
        return;
    }
    const char *p = strstr(json, "\"request\":\"");
    if (p == NULL) {
        problem("verify_request without a request key");
        return;
    }
    p += strlen("\"request\":\"");
    const char *end = strchr(p, '"');
    pthread_mutex_lock(&g_req_lock);
    if (end != NULL && end - p < 31 && g_req_count < 8) {
        memcpy(g_req_keys[g_req_count], p, (size_t)(end - p));
        g_req_keys[g_req_count][end - p] = 0;
        g_req_count++;
    }
    pthread_mutex_unlock(&g_req_lock);
}

static NvStatus alerts_get_fn(NvCore *core, const char *query, char *out, size_t cap, size_t *len) {
    return nv_alerts_get(core, query, out, cap, len);
}

/* A trigger from the app wakes channel 1; each picture is fetched and
 * answered with a person; the alert is logged, gets a snapshot and
 * feedback; then the camera is muted. */
static void alerts(NvCore *core, uint64_t device) {
    char json[512];
    uint64_t trigger = 0;
    snprintf(json, sizeof json,
             "{\"source\":\"device\",\"camera\":{\"device\":%llu,\"channel\":1},\"kind\":\"people\"}",
             (unsigned long long)device);
    if (nv_alerts_trigger_put(core, json, &trigger) != NV_ERR_NOT_CONFIGURED ||
        nv_alerts_configure(core, "{") != NV_ERR_INVALID_ARGUMENT) {
        problem("smart alerts before configure");
    }
    char settings[512];
    snprintf(settings, sizeof settings,
             "{\"cameras\":[{\"camera\":{\"device\":%llu,\"channel\":1}}],\"utc_offset_min\":0,"
             "\"acquisition\":{\"recheck\":false}}",
             (unsigned long long)device);
    if (nv_alerts_configure(core, settings) != NV_OK ||
        nv_alerts_trigger_put(core, json, &trigger) != NV_OK || trigger == 0) {
        problem("nv_alerts_configure or nv_alerts_trigger_put");
        return;
    }
    int answered = 0;
    for (int i = 0; i < 1000 && atomic_load(&g_alerts_new) == 0; i++) {
        char key[32] = {0};
        pthread_mutex_lock(&g_req_lock);
        if (answered < g_req_count) {
            memcpy(key, g_req_keys[answered], sizeof key);
            answered++;
        }
        pthread_mutex_unlock(&g_req_lock);
        if (key[0] == 0) {
            sleep_ms(10);
            continue;
        }
        NvThumbnail info;
        size_t len = 0;
        uint8_t *frames = NULL;
        if (nv_alert_frames_get(core, key, &info, NULL, 0, &len) != NV_ERR_BUFFER_TOO_SMALL ||
            len == 0 || (frames = malloc(len)) == NULL ||
            nv_alert_frames_get(core, key, &info, frames, len, &len) != NV_OK ||
            info.kind != NV_THUMB_H264_KEYFRAME || frames[0] != 0 || frames[1] != 0) {
            problem("nv_alert_frames_get");
        }
        free(frames);
        if (nv_alert_detections_put(core, key,
                                    "{\"detections\":[{\"class\":\"person\",\"score\":0.9,"
                                    "\"box\":{\"x\":0.4,\"y\":0.2,\"w\":0.2,\"h\":0.5}}]}") !=
            NV_OK) {
            problem("nv_alert_detections_put");
        }
        if (nv_alert_detections_put(core, key, "{\"detections\":[]}") != NV_ERR_UNKNOWN_ID) {
            problem("an answered request was answered again");
        }
    }
    if (atomic_load(&g_alerts_new) == 0 || atomic_load(&g_alerts_configured) == 0) {
        problem("no alert from a verified person");
        return;
    }
    char *log = query_json(core, alerts_get_fn, "{\"decision\":[\"alert\"]}");
    long long alert_id = 0;
    char *entry = log ? first_object(log, "entries") : NULL;
    if (entry == NULL || !number_member(entry, "alert_id", &alert_id) || alert_id <= 0) {
        problem("nv_alerts_get");
    }
    free(entry);
    free(log);
    static const uint8_t jpeg[] = {0xff, 0xd8, 'e', 'x', 0xff, 0xd9};
    uint8_t back[16];
    size_t back_len = 0;
    if (nv_alert_snapshot_put(core, (uint64_t)alert_id, jpeg, sizeof jpeg) != NV_OK ||
        nv_alert_snapshot_get(core, (uint64_t)alert_id, back, sizeof back, &back_len) != NV_OK ||
        back_len != sizeof jpeg || memcmp(back, jpeg, sizeof jpeg) != 0 ||
        nv_alert_snapshot_get(core, (uint64_t)alert_id + 1000, back, sizeof back, &back_len) !=
            NV_ERR_UNKNOWN_ID) {
        problem("alert snapshots");
    }
    snprintf(json, sizeof json, "{\"alert_id\":%lld,\"vote\":\"up\"}", alert_id);
    char camera[128];
    snprintf(camera, sizeof camera, "{\"device\":%llu,\"channel\":1}", (unsigned long long)device);
    if (nv_alert_feedback(core, json) != NV_OK ||
        nv_alert_feedback(core, "{\"alert_id\":1,\"vote\":\"maybe\"}") != NV_ERR_INVALID_ARGUMENT ||
        nv_alert_mute(core, camera, 900) != NV_OK || nv_alert_mute(core, camera, 0) != NV_OK ||
        nv_alert_mute(core, "{}", 60) != NV_ERR_INVALID_ARGUMENT ||
        nv_alert_frames_get(core, "v0", NULL, NULL, 0, NULL) != NV_ERR_INVALID_ARGUMENT) {
        problem("alert feedback, mute, and refusals");
    }
    {
        char sun[512];
        size_t sun_len = 0;
        if (nv_alerts_sun_times(core,
                                "{\"lat\":12.3,\"lon\":-45.6,\"at_ms\":1718971200000,"
                                "\"utc_offset_min\":-180}",
                                sun, sizeof sun, &sun_len) != NV_OK ||
            strstr(sun, "\"kind\":\"normal\"") == NULL ||
            nv_alerts_sun_times(core, "{\"lat\":91,\"lon\":0,\"at_ms\":0}", sun, sizeof sun,
                                &sun_len) != NV_ERR_INVALID_ARGUMENT) {
            problem("nv_alerts_sun_times");
        }
    }
    /* Off: nothing stays woken. */
    if (nv_alerts_configure(core, "{\"enabled\":false}") != NV_OK) {
        problem("turning smart alerts off");
    }
}

int main(int argc, char **argv) {
    if (argc != 4) {
        fprintf(stderr, "usage: smoke HOST PORT WORKDIR\n");
        return 2;
    }
    const char *work = argv[3];
    const char *password = getenv("NVRVIEW_TEST_PASSWORD");
    if (password == NULL) {
        password = "example-password";
    }
    if (nv_abi_version() != NV_ABI_VERSION) {
        fprintf(stderr, "smoke: ABI mismatch\n");
        return 1;
    }
    if (nv_abi_minor() != NV_ABI_MINOR) {
        fprintf(stderr, "smoke: ABI minor mismatch\n");
        return 1;
    }

    NvCallbacks cb = {NULL, on_action, on_event, on_audio_tap};
    NvCore *core = NULL;
    NvConfig config = {NULL, 0, 0, work};
    if (nv_core_new(&config, cb, &core) != NV_OK || core == NULL) {
        fprintf(stderr, "smoke: nv_core_new failed\n");
        return 1;
    }
    atomic_store(&g_core, core);

    uint64_t device = 0, tile = 0, bad_tile = 0;
    if (nv_device_add(core, argv[1], (uint16_t)atoi(argv[2]), "example-user", password,
                      &device) != NV_OK || device == 0) {
        fprintf(stderr, "smoke: nv_device_add failed\n");
        return 1;
    }
    if (nv_tile_open(core, device, 0, NV_TIER_SUB, &g_tile) != NV_OK || g_tile == 0) {
        fprintf(stderr, "smoke: nv_tile_open failed\n");
        return 1;
    }
    tile = g_tile;

    pthread_t renderer;
    if (nv_audio_set_output(core, 48000, 2) != NV_OK ||
        nv_tile_audio_set(core, tile, true, 1.0f) != NV_OK ||
        nv_tile_audio_tap(core, tile, true) != NV_OK) {
        problem("audio setup failed");
    }
    {
        float mono[16];
        if (nv_audio_render(core, mono, 16, 1, nv_clock_now_ns()) != NV_ERR_INVALID_ARGUMENT) {
            problem("a channel count other than the output's was accepted");
        }
    }
    if (pthread_create(&renderer, NULL, render_thread, core) != 0) {
        fprintf(stderr, "smoke: cannot start the render thread\n");
        return 1;
    }

    phase("start");
    for (int i = 0; i < 1000 && atomic_load(&g_answered) < 50; i++) {
        sleep_ms(10);
    }
    phase("first-frames");
    unsigned long decoded = atomic_load(&g_answered);

    /* Focus: the main stream starts as generation 1; generation 0 ends with
     * a Reset once generation 1 has shown a picture. */
    if (nv_tile_set_tier(core, tile, NV_TIER_MAIN) != NV_OK) {
        problem("nv_tile_set_tier failed");
    }
    for (int i = 0; i < 1000 && (atomic_load(&g_gen_answered[1]) < 20 ||
                                  atomic_load(&g_gen_resets[0]) < 1); i++) {
        sleep_ms(10);
    }
    phase("tier-switch");
    if (nv_tile_retry(core, tile + 100) != NV_ERR_UNKNOWN_ID) {
        problem("nv_tile_retry accepted an unknown tile");
    }
    if (nv_talk_set_conversation(core, tile + 100, true) != NV_ERR_UNKNOWN_ID) {
        problem("nv_talk_set_conversation accepted an unknown tile");
    }
    if (nv_talk_set_conversation(core, tile, true) != NV_OK ||
        nv_talk_set_conversation(core, tile, false) != NV_OK) {
        problem("nv_talk_set_conversation refused a tile");
    }
    /* Camera motion (minor 11) on the mock's pan-and-tilt camera (channel 0). */
    {
        char info[1024];
        size_t info_len = 0;
        char request[64];
        snprintf(request, sizeof request, "{\"tile\":%llu}", (unsigned long long)tile);
        if (nv_ptz_info(core, request, info, sizeof info, &info_len) != NV_OK ||
            !contains(info, info_len, "\"pan_tilt\":true") ||
            !contains(info, info_len, "\"speeds\":[1,64]")) {
            problem("nv_ptz_info did not describe the camera");
        }
        if (nv_ptz_move(core, tile, "sideways", 20) != NV_ERR_INVALID_ARGUMENT ||
            nv_ptz_move(core, tile, "left", 0) != NV_ERR_INVALID_ARGUMENT ||
            nv_ptz_move(core, tile + 100, "left", 20) != NV_ERR_UNKNOWN_ID ||
            nv_ptz_stop(core, tile + 100) != NV_ERR_UNKNOWN_ID ||
            nv_motion_grant(core, device, 0, "", true) != NV_ERR_INVALID_ARGUMENT ||
            nv_motion_grant(core, device, 0, "example-client", true) != NV_OK ||
            nv_motion_grant(core, device, 0, "example-client", false) != NV_OK) {
            problem("a camera motion call mishandled its arguments");
        }
        if (nv_ptz_move(core, tile, "left", 20) != NV_OK) {
            problem("nv_ptz_move failed");
        }
        for (int i = 0; i < 1000 && atomic_load(&g_ptz_move) < 1; i++) {
            sleep_ms(10);
        }
        if (nv_ptz_stop(core, tile) != NV_OK || nv_ptz_preset(core, tile, 1) != NV_OK ||
            nv_ptz_lens(core, tile, "zoom", 99999) != NV_OK) {
            problem("a camera motion call failed");
        }
        for (int i = 0; i < 1000 && (atomic_load(&g_ptz_stop) < 1 || atomic_load(&g_ptz_preset) < 1 ||
                                      atomic_load(&g_ptz_zoom_clamped) < 1); i++) {
            sleep_ms(10);
        }
        if (atomic_load(&g_ptz_move) != 1 || atomic_load(&g_ptz_stop) < 1 ||
            atomic_load(&g_ptz_preset) != 1 || atomic_load(&g_ptz_zoom_clamped) != 1) {
            problem("camera motion events did not arrive");
        }
    }
    if (atomic_load(&g_gen_answered[1]) < 20 || atomic_load(&g_gen_resets[0]) != 1) {
        problem("the tier switch did not complete");
    }
    {
        float peak = -1, rms = -1;
        if (nv_tile_audio_level(core, tile, &peak, &rms) != NV_OK || peak < 0 || peak > 1 ||
            rms < 0 || rms > peak + 1e-6f) {
            problem("nv_tile_audio_level");
        }
    }
    {
        char key[64], json[512], *all = NULL;
        size_t len = 0;
        if (nv_device_key(core, device, key, sizeof key, &len) != NV_OK || len == 0 ||
            strlen(key) != len) {
            problem("nv_device_key");
        }
        snprintf(json, sizeof json,
                 "{\"id\":\"smoke\",\"name\":\"Smoke\",\"root\":{\"split\":\"row\","
                 "\"ratio\":0.5,\"a\":{\"camera\":{\"device\":\"%s\",\"channel\":0}},"
                 "\"b\":{\"camera\":null}}}",
                 key);
        if (nv_layout_put(core, json) != NV_OK) {
            problem("nv_layout_put");
        }
        if (nv_layouts_get(core, NULL, 0, &len) != NV_ERR_BUFFER_TOO_SMALL || len == 0 ||
            (all = malloc(len + 1)) == NULL || nv_layouts_get(core, all, len + 1, &len) != NV_OK ||
            strstr(all, "\"smoke\"") == NULL) {
            problem("nv_layouts_get");
        }
        free(all);
        if (nv_layout_delete(core, "smoke") != NV_OK ||
            nv_layout_delete(core, "smoke") != NV_ERR_UNKNOWN_ID) {
            problem("nv_layout_delete");
        }
        recordings(core, key, work);
        phase("recordings");
        playback(core, key);
        phase("playback");
        capture(core, tile, work);
        phase("capture");
        alerts(core, device);
        phase("alerts");
    }

    if (nv_tile_close(core, tile) != NV_OK) {
        problem("nv_tile_close failed");
    }
    phase("ptz-etc");
    unsigned long actions_at_close = atomic_load(&g_decodes) + atomic_load(&g_configures);
    sleep_ms(200);
    if (atomic_load(&g_decodes) + atomic_load(&g_configures) != actions_at_close) {
        problem("on_action after nv_tile_close returned");
    }
    if (nv_tile_close(core, tile) != NV_ERR_UNKNOWN_ID) {
        problem("second close was not NV_ERR_UNKNOWN_ID");
    }
    if (nv_tile_open(core, device, 0, (NvStreamTier)3, &bad_tile) != NV_ERR_INVALID_ARGUMENT) {
        problem("tier 3 was accepted");
    }
    if (nv_device_remove(core, device) != NV_OK) {
        problem("nv_device_remove failed");
    }
    /* Rendering stops before the core is freed. */
    atomic_store(&g_stop_render, true);
    pthread_join(renderer, NULL);

    NvTileLayout layout[2];
    NvSize sizes[2] = {{1920, 1080}, {0, 0}};
    if (nv_grid_layout(2, (NvSize){800, 300}, 2, sizes, false, layout) != NV_OK ||
        layout[0].dest.width == 0 || layout[1].source.width != 0) {
        problem("nv_grid_layout");
    }

    /* Discovery (minor 1): bad requests are refused; a search on loopback
     * only (nothing answers there) starts and stops, and a search still
     * running when the core is freed ends with it. Both stay on loopback:
     * nothing goes out on the network, and closing a socket bound to some
     * other interfaces can take the system about a second (measured on
     * macOS), which the free would wait for. NULL (the defaults) is
     * covered by the Rust tests (tests/ffi.rs). */
    if (nv_discover_start(core, "{\"seconds\":0}") != NV_ERR_INVALID_ARGUMENT ||
        nv_discover_start(NULL, NULL) != NV_ERR_INVALID_ARGUMENT ||
        nv_discover_stop(core) != NV_OK ||
        nv_discover_start(core, "{\"seconds\":30,\"interfaces\":[\"127.0.0.1\"],\"onvif\":false}") !=
            NV_OK ||
        nv_discover_stop(core) != NV_OK ||
        nv_discover_start(core, "{\"seconds\":30,\"interfaces\":[\"127.0.0.1\"]}") != NV_OK) {
        problem("discovery start/stop");
    }

    /* Updates (minor 2): the pure helpers, refused requests, and a check
     * with no trusted key (it starts and reports the failure as an event;
     * nothing is fetched), then cancel. */
    {
        char out[256];
        size_t out_len = 0;
        if (nv_update_parse_key("not a key", out, sizeof out, &out_len) != NV_ERR_INVALID_ARGUMENT ||
            nv_update_parse_source("http://updates.example.com/", out, sizeof out, &out_len) != NV_OK ||
            strstr(out, "insecure_source") == NULL ||
            nv_update_parse_source("https://updates.example.com/u", NULL, 0, &out_len) !=
                NV_ERR_BUFFER_TOO_SMALL ||
            out_len == 0 || nv_update_check(core, "{") != NV_ERR_INVALID_ARGUMENT ||
            nv_update_cancel(core) != NV_OK ||
            nv_update_check(core, "{\"source\":\"https://updates.example.com/u\",\"keys\":[],"
                                  "\"current\":\"0.3.0\"}") != NV_OK ||
            nv_update_cancel(core) != NV_OK) {
            problem("update helpers and check");
        }
    }

    phase("close-etc");
    nv_core_free(core);
    unsigned long after_free = atomic_load(&g_callbacks);
    sleep_ms(200);
    if (atomic_load(&g_callbacks) != after_free) {
        problem("callback after nv_core_free returned");
    }
    nv_core_free(NULL);
    phase("free");

    printf("smoke: configures=%lu decodes=%lu answered=%lu events=%lu tile_closed=%lu "
           "renders=%lu audible_renders=%lu tap_blocks=%lu recordings_changed=%lu "
           "thumbnails_ready=%lu downloads_done=%lu trashed=%lu\n",
           atomic_load(&g_configures), atomic_load(&g_decodes), decoded,
           atomic_load(&g_events), atomic_load(&g_tile_closed), atomic_load(&g_renders),
           atomic_load(&g_loud), atomic_load(&g_taps), atomic_load(&g_rec_changed),
           atomic_load(&g_thumb_ready), atomic_load(&g_download_done),
           atomic_load(&g_download_trashed));
    if (atomic_load(&g_renders) < 20) {
        problem("the render thread barely ran");
    }
    if (decoded < 50) {
        problem("fewer than 50 decodes in 10 s");
    }
    if (atomic_load(&g_configures) != 2) {
        problem("expected exactly one Configure per generation");
    }
    if (atomic_load(&g_tile_closed) - atomic_load(&g_pb_tile_closed) != 1) {
        problem("expected exactly one tile_closed");
    }
    if (atomic_load(&g_problems) != 0) {
        return 1;
    }
    printf("smoke: ok\n");
    return 0;
}
