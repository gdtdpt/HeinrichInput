#!/usr/bin/env python3
"""M0a 可行性探针：能否用「拆字数据 + 反推字根表」复现五笔 86 编码。

思路（不手抄字根表）：
  1. 从码表取单字 → 码。
  2. 从拆字字典取 字 → 若干拆法（部件序列）。
  3. 若某个拆法恰好 4 个部件、且码表是 4 码，则按位对齐，统计「部件 → 键」共现，
     取多数作为该部件的键位（这就是数据驱动的字根表）。
  4. 用反推出的表重新编码，与码表比对，得到一致率。

只统计 4 部件 / 4 码的情形：不足四码需要「末笔识别码」，那是另一块（需要笔顺数据），
本探针单独报告这部分占比，不混进一致率。
"""

import json
import re
import sys
from collections import Counter, defaultdict
from pathlib import Path

RAW = Path(__file__).resolve().parents[2] / "data" / "raw"
OUT = Path(__file__).resolve().parents[2] / "data" / "derived"


def load_dict(path: Path) -> dict[str, str]:
    """解析 rime 词典：text<TAB>code[<TAB>weight]。只取单字。"""
    out: dict[str, str] = {}
    started = False
    for line in path.read_text(encoding="utf-8").splitlines():
        if line.strip() == "...":
            started = True
            continue
        if not started:
            continue
        if not line.strip() or line.startswith("#"):
            continue
        parts = line.split("\t")
        if len(parts) < 2:
            continue
        text, code = parts[0], parts[1].strip()
        if len(text) != 1:
            continue
        if not re.fullmatch(r"[a-y]{1,4}", code):
            continue
        # 同一字可能有多条（多音/多码），保留第一条（词频排序后通常最常用）
        out.setdefault(text, code)
    return out


def load_chaizi(path: Path) -> dict[str, list[list[str]]]:
    out: dict[str, list[list[str]]] = {}
    for line in path.read_text(encoding="utf-8").splitlines():
        if not line.strip():
            continue
        parts = line.split("\t")
        ch = parts[0]
        splits = [p.split() for p in parts[1:] if p.strip()]
        if splits:
            out[ch] = splits
    return out


def load_common(path: Path) -> set[str]:
    chars = set()
    for i, line in enumerate(path.read_text(encoding="utf-8").splitlines()):
        if i == 0:
            continue
        parts = line.split(",")
        if len(parts) >= 2 and parts[1]:
            chars.add(parts[1])
    return chars


def learn_table(dict_map, chaizi, charset):
    """从 4 部件 / 4 码的对齐中反推「部件 → 键」。"""
    votes: dict[str, Counter] = defaultdict(Counter)
    used = 0
    for ch in charset & dict_map.keys() & chaizi.keys():
        code = dict_map[ch]
        if len(code) != 4:
            continue
        for split in chaizi[ch]:
            if len(split) != 4:
                continue
            for comp, letter in zip(split, code):
                votes[comp][letter] += 1
            used += 1
            break  # 每个字只用第一个 4 部件的拆法
    table = {}
    for comp, cnt in votes.items():
        letter, n = cnt.most_common(1)[0]
        total = sum(cnt.values())
        table[comp] = {"key": letter, "confidence": n / total, "n": total}
    return table, used


def encode(split, table):
    keys = []
    for comp in split:
        info = table.get(comp)
        if info is None:
            return None, comp
        keys.append(info["key"])
    return "".join(keys), None


def main() -> int:
    dpath = RAW / "wubi86.dict.yaml"
    cpath = RAW / "chaizi-jt.txt"
    kpath = RAW / "common3500.csv"
    for p in (dpath, cpath, kpath):
        if not p.exists():
            print(f"缺少数据文件：{p}", file=sys.stderr)
            return 1

    dict_map = load_dict(dpath)
    chaizi = load_chaizi(cpath)
    common = load_common(kpath)

    common_in_dict = common & dict_map.keys()
    common_with_chaizi = common_in_dict & chaizi.keys()

    # 码长分布
    len_dist = Counter(len(dict_map[c]) for c in common_in_dict)

    # 拆分部件数分布
    split_len_dist = Counter()
    for c in common_with_chaizi:
        split_len_dist[min(len(s) for s in chaizi[c])] += 1

    table, aligned = learn_table(dict_map, chaizi, common_in_dict)

    # 评估：4 部件拆法能否复现 4 码
    ok = 0
    mismatch = []
    unresolvable = 0
    total_eval = 0
    for c in sorted(common_with_chaizi):
        code = dict_map[c]
        if len(code) != 4:
            continue
        best = None
        for split in chaizi[c]:
            if len(split) != 4:
                continue
            enc, missing = encode(split, table)
            if enc is None:
                unresolvable += 1
                break
            if best is None:
                best = enc
        total_eval += 1
        if best is None:
            continue
        if best == code:
            ok += 1
        else:
            mismatch.append({"char": c, "dict": code, "gen": best,
                             "split": " ".join(chaizi[c][0])})

    rate = ok / total_eval if total_eval else 0.0
    report = {
        "dict_single_chars": len(dict_map),
        "common3500_chars": len(common),
        "common_in_dict": len(common_in_dict),
        "common_with_chaizi": len(common_with_chaizi),
        "code_len_dist": dict(sorted(len_dist.items())),
        "min_split_len_dist": dict(sorted(split_len_dist.items())),
        "table_entries": len(table),
        "aligned_samples": aligned,
        "eval_4comp_4code": total_eval,
        "eval_matched": ok,
        "eval_rate": round(rate, 4),
        "unresolvable_component": unresolvable,
        "mismatch_samples": mismatch[:40],
    }
    OUT.mkdir(parents=True, exist_ok=True)
    (OUT / "probe-report.json").write_text(
        json.dumps(report, ensure_ascii=False, indent=2), encoding="utf-8")

    low_conf = {k: v for k, v in table.items() if v["confidence"] < 0.9}
    print(f"码表单字条目           : {len(dict_map)}")
    print(f"常用 3500 在码表内     : {len(common_in_dict)}")
    print(f"其中拆字字典有拆法     : {len(common_with_chaizi)}")
    print(f"码长分布（常用字）     : {dict(sorted(len_dist.items()))}")
    print(f"最短拆法部件数分布     : {dict(sorted(split_len_dist.items()))}")
    print(f"反推出的部件→键 条目   : {len(table)}  (低置信 <0.9: {len(low_conf)})")
    print(f"参与对齐的 4 部件样本  : {aligned}")
    print(f"可评估（4 部件+4 码）  : {total_eval}")
    print(f"复现成功               : {ok}   一致率 = {rate:.2%}")
    print(f"部件缺失无法评估       : {unresolvable}")
    if mismatch:
        print("\n不一致样例（前 15）：")
        for m in mismatch[:15]:
            print(f"  {m['char']}  码表={m['dict']}  生成={m['gen']}  拆={m['split']}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
