#!/usr/bin/env python3
"""Per-frame comparison of nirlock-bench --dump and bench-opencv --dump.

usage: compare.py RUST.dump.jsonl OPENCV.dump.jsonl

Prints, over the frames both sides detected: landmark max |delta| (px),
box IoU, and the cosine between the Rust and OpenCV embeddings of the same
frame for AuraFace and SFace (M2 gate: landmarks <= 0.5 px, IoU >= 0.98,
cos >= 0.99 per frame). Reads only; writes nothing.
"""
import json
import math
import sys


def load(path):
    rows = {}
    with open(path) as f:
        for line in f:
            line = line.strip()
            if line:
                r = json.loads(line)
                rows[r["file"]] = r
    return rows


def cos(a, b):
    na = math.sqrt(sum(x * x for x in a))
    nb = math.sqrt(sum(x * x for x in b))
    return sum(x * y for x, y in zip(a, b)) / (na * nb)


def iou(a, b):
    x1, y1 = max(a["x"], b["x"]), max(a["y"], b["y"])
    x2 = min(a["x"] + a["w"], b["x"] + b["w"])
    y2 = min(a["y"] + a["h"], b["y"] + b["h"])
    inter = max(0.0, x2 - x1) * max(0.0, y2 - y1)
    return inter / (a["w"] * a["h"] + b["w"] * b["h"] - inter)


def summary(name, v, fmt="{:.5f}", worst=min):
    if not v:
        print(f"  {name:<14} n=0")
        return
    s = sorted(v)
    n = len(s)
    med = s[n // 2] if n % 2 else 0.5 * (s[n // 2 - 1] + s[n // 2])
    print(f"  {name:<14} n={n} worst={fmt.format(worst(s))} median={fmt.format(med)} best={fmt.format(max(s) if worst is min else min(s))}")


def main():
    rust, cv = load(sys.argv[1]), load(sys.argv[2])
    both = [f for f in rust if f in cv]
    det_r = {f for f in rust if rust[f]["face"]}
    det_c = {f for f in cv if cv[f]["face"]}
    print(f"frames: {len(both)} in both dumps; face in rust {len(det_r)}, in opencv {len(det_c)}, disagree {len(det_r ^ det_c)}")
    lm, ious, ca, cs, sc = [], [], [], [], []
    for f in both:
        r, c = rust[f], cv[f]
        if not (r["face"] and c["face"]):
            continue
        lm.append(max(max(abs(p[0] - q[0]), abs(p[1] - q[1]))
                      for p, q in zip(r["face"]["lm"], c["face"]["lm"])))
        ious.append(iou(r["face"], c["face"]))
        sc.append(abs(r["face"]["score"] - c["face"]["score"]))
        if r.get("auraface") and c.get("auraface"):
            ca.append(cos(r["auraface"], c["auraface"]))
        if r.get("sface") and c.get("sface"):
            cs.append(cos(r["sface"], c["sface"]))
    summary("landmark |d| px", lm, "{:.3f}", worst=max)
    summary("score |d|", sc, "{:.4f}", worst=max)
    summary("box IoU", ious, "{:.6f}")
    summary("auraface cos", ca, "{:.7f}")
    summary("sface cos", cs, "{:.7f}")
    bad = sum(1 for x in lm if x > 0.5) + sum(1 for x in ious if x < 0.98) + sum(1 for x in ca + cs if x < 0.99)
    print("M2 gate:", "PASS" if bad == 0 else f"FAIL ({bad} violations)")


if __name__ == "__main__":
    main()
