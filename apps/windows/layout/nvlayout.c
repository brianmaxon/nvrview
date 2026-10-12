/*
 * Prints what the C compiler makes of nvrview.h: NV_ABI_VERSION and
 * NV_ABI_MINOR, the size of every struct, the offset of every field, and
 * every enum constant. The C# smoke test (NvLayoutCheck) compares its
 * declarations with this output, so the bindings cannot drift from the
 * header unnoticed. Build it with the compiler and target the app uses
 * (MSVC or clang on Windows x64).
 *
 * usage: nvlayout > layout.txt
 */
#include "nvrview.h"

#include <stddef.h>
#include <stdio.h>

#define SIZE(T) printf("size %s %u\n", #T, (unsigned)sizeof(T))
#define OFF(T, f) printf("offset %s.%s %u\n", #T, #f, (unsigned)offsetof(T, f))
#define VAL(c) printf("value %s %ld\n", #c, (long)(c))

int main(void) {
    printf("abi %d\n", NV_ABI_VERSION);
    printf("abi_minor %d\n", NV_ABI_MINOR);

    SIZE(NvConfig);
    OFF(NvConfig, log_dir);
    OFF(NvConfig, log_max_files);
    OFF(NvConfig, log_max_total_bytes);
    OFF(NvConfig, data_dir);

    SIZE(NvBytes);
    OFF(NvBytes, data);
    OFF(NvBytes, len);

    SIZE(NvSize);
    OFF(NvSize, width);
    OFF(NvSize, height);

    SIZE(NvRect);
    OFF(NvRect, x);
    OFF(NvRect, y);
    OFF(NvRect, width);
    OFF(NvRect, height);

    SIZE(NvColor);
    OFF(NvColor, matrix);
    OFF(NvColor, full_range);
    OFF(NvColor, ycbcr_to_rgb);

    SIZE(NvConfigure);
    OFF(NvConfigure, codec);
    OFF(NvConfigure, parameter_sets);
    OFF(NvConfigure, parameter_set_count);
    OFF(NvConfigure, coded_size);
    OFF(NvConfigure, visible_rect);
    OFF(NvConfigure, color);

    SIZE(NvDecode);
    OFF(NvDecode, access_unit);
    OFF(NvDecode, keyframe);
    OFF(NvDecode, pts_us);

    SIZE(NvAction);
    OFF(NvAction, kind);
    OFF(NvAction, configure);
    OFF(NvAction, decode);
    OFF(NvAction, status);
    OFF(NvAction, generation);

    SIZE(NvAudioBlock);
    OFF(NvAudioBlock, samples);
    OFF(NvAudioBlock, sample_count);
    OFF(NvAudioBlock, sample_rate);
    OFF(NvAudioBlock, epoch);
    OFF(NvAudioBlock, first_sample);
    OFF(NvAudioBlock, camera_time_us);
    OFF(NvAudioBlock, wall_clock_us);
    OFF(NvAudioBlock, has_camera_time);
    OFF(NvAudioBlock, has_wall_clock);
    OFF(NvAudioBlock, discontinuity);

    SIZE(NvCallbacks);
    OFF(NvCallbacks, context);
    OFF(NvCallbacks, on_action);
    OFF(NvCallbacks, on_event);
    OFF(NvCallbacks, on_audio_tap);

    SIZE(NvTileLayout);
    OFF(NvTileLayout, cell);
    OFF(NvTileLayout, dest);
    OFF(NvTileLayout, source);

    SIZE(NvThumbnail);
    OFF(NvThumbnail, kind);
    OFF(NvThumbnail, width);
    OFF(NvThumbnail, height);
    OFF(NvThumbnail, pts_us);

    VAL(NV_OK);
    VAL(NV_ERR_INVALID_ARGUMENT);
    VAL(NV_ERR_UNKNOWN_ID);
    VAL(NV_ERR_CLOSED);
    VAL(NV_ERR_LIMIT);
    VAL(NV_ERR_INTERNAL);
    VAL(NV_ERR_BUFFER_TOO_SMALL);
    VAL(NV_ERR_NOT_PERMITTED);
    VAL(NV_ERR_UNSUPPORTED);
    VAL(NV_ERR_NOT_CONFIGURED);
    VAL(NV_PENDING);
    VAL(NV_ACTION_CONFIGURE);
    VAL(NV_ACTION_DECODE);
    VAL(NV_ACTION_FLUSH);
    VAL(NV_ACTION_RESET);
    VAL(NV_ACTION_CLEAR);
    VAL(NV_ACTION_STATUS);
    VAL(NV_CODEC_H264);
    VAL(NV_CODEC_H265);
    VAL(NV_MATRIX_BT601);
    VAL(NV_MATRIX_BT709);
    VAL(NV_MATRIX_BT2020);
    VAL(NV_TILE_WAITING);
    VAL(NV_TILE_PLAYING);
    VAL(NV_TILE_RECOVERING);
    VAL(NV_TILE_UNDECODABLE);
    VAL(NV_TIER_MAIN);
    VAL(NV_TIER_SUB);
    VAL(NV_TIER_MOBILE);
    VAL(NV_TIER_EXTERN);
    VAL(NV_FAIL_INVALID_PARAMETER_SETS);
    VAL(NV_FAIL_SESSION_CREATE_FAILED);
    VAL(NV_FAIL_NOT_CONFIGURED);
    VAL(NV_FAIL_INVALID_ACCESS_UNIT);
    VAL(NV_FAIL_SUBMIT_FAILED);
    VAL(NV_FAIL_DECODE_ERROR);
    VAL(NV_FAIL_NO_PICTURE);
    VAL(NV_THUMB_H264_KEYFRAME);
    VAL(NV_THUMB_H265_KEYFRAME);
    VAL(NV_THUMB_IMAGE);
    VAL(NV_SEEK_KEYFRAME);
    VAL(NV_SEEK_EXACT);
    VAL(NV_PLAYBACK_BUFFERING);
    VAL(NV_PLAYBACK_PLAYING);
    VAL(NV_PLAYBACK_PAUSED);
    VAL(NV_PLAYBACK_ENDED);
    VAL(NV_PLAYBACK_FAILED);

    /* C enums are int-sized; the bindings declare them as 32-bit. */
    printf("size enum %u\n", (unsigned)sizeof(NvStatus));
    return 0;
}
