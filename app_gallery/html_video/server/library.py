from __future__ import annotations

import json
import os
import re
from dataclasses import dataclass
from pathlib import Path
from typing import Any


INDEX_FILE = "index.json"
DEFAULT_HTML_FILE_NAMES = ("template.html", "frame.html", "index.html", "index.htm")
DEFAULT_SKILL_FILE_NAMES = ("SKILL.md", "skill.md")


class LibraryError(Exception):
    pass


class RootNotFound(LibraryError):
    def __str__(self) -> str:
        return "html video library root not found"


class TemplateNotFound(LibraryError):
    def __init__(self, template_id: str) -> None:
        super().__init__(template_id)
        self.template_id = template_id

    def __str__(self) -> str:
        return f"html video template not found: {self.template_id}"


class InvalidIndex(LibraryError):
    def __init__(self, path: Path, reason: str) -> None:
        super().__init__(path, reason)
        self.path = path
        self.reason = reason

    def __str__(self) -> str:
        return f"invalid index in {self.path}: {self.reason}"


class LibraryJsonError(LibraryError):
    def __init__(self, error: json.JSONDecodeError) -> None:
        super().__init__(error)
        self.error = error

    def __str__(self) -> str:
        return f"html video library JSON error: {self.error}"


@dataclass(frozen=True)
class IndexTemplate:
    id: str
    title: str
    intent: str = ""
    summary: str | None = None
    tags: tuple[str, ...] = ()


def resolve_root() -> Path | None:
    raw = os.environ.get("TEMPLATES_DIR")
    if raw:
        candidate = Path(raw)
        if _is_library_root(candidate):
            return candidate

    server_dir = Path(__file__).resolve().parent
    for candidate in (server_dir / "templates", server_dir.parent / "templates"):
        if _is_library_root(candidate):
            return candidate

    return None


def search(intent: str, top: int) -> list[dict[str, Any]]:
    root = resolve_root()
    if root is None:
        raise RootNotFound()

    tokens = tokenize(intent)
    query_is_empty = len(tokens) == 0
    results: list[dict[str, Any]] = []
    for item in _load_index(root):
        item_score = score(item.intent, tokens, item)
        if not query_is_empty and item_score == 0.0:
            continue
        results.append(
            {
                "id": item.id,
                "title": item.title,
                "intent": item.intent,
                "summary": item.summary,
                "tags": list(item.tags),
                "score": item_score,
            }
        )

    results.sort(key=lambda item: (-item["score"], item["id"], item["title"]))
    if top > 0:
        del results[top:]
    return results


def get_template(template_id: str) -> dict[str, Any]:
    root = resolve_root()
    if root is None:
        raise RootNotFound()

    items = _load_index(root)
    item = next((candidate for candidate in items if candidate.id == template_id), None)
    if item is None:
        raise TemplateNotFound(template_id)

    template_dir = _resolve_template_dir(root, item.id)
    metadata = {
        "id": item.id,
        "title": item.title,
        "intent": item.intent,
        "summary": item.summary,
        "tags": list(item.tags),
    }
    return {
        "metadata": metadata,
        "html": _read_template_html(template_dir),
        "skill_md": _read_template_skill_md(template_dir),
    }


def tokenize(value: str) -> list[str]:
    return [
        token
        for token in (
            re.sub(r"^\W+|\W+$", "", chunk, flags=re.UNICODE).lower()
            for chunk in value.split()
        )
        if token
    ]


def score(intent: str, tokens: list[str], item: IndexTemplate) -> float:
    if not tokens:
        return 1.0

    total = 0.0
    item_id = item.id.lower()
    title = item.title.lower()
    intent_index = intent.lower()
    summary = (item.summary or "").lower()
    tags = " ".join(item.tags).lower()

    for token in tokens:
        if token in item_id:
            total += 10.0
        if token in title:
            total += 6.0
        if token in intent_index:
            total += 4.0
        if summary and token in summary:
            total += 1.5
        if token in tags:
            total += 0.5

    return total


def _is_library_root(path: Path) -> bool:
    return (path / INDEX_FILE).is_file()


def _load_index(root: Path) -> list[IndexTemplate]:
    index_path = root / INDEX_FILE
    try:
        raw = index_path.read_text()
    except FileNotFoundError as error:
        raise InvalidIndex(index_path, "missing index") from error

    try:
        parsed = json.loads(raw)
    except json.JSONDecodeError as error:
        raise LibraryJsonError(error) from error

    if isinstance(parsed, list):
        raw_items = parsed
    elif isinstance(parsed, dict):
        templates = parsed.get("templates") or []
        items = parsed.get("items") or []
        raw_items = templates if templates else items
    else:
        raise InvalidIndex(index_path, "expected array or object")

    return [_parse_index_template(item, index_path) for item in raw_items]


def _parse_index_template(raw: Any, index_path: Path) -> IndexTemplate:
    if not isinstance(raw, dict):
        raise InvalidIndex(index_path, "template entry must be an object")

    try:
        template_id = raw["id"]
        title = raw["title"]
    except KeyError as error:
        raise InvalidIndex(index_path, f"missing template field: {error.args[0]}") from error

    if not isinstance(template_id, str) or not isinstance(title, str):
        raise InvalidIndex(index_path, "template id and title must be strings")

    tags = raw.get("tags", [])
    if tags is None:
        tags = []
    if not isinstance(tags, list):
        raise InvalidIndex(index_path, "template tags must be an array")

    summary = raw.get("summary")
    if summary is not None and not isinstance(summary, str):
        raise InvalidIndex(index_path, "template summary must be a string")

    intent = raw.get("intent", "")
    if not isinstance(intent, str):
        raise InvalidIndex(index_path, "template intent must be a string")

    return IndexTemplate(
        id=template_id,
        title=title,
        intent=intent,
        summary=summary,
        tags=tuple(str(tag) for tag in tags),
    )


def _resolve_template_dir(root: Path, template_id: str) -> Path:
    package_dir = root / "templates" / template_id
    if package_dir.is_dir():
        return package_dir

    direct_dir = root / template_id
    if direct_dir.is_dir():
        return direct_dir

    raise TemplateNotFound(template_id)


def _read_template_html(template_dir: Path) -> str:
    for name in DEFAULT_HTML_FILE_NAMES:
        candidate = template_dir / name
        if candidate.is_file():
            return candidate.read_text()
    raise TemplateNotFound(template_dir.name)


def _read_template_skill_md(template_dir: Path) -> str:
    for name in DEFAULT_SKILL_FILE_NAMES:
        candidate = template_dir / name
        if candidate.is_file():
            return candidate.read_text()
    raise TemplateNotFound(template_dir.name)
