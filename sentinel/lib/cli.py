from __future__ import annotations


def resolve_inputs(positional: list[str], optional: list[str] | None) -> list[str]:
    inputs = list(positional)
    if optional:
        inputs.extend(optional)
    if not inputs:
        raise SystemExit("No input paths provided.")
    return inputs
