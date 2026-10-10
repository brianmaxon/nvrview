# On-device vision

How good and how cheap on-device detection of people, vehicles, and animals
is on a Mac, for cameras that have no AI of their own, measured before the
frame tap ([ARCHITECTURE.md](ARCHITECTURE.md#extension-points)) is built.
Cameras and NVRs that do run their own AI already push `people`, `vehicle`
and `dog_cat` events ([protocol.md](protocol.md#detection-events)); this is
for the rest, and for what the device does not detect.

Measured with `tools/vision-lab` on one real recording, kept on the
developer's machine and never committed. Only aggregate numbers appear here.

## Summary

- **Cheap enough to run on every camera.** On the Neural Engine, Vision's
  human and animal detectors together cost about **1.2–1.6 % of one CPU
  core per camera at 2 pictures per second**, about 45 MB per process, with
  13–16 ms per picture. Sixteen cameras at 2 fps used about 20 % of one core
  in total (of 14), with no GPU use.
- **Keep everything on the Neural Engine.** The same models on the GPU cost
  as little CPU but kept the GPU 57–80 % busy (it also draws the tiles) and
  took 170–370 MB more memory; on the CPU alone they cost 8–10 times as much
  CPU.
- **Vision's built-ins were the cleanest.** On 600 labeled pictures with
  nothing to detect (including about a minute of abrupt exposure
  changes), `VNDetectHumanRectanglesRequest` and
  `VNRecognizeAnimalsRequest` raised **no false detections**. A COCO object
  detector is needed for vehicles: YOLOv3 raised none either; YOLOv3-Tiny one
  tiny person box; RT-DETR (the permissively licensed modern option) fired
  "person" on a static object in 72 % of pictures at 0.5 confidence and in
  none at 0.7.
- **Recall on real stills is poor for Vision, high for small vision-language
  models; RT-DETR with tiles is the cheap middle.** On 400 hand-labeled
  stills (see "Recall on labeled stills" and "RT-DETR on the labeled
  stills"),
  Vision's human detector raised no false alarm but found only about 35 % of
  people; gemma4 (e4b) found 99 % of people and 97 % of vehicles with 6 % and
  10 % false alarms; ministral-3 (8B) was the precise one (0.4 % and 2 %)
  but missed about a quarter of people. Event-level recall on video is still
  not measured.

## Method

`tools/vision-lab` is a standalone command-line Swift package (not part of
the app):

1. Reads an Annex-B `.h264`/`.h265` file, groups NAL units into pictures, and
   decodes every picture with VideoToolbox, hardware decoder required
   (confirmed through `UsingHardwareAcceleratedVideoDecoder`). Output is
   IOSurface-backed NV12 (`420v`), the decoder's native format.
2. Samples pictures the way a frame tap would: the first picture of every
   1/R second of stream time (`--fps R`), or an explicit list for scoring.
3. Hands the decoder's `CVPixelBuffer` straight to one
   `VNImageRequestHandler` per picture shared by all detectors: no copy or
   conversion in our code. Vision scales and converts it to each model's input.
4. Records per picture and per detector the latency (wall time of
   `perform`), the detections, and over the run: process CPU time
   (`getrusage`), physical footprint and its peak (`task_info`), GPU
   "Device Utilization %" sampled every 250 ms from the IOAccelerator
   registry entry (system-wide; no privileges), and the CPU time of the
   helper processes doing work for us (`VTDecoderXPCService`, `aned`, read
   with `ps`; system-wide).
5. `vision-lab plan` asks Core ML where it intends to run each operation of a
   model (`MLComputePlan`), which shows Neural Engine placement without
   `powermetrics` (that needs root, so energy and Neural Engine busy time
   were not measured).
6. `vision-lab eval` scores results against a label file
   (`frame,person,vehicle,animal`, 0/1): picture-level precision, recall and
   false-positive rate per detector, class and threshold; and event-level
   recall (labeled pictures closer than 4 s form one event; an event counts
   as detected when any sampled picture inside it fires), false episodes per
   hour, and time to first detection.

Input: one 10-minute 640x480 H.264 sub-stream recording, 2644 pictures (67
keyframes), about 4.4 pictures per second on average. Annex-B carries no
timestamps, so pictures are assumed evenly spaced; the camera's on-screen
clock showed that the real spacing varies by a few hundred milliseconds,
which only shifts which picture a sample takes.

Machine: Apple M3 Max (14 CPU cores), macOS 26.7, Swift 6.4, release build.
Other work was running on the machine during all runs (load average 6–27,
briefly higher), so latencies are pessimistic and noisy; CPU time per process
is affected much less.

Two kinds of run:

- **Offline**: decode as fast as possible and run one configuration on every
  picture (2644 inferences), to get the cost of one inference.
- **Real time**: decode at the source rate for 120 s, as a live tile does,
  sample at 1 or 2 fps, and run 1 or 16 such processes at once to see
  steady-state CPU per camera and contention for the Neural Engine. Each
  process loads its own model copy; an app would share one, so the memory
  figures are per-process upper bounds.

## Models and licenses

| Detector | Classes | Where it comes from | License of code and weights |
| --- | --- | --- | --- |
| `VNDetectHumanRectanglesRequest` (whole body, or upper body) | person | macOS (Vision) | part of macOS; nothing to distribute |
| `VNRecognizeAnimalsRequest` | cat, dog | macOS (Vision) | part of macOS |
| `VNDetectFaceRectanglesRequest` | face (cost reference only) | macOS (Vision) | part of macOS |
| `VNClassifyImageRequest` | whole-picture labels (1303), mapped: people/adult/child/…, car/truck/bus/…, dog/cat/animal/… | macOS (Vision) | part of macOS |
| YOLOv3-Tiny FP16 (17.8 MB), YOLOv3 FP16 (124 MB) | 80 COCO classes, with non-maximum suppression in the model | Core ML conversions from Apple's model gallery (developer.apple.com/machine-learning/models) | Model metadata: "YOLO License" of the original Darknet project, which places Darknet in the public domain. Apple's download page states no license for its conversion. Weights trained on COCO (annotations CC BY 4.0). |
| RT-DETR R18 (rtdetr_r18vd), 640x640, FP16 | 80 COCO classes | converted locally by `tools/vision-lab/scripts/convert-rtdetr.py` from the Hugging Face release (about 80 MB download) | Apache-2.0 for the original code and for the released weights (model card); trained on COCO |

AGPL-3.0 models (the Ultralytics YOLOv5/v8/v11 family and their weights) were
not considered: they cannot ship alongside MIT OR Apache-2.0 code.

Notes on the Core ML models:

- Models are never committed. They live in the git-ignored `captures/`
  directory; vision-lab compiles each once next to itself.
- For distribution, RT-DETR is the cleanest: Apache-2.0 on both code and
  weights, and we produce the Core ML file ourselves. YOLOv3's weights are
  public domain, but the converted file we measured came from Apple's gallery
  without stated terms; to ship YOLOv3 we would convert the original Darknet
  weights ourselves.
- The RT-DETR conversion needed two changes, both in the script: a fixed
  anchor size (`anchor_image_size`), and a batch-size-1 version of the
  multi-scale deformable attention that keeps every tensor at rank 5 or less
  (Core ML's limit). The script checks the patched model against the
  original before converting (outputs within 1e-3), and the Core ML model's
  detections matched PyTorch's on the same input to two decimals, for FP16
  and FP32 alike.
- `MLComputePlan` placed every neural network operation of both YOLOv3
  models and RT-DETR on the Neural Engine with `cpu-ane` (YOLOv3's
  non-maximum suppression and RT-DETR's 10 bookkeeping operations on the CPU).
  With `all`, RT-DETR was split between the Neural Engine (432 operations) and
  the GPU (73), and was slower than with `cpu-ane`.

## Results

### Cost of one inference (offline, every picture, 640x480 input)

Latency is the wall time of one `perform`, including Vision's scaling and
color conversion. CPU is this process's CPU time per picture. Memory is the
process's peak footprint (decoding alone: 27 MB).

| Detector | Compute | Latency p50 / p90 (ms) | CPU per picture (ms) | Peak memory (MB) | GPU busy |
| --- | --- | --- | --- | --- | --- |
| decode only | VideoToolbox | 0.37 / 1.4 (decode) | 0.06 (+0.14 in the decoder service) | 27 | — |
| human | auto (Neural Engine) | 5.1 / 9.0 | 2.3 | 46 | ~1 % |
| human | Neural Engine | 4.9 / 5.3 | 2.0 | 44 | ~1 % |
| human | GPU | 7.9 / 10.3 | 2.5 | 220 | 57 % |
| human | CPU | 17.7 / 18.7 | 22.5 | 83 | ~2 % |
| human, upper body | auto | 4.9 / 5.0 | 1.8 | 46 | — |
| animal | auto (Neural Engine) | 4.9 / 6.3 | 2.2 | 44 | ~1 % |
| animal | GPU | 8.0 / 10.2 | 2.4 | 216 | 60 % |
| animal | CPU | 17.6 / 18.1 | 22.2 | 83 | ~1 % |
| face | auto | 4.9 / 5.2 | 1.9 | 44 | — |
| scene classification | auto | 9.8 / 10.6 | 4.2 | 34 | — |
| YOLOv3-Tiny | all | 6.1 / 9.1 | 3.5 | 54 | 2 % |
| YOLOv3-Tiny | CPU + Neural Engine | 7.3 / 9.5 | 4.2 | 54 | ~1 % |
| YOLOv3-Tiny | CPU + GPU | 9.0 / 12.6 | 2.8 | 320 | 60 % |
| YOLOv3-Tiny | CPU | 10.6 / 16.4 | 16.7 | 75 | ~1 % |
| YOLOv3 | CPU + Neural Engine | 18.2 / 20.0 | 8.4 | 104 | ~1 % |
| YOLOv3 | CPU + GPU | 14.0 / 15.1 | 2.4 | 411 | 78 % |
| RT-DETR R18 | CPU + Neural Engine | 12.8 / 14.5 | 4.9 | 65 | 3 % |
| RT-DETR R18 | all (Neural Engine + GPU) | 18.8 / 21.0 | 7.2 | 237 | 27 % |
| RT-DETR R18 | CPU + GPU | 13.7 / 29.0 | 6.9 | 321 | 80 % |
| human + animal + YOLOv3-Tiny | auto / all | 15.4 / 17.3 | 7.5 | 70 | ~1 % |

First-load cost: compiling a model for the Neural Engine took up to 2.3 s
(RT-DETR) the first time and under 0.15 s once cached by the system.

### Steady state per camera (real time, 120 s)

Per process (one camera each). CPU is percent of one core, including
decoding every picture of the stream.

| Configuration | Cameras | CPU per camera | All cameras | Latency p50 / p90 / p99 (ms) | Memory per process (MB) |
| --- | --- | --- | --- | --- | --- |
| decode only | 1 | 0.2 % | 0.2 % | — | 26 |
| decode only | 16 | 0.2 % | 3.3 % | — | 26 |
| human + animal, 2 fps | 1 | 1.6 % | 1.6 % | 13 / 17 / 23 | 45 |
| human + animal, 2 fps | 16 | 1.2 % | 20 % | 16 / 21 / 31 | 44 |
| human + animal + YOLOv3-Tiny, 2 fps | 1 | 3.1 % | 3.1 % | 25 / 32 / 43 | 69 |
| human + animal + YOLOv3-Tiny, 2 fps | 16 | 1.8 % | 30 % | 36 / 46 / 71 | 65 |
| human + animal + YOLOv3-Tiny, 1 fps | 16 | 1.1 % | 18 % | 26 / 34 / 51 | 63 |
| YOLOv3, 2 fps | 16 | 2.1 % | 33 % | 73 / 86 / 133 | 104 |
| RT-DETR R18 on the Neural Engine, 2 fps | 16 | 1.25 % | 20 % | 117 / 127 / 150 | 57 |

- The hardware decoder's service used about 5 s of CPU per 120 s for 16
  sub-streams (about 0.3 % of a core per stream), system-wide.
- GPU utilization stayed at the idle level in every Neural Engine run.
- `aned` showed almost no CPU time: Neural Engine requests are made from the
  calling process, so the process CPU numbers above already include them.
- Real-time latency is higher than offline latency for the same model: the
  hardware idles between pictures and has to ramp up again. With 16 cameras
  the Neural Engine queues requests: 16 cameras of RT-DETR at 2 fps
  (32 inferences per second) took 117 ms each at the median, but kept up.
  Every sampled picture was processed in all runs.

### Accuracy

Ground truth: one picture per second for the whole recording (600
pictures), each checked by eye and labeled for person, vehicle and animal.
None was present in any picture. A person, vehicle, or animal that stayed
still the whole time would also have been seen, and none was.

Picture-level false positives (fraction of the 600 pictures in which the
detector reported the class at or above the threshold) and false episodes
(detections merged like events) at 1 fps:

| Detector | Threshold | Person | Vehicle | Animal | False episodes per hour |
| --- | --- | --- | --- | --- | --- |
| Vision human (body and upper body) | its own | 0 | — | — | 0 |
| Vision animals | its own | — | — | 0 | 0 |
| Vision scene classification | 0.3 | 0.8 % | 0.7 % | 0 | 24 (person 18, vehicle 6) |
| Vision scene classification | 0.5 | 0 | 0 | 0 | 0 |
| YOLOv3-Tiny | 0.3–0.5 | 0.17 % (one box of 1 % x 5 % of the picture) | 0 | 0 | 6 |
| YOLOv3 | 0.1 and up | 0 | 0 | 0 | 0 |
| RT-DETR R18 | 0.5 | 72.5 % (one static object) | 0.7 % | 0 | 96 |
| RT-DETR R18 | 0.7 | 0 | 0 | 0 | 0 |

The scene classifier's and YOLOv3-Tiny's few false positives fell in the
seconds of abrupt exposure change. RT-DETR's came from one stationary object
it took for a person in most pictures.

Not measured: recall (picture-level and event-level) and time to first
detection, because there were no events to detect.

### Recall on labeled stills

Ground truth: 400 stills (1280x720 or larger) sampled, stratified by day and
night, from a large archive of motion-triggered snapshots of real security
cameras, each labeled by hand for whether a person, a vehicle (parked
ones count), or an animal is present: 135 with a person (33 at night), 200
with a vehicle (79 at night), none with an animal, 181 at night in all. The
pictures and per-picture results stay on the developer's machine.

Recall (share of labeled pictures found), false-positive rate (share of
pictures without the class where it was reported), overall / day / night, in
percent:

| Detector | Person recall | Person false positives | Vehicle recall | Vehicle false positives |
| --- | --- | --- | --- | --- |
| Vision human rectangles (confidence ≥ 0.5) | 35 / 32 / 42 | 0 / 0 / 0 | — | — |
| Vision scene classification, person labels ≥ 0.3 | 39 / 38 / 42 | 1.5 / 2.6 / 0.7 | — | — |
| Vision scene classification, vehicle labels ≥ 0.1 | — | — | 72 / 73 / 71 | 3.5 / 6.1 / 1.0 |
| Vision scene classification, vehicle labels ≥ 0.3 | — | — | 51 / 55 / 44 | 0 / 0 / 0 |
| ministral-3 8B (Ollama, temperature 0) | 73 / 71 / 82 | 0.4 / 0.9 / 0 | 85 / 80 / 91 | 2.0 / 2.0 / 2.0 |
| gemma4 e4b (Ollama, temperature 0) | 99 / 100 / 97 | 6.0 / 2.6 / 8.8 | 97 / 97 / 96 | 10.5 / 13.3 / 7.8 |
| qwen3-vl 4B (206-picture shuffled subset; 9 % unusable answers excluded) | 97 / 98 / 94 | 5.2 / 2.0 / 7.7 | 98 / 96 / 100 | 15.3 / 17.0 / 13.3 |

The language models were asked one fixed question per picture (which of
person, vehicle, animal is present, answered as JSON). Animal false
positives (no animal in any picture): Vision 0, ministral 10, gemma4 4,
qwen3-vl 2 of 206.

- **Vision misses people in daylight too.** Its recall is no better by day
  than by night, so lighting is not the cause; small, distant figures in
  large frames are the likely one (cropping or tiling untested). Lowering
  thresholds trades badly: scene classification at 0.05 finds 80 % of people
  with 42 % false positives.
- **The language models find nearly everything but cost seconds.** Median
  time per picture on a shared machine (load average 9–15 from other work,
  so pessimistic): gemma4 3.0 s (about 18 GB of GPU memory), ministral 5.8 s
  (about 7 GB), qwen3-vl 20 s (its reasoning cannot be turned off, and 9 % of
  answers ran out of tokens). Vision took 12–14 ms per request on the Neural
  Engine.
- **They do not combine as a confirmation.** ministral never said "present"
  when gemma4 said "absent", so gemma4 AND ministral equals ministral, and
  gemma4 OR ministral equals gemma4. gemma4 AND Vision is limited by Vision's
  recall (person 62 % at 2.6 % false positives).
- **Caveats.** Labels mark presence, not whether a notification is
  deserved: a parked vehicle counts, and some vehicle "false positives" may
  be small or ambiguous objects the labeler left unmarked. One prompt per
  model, no confidences (booleans only), no localization scored.

### RT-DETR on the labeled stills

The same 400 stills through RT-DETR R18 and R50 (converted by
`convert-rtdetr.py`, run on the Neural Engine with `cpuAndNeuralEngine`).
Person is COCO person; vehicle is car, truck, bus, or motorcycle. "Tiles"
adds four overlapping crops (each 60 % of the frame, 20 % overlap) to the
whole-frame pass, keeping the highest score. Recall / false-positive rate,
percent:

| Detector | Threshold | Person | Vehicle | Time per picture (p50) |
| --- | --- | --- | --- | --- |
| R18, whole frame | 0.4 | 84 / 8.3 | 74 / 6.0 | 14 ms |
| R18, whole frame | 0.5 | 79 / 3.4 | 68 / 1.5 | 14 ms |
| R18, whole frame | 0.7 | 59 / 0.4 | 47 / 0.5 | 14 ms |
| R18, whole frame + tiles | 0.5 | 96 / 7.2 | 78 / 5.0 | 53 ms |
| R18, whole frame + tiles | 0.6 | 87 / 2.3 | 72 / 3.0 | 53 ms |
| R50, whole frame | 0.5 | 84 / 3.4 | 78 / 4.5 | 23 ms |
| R50, whole frame + tiles | 0.5 | 98 / 8.3 | 86 / 8.5 | 93 ms |
| R50, whole frame + tiles | 0.6 | 96 / 4.5 | 83 / 4.5 | 93 ms |
| R18, whole frame + tiles 0.5, AND gemma4 | — | 96 / 0.8 | 77 / 2.0 | — |
| R18, whole frame + tiles 0.3, AND gemma4 | — | 98 / 4.5 | 91 / 6.5 | — |

Times exclude decoding the JPEG; scaling a 4K frame to 640x640 adds about
10 ms. The machine was shared (load average 4–7 of 14 cores).

- **Tiling is what fixes small people:** it added 16 points of person recall
  and 10 of vehicle recall at about four times the cost. Small, distant
  figures, not lighting, were the gap, as with Vision.
- **RT-DETR then gemma4 is the best pair measured.** RT-DETR removes most of
  gemma4's false alarms and gemma4 removes most of RT-DETR's, while recall
  stays at RT-DETR's. ministral as the confirmer caps person recall at its
  own 73 %. Vision adds nothing on person (RT-DETR already finds everything
  Vision finds) and a few points on vehicles.
- **Night is the weak spot:** at 0.5 on the whole frame, person precision is
  99 % by day and 76 % at night; vehicle recall falls from 74 % to 58 %.
- **No repeating static false person** at a fixed place in any viewpoint
  (unlike the earlier study's recording); one single-picture false person
  at 0.71 was a household object. Some vehicle false positives are real
  partial or distant vehicles the labels missed.
- **Animal classes are not usable ungated:** with no animal in any picture,
  tiling raised animal false positives to 15 % at 0.5, mostly "bird" in
  foliage.
- **Caveats:** thresholds were chosen on the same 400 pictures (no held-out
  set), so treat them as optimistic; stretching and letterboxing scored
  about the same, and stretching is the default.

## Cost per camera at 1–2 fps

From the real-time runs on this machine, for a 640x480 sub-stream:

| What runs | CPU per camera | Memory | GPU | Neural Engine |
| --- | --- | --- | --- | --- |
| decoding the sub-stream (already paid for viewing) | 0.2 % + 0.3 % in the decoder service | 26 MB | — | — |
| Vision human + animal, 1–2 fps | +1–1.4 % | +18 MB | none | yes |
| + a COCO detector for vehicles (YOLOv3-Tiny, RT-DETR), 1–2 fps | +0.6–1.5 % | +20–30 MB (a shared model costs this once per app, not per camera) | none | yes |

So 16 cameras with people, animal and vehicle detection at 2 fps cost about
a third of one core out of 14, and no GPU.

## Recommendation for the frame tap

1. **Sample at 2 fps by default, 1 fps under load.** At 2 fps a person
   crossing the picture in 2–3 s is seen in four or more pictures; cost scales
   linearly with the rate. Let the tap pick the picture already decoded for
   display (no extra decode), and drop rather than queue when inference is
   behind (the bounded, lossy rule of the extension points).
2. **Detectors: Vision's human and animal requests first**, for every camera
   without device AI. They are free to ship, cheap, ran on the Neural Engine
   by default, and raised no false alarms here. On labeled stills they find
   only about a third of people ("Recall on labeled stills"), so treat them
   as a precise trigger, not the only detector. Add **one COCO detector only
   where vehicles matter** (or "other objects"), with a high threshold (0.7)
   and suppression of boxes that never move (a static object that looks like
   a person is the typical false alarm, as with RT-DETR above).
3. **Model choice: RT-DETR converted by our script** (Apache-2.0 code and
   weights), for people as well as vehicles: R18 (39 MB, 14 ms) with tiles
   at 0.5 found 96 % of people on labeled stills, against Vision's 35 %. R50
   (83 MB, 23 ms) is better on vehicles. Confirm its hits with a
   vision-language model (gemma4) before notifying, failing open. Never AGPL
   models.
4. **Neural Engine only.** Set Vision's main-stage device to the Neural
   Engine explicitly and Core ML's compute units to `cpuAndNeuralEngine`.
   The GPU belongs to the renderer: GPU inference made it 57–80 % busy for one
   stream at full rate, and `all` split RT-DETR onto the GPU and was slower.
5. **No batching needed for now.** Vision has no batch API for these requests,
   and 16 cameras at 2 fps kept up with one request at a time. One shared
   model instance and one inference queue (serial, lower priority than
   decoding and drawing) per app, not per camera.
6. **Budget impact:** about 1.2–2 % of one core and 20–45 MB per camera at
   2 fps, no GPU. Add a row to the budgets for detection at M3 and measure
   energy with `powermetrics` (needs root) or the energy report there.
7. **Use device detections where they exist.** For cameras with their own AI,
   take the device's events and run no model, except for what the device does
   not detect.

## Open questions

- **Recall on video.** Picture-level recall on stills is measured above, for
  Vision, RT-DETR, and three vision-language models. Event-level recall
  needs recordings with real events. The cheapest ground truth
  is the device itself: record a sub-stream from a camera with device AI
  while logging its `people`/`vehicle`/`dog_cat` pushes (command 33) and use
  them as labels for vision-lab's event-level score, then check a sample by
  eye. Night (IR) and small, distant figures matter most.
- **Input size.** Vision scales 640x480 to each model's input (416x416 for
  YOLOv3, 640x640 for RT-DETR) by stretching. Letterboxing (`--scale-fit`,
  YOLOv3 only) and running on the main stream for small objects are untested.
- **Energy.** Not measured; `powermetrics` needs root. Neural Engine busy
  time is not visible without it either.
- **Real timestamps.** Annex-B files carry none; the tap will have camera
  time, so sampling there will be exact.
- **Model updates.** Whether newer permissive detectors (D-FINE, RT-DETRv2,
  both Apache-2.0) convert as cleanly and do better on night footage.
- **Windows.** The same measurement with ONNX Runtime and DirectML is still
  to do.

## Reproducing

```sh
cd tools/vision-lab
swift build -c release
B=.build/release/vision-lab
# cost of one inference on every picture
$B run REC.h264 --duration SECONDS --detectors human,animal --report out.json
# a live tile at 2 fps
$B run REC.h264 --duration SECONDS --realtime --max-seconds 120 --fps 2 \
  --detectors human,animal,coreml --model MODEL.mlpackage --units cpu-ane
# scoring: results for the labeled pictures, events from a sampled run
$B run REC.h264 --duration SECONDS --frames labels.csv --detectors ... --results labeled.jsonl
$B run REC.h264 --duration SECONDS --fps 2 --detectors ... --results fps2.jsonl
$B eval --labels labels.csv --results labeled.jsonl --events fps2.jsonl --source-fps FPS
# where Core ML runs a model
$B plan --model MODEL.mlpackage --units cpu-ane
```

Keep recordings, exported pictures, labels, results, and models under the
git-ignored `captures/` directory. `swift test` in `tools/vision-lab` runs the
pipeline on the synthetic fixtures only (set `VISION_LAB_MODEL` to a model
path to include a Core ML detector).
