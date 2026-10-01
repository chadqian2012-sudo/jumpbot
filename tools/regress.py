#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""离线回归：把 debug/ 下所有历史帧跑一遍识别，快速发现退化或新增的失败场景。

用法：
    python tools/regress.py            # 跑 debug/ 下全部帧
    python tools/regress.py --dir X    # 只跑指定目录

输出：
    - 每帧的识别结果（棋子/目标/沿轴距离/横向比）
    - 摘要：无目标帧、疑似异常帧（横向比过大、距离越界、棋子模板分过低）
    - 对无目标帧自动保存标注图到 debug/regress/，便于人眼确认
"""
from __future__ import annotations

import argparse
import sys
from pathlib import Path

import cv2

PROJECT_DIR = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(PROJECT_DIR))

import jumpbot as J  # noqa: E402  (需要先设置 sys.path)


UI_HINT = ("gameover", "state_start", "sample_000")

# 人工核对过的分数样本（帧文件名里的分数是我逐张看图记录的真值）：
# run_20260930_224614 那一轮，jump_NNN_raw_after.png ↔ 真值分数
VERIFIED_SCORES = {
    1: 2, 2: 6, 3: 12, 4: 20, 5: 21, 6: 23, 7: 27, 8: 33, 9: 41, 10: 51,
    11: 63, 12: 77, 13: 78, 14: 79, 15: 80, 16: 82, 17: 86, 18: 92, 19: 100, 20: 110,
}
VERIFIED_RUN = "run_20260930_224614"


def check_score_reader(cfg: J.Config) -> tuple:
    """用人工核对的真值验证读分器（这是命中率统计的基础，必须可靠）。"""
    run_dir = PROJECT_DIR / "debug" / VERIFIED_RUN
    if not run_dir.exists():
        return 0, 0
    ok = total = 0
    for idx, want in sorted(VERIFIED_SCORES.items()):
        path = run_dir / f"jump_{idx:03d}_raw_after.png"
        frame = cv2.imread(str(path))
        if frame is None:
            continue
        total += 1
        got = J.read_score(frame, cfg)
        if got == want:
            ok += 1
        else:
            print(f"  [读分错误] #{idx}: 期望 {want} 读出 {got}")
    return ok, total


def check_screen_classify(cfg: J.Config) -> tuple:
    """验证 界面/弹窗/游戏中 三态判定不互相误判。"""
    cases = [
        ("弹窗", PROJECT_DIR / "debug" / "state_now6" / "probe.png", "modal"),
        ("结算页", PROJECT_DIR / "debug" / "gameover" / "probe.png", "ui"),
        ("开始页", PROJECT_DIR / "debug" / "state_start" / "sample_000.png", "ui"),
        ("游戏中", PROJECT_DIR / "debug" / "newskin" / "sample_002.png", "play"),
        ("游戏帧", PROJECT_DIR / "debug" / "samples" / "sample_004.png", "play"),
    ]
    ok = total = 0
    for name, path, want in cases:
        frame = cv2.imread(str(path))
        if frame is None:
            continue
        total += 1
        state, _c, _s = J.classify_screen(frame)
        if state in (J.ScreenState.START, J.ScreenState.GAME_OVER):
            got = "ui"
        elif J.is_blocking_modal(frame, cfg):
            got = "modal"
        else:
            got = "play"
        if got == want:
            ok += 1
        else:
            print(f"  [界面判定错误] {name}: 期望 {want} 得到 {got}")
    return ok, total

# 只挑“真实游戏整帧”：排除掩码图、标注图、分数裁剪图、放大图等派生产物
FRAME_PATTERNS = ("probe.png", "fail_*.png", "jump_*_raw*.png", "dry_*.png",
                  "sample_*.png", "before.png", "after_*.png")
EXCLUDE_PARTS = ("regress", "annotated", "masks", "masks2", "score", "hud",
                 "digit", "center_check", "zoom", "chk_", "verify_", "piece_",
                 "block", "fail_mask", "fail2", "fail3", "fail5", "fail6", "fail7",
                 "now3_blockmask", "round_block", "edge_", "green_", "white_",
                 "start_block", "gray_", "dbg26")


def is_game_frame(path: Path) -> bool:
    text = str(path).lower()
    if any(part.lower() in text for part in EXCLUDE_PARTS):
        return False
    name = path.name.lower()
    return any(__import__("fnmatch").fnmatch(name, pat) for pat in FRAME_PATTERNS)


def has_block_above(frame, cfg) -> bool:
    """画面里是否存在"位于棋子上方、尺寸够格"的方块候选。

    为什么关键帧闸门需要它：`debug/fail_*.png` 是运行时自动保存的"识别连续失败"现场，
    里面混着**刚落地、镜头还在过渡**的瞬间帧——那种画面里所有方块都在棋子下方
    （下一块还没滚进画面），本来就不该有合法目标。实测
    `fail_20261001_132216.png`（分数 271）：唯一在上方的候选是画面顶部被裁掉的
    101x43 小装饰，尺寸不达标 → 整帧确实无目标。
    旧规则"所有 fail_*.png 都必须识别出目标"会把这类瞬时帧当成识别回归，属于误报。
    """
    vision = J.Vision(cfg)
    piece_mask, block_mask = vision.masks(frame)
    if block_mask is None:
        return False
    piece = vision.find_piece(frame, piece_mask)
    if piece is None:
        return False
    h, w = frame.shape[:2]
    num, _labels, stats, _cent = cv2.connectedComponentsWithStats(block_mask, 8)
    for i in range(1, num):
        _x, y, _bw, bh, area = stats[i]
        if area < cfg.block_min_area_ratio * w * h:
            continue
        if (y + bh / 2.0) < piece.y - cfg.target_min_up_ratio * h:
            return True
    return False


def main() -> int:
    parser = argparse.ArgumentParser(description="跳一跳识别离线回归")
    parser.add_argument("--dir", default=str(PROJECT_DIR / "debug"))
    parser.add_argument("--config", default=str(PROJECT_DIR / "config.json"))
    parser.add_argument("--verbose", action="store_true", help="打印每一帧")
    args = parser.parse_args()

    cfg = J.Config.load(Path(args.config))
    frames = sorted(p for p in Path(args.dir).rglob("*.png") if is_game_frame(p))
    if not frames:
        print(f"{args.dir} 下没有 png 帧")
        return 2

    out_dir = PROJECT_DIR / "debug" / "regress"
    ok = missing = 0
    suspicious: list = []
    no_target: list = []
    for path in frames:
        frame = cv2.imread(str(path))
        if frame is None:
            continue
        vision = J.Vision(cfg)
        piece, target, _pm, _bm = vision.detect(frame)
        ui = J.match_ui(frame)
        name = path.name
        is_ui_frame = any(h in str(path) for h in UI_HINT)
        # `*_raw_after.png` 是刚落地那一瞬（下一目标可能还没进入画面），
        # 不参与判定，仅提示。
        informational = name.endswith("_raw_after.png")
        if target is None:
            if is_ui_frame or ui is not None:
                if args.verbose:
                    print(f"  [界面] {name}（正常：界面页无目标）")
                continue
            if informational:
                if args.verbose:
                    print(f"  [落地帧·仅提示] {name}")
                continue
            missing += 1
            no_target.append(name)
            out_dir.mkdir(parents=True, exist_ok=True)
            vis = frame.copy()
            if piece is not None:
                x, y, w, h = piece.bbox
                cv2.rectangle(vis, (x, y), (x + w, y + h), (255, 0, 0), 2)
            cv2.imwrite(str(out_dir / f"notarget_{name}"), vis)
            print(f"  [无目标] {name}  piece={None if piece is None else (round(piece.x), int(piece.y))}")
            continue
        ok += 1
        _axis, along, cross = J.axis_components(target.x - piece.x, target.y - piece.y,
                                                cfg.axis_slope)
        ratio = cross / max(abs(along), 1.0)
        note = ""
        if ratio > cfg.axis_max_cross_ratio or along < cfg.min_jump_dist_ratio * frame.shape[1] \
                or along > cfg.max_jump_dist_ratio * frame.shape[1]:
            note = "  <-- 越界，可疑"
            suspicious.append(name)
        elif piece.tpl_score and piece.tpl_score < cfg.piece_template_min_score:
            note = f"  <-- 棋子模板分低({piece.tpl_score:.2f})"
            suspicious.append(name)
        if args.verbose or note:
            print(f"  {name}: piece=({piece.x:.0f},{piece.y:.0f},s={piece.tpl_score:.2f}) "
                  f"target=({target.x:.0f},{target.y:.0f}) 沿轴={along:.0f} 横比={ratio:.2f}{note}")

    # 关键帧闸门：历史 fail_*.png（除分享面板那张）必须能识别，基准帧中心必须不偏。
    # 全量帧统计里混有大量“修复前”的旧帧，只作参考，不参与判定。
    key_bad = []
    key_transient = []
    for path in sorted((PROJECT_DIR / "debug").glob("fail_*.png")):
        # 跳过：分享面板帧（本就应识别失败，现在会被遮挡安全闸拦下）与掩码类派生产物
        if "232538" in path.name or "mask" in path.name.lower():
            continue
        frame = cv2.imread(str(path))
        if frame is None:
            continue
        if J.Vision(cfg).detect(frame)[1] is None and J.match_ui(frame) is None:
            # 区分"识别回归"与"镜头过渡瞬时帧"：后者画面里没有任何位于棋子上方、
            # 尺寸够格的方块，本来就不该有目标（见 has_block_above 的说明）。
            if has_block_above(frame, cfg):
                key_bad.append(path.name)
            else:
                key_transient.append(path.name)
    baselines = [
        (PROJECT_DIR / "debug" / "samples" / "sample_004.png", (404, 501)),
        (PROJECT_DIR / "debug" / "state2" / "probe.png", (404, 501)),
    ]
    for path, want in baselines:
        frame = cv2.imread(str(path))
        if frame is None:
            continue
        target = J.Vision(cfg).detect(frame)[1]
        if target is None or abs(target.x - want[0]) > 3 or abs(target.y - want[1]) > 3:
            key_bad.append(f"{path.name}(基准偏移)")

    print()
    print(f"帧总数 {len(frames)}：识别成功 {ok}，无目标 {missing}，可疑 {len(suspicious)}（仅供参考）")
    score_ok, score_total = check_score_reader(cfg)
    if score_total:
        print(f"读分器（人工核对样本）：{score_ok}/{score_total} 正确")
    cls_ok, cls_total = check_screen_classify(cfg)
    if cls_total:
        print(f"界面/弹窗判定：{cls_ok}/{cls_total} 正确")
    if no_target:
        print(f"无目标帧（已存 debug/regress/）：{no_target}")
    if suspicious:
        print(f"可疑帧：{suspicious}")
    if key_transient:
        print(f"镜头过渡瞬时帧（无目标属正常，不计入闸门）：{key_transient}")
    print(f"关键帧闸门：{'全部通过' if not key_bad else '失败 ' + str(key_bad)}")
    all_ok = (not key_bad and score_ok == score_total and cls_ok == cls_total)
    return 0 if all_ok else 1


if __name__ == "__main__":
    raise SystemExit(main())
