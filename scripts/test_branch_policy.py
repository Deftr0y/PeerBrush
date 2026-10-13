"""Branch-policy regressions; independent of engine and desktop prerequisites."""

import unittest

from check_branch_policy import validate_event, validate_name, validate_pull_request


class BranchPolicyTests(unittest.TestCase):
    def test_allowed_branches(self):
        for name in ("main", "dev", "site-content", "version/0.26", "version/0.26.1",
                     "feature/0.26/selection", "fix/0.26/save-locks",
                     "maintenance/archive-v0.1.2", "maintenance/site-development-guide"):
            with self.subTest(name=name):
                validate_name(name)

    def test_forbidden_names(self):
        for name in ("codex/0.26", "Codex/topic", "CODEX", "version/0.26/feature1",
                     "feature/selection", "DEV", "feature/0.26/with spaces"):
            with self.subTest(name=name), self.assertRaises(ValueError):
                validate_name(name)

    def test_promotion_destinations(self):
        for head, base in (("feature/0.26/selection", "version/0.26"),
                           ("fix/0.26/save-locks", "version/0.26"),
                           ("version/0.26", "dev"), ("dev", "main"),
                           ("maintenance/site-development-guide", "site-content")):
            with self.subTest(head=head, base=base):
                validate_pull_request(head, base)

    def test_wrong_destinations(self):
        for head, base in (("feature/0.26/selection", "main"),
                           ("feature/0.26/selection", "dev"),
                           ("feature/0.27/selection", "version/0.26"),
                           ("version/0.26", "main"), ("main", "dev"),
                           ("maintenance/archive-v0.1.2", "site-content")):
            with self.subTest(head=head, base=base), self.assertRaises(ValueError):
                validate_pull_request(head, base)

    def test_event_uses_pr_head_not_merge_ref(self):
        validate_event({"pull_request": {"head": {"ref": "feature/0.26/selection"},
                                         "base": {"ref": "version/0.26"}}}, "refs/pull/5/merge")
        with self.assertRaises(ValueError):
            validate_event({"pull_request": {"head": {"ref": "codex/0.26"},
                                             "base": {"ref": "version/0.26"}}}, "refs/pull/5/merge")

    def test_push_and_tag_events(self):
        validate_event({}, "refs/heads/dev")
        validate_event({}, "refs/tags/v0.26.0")
        with self.assertRaises(ValueError):
            validate_event({}, "refs/heads/codex/0.26")
        with self.assertRaises(ValueError):
            validate_event({}, "refs/pull/5/merge")


if __name__ == "__main__":
    unittest.main()
