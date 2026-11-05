"""Write the output fixtures to tests/fixtures/output.

Usage (from the repository root):
    uv run --project tests/reference python tests/reference/generate.py

Check reproducibility with `git diff tests/fixtures/output` after a run.
The files hold data only, for machine reading. Sources, rounding rules
and the schema are in doc/design/output_driver/testing.md.
"""

import json
import sys
from pathlib import Path

import cases
import reference as ref
import spec

OUT = Path(__file__).resolve().parents[1] / "fixtures" / "output"


def dumps(obj, indent=0):
    """Indented JSON with one line per scalar list. Deterministic for equal input."""
    pad = " " * indent
    if isinstance(obj, dict):
        if not obj:
            return "{}"
        items = [f"{pad} {json.dumps(k)}: {dumps(v, indent + 1)}" for k, v in obj.items()]
        return "{\n" + ",\n".join(items) + "\n" + pad + "}"
    if isinstance(obj, list) and any(isinstance(v, (dict, list)) for v in obj):
        if all(isinstance(v, list) and not any(isinstance(x, (dict, list)) for x in v) for v in obj):
            items = [pad + " " + json.dumps(v, allow_nan=False) for v in obj]
        else:
            items = [pad + " " + dumps(v, indent + 1) for v in obj]
        return "[\n" + ",\n".join(items) + "\n" + pad + "]"
    return json.dumps(obj, allow_nan=False)


def finish(case):
    spec.validate(case)
    case["requires"] = spec.requires(case)
    case["expected"] = spec.expect(case)
    return case


def check_invalid(item):
    try:
        spec.validate(item["input"])
    except spec.FixtureError as e:
        if e.kind != item["error"]:
            raise AssertionError(f"{item['id']}: got {e.kind}, want {item['error']}") from e
        return item
    raise AssertionError(f"{item['id']}: accepted, want {item['error']}")


def build():
    """File name to bytes."""
    groups = {
        "codes": cases.codes_group(),
        "luminance": cases.luminance_group(),
        "gamut": cases.gamut_group(),
        "mixed": cases.mixed_group(),
        "geometry": cases.geometry_group(),
        "state": cases.state_group(),
    }
    files, seen = {}, set()
    for group, items in groups.items():
        done = [finish(c) for c in items]
        body = {"cases": done}
        if group == "state":
            frames = {c["id"]: c for c in done}
            body["sequences"] = [s for d in ref.DEPTHS for s in cases.sequences(d, frames)]
        files[f"{group}.json"] = body
        for c in done + body.get("sequences", []):
            if c["id"] in seen:
                raise AssertionError(f"duplicate id {c['id']}")
            seen.add(c["id"])
    files["scalar.json"] = cases.scalar_tables()
    files["invalid.json"] = {"cases": [check_invalid(i) for i in cases.invalid_group()]}
    return {name: (dumps(body) + "\n").encode("ascii") for name, body in files.items()}


def main():
    out = build()
    OUT.mkdir(parents=True, exist_ok=True)
    for p in OUT.glob("*.json"):
        if p.name not in out:
            p.unlink()
    for n, b in out.items():
        (OUT / n).write_bytes(b)
    print(f"wrote {len(out)} files to {OUT}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
