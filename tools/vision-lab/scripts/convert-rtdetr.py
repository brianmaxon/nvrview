#!/usr/bin/env python3
"""Convert RT-DETR (Apache-2.0 code and weights) to a Core ML package.

Usage: python3 -I convert-rtdetr.py OUTPUT.mlpackage [--model PekingU/rtdetr_r18vd] [--size 640]

Needs torch, transformers and coremltools. Downloads the weights from Hugging
Face (about 80 MB for rtdetr_r18vd) into the usual Hugging Face cache. Write
the output outside the repository (for example under captures/, which is
git-ignored): models are never committed.

The package takes a SIZE x SIZE RGB image and returns the decoder's raw
outputs: `logits` (1, queries, classes) and `pred_boxes` (1, queries, 4;
center x, center y, width, height, normalized, origin top left).
vision-lab decodes them (sigmoid, threshold); class names are stored in the
model's metadata under `labels`, comma-separated.
"""

import argparse
import json

import coremltools as ct
import numpy as np
import torch
from transformers import RTDetrForObjectDetection
from transformers.models.rt_detr import modeling_rt_detr

F = torch.nn.functional


def deformable_attention_rank5(
    self, hidden_states, attention_mask=None, encoder_hidden_states=None, encoder_attention_mask=None,
    position_embeddings=None, reference_points=None, spatial_shapes=None, spatial_shapes_list=None,
    level_start_index=None, output_attentions=False,
):
    """Multi-scale deformable attention for batch size 1 with no tensor above
    rank 5 (Core ML's limit). Same arithmetic as the stock module, with the
    batch dimension dropped."""
    if position_embeddings is not None:
        hidden_states = hidden_states + position_embeddings
    queries = hidden_states[0]
    nq = queries.shape[0]
    heads, levels, points = self.n_heads, self.n_levels, self.n_points
    head_dim = self.d_model // heads

    value = self.value_proj(encoder_hidden_states[0])
    if attention_mask is not None:
        value = value.masked_fill(~attention_mask[0][..., None], float(0))
    offsets = self.sampling_offsets(queries).view(nq, heads, levels, points, 2)
    weights = F.softmax(self.attention_weights(queries).view(nq, heads, levels * points), -1)

    ref = reference_points[0]
    if ref.shape[-1] == 4:
        locations = ref[:, None, :, None, :2] + offsets / points * ref[:, None, :, None, 2:] * 0.5
    else:
        normalizer = torch.stack([spatial_shapes[..., 1], spatial_shapes[..., 0]], -1)
        locations = ref[:, None, :, None, :] + offsets / normalizer[None, None, :, None, :]
    grids = 2 * locations - 1

    sampled = []
    start = 0
    for level, (height, width) in enumerate(spatial_shapes_list):
        v = value[start:start + height * width]
        start += height * width
        v = v.view(height * width, heads, head_dim).permute(1, 2, 0).reshape(heads, head_dim, height, width)
        grid = grids[:, :, level].permute(1, 0, 2, 3)
        sampled.append(F.grid_sample(v, grid, mode="bilinear", padding_mode="zeros", align_corners=False))
    stacked = torch.cat(sampled, dim=-1)
    out = (stacked * weights.permute(1, 0, 2).unsqueeze(1)).sum(-1)
    out = out.reshape(heads * head_dim, nq).transpose(0, 1)
    return self.output_proj(out)[None], weights


class Wrapper(torch.nn.Module):
    def __init__(self, model):
        super().__init__()
        self.model = model

    def forward(self, pixel_values):
        out = self.model(pixel_values=pixel_values, return_dict=True)
        return out.logits, out.pred_boxes


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("output")
    ap.add_argument("--model", default="PekingU/rtdetr_r18vd")
    ap.add_argument("--size", type=int, default=640)
    ap.add_argument("--precision", choices=["fp16", "fp32"], default="fp16")
    args = ap.parse_args()

    # A fixed anchor size makes the model precompute its anchors instead of
    # building them with in-place writes, which the converter cannot handle.
    model = RTDetrForObjectDetection.from_pretrained(
        args.model, anchor_image_size=[args.size, args.size]).eval()
    labels = [model.config.id2label[i] for i in range(len(model.config.id2label))]
    example = torch.rand(1, 3, args.size, args.size)
    with torch.no_grad():
        reference = Wrapper(model)(example)
        modeling_rt_detr.RTDetrMultiscaleDeformableAttention.forward = deformable_attention_rank5
        patched = Wrapper(model)(example)
        for a, b in zip(reference, patched):
            diff = (a - b).abs().max().item()
            if diff > 1e-3:
                raise SystemExit(f"patched attention differs from the original by {diff}")
        traced = torch.jit.trace(Wrapper(model).eval(), example, strict=False)

    # The Hugging Face image processor only rescales to 0..1 (no mean/std).
    mlmodel = ct.convert(
        traced,
        inputs=[ct.ImageType(name="image", shape=example.shape, scale=1 / 255.0,
                             color_layout=ct.colorlayout.RGB)],
        outputs=[ct.TensorType(name="logits"), ct.TensorType(name="pred_boxes")],
        minimum_deployment_target=ct.target.macOS15,
        compute_precision=ct.precision.FLOAT16 if args.precision == "fp16" else ct.precision.FLOAT32,
    )
    mlmodel.short_description = f"{args.model} converted with coremltools; raw DETR outputs"
    mlmodel.license = "Apache-2.0 (RT-DETR code and weights)"
    mlmodel.user_defined_metadata["labels"] = ",".join(labels)
    mlmodel.user_defined_metadata["source"] = args.model
    mlmodel.save(args.output)
    print(json.dumps({"saved": args.output, "classes": len(labels)}))


if __name__ == "__main__":
    main()
