import copy
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest
from unittest.mock import patch

from impact import Model, ROOT, SUITES, changed, event_changes, successful_base, emit
from run import commands
from check import check_results


class SelectionTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.model = Model()

    def test_dependency_closure_selects_consumers_not_dependencies(self):
        plan = self.model.plan(["crates/app/src/calc.rs"])
        self.assertEqual(plan["rust"], ["wes", "wes-desktop"])
        self.assertEqual(plan["suites"], ["examples"])
        self.assertFalse(plan["full"])

    def test_core_change_selects_all_core_consumers_and_contract_suites(self):
        plan = self.model.plan(["crates/core/src/value.rs"])
        self.assertEqual(set(plan["rust"]), set(self.model.crates) - {"wes-budgets"})
        self.assertEqual(set(plan["suites"]), SUITES)

    def test_gui_change_does_not_run_engine_or_go_tests(self):
        plan = self.model.plan(["gui/src/surface/Cell.tsx"])
        self.assertEqual(plan["rust"], ["wes-desktop"])
        self.assertEqual(plan["suites"], ["gui"])

    def test_transport_change_includes_gui(self):
        plan = self.model.plan(["crates/app/src/web/value.rs"])
        self.assertEqual(plan["rust"], ["wes", "wes-desktop"])
        self.assertEqual(set(plan["suites"]), {"gui", "examples"})

    def test_gui_consumers_of_app_fixture_and_app_consumers_of_gui_code(self):
        plan = self.model.plan(["crates/app/tests/fixtures/stream-error.txt"])
        self.assertIn("gui", plan["suites"])
        plan = self.model.plan(["gui/src/terminal-input.ts"])
        self.assertEqual(plan["rust"], ["wes", "wes-desktop"])
        self.assertEqual(set(plan["suites"]), {"gui", "examples"})

    def test_extractor_change_includes_its_real_api_consumers(self):
        plan = self.model.plan(["tools/describe/internal/contract/openapi.go"])
        self.assertEqual(plan["rust"], ["wes", "wes-desktop"])
        self.assertEqual(set(plan["suites"]), {"extractor", "examples"})

    def test_shared_budget_change_reaches_rust_and_gui(self):
        plan = self.model.plan(["packages/budgets/catalog.json"])
        self.assertIn("wes-budgets", plan["rust"])
        self.assertIn("wes-desktop", plan["rust"])
        self.assertIn("gui", plan["suites"])

    def test_shared_view_source_reaches_native_and_browser_consumers(self):
        for path in ["views/timeline/View.tsx", "packages/view-sdk/index.ts", "tools/view-package/template/View.tsx"]:
            with self.subTest(path=path):
                plan = self.model.plan([path])
                self.assertIn("wes-views", plan["rust"])
                self.assertIn("wes", plan["rust"])
                self.assertEqual(set(plan["suites"]), {"gui", "compiler", "examples"})

    def test_examples_and_acceptance_fixtures_include_cross_language_consumers(self):
        for path in ["examples/quickstart/main.wes", "examples/api-import/readme/check.py", "tests/fixtures/catalog.types.yaml"]:
            with self.subTest(path=path):
                plan = self.model.plan([path])
                self.assertIn("wes-core", plan["rust"])
                self.assertIn("wes", plan["rust"])
                self.assertIn("gui", plan["suites"])
                self.assertIn("extractor", plan["suites"])

    def test_global_and_unclassified_changes_fail_closed(self):
        paths = ["Cargo.toml", "Cargo.lock", "rust-toolchain.toml", "crates/app/Cargo.toml",
                 "crates/views/build.rs", "package-lock.json", "gui/package.json", "tools/describe/go.mod",
                 ".github/workflows/ci.yml", "ci/tests.toml", "tools/ci/impact.py", "docs/testing.md",
                 "AGENTS.md", "new-module/source.py", ".cargo/config.toml", "../outside", "/absolute"]
        for path in paths:
            with self.subTest(path=path):
                plan = self.model.plan([path])
                self.assertTrue(plan["full"])
                self.assertEqual(set(plan["rust"]), set(self.model.crates))
                self.assertEqual(set(plan["suites"]), SUITES)

    def test_documentation_and_empty_diff_skip_expensive_jobs(self):
        for paths in [[], ["README.md"], ["docs/development.md", "LICENSE"]]:
            with self.subTest(paths=paths):
                plan = self.model.plan(paths)
                self.assertFalse(plan["full"])
                self.assertEqual(plan["rust"], [])
                self.assertEqual(plan["suites"], [])

    def test_literal_inputs_outside_crates_override_documentation_exclusion(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            shutil.copy(ROOT / "Cargo.toml", root / "Cargo.toml")
            (root / "ci").mkdir()
            shutil.copy(ROOT / "ci/tests.toml", root / "ci/tests.toml")
            for member in self.model.crates.values():
                (root / member).mkdir(parents=True)
                shutil.copy(ROOT / member / "Cargo.toml", root / member / "Cargo.toml")
            source = root / "crates/core/input.rs"
            for expression in ['include_str!("../../docs/contract.md")',
                               'include_str!(r#"../../docs/contract.md"#)',
                               'include!("../../docs/contract.md")',
                               '#[path = "../../docs/contract.md"] mod external;']:
                with self.subTest(expression=expression):
                    source.write_text(expression)
                    plan = Model(root).plan(["docs/contract.md"])
                    self.assertIn("wes-core", plan["rust"])
                    self.assertIn("gui", plan["suites"])

    def test_graph_includes_build_dev_and_target_dependencies(self):
        self.assertIn("wes-views", self.model.reverse["wes-core"])
        self.assertIn("wes-desktop", self.model.reverse["wes"])
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            shutil.copy(ROOT / "Cargo.toml", root / "Cargo.toml")
            (root / "ci").mkdir()
            shutil.copy(ROOT / "ci/tests.toml", root / "ci/tests.toml")
            for member in self.model.crates.values():
                (root / member).mkdir(parents=True)
                shutil.copy(ROOT / member / "Cargo.toml", root / member / "Cargo.toml")
            with (root / "crates/core/Cargo.toml").open("a") as stream:
                stream.write('\n[target.\'cfg(windows)\'.dev-dependencies]\nbudget-alias = {package = "wes-budgets", path = "../budgets"}\n')
            self.assertIn("wes-core", Model(root).closure({"wes-budgets"}))

    def test_every_tracked_input_is_accounted_for_or_forces_full(self):
        paths = subprocess.check_output(["git", "ls-files", "-z"], cwd=ROOT).decode().split("\0")
        for path in filter(None, paths):
            plan = self.model.plan([path])
            self.assertTrue(plan["full"] or plan["rust"] or plan["suites"] or plan["documentation"], path)

    def test_tracked_links_need_an_explicit_ownership_policy(self):
        index = subprocess.check_output(["git", "ls-files", "--stage", "-z"], cwd=ROOT).split(b"\0")
        links = [entry for entry in index if entry.startswith((b"120000 ", b"160000 "))]
        self.assertEqual(links, [], "Tracked links/submodules need ownership modeling before selective CI can use them")

    def test_unsupported_cargo_member_is_rejected(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / "ci").mkdir()
            shutil.copy(ROOT / "ci/tests.toml", root / "ci/tests.toml")
            (root / "Cargo.toml").write_text('[workspace]\nmembers = ["crates/*"]\n')
            with self.assertRaises(ValueError):
                Model(root)

    def test_runner_uses_argv_and_never_selects_unknown_packages(self):
        plan = self.model.plan(["gui/src/surface/Cell.tsx"])
        argv = commands("rust", plan, self.model)[0][1]
        self.assertIn("--workspace", argv)
        self.assertNotIn("wes-desktop", argv)  # it is not excluded
        self.assertIn("wes-engine", argv)
        self.assertEqual(commands("extractor", plan, self.model), [])
        plan["rust"] = ["--config=malicious"]
        with self.assertRaises(ValueError):
            commands("rust", plan, self.model)

    def test_full_plan_cannot_silently_omit_suites(self):
        plan = self.model.plan([], "full")
        plan["rust"].remove("wes")
        with self.assertRaises(ValueError):
            commands("rust", plan, self.model)

    def test_ci_outputs_match_actual_suites(self):
        with tempfile.TemporaryDirectory() as tmp:
            output = Path(tmp) / "outputs"
            with patch.dict("os.environ", {"GITHUB_STEP_SUMMARY": ""}), patch("builtins.print"):
                emit(self.model.plan(["gui/src/surface/Cell.tsx"]), output)
            values = dict(line.split("=", 1) for line in output.read_text().splitlines())
            self.assertEqual(values["client"], "true")
            self.assertEqual(values["engine"], "true")
            self.assertEqual(values["desktop"], "true")
            self.assertEqual(values["go_build"], "false")
            self.assertEqual(json.loads(values["plan"])["rust"], ["wes-desktop"])


class GitTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        self.git("init", "-q")
        self.git("config", "user.name", "Synthetic")
        self.git("config", "user.email", "synthetic@example.invalid")
        self.git("config", "commit.gpgsign", "false")
        self.path = self.root / "gui/file with spaces.ts"
        self.path.parent.mkdir()
        self.path.write_text("synthetic fixture\n")
        self.commit()
        self.base = self.git("rev-parse", "HEAD").strip()

    def git(self, *args):
        env = {key: value for key, value in os.environ.items() if not key.startswith("GIT_")}
        env.update(GIT_CONFIG_NOSYSTEM="1", GIT_CONFIG_GLOBAL=os.devnull)
        return subprocess.check_output(["git", "-C", str(self.root), "-c", "core.hooksPath=" + os.devnull, *args], stderr=subprocess.PIPE, env=env).decode()

    def commit(self):
        self.git("add", "-A")
        self.git("commit", "-qm", "synthetic change")

    def test_rename_keeps_old_and_new_ownership(self):
        dest = self.root / "docs/renamed.md"
        dest.parent.mkdir()
        self.path.rename(dest)
        self.commit()
        paths, reason = changed(self.root, self.base, "HEAD")
        self.assertEqual(set(paths), {"gui/file with spaces.ts", "docs/renamed.md"})
        self.assertIsNone(reason)
        self.assertIn("gui", Model().plan(paths)["suites"])

    def test_delete_only_is_not_an_empty_diff(self):
        self.path.unlink()
        self.commit()
        paths, _ = changed(self.root, self.base, "HEAD")
        self.assertEqual(paths, ["gui/file with spaces.ts"])

    def test_mode_only_is_a_real_change(self):
        self.path.chmod(0o755)
        self.commit()
        paths, _ = changed(self.root, self.base, "HEAD")
        self.assertEqual(paths, ["gui/file with spaces.ts"])

    def test_symlink_forces_full(self):
        self.path.unlink()
        self.path.symlink_to("../../external")
        self.commit()
        paths, reason = changed(self.root, self.base, "HEAD")
        self.assertEqual(paths, ["gui/file with spaces.ts"])
        self.assertIsNotNone(reason)

    def test_push_includes_changes_from_unverified_previous_push(self):
        self.path.write_text("second change\n")
        self.commit()
        before = self.git("rev-parse", "HEAD").strip()
        (self.root / "README.md").write_text("documentation\n")
        self.commit()
        event = {"ref": "refs/heads/main", "before": before}
        with patch("impact.successful_base", return_value=self.base):
            paths, reason = event_changes(self.root, "push", event, {})
        self.assertIsNone(reason)
        self.assertEqual(set(paths), {"gui/file with spaces.ts", "README.md"})

    def test_pr_includes_unverified_base_changes(self):
        self.path.write_text("base branch change\n")
        self.commit()
        merge_base = self.git("rev-parse", "HEAD").strip()
        (self.root / "README.md").write_text("PR docs\n")
        self.commit()
        with patch("impact.successful_base", return_value=self.base):
            paths, reason = event_changes(self.root, "pull_request", {"pull_request": {"base": {"sha": merge_base}}}, {})
        self.assertIsNone(reason)
        self.assertIn("gui/file with spaces.ts", paths)

    def test_missing_base_api_failure_and_force_push_fail_closed(self):
        events = [
            ("push", {"ref": "refs/heads/main", "before": "0" * 40}),
            ("push", {"ref": "refs/heads/main", "before": self.base, "forced": True}),
            ("push", {"ref": "refs/heads/main", "before": "f" * 40}),
            ("pull_request", {"pull_request": {"base": {"sha": "f" * 40}}}),
            ("push", {"ref": "refs/heads/main", "before": self.base}),
        ]
        for kind, event in events:
            with self.subTest(event=event), patch("impact.successful_base", side_effect=OSError("unavailable")):
                _, reason = event_changes(self.root, kind, event, {})
                self.assertIsNotNone(reason)

    def test_non_ancestor_push_base_fails_closed(self):
        self.git("checkout", "--orphan", "other")
        self.git("rm", "-rf", ".")
        (self.root / "unrelated").write_text("other history")
        self.commit()
        _, reason = event_changes(self.root, "push", {"ref": "refs/heads/main", "before": self.base}, {})
        self.assertIsNotNone(reason)

    def test_scheduled_manual_tag_and_merge_group_are_full(self):
        for kind in ["schedule", "workflow_dispatch", "merge_group", "push"]:
            _, reason = event_changes(self.root, kind, {"ref": "refs/tags/v0.1.0"}, {})
            self.assertIsNotNone(reason)

    def test_last_green_ignores_successful_pr_runs_and_missing_ancestors(self):
        runs = {"workflow_runs": [
            {"head_sha": self.base, "event": "pull_request"},
            {"head_sha": "f" * 40, "event": "push"},
            {"head_sha": self.base, "event": "push"},
        ]}
        with patch("impact.urllib.request.urlopen") as request:
            request.return_value.__enter__.return_value.read.return_value = json.dumps(runs).encode()
            result = successful_base(self.root, {"GITHUB_REPOSITORY": "example/wes", "GH_TOKEN": "synthetic"})
        self.assertEqual(result, self.base)

    def test_empty_successful_history_fails_closed(self):
        with patch("impact.urllib.request.urlopen") as request:
            request.return_value.__enter__.return_value.read.return_value = b'{"workflow_runs":[]}'
            with self.assertRaises(ValueError):
                successful_base(self.root, {"GITHUB_REPOSITORY": "example/wes", "GH_TOKEN": "synthetic"})


class GateTests(unittest.TestCase):
    def jobs(self):
        return {"plan": {"result": "success", "outputs": {"client": "false", "engine": "true"}},
                "client-and-extractor": {"result": "skipped"}, "engine-and-examples": {"result": "success"}}

    def test_only_explicitly_omitted_jobs_may_be_skipped(self):
        check_results(self.jobs())
        for result in ["skipped", "failure", "cancelled"]:
            jobs = self.jobs()
            jobs["engine-and-examples"]["result"] = result
            with self.assertRaises(ValueError):
                check_results(jobs)

    def test_failed_or_cancelled_planner_cannot_pass_the_gate(self):
        for result in ["failure", "cancelled", "skipped"]:
            jobs = self.jobs()
            jobs["plan"]["result"] = result
            with self.assertRaises(ValueError):
                check_results(jobs)


if __name__ == "__main__":
    unittest.main()
