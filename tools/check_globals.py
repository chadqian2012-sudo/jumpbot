#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""静态扫描：函数里给"模块级全局名"赋值却没写 global（=隐藏定时炸弹）。

为什么需要：2026-10-01 实测踩过一次——`run_daemon` 在 except 分支里写
`_LAST_STOP_REASON = ...` 却没声明 `global`，Python 于是把它当局部变量，函数里
另一处读取它就抛 `UnboundLocalError`，导致 **500 跳一轮正常跑完后 daemon 直接崩溃**，
现象是"没有第二轮、日志里也没有 [daemon] 本轮结束"。pyflakes 抓不到这类问题
（它不是语法错误），所以要单独扫。

判据：某名字在函数内既被赋值又被读取，同时它是模块级名字（含函数/类/import），
且该函数没有 `global <name>` —— 就报出来人工确认。

注意：**这是启发式**，"先赋值后读取"的局部遮蔽（例如函数里 `probe = Vision(cfg)`）
会被误报，需要人工看一眼。已知的两处无害误报见 README。

用法：
    python tools\\check_globals.py                 # 扫 jumpbot.py + tools/*.py
    python tools\\check_globals.py --exit-code      # 有命中就返回 1（可挂 CI）
"""
from __future__ import annotations

import argparse
import ast
import sys
from pathlib import Path

PROJECT_DIR = Path(__file__).resolve().parent.parent


def module_level_names(tree: ast.Module) -> set:
    names: set = set()
    for node in tree.body:
        if isinstance(node, ast.Assign):
            for tgt in node.targets:
                names |= {n.id for n in ast.walk(tgt) if isinstance(n, ast.Name)}
        elif isinstance(node, ast.AnnAssign) and isinstance(node.target, ast.Name):
            names.add(node.target.id)
        elif isinstance(node, (ast.Import, ast.ImportFrom)):
            for alias in node.names:
                names.add((alias.asname or alias.name).split(".")[0])
        elif isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef, ast.ClassDef)):
            names.add(node.name)
    return names


def scan(path: Path) -> list:
    tree = ast.parse(path.read_text(encoding="utf-8"), filename=str(path))
    globals_ = module_level_names(tree)
    hits = []
    for fn in [n for n in ast.walk(tree)
               if isinstance(n, (ast.FunctionDef, ast.AsyncFunctionDef))]:
        declared = set()
        assigned, loaded = set(), set()
        for node in ast.walk(fn):
            if isinstance(node, ast.Global):
                declared |= set(node.names)
            elif isinstance(node, ast.Name):
                (assigned if isinstance(node.ctx, ast.Store) else loaded).add(node.id)
        args = set()
        a = fn.args
        for x in (list(a.args) + list(a.posonlyargs) + list(a.kwonlyargs)
                  + ([a.vararg] if a.vararg else []) + ([a.kwarg] if a.kwarg else [])):
            args.add(x.arg)
        for name in sorted(assigned & loaded & globals_):
            if name in args or name in declared:
                continue
            hits.append((fn.name, fn.lineno, name))
    return hits


def main() -> int:
    try:
        sys.stdout.reconfigure(encoding="utf-8", errors="replace")
    except (AttributeError, ValueError):
        pass
    ap = argparse.ArgumentParser(description="扫描漏写 global 的隐患")
    ap.add_argument("--root", default=str(PROJECT_DIR))
    ap.add_argument("--exit-code", action="store_true")
    args = ap.parse_args()

    root = Path(args.root)
    targets = [root / "jumpbot.py"] + sorted((root / "tools").glob("*.py"))
    total = 0
    for path in targets:
        if not path.exists():
            continue
        try:
            hits = scan(path)
        except SyntaxError as exc:
            print(f"{path.name}: 解析失败 {exc}", file=sys.stderr)
            continue
        if not hits:
            continue
        print(f"== {path.name}")
        for fn, line, name in hits:
            print(f"   {fn}() 行{line}：赋值并读取模块级名 {name}（需人工确认是否漏 global）")
        total += len(hits)
    print(f"\n共 {total} 处待确认")
    return 1 if (args.exit_code and total) else 0


if __name__ == "__main__":
    raise SystemExit(main())
