#!/usr/bin/env python3
"""PR description check: the description must follow the PR template.

    scripts/check-pr-body.py [body-file]     the body from a file, or from the
                                              PR_BODY environment variable (CI)

Rules, read from .github/pull_request_template.md so the two never disagree:
  - every `## ` section of the template is in the body, and not empty
    (HTML comments and the empty screenshot table don't count as text);
  - "None" is an answer only where the template allows it;
  - "Screenshots" holds an image, or is just "No UI change";
  - every checklist item of the template is ticked (`- [x]`) in the body's
    checklist section.
Code (fenced blocks and `inline`) counts as text, never as a heading, a
checkbox or a comment. A heading may end in punctuation ("What and why:").

CI runs it on every change to a PR, its description included
(.github/workflows/pr-description.yml); the check is required for merging.
Tests: scripts/test_check_pr_body.py.
"""
import os
import re
import sys

TEMPLATE = ".github/pull_request_template.md"
SCREENSHOTS = "screenshots"
NO_UI = "no ui change"
# Sections where "None" is an answer; elsewhere it is a placeholder.
NONE_OK = {"contract changes", "risks and follow-ups"}
PLACEHOLDER = {"none", "n/a", "na", "-", "tbd", "todo"}

FENCE = re.compile(r"^\s*(```|~~~)")
INLINE_CODE = re.compile(r"`+[^`\n]*`+")
# An unclosed comment hides the rest of the page, so it counts to the end.
COMMENT = re.compile(r"<!--.*?(-->|\Z)", re.S)
HEADING = re.compile(r"^##\s+(.+?)[\s:?.!#]*$")
CHECKBOX = re.compile(r"^\s*[-*+]\s+\[( |x|X)\]\s+(.+?)\s*$")
BULLET = re.compile(r"^([-*+]|\d+\.)\s+")
EMPTY_ROW = re.compile(r"^\|[\s|:-]*\|$")
# Only what GitHub really shows: an embedded image with a real address, or a
# browser upload.
MEDIA = re.compile(
    r"!\[[^\]]*\]\(\s*https?://\S+\)|<(img|video)\s[^>]*src=[\"']?https?://|user-attachments/assets/",
    re.I,
)


def norm(text):
    return " ".join(text.split()).lower()


def plain_code(span):
    """Inline code as text, without comment markers, which GitHub shows."""
    return span.group(0).replace("<!--", "").replace("-->", "")


def without_code(text):
    """The text with fenced code replaced by a neutral word and inline code
    made harmless, as GitHub shows it."""
    out, fenced = [], False
    for line in text.splitlines():
        if FENCE.match(line):
            fenced = not fenced
            out.append("code")
        elif fenced:
            out.append("code")
        else:
            out.append(INLINE_CODE.sub(plain_code, line))
    return "\n".join(out)


def sections(text):
    """`## ` heading (normalized) → its lines, in order. A repeated heading
    is not a new section: its lines are added to the first one."""
    out, current = {}, None
    for line in COMMENT.sub("", without_code(text)).splitlines():
        m = HEADING.match(line)
        if m:
            current = norm(m.group(1))
            out.setdefault(current, [])
        elif current is not None:
            out[current].append(line)
    return out


def answer(lines, skeleton):
    """The section's text, without whitespace, list markers and the
    template's own table."""
    kept = []
    for line in lines:
        s = BULLET.sub("", line.strip())
        if s and not EMPTY_ROW.match(s) and norm(s) not in skeleton:
            kept.append(s)
    return norm(" ".join(kept))


def checklist(lines):
    """Checklist item text (normalized) → (text as written, ticked?), in order."""
    items = {}
    for line in lines:
        m = CHECKBOX.match(line)
        if m:
            items[norm(m.group(2))] = (m.group(2), m.group(1) != " ")
    return items


def short(text):
    return text if len(text) <= 60 else text[:57].rstrip() + "..."


def check(body, template):
    errors = []
    # The template's code (e.g. `make test`) is compared as the body's is.
    want, got = sections(template), sections(body)
    # Lines of the template's sections that are not an answer, e.g. the
    # header row "| Before | After |" of the screenshot table.
    skeleton = {norm(l) for lines in want.values() for l in lines if l.strip().startswith("|")}

    for name, lines in want.items():
        title = name.capitalize()
        wanted = checklist(lines)
        if name not in got:
            errors.append(f'Section "{title}" is missing.')
            continue
        if wanted:
            ticked = checklist(got[name])
            for key, (item, _) in wanted.items():
                if key not in ticked:
                    errors.append(f'Checklist item is missing: "{short(item)}"')
                elif not ticked[key][1]:
                    errors.append(f'Checklist item is not ticked: "{short(item)}"')
            continue
        text = answer(got[name], skeleton)
        bare = text.strip(".")
        if name == SCREENSHOTS:
            if bare != NO_UI and not MEDIA.search("\n".join(got[name])):
                errors.append('Section "Screenshots" has no image. Add before and after screenshots, or write just "No UI change".')
        elif not text:
            errors.append(f'Section "{title}" is empty. Fill it in, or write "None" where the template allows it.')
        elif name not in NONE_OK and bare in PLACEHOLDER:
            errors.append(f'Section "{title}" needs a real answer, not "{text}".')
    return errors


def main():
    if len(sys.argv) > 1:
        with open(sys.argv[1], encoding="utf-8") as f:
            body = f.read()
    else:
        body = os.environ.get("PR_BODY", "")
    root = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..")
    with open(os.path.join(root, TEMPLATE), encoding="utf-8") as f:
        template = f.read()

    errors = check(body, template)
    if errors:
        print("The PR description does not follow .github/pull_request_template.md:\n", file=sys.stderr)
        for e in errors:
            print(f"  - {e}", file=sys.stderr)
        print("\nEdit the PR description; this check runs again on every edit.", file=sys.stderr)
        return 1
    print("PR description is complete.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
