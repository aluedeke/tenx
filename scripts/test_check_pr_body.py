#!/usr/bin/env python3
"""Tests for scripts/check-pr-body.py, against the real PR template.

    python3 scripts/test_check_pr_body.py
"""
import importlib.util
import os
import re
import sys
import unittest

sys.dont_write_bytecode = True  # no scripts/__pycache__ from loading the script

HERE = os.path.dirname(os.path.abspath(__file__))
spec = importlib.util.spec_from_file_location("check_pr_body", os.path.join(HERE, "check-pr-body.py"))
cpb = importlib.util.module_from_spec(spec)
spec.loader.exec_module(cpb)

with open(os.path.join(HERE, "..", cpb.TEMPLATE), encoding="utf-8") as f:
    TEMPLATE = f.read()

ANSWERS = {
    "What and why": "Adds X, so that Y.",
    "What changed": "- `src/x.rs`: new.",
    "Contract changes": "None",
    "How it was tested": "`make test`: passed.",
    "Risks and follow-ups": "None",
}
IMAGE = "| did not exist | ![after](https://github.com/o/r/blob/abc/x.png?raw=true) |"


def filled(screenshots=None, tick=True, **answers):
    """The template with every section answered and every box ticked."""
    body = TEMPLATE
    for name, text in {**ANSWERS, **answers}.items():
        body = body.replace(f"## {name}\n", f"## {name}\n\n{text}\n", 1)
    if screenshots is None:
        body = body.replace("|  |  |", IMAGE)
    else:
        body = re.sub(r"\| Before \| After \|\n\| --- \| --- \|\n\|  \|  \|", screenshots, body)
    if tick:
        body = body.replace("- [ ]", "- [x]")
    return body


def errors(body):
    return cpb.check(body, TEMPLATE)


class CheckPrBody(unittest.TestCase):
    def test_complete_description_passes(self):
        self.assertEqual(errors(filled()), [])
        self.assertEqual(errors(filled(screenshots="No UI change")), [])

    def test_template_as_is_fails_on_every_section_and_box(self):
        e = errors(TEMPLATE)
        self.assertEqual(sum("Section" in x for x in e), 6)
        self.assertEqual(sum("not ticked" in x for x in e), 5)

    def test_empty_body(self):
        self.assertIn('Section "What and why" is missing.', errors(""))

    def test_unticked_box(self):
        body = filled().replace("- [x] AI review", "- [ ] AI review")
        self.assertEqual(len(errors(body)), 1)
        self.assertIn("not ticked", errors(body)[0])

    def test_ticks_outside_the_checklist_do_not_count(self):
        body = filled(tick=False)
        copies = "\n".join(l.replace("- [ ]", "- [x]") for l in TEMPLATE.splitlines() if l.startswith("- [ ]"))
        self.assertTrue(errors(body + "\n## Notes\n\n" + copies))
        self.assertTrue(errors(body.replace("## Checklist", "## Checklist\n\n```\n" + copies + "\n```")))

    def test_screenshot_needs_a_real_image(self):
        self.assertTrue(errors(filled(screenshots="| TODO.png | TODO |")))
        self.assertTrue(errors(filled(screenshots="No UI change in the column, but the header moved")))
        self.assertEqual(errors(filled(screenshots='<img src="https://x/y.png" width="400">')), [])

    def test_screenshot_needs_a_real_address(self):
        self.assertTrue(errors(filled(screenshots="| ![before](TODO) | <img src=\"\"> |")))

    def test_heading_may_end_in_punctuation(self):
        self.assertEqual(errors(filled().replace("## What and why", "## What and Why:")), [])

    def test_plus_list_marker(self):
        self.assertEqual(errors(filled().replace("- [x]", "+ [x]")), [])

    def test_messages_show_the_item_as_written(self):
        body = filled().replace("- [x] AI review", "- [ ] AI review")
        self.assertIn("`/code-review`", errors(body)[0])

    def test_none_only_where_allowed(self):
        self.assertTrue(errors(filled(**{"What and why": "None"})))
        self.assertTrue(errors(filled(**{"What changed": "- None"})))
        self.assertTrue(errors(filled(**{"How it was tested": "* TBD"})))
        self.assertEqual(errors(filled(**{"Contract changes": "- None."})), [])

    def test_heading_in_code_is_no_section(self):
        body = filled(**{"What and why": ""})
        self.assertTrue(errors("```\n## What and why\nfoo\n```\n" + body))

    def test_comment_marker_in_code_hides_nothing(self):
        body = filled(**{"What and why": "Fixes the `<!--` parser."})
        self.assertEqual(errors(body), [])
        body = filled(**{"What and why": "Fixes the parser:\n\n```\n<!-- x\n```"})
        self.assertEqual(errors(body), [])

    def test_unclosed_comment_hides_the_rest(self):
        self.assertTrue(errors(filled().replace("## What changed", "<!-- oops\n## What changed")))

    def test_windows_line_endings(self):
        self.assertEqual(errors(filled().replace("\n", "\r\n")), [])


if __name__ == "__main__":
    unittest.main()
