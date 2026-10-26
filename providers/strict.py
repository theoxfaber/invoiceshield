"""Strict parsing of model output. The model is never trusted to follow a schema.

`extract_json` accepts exactly one JSON object, optionally inside a ```json
fence. Anything else (prose around two objects, truncation, arrays, NaN) is a
parse failure, which callers turn into REVIEW.
"""
from __future__ import annotations

import json
import re

from pydantic import BaseModel, ValidationError

_FENCE = re.compile(r"^```(?:json)?\s*\n(.*)\n```\s*$", re.S)
MAX_LEN = 64 * 1024


def _no_constants(name):
    raise ValueError(f"non-standard JSON constant {name}")


def extract_json(text: str | None) -> tuple[dict | None, str | None]:
    if text is None:
        return None, "empty output"
    t = text.strip()
    if not t:
        return None, "empty output"
    if len(t) > MAX_LEN:
        return None, "output too large"
    m = _FENCE.match(t)
    if m:
        t = m.group(1).strip()
    try:
        obj = json.loads(t, parse_constant=_no_constants)
    except (json.JSONDecodeError, ValueError) as e:
        return None, f"not a single JSON object: {str(e)[:120]}"
    if not isinstance(obj, dict):
        return None, "top level is not an object"
    return obj, None


def parse_model(text_or_obj, model: type[BaseModel]):
    """Return (instance, None) or (None, error). Never raises."""
    if isinstance(text_or_obj, dict):
        obj, err = text_or_obj, None
    else:
        obj, err = extract_json(text_or_obj)
    if err:
        return None, err
    try:
        return model.model_validate(obj, strict=True), None
    except ValidationError as e:
        return None, f"schema: {e.errors()[0]['type']} at {e.errors()[0]['loc']}"
