# -*- coding: utf-8 -*-
"""生成 M3 总验收语料：crates/heinrich-core/tests/corpus/mixed.tsv

用法（在仓库根目录跑，读 data/ 下的词库，覆盖写出语料）：python tools/gen-corpus.py
改词库之后重跑即可刷新语料；语料本身是**提交进仓库**的，这个脚本只是它的来历说明。
"""
import io, collections

syll = set()
for ln in io.open('data/pinyin/syllables.txt', encoding='utf-8'):
    ln = ln.strip()
    if ln and not ln.startswith('#'):
        syll.add(ln)

wubi = []           # (code, word, weight, short)
for ln in io.open('data/wubi86/lexicon.tsv', encoding='utf-8'):
    if ln.startswith('#') or not ln.strip():
        continue
    f = ln.rstrip('\n').split('\t')
    wubi.append((f[0], f[1], int(f[2]), int(f[3])))

py = []             # (pinyin, word, weight)
for ln in io.open('data/pinyin/lexicon.tsv', encoding='utf-8'):
    if ln.startswith('#') or not ln.strip():
        continue
    f = ln.rstrip('\n').split('\t')
    py.append((f[0], f[1], int(f[2])))

wubi_codes = {c for c, _, _, _ in wubi}
full = {}           # char -> 全码（字典序最小，与 attach_wubi_codes 同约定）
for c, w, _, s in wubi:
    if len(w) == 1 and s == 0:
        if w not in full or c < full[w]:
            full[w] = c
py_char = collections.defaultdict(set)   # char -> 可能读音
py_word = {}
for p, w, wt in py:
    if len(w) == 1:
        py_char[w].add(p)
    else:
        cur = py_word.get(w)
        if cur is None or wt > cur[1]:
            py_word[w] = (p, wt)


def segmentable(s):
    n = len(s)
    ok = [False] * (n + 1)
    ok[0] = True
    for i in range(n):
        if not ok[i]:
            continue
        for j in range(i + 1, min(n, i + 6) + 1):
            if s[i:j] in syll:
                ok[j] = True
    return ok[n]


def spread(lst, n):
    if len(lst) <= n:
        return lst
    step = len(lst) / n
    return [lst[int(i * step)] for i in range(n)]


# ---- 纯五笔 50：25 条「长得像拼音」（验 D48 五笔优先）+ 25 条不像 ----
two_char = [e for e in wubi if len(e[1]) == 2 and len(e[0]) == 4]
two_char.sort(key=lambda e: -e[2])
like = [e for e in two_char if segmentable(e[0])]
unlike = [e for e in two_char if not segmentable(e[0])]
wubi_cases = [(e, 'like') for e in spread(like, 25)] + [(e, 'plain') for e in spread(unlike, 25)]

# ---- 纯拼音 50：2 字词，拼音串不是五笔精确码；前 25 用高频、后 25 分散 ----
pin_pool = []
for w, (p, wt) in py_word.items():
    if len(w) != 2 or len(p) < 5 or p in wubi_codes:
        continue
    if not segmentable(p):
        continue
    pin_pool.append((w, p, wt))
pin_pool.sort(key=lambda e: -e[2])
pinyin_cases = spread(pin_pool[:200], 25) + spread(pin_pool[200:], 25)

# ---- 交替 50：2 字词，段 1 = 字 1 的读音，段 2 = 字 2 的全码 ----
mix_pool = []
for w, (p, wt) in py_word.items():
    if len(w) != 2:
        continue
    c1, c2 = w[0], w[1]
    if c2 not in full or c1 not in py_char:
        continue
    hit = None
    for i in range(1, len(p)):
        a, b = p[:i], p[i:]
        if a in py_char[c1] and b in py_char[c2]:
            hit = a
            break
    if hit is None:
        continue
    mix_pool.append((w, hit, full[c2], wt))
mix_pool.sort(key=lambda e: -e[3])
mix_cases = spread(mix_pool[:200], 25) + spread(mix_pool[200:], 25)

lines = [
    '# 亨利 / Heinrich —— 混输总验收语料（M3 总验收，150 例）',
    '# 生成方式：从真实词库分层取样（前 25 高频、后 25 全量分散），不是手挑的「容易过」的样例。',
    '# format: code<TAB>expect_source<TAB>expect_word<TAB>kind<TAB>note',
    '#   expect_source: wubi / pinyin / empty（empty 不校验词）',
    '#   kind: wubi-like（码恰好也切得成拼音 → 验 D48 五笔优先）/ wubi / pinyin / mixed',
]
rows = []
for (c, w, wt, s), tag in wubi_cases:
    rows.append((c, 'wubi', w, 'wubi-like' if tag == 'like' else 'wubi', ''))
for w, p, wt in pinyin_cases:
    rows.append((p, 'pinyin', w, 'pinyin', ''))
for w, a, c2, wt in mix_cases:
    rows.append((a + "'" + c2, 'pinyin', w, 'mixed', ''))
# 负例：拼不出真词的组合必须不出窗（D64）
for code in ("hao'ni'", "hao'ktnq", "vb'kkkk"):
    rows.append((code, 'empty', '', 'empty', 'D64 无真词'))
for r in rows:
    lines.append('\t'.join(r))
io.open('crates/heinrich-core/tests/corpus/mixed.tsv', 'w', encoding='utf-8', newline='\n').write('\n'.join(lines) + '\n')
print('rows', len(rows))
for k in ('wubi-like', 'wubi', 'pinyin', 'mixed', 'empty'):
    print(' ', k, sum(1 for r in rows if r[3] == k))
