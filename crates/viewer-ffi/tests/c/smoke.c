/*
 * A C client of the nvrview core: opens one tile on a (mock) device, plays
 * it with a fake decoder that answers every Decode from inside the
 * callback, and checks the shutdown guarantees. Built and run by
 * scripts/test-c-ffi.sh against a spawned mock-nvr.
 *
 * usage: smoke HOST PORT   (password from NVRVIEW_TEST_PASSWORD)
 */
/* nanosleep is POSIX; strict C11 on glibc hides it without this. */
#define _POSIX_C_SOURCE 200809L

#include "nvrview.h"

#include <stdatomic.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

static NvCore *_Atomic g_core;
static atomic_ulong g_configures, g_decodes, g_answered, g_events, g_callbacks;
static atomic_ulong g_problems, g_tile_closed;
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
        if (nv_tile_decoded(atomic_load(&g_core), tile, d->pts_us) == NV_OK) {
            atomic_fetch_add(&g_answered, 1);
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

    NvCallbacks cb = {NULL, on_action, on_event};
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

    for (int i = 0; i < 1000 && atomic_load(&g_answered) < 50; i++) {
        sleep_ms(10);
    }
    unsigned long decoded = atomic_load(&g_answered);

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

    NvTileLayout layout[2];
    NvSize sizes[2] = {{1920, 1080}, {0, 0}};
    if (nv_grid_layout(2, (NvSize){800, 300}, 2, sizes, false, layout) != NV_OK ||
        layout[0].dest.width == 0 || layout[1].source.width != 0) {
        problem("nv_grid_layout");
    }

    nv_core_free(core);
    unsigned long after_free = atomic_load(&g_callbacks);
    sleep_ms(200);
    if (atomic_load(&g_callbacks) != after_free) {
        problem("callback after nv_core_free returned");
    }
    nv_core_free(NULL);

    printf("smoke: configures=%lu decodes=%lu answered=%lu events=%lu tile_closed=%lu\n",
           atomic_load(&g_configures), atomic_load(&g_decodes), decoded,
           atomic_load(&g_events), atomic_load(&g_tile_closed));
    if (decoded < 50) {
        problem("fewer than 50 decodes in 10 s");
    }
    if (atomic_load(&g_configures) != 1) {
        problem("expected exactly one Configure");
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
