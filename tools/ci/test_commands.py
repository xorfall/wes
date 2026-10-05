"""Check launcher arguments against the examples' actual CLI parsers."""
import argparse
import ast
from pathlib import Path
import unittest

from impact import Model, ROOT
from run import commands


class ExampleCommandTests(unittest.TestCase):
    def test_view_examples_accept_the_runner_arguments_before_any_build(self):
        model = Model()
        plan = model.plan([], "full verification")
        checked = set()
        for directory, argv in commands("examples", plan, model):
            source = ROOT / argv[1]
            if source.parent.name not in {"view-packages", "view-instances"}:
                continue
            with self.subTest(example=source.parent.name):
                # Execute only the real argparse declarations, not the script's
                # runtime, subprocesses or services. A new required option must
                # therefore fail this cheap planner check too.
                nodes = []
                for node in ast.parse(source.read_text(), filename=str(source)).body:
                    if isinstance(node, ast.Assign) and any(
                        isinstance(target, ast.Name) and target.id == "parser" for target in node.targets
                    ):
                        nodes.append(node)
                    elif (isinstance(node, ast.Expr) and isinstance(node.value, ast.Call)
                          and isinstance(node.value.func, ast.Attribute)
                          and isinstance(node.value.func.value, ast.Name)
                          and node.value.func.value.id == "parser"
                          and node.value.func.attr == "add_argument"):
                        nodes.append(node)
                namespace = {"argparse": argparse, "Path": Path}
                exec(compile(ast.Module(body=nodes, type_ignores=[]), str(source), "exec"), namespace)
                parsed = namespace["parser"].parse_args(argv[2:])
                binary = parsed.binary
                if not binary.is_absolute():
                    binary = directory / binary
                self.assertEqual(binary, ROOT / "target/debug/wes")
                checked.add(source.parent.name)
        self.assertEqual(checked, {"view-packages", "view-instances"})


if __name__ == "__main__":
    unittest.main()
