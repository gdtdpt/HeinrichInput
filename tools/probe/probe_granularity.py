#!/usr/bin/env python3
"""M0a 探针第二步：确认「拆解粒度」不匹配是结构性问题，还是可以靠递归展开解决。

要回答两个问题：
  Q1 码长 × 拆法部件数 的分布矩阵 —— 若拆解是字根级，两者应大致同步增长。
  Q2 把 chaizi 的部件递归展开到底，叶子数会不会自然收敛到 3~4（五笔的字根数）？
     若收敛，递归路线可行；若叶子数一路膨胀到笔画级，说明 chaizi 的拆解轴与五笔不同。
"""

import json
from collections import Counter
from pathlib import Path

from probe_roots import load_chaizi, load_common, load_dict  # noqa: E402

RAW = Path(__file__).resolve().parents[2] / "data" / "raw"
OUT = Path(__file__).resolve().parents[2] / "data" / "derived"


def expand(ch: str, chaizi, depth: int, memo: dict[str, int]) -> int:
    """返回该字递归展开后的叶子数；无法再拆则记 1。"""
    if ch in memo:
        return memo[ch]
    if depth <= 0 or ch not in chaizi:
        memo[ch] = 1
        return 1
    split = chaizi[ch][0]
    total = sum(expand(c, chaizi, depth - 1, memo) for c in split)
    memo[ch] = total
    return total


def main() -> None:
    dict_map = load_dict(RAW / "wubi86.dict.yaml")
    chaizi = load_chaizi(RAW / "chaizi-jt.txt")
    common = load_common(RAW / "common3500.csv") & dict_map.keys() & chaizi.keys()

    matrix = Counter()
    for c in common:
        matrix[(len(dict_map[c]), len(chaizi[c][0]))] += 1

    print("Q1  码长 × 首拆部件数（常用字）")
    print("      部件数 →   2      3      4      5+")
    for clen in range(1, 5):
        row = [matrix.get((clen, n), 0) for n in (2, 3, 4)]
        more = sum(v for (cl, n), v in matrix.items() if cl == clen and n >= 5)
        print(f"    码长 {clen}    {row[0]:<6} {row[1]:<6} {row[2]:<6} {more}")

    print("\nQ2  递归展开后的叶子数分布（深度上限）")
    result = {}
    for depth in (1, 2, 3, 4):
        memo: dict[str, int] = {}
        dist = Counter()
        for c in common:
            dist[min(expand(c, chaizi, depth, memo), 9)] += 1
        top = dict(sorted(dist.items()))
        result[f"depth_{depth}"] = top
        print(f"    深度 {depth}: {top}")

    report = {
        "matrix_code_len_x_split_len": {
            f"{k[0]}_{k[1]}": v for k, v in sorted(matrix.items())
        },
        "recursive_leaf_dist": result,
    }
    OUT.mkdir(parents=True, exist_ok=True)
    (OUT / "probe-granularity.json").write_text(
        json.dumps(report, ensure_ascii=False, indent=2), encoding="utf-8")
    print(f"\n已写出 {OUT / 'probe-granularity.json'}")


if __name__ == "__main__":
    main()
