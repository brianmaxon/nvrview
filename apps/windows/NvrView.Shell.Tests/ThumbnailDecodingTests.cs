using NvrView.Interop;

namespace NvrView.Shell.Tests;

/// <summary>
/// The platform-independent parts of the cover pictures: sizing, the NV12 conversion and shrinking, and the
/// bounded decode queue (docs/UX.md "Recordings", Thumbnails). The queue is stepped by hand (RunNext), so
/// nothing here waits on a clock; the one test with the worker thread waits on its callbacks.
/// </summary>
internal static class ThumbnailDecodingTests
{
    private static RecordedThumbnail Keyframe(long pts) => new(NvThumbKind.H264Keyframe, 320, 180, pts, [0, 0, 0, 1, 0x65]);

    private static RecordedThumbnail Small(long pts) => new(NvThumbKind.Image, 4, 4, pts, [0xff, 0xd8, 0xff, 0xd9]);

    // A pitch-padded NV12 picture: luma rows of `pitch` bytes, then chroma; each half of the width one color.
    private static byte[] TwoColors(int width, int height, int pitch, (byte Y, byte U, byte V) left, (byte Y, byte U, byte V) right)
    {
        var data = new byte[pitch * height * 3 / 2];
        Array.Fill(data, (byte)0xEE);
        for (int y = 0; y < height; y++)
        {
            for (int x = 0; x < width; x++)
            {
                data[y * pitch + x] = x < width / 2 ? left.Y : right.Y;
            }
        }
        for (int y = 0; y < height / 2; y++)
        {
            for (int x = 0; x < width; x += 2)
            {
                (byte Y, byte U, byte V) c = x < width / 2 ? left : right;
                data[pitch * height + y * pitch + x] = c.U;
                data[pitch * height + y * pitch + x + 1] = c.V;
            }
        }
        return data;
    }

    private static void Near((byte R, byte G, byte B) expected, byte[] bgr, int index, int tolerance, string what)
    {
        (byte R, byte G, byte B) got = (bgr[index * 3 + 2], bgr[index * 3 + 1], bgr[index * 3]);
        T.Check(Math.Abs(got.R - expected.R) <= tolerance && Math.Abs(got.G - expected.G) <= tolerance && Math.Abs(got.B - expected.B) <= tolerance,
            $"{what}: expected {expected}, got {got}");
    }

    public static void Run()
    {
        T.Run("thumbnails: sizes keep the aspect and never enlarge", () =>
        {
            T.Equal((320, 180), ThumbnailSizing.Fit(1920, 1080));
            T.Equal((320, 180), ThumbnailSizing.Fit(480, 270));
            T.Equal((320, 240), ThumbnailSizing.Fit(320, 240));
            T.Equal((192, 96), ThumbnailSizing.Fit(192, 96));
            T.Equal((180, 320), ThumbnailSizing.Fit(1080, 1920));
            T.Equal((320, 1), ThumbnailSizing.Fit(6400, 1));
            T.Equal((0, 0), ThumbnailSizing.Fit(0, 100));
        });

        T.Run("thumbnails: NV12 converts with the matrix and range, and the padding is ignored", () =>
        {
            // Video range BT.601: red is Y 81, U 90, V 240; blue is Y 41, U 240, V 110.
            byte[] a = TwoColors(8, 4, 16, (81, 90, 240), (41, 240, 110));
            byte[] bgr = Nv12Scaler.ToBgr24(a, 16, 16 * 4, 8, 4, 8, 4, YuvMatrix.Bt601, fullRange: false);
            T.Equal(8 * 4 * 3, bgr.Length);
            Near((254, 0, 0), bgr, 1, 4, "red");
            Near((0, 0, 255), bgr, 6, 5, "blue");
            // Full range BT.709: Y 128, U 128, V 128 is mid gray; Y 255 is white.
            byte[] b = TwoColors(8, 4, 16, (128, 128, 128), (255, 128, 128));
            byte[] gray = Nv12Scaler.ToBgr24(b, 16, 16 * 4, 8, 4, 8, 4, YuvMatrix.Bt709, fullRange: true);
            Near((128, 128, 128), gray, 0, 1, "gray");
            Near((255, 255, 255), gray, 7, 1, "white");
            T.Equal(YuvMatrix.Bt601, Nv12Scaler.GuessMatrix(480));
            T.Equal(YuvMatrix.Bt709, Nv12Scaler.GuessMatrix(1080));
        });

        T.Run("thumbnails: shrinking averages the pixels each one covers", () =>
        {
            // 64 x 8 with black on the left half and white on the right (video range), shrunk to 4 x 1 and 1 x 1.
            byte[] a = TwoColors(64, 8, 64, (16, 128, 128), (235, 128, 128));
            byte[] four = Nv12Scaler.ToBgr24(a, 64, 64 * 8, 64, 8, 4, 1, YuvMatrix.Bt709, fullRange: false);
            Near((0, 0, 0), four, 0, 1, "left");
            Near((0, 0, 0), four, 1, 1, "left of center");
            Near((255, 255, 255), four, 2, 1, "right of center");
            Near((255, 255, 255), four, 3, 1, "right");
            byte[] one = Nv12Scaler.ToBgr24(a, 64, 64 * 8, 64, 8, 1, 1, YuvMatrix.Bt709, fullRange: false);
            Near((128, 128, 128), one, 0, 3, "average");
            // A visible area smaller than the decoded one (padding rows and columns) leaves the padding out.
            byte[] cropped = Nv12Scaler.ToBgr24(a, 64, 64 * 8, 32, 8, 1, 1, YuvMatrix.Bt709, fullRange: false);
            Near((0, 0, 0), cropped, 0, 1, "cropped");
            bool refused = false;
            try
            {
                _ = Nv12Scaler.ToBgr24(new byte[10], 64, 64 * 8, 64, 8, 4, 1, YuvMatrix.Bt709, fullRange: false);
            }
            catch (ArgumentException)
            {
                refused = true;
            }
            T.Check(refused, "a picture that does not fit its sizes is refused");
        });

        T.Run("thumbnails: the queue runs the newest request first and drops the oldest when full", () =>
        {
            var ran = new List<long>();
            var answers = new List<(long Pts, RecordedThumbnail? Answer)>();
            using var queue = new ThumbnailDecodeQueue(k =>
            {
                ran.Add(k.PtsUs);
                return Small(k.PtsUs);
            }, capacity: 3, background: false);
            for (long pts = 1; pts <= 4; pts++)
            {
                long p = pts;
                queue.Submit(Keyframe(p), small => answers.Add((p, small)));
            }
            // The fourth request pushed the first out.
            T.Equal(3, queue.Count);
            T.Equal(1, answers.Count);
            T.Check(answers[0].Pts == 1 && ReferenceEquals(answers[0].Answer, ThumbnailDecodeQueue.Canceled));
            T.Check(queue.RunNext() && queue.RunNext() && queue.RunNext() && !queue.RunNext());
            T.Check(ran.SequenceEqual([4L, 3L, 2L]), "newest first");
            T.Check(answers.Skip(1).Select(a => a.Pts).SequenceEqual([4L, 3L, 2L]) && answers.Skip(1).All(a => a.Answer is { Kind: NvThumbKind.Image }));
        });

        T.Run("thumbnails: canceling answers every waiting request once, and a failing decode answers null", () =>
        {
            var errors = new List<string>();
            var answers = new List<RecordedThumbnail?>();
            using var queue = new ThumbnailDecodeQueue(k => k.PtsUs == 9 ? throw new InvalidOperationException("no decoder") : null,
                background: false, error: errors.Add);
            queue.Submit(Keyframe(1), answers.Add);
            queue.Submit(Keyframe(2), answers.Add);
            T.Equal(2, queue.CancelQueued());
            T.Equal(0, queue.Count);
            T.Check(answers.Count == 2 && answers.All(a => ReferenceEquals(a, ThumbnailDecodeQueue.Canceled)));
            T.Equal(0, queue.CancelQueued());
            answers.Clear();
            queue.Submit(Keyframe(9), answers.Add);
            queue.Submit(Keyframe(5), answers.Add);
            T.Check(queue.RunNext() && queue.RunNext());
            T.Check(answers.Count == 2 && answers.All(a => a is null), "a picture that does not decode, and one that throws, are answered null");
            T.Check(errors.Count == 1 && errors[0].Contains("no decoder", StringComparison.Ordinal));
            queue.Dispose();
            var late = new List<RecordedThumbnail?>();
            queue.Submit(Keyframe(3), late.Add);
            T.Check(late.Count == 1 && ReferenceEquals(late[0], ThumbnailDecodeQueue.Canceled), "a disposed queue drops new requests");
        });

        T.Run("thumbnails: the last keyframe of a stream is cut out with its parameter sets", () =>
        {
            byte[] sc = [0, 0, 0, 1];
            // H.264: SPS, PPS, IDR, P, SPS, PPS, IDR, IDR (second slice), P.
            byte[] stream = [.. sc, 0x67, 1, .. sc, 0x68, 2, .. sc, 0x65, 3, .. sc, 0x41, 4,
                .. sc, 0x67, 5, .. sc, 0x68, 6, .. sc, 0x65, 7, .. sc, 0x65, 8, .. sc, 0x41, 9];
            byte[] key = AnnexBKeyframes.Last(stream, h265: false);
            T.Check(key.SequenceEqual([.. sc, 0x67, 5, .. sc, 0x68, 6, .. sc, 0x65, 7, .. sc, 0x65, 8]));
            T.Equal(0, AnnexBKeyframes.Last([.. sc, 0x41, 4], h265: false).Length);
            // H.265: VPS (32), SPS (33), IDR_W_RADL (19), TRAIL_R (1).
            byte[] h265 = [.. sc, 0x40, 1, 1, .. sc, 0x42, 1, 2, .. sc, 0x26, 1, 3, .. sc, 0x02, 1, 4];
            T.Check(AnnexBKeyframes.Last(h265, h265: true).SequenceEqual(h265[..^7]));
        });

        T.Run("thumbnails: the worker thread decodes off the caller's thread", () =>
        {
            int caller = Environment.CurrentManagedThreadId;
            int worker = -1;
            using var done = new ManualResetEventSlim();
            RecordedThumbnail? answer = null;
            using var queue = new ThumbnailDecodeQueue(k =>
            {
                worker = Environment.CurrentManagedThreadId;
                return Small(k.PtsUs);
            });
            queue.Submit(Keyframe(7), small =>
            {
                answer = small;
                done.Set();
            });
            T.Check(done.Wait(TimeSpan.FromSeconds(60)), "the worker answered");
            T.Check(worker != caller && worker != -1);
            T.Equal(7L, answer!.PtsUs);
        });
    }
}
