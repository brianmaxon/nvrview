/*
 * A C client of the nvrview core: opens one tile on a (mock) device, plays
 * it with a fake decoder that answers every Decode from inside the
 * callback, switches it to the main stream (a second generation), renders
 * its audio from a separate (audio-device-like) thread with a tap attached
 * and reads its level, stores a layout, and checks the shutdown guarantees. Built and run by
 * scripts/test-c-ffi.sh against a spawned mock-nvr.
 *
 * usage: smoke HOST PORT   (password from NVRVIEW_TEST_PASSWORD)
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
/* nv_tile_open writes the ID here, and only then lets actions run, so the
 * callback may read it without further synchronization. */
static uint64_t g_tile;

static void sleep_ms(long ms) {
    struct timespec ts = {ms / 1000, (ms % 1000) * 1000000L};
    nanosleep(&ts, NULL);
}

static void problem(const char *what) {
    fprintf(stderr, "smoke: %s\n", what);
    atomic_fetch_add(&g_problems, 1);
}

static void on_action(void *context, uint64_t tile, const NvAction *action) {
    (void)context;
    atomic_fetch_add(&g_callbacks, 1);
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

static void on_event(void *context, const char *json, size_t len) {
    (void)context;
    atomic_fetch_add(&g_callbacks, 1);
    atomic_fetch_add(&g_events, 1);
    if (len < 10 || json[0] != '{' || json[len - 1] != '}') {
        problem("event is not a JSON object");
    }
    static const char closed[] = "\"type\":\"tile_closed\"";
    for (size_t i = 0; i + sizeof closed - 1 <= len; i++) {
        if (memcmp(json + i, closed, sizeof closed - 1) == 0) {
            atomic_fetch_add(&g_tile_closed, 1);
            break;
        }
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

int main(int argc, char **argv) {
    if (argc != 3) {
        fprintf(stderr, "usage: smoke HOST PORT\n");
        return 2;
    }
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
    if (nv_core_new(NULL, cb, &core) != NV_OK || core == NULL) {
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

    for (int i = 0; i < 1000 && atomic_load(&g_answered) < 50; i++) {
        sleep_ms(10);
    }
    unsigned long decoded = atomic_load(&g_answered);

    /* Focus: the main stream starts as generation 1; generation 0 ends with
     * a Reset once generation 1 has shown a picture. */
    if (nv_tile_set_tier(core, tile, NV_TIER_MAIN) != NV_OK) {
        problem("nv_tile_set_tier failed");
    }
    for (int i = 0; i < 1000 && atomic_load(&g_gen_answered[1]) < 20; i++) {
        sleep_ms(10);
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
    }

    if (nv_tile_close(core, tile) != NV_OK) {
        problem("nv_tile_close failed");
    }
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
     * running when the core is freed ends with it. */
    if (nv_discover_start(core, "{\"seconds\":0}") != NV_ERR_INVALID_ARGUMENT ||
        nv_discover_start(NULL, NULL) != NV_ERR_INVALID_ARGUMENT ||
        nv_discover_stop(core) != NV_OK ||
        nv_discover_start(core, "{\"seconds\":30,\"interfaces\":[\"127.0.0.1\"],\"onvif\":false}") !=
            NV_OK ||
        nv_discover_stop(core) != NV_OK || nv_discover_start(core, NULL) != NV_OK) {
        problem("discovery start/stop");
    }

    nv_core_free(core);
    unsigned long after_free = atomic_load(&g_callbacks);
    sleep_ms(200);
    if (atomic_load(&g_callbacks) != after_free) {
        problem("callback after nv_core_free returned");
    }
    nv_core_free(NULL);

    printf("smoke: configures=%lu decodes=%lu answered=%lu events=%lu tile_closed=%lu "
           "renders=%lu audible_renders=%lu tap_blocks=%lu\n",
           atomic_load(&g_configures), atomic_load(&g_decodes), decoded,
           atomic_load(&g_events), atomic_load(&g_tile_closed), atomic_load(&g_renders),
           atomic_load(&g_loud), atomic_load(&g_taps));
    if (atomic_load(&g_renders) < 20) {
        problem("the render thread barely ran");
    }
    if (decoded < 50) {
        problem("fewer than 50 decodes in 10 s");
    }
    if (atomic_load(&g_configures) != 2) {
        problem("expected exactly one Configure per generation");
    }
    if (atomic_load(&g_tile_closed) != 1) {
        problem("expected exactly one tile_closed");
    }
    if (atomic_load(&g_problems) != 0) {
        return 1;
    }
    printf("smoke: ok\n");
    return 0;
}
