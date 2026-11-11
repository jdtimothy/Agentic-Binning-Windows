#!/usr/bin/env python3
"""Stands in for the GenieX CLI in tests: answers the commands the app runs, in the CLI's shapes.

It checks the things `geniex infer` itself would reject, so a test fails if the app builds a
command the real CLI would not accept (see cli/cmd/geniex/infer.go in qualcomm/GenieX).
"""
import json
import os
import re
import sys

args = sys.argv[1:]
log = os.environ.get("FAKE_GENIEX_LOG")
if log:
    with open(log, "a", encoding="utf-8") as f:
        f.write(json.dumps(args) + "\n")

MODELS = [
    {"name": "LiquidAI/LFM3-VL-2B-GGUF", "size": 1500000000, "runtime": "llama_cpp", "type": "vlm", "precisions": ["Q4_0"]},
    {"name": "LiquidAI/LFM3-1.7B-GGUF", "size": 1050000000, "runtime": "llama_cpp", "type": "llm", "precisions": ["Q4_0"]},
]

if "--version" in args:
    print("geniex version v0.10.0-fake")
    sys.exit(0)
if "list" in args:
    print(json.dumps(MODELS))
    sys.exit(0)
if "infer" not in args:
    print("Error: unknown command", file=sys.stderr)
    sys.exit(1)


def value(flag):
    return args[args.index(flag) + 1] if flag in args else None


model = args[args.index("infer") + 1]
for flag in ("--compute", "--nctx", "--max-tokens", "--temperature", "--top-k", "--top-p", "--min-p", "--repetition-penalty", "--grammar-path", "-i"):
    if value(flag) is None:
        print(f"Error: missing {flag}", file=sys.stderr)
        sys.exit(1)
if "--think=false" not in args or float(value("--min-p")) <= 0:
    print("Error: thinking must be off and min-p above zero", file=sys.stderr)
    sys.exit(1)
grammar = open(value("--grammar-path"), encoding="utf-8").read()
if not grammar.startswith("root"):
    print("Error: bad grammar", file=sys.stderr)
    sys.exit(1)
prompt = open(value("-i"), encoding="utf-8").read().strip()
for i, line in enumerate(prompt.split("\n")):
    print(("> " if i == 0 else ". ") + line)

if "VL" in model:
    # The CLI's own pattern for image paths; each must exist, and is removed from the text.
    pattern = re.compile(r"(?:[a-zA-Z]:)?(?:\./|/|\\)[\S\\ ]+?\.(?i:jpg|jpeg|png|webp|mp3|wav)\b")
    images = pattern.findall(prompt)
    for image in images:
        if not os.path.isfile(image):
            print(f"Error: file not found: {image}")
            sys.exit(1)
    text = pattern.sub("", prompt).strip()
    if not images or "/" in text or value("-s") is None:
        print("Error: Oops. No image or no system prompt.", file=sys.stderr)
        sys.exit(1)
    video = text.startswith("Video:")
    reply = {
        "description": f"A real-looking reply for {len(images)} image(s).",
        "actions": [{"t": "0:00", "text": "It starts"}, {"t": "0:02", "text": "It goes on"}] if video else [],
        "tags": ["fake", "geniex", "clip" if video else "still"],
        "suggested_name": "Fake Clip" if video else "Fake Still",
        "shot_type": "wide",
    }
else:
    lines = prompt.split("\n")
    start = next(i for i, l in enumerate(lines) if l.startswith("Items to sort"))
    items = []
    for line in lines[start + 1:]:
        if not line.strip():
            break
        cols = line.split("\t")
        items.append({"id": int(cols[0]), "category": "Fakes", "name": cols[2] + " sorted"})
    existing = "Fakes\t" in prompt
    reply = {"new_categories": [] if existing else [{"name": "Fakes", "description": "Everything the fake sorted"}], "assignments": items}

out = json.dumps(reply)
print()
sys.stdout.write(out)
sys.stdout.flush()
print("\n")
print("total time:     1.000000s\nttft:           0.100000s\nprompt time:    0.100000s\nprompt tokens:  300 token(s)\nprompt speed:   3000.000000 tok/s\ndecode time:    0.900000s\ndecode tokens:  40 token(s)\ndecode speed:   44.400000 tok/s\nstop reason:    eos\n")
if "--test-mode" in args:
    profile = {"TTFT": 100000, "MediaTime": 0, "PromptTime": 100000, "DecodeTime": 900000, "PromptTokens": 300, "TotalPromptTokens": 300, "GeneratedTokens": 40, "PrefillSpeed": 3000.0, "DecodingSpeed": 44.4, "DraftNTotal": 0, "DraftNAccepted": 0, "StopReason": "eos"}
    print(json.dumps({"Input": prompt, "Output": out, "Profile": profile, "Error": None}), file=sys.stderr)
